//! What `records export` asks an Epic organization for, in order: the
//! patient, every search a patient-facing app may make (Epic splits several
//! resources by category), each `DocumentReference`'s and
//! `DiagnosticReport`'s `Binary` right after it, then the medications,
//! practitioners, organizations and locations the records point at.
//!
//! A refused search, an unreadable answer or a file on another server is a
//! skip in the trailer, never the end of the export; only a sign-in that
//! stops working, a network that does, or a stream that cannot be written
//! ends it.

use std::collections::{BTreeSet, HashSet};

use pk_cli_core::transport::Transport;
use pk_cli_core::CliError;
use serde_json::Value;

use crate::fhir::{Failure, Fhir, RawResource};
use pk_med_records::{RecordsWriter, Skip};

/// The searches, as (resource type, extra parameters, whether `--since`
/// narrows it by `date`). The categories are the ones Epic's
/// CapabilityStatement documents for each resource; one an organization
/// does not serve answers 400 and becomes an `unsupported` skip.
pub type SearchPlan = (&'static str, &'static [(&'static str, &'static str)], bool);

pub const SEARCHES: &[SearchPlan] = &[
    ("AllergyIntolerance", &[], false),
    ("Condition", &[("category", "problem-list-item")], false),
    ("Condition", &[("category", "encounter-diagnosis")], false),
    ("Condition", &[("category", "health-concern")], false),
    ("Observation", &[("category", "laboratory")], true),
    ("Observation", &[("category", "vital-signs")], true),
    ("Observation", &[("category", "social-history")], true),
    ("Observation", &[("category", "core-characteristics")], true),
    ("Observation", &[("category", "LDA")], true),
    ("Observation", &[("category", "LDA-property")], true),
    ("Observation", &[("category", "LDA-assessment")], true),
    (
        "Observation",
        &[("category", "functional-mental-status")],
        true,
    ),
    ("Observation", &[("category", "periodontal")], true),
    ("Observation", &[("category", "labor-delivery")], true),
    ("Observation", &[("category", "newborn-delivery")], true),
    (
        "Observation",
        &[("category", "obstetrics-gynecology")],
        true,
    ),
    ("Observation", &[("category", "survey")], true),
    ("Observation", &[("category", "sdoh")], true),
    ("Observation", &[("category", "imaging")], true),
    (
        "Observation",
        &[("category", "care-experience-preference")],
        true,
    ),
    ("DiagnosticReport", &[], true),
    ("DocumentReference", &[("category", "clinical-note")], true),
    ("DocumentReference", &[], true),
    ("Encounter", &[], true),
    ("Immunization", &[], true),
    ("MedicationRequest", &[], false),
    ("MedicationDispense", &[], false),
    ("Procedure", &[], true),
    // Epic's Procedure categories include SDOH intervention.
    ("Procedure", &[("category", "410606002")], true), // SNOMED CT SDOH intervention
    // CarePlan requires a category: longitudinal, encounter-level, and US
    // Core's assess-plan.
    ("CarePlan", &[("category", "38717003")], false), // SNOMED CT longitudinal
    ("CarePlan", &[("category", "734163000")], false), // SNOMED CT encounter-level
    ("CarePlan", &[("category", "assess-plan")], false),
    ("CareTeam", &[], false),
    ("Goal", &[], false),
    ("Coverage", &[], false),
    ("Device", &[], false),
    ("ServiceRequest", &[], false),
    ("Specimen", &[], false),
    ("Media", &[], false),
    ("RelatedPerson", &[], false),
];

/// Types read by reference once the searches are done.
pub const REFERENCED: &[&str] = &[
    "Medication",
    "Practitioner",
    "PractitionerRole",
    "Organization",
    "Location",
];

/// Reads by reference one export makes at most.
pub const REFERENCE_CAP: usize = 2000;

/// The query for one search.
pub fn params(
    patient: &str,
    extra: &[(&str, &str)],
    dated: bool,
    since: Option<&str>,
) -> Vec<(String, String)> {
    let mut p = vec![("patient".to_string(), patient.to_string())];
    p.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    if let (true, Some(d)) = (dated, since) {
        p.push(("date".into(), format!("ge{d}")));
    }
    p
}

fn describe(resource_type: &str, params: &[(String, String)]) -> String {
    let q: Vec<String> = params
        .iter()
        .filter(|(k, _)| k != "patient")
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if q.is_empty() {
        resource_type.to_string()
    } else {
        format!("{resource_type}?{}", q.join("&"))
    }
}

/// The types `--since` leaves unbounded, for the header's
/// `since_unbounded`: the Patient, every type with a search that carries no
/// date, and the types read by reference.
pub fn since_unbounded() -> Vec<String> {
    let mut types: BTreeSet<&str> = BTreeSet::from(["Patient"]);
    types.extend(
        SEARCHES
            .iter()
            .filter(|(_, _, dated)| !dated)
            .map(|(t, _, _)| *t),
    );
    types.extend(REFERENCED.iter().copied());
    types.into_iter().map(str::to_string).collect()
}

/// `Binary/<id>` for an attachment URL on this server, relative or absolute.
pub fn binary_id(url: &str) -> Option<String> {
    let tail = url
        .trim_end_matches('/')
        .rsplit_once("Binary/")
        .map(|(_, id)| id)?;
    (!tail.is_empty() && !tail.contains('/') && !tail.contains('?')).then(|| tail.to_string())
}

/// The attachment URLs of a DocumentReference (`content[].attachment`) or
/// a DiagnosticReport (`presentedForm[]`).
pub fn attachment_urls(v: &Value) -> Vec<String> {
    let mut urls = Vec::new();
    for c in v["content"].as_array().into_iter().flatten() {
        if let Some(u) = c.pointer("/attachment/url").and_then(Value::as_str) {
            urls.push(u.to_string());
        }
    }
    for f in v["presentedForm"].as_array().into_iter().flatten() {
        if let Some(u) = f["url"].as_str() {
            urls.push(u.to_string());
        }
    }
    urls
}

/// Relative references (`Type/id`) to the types in [`REFERENCED`].
pub fn references(v: &Value, out: &mut BTreeSet<(String, String)>) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get("reference") {
                if let Some((t, id)) = r.split_once('/') {
                    if REFERENCED.contains(&t) && !id.is_empty() && !id.contains('/') {
                        out.insert((t.to_string(), id.to_string()));
                    }
                }
            }
            for v in map.values() {
                references(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| references(v, out)),
        _ => {}
    }
}

fn skip_of(
    resource_type: &str,
    id: Option<&str>,
    search: Option<String>,
    code: &str,
    reason: String,
) -> Skip {
    Skip {
        resource_type: Some(resource_type.to_string()),
        id: id.map(str::to_string),
        search,
        reason,
        code: code.to_string(),
    }
}

/// Run the export into `w`. `note` hears one line per search.
pub fn run<T: Transport, W: std::io::Write>(
    fhir: &Fhir<'_, T>,
    w: &mut RecordsWriter<W>,
    patient: &str,
    since: Option<&str>,
    note: &dyn Fn(String),
) -> Result<(), CliError> {
    let mut refs = BTreeSet::new();
    let mut tried_binaries = HashSet::new();
    match fhir.read(&format!("Patient/{patient}")) {
        Ok(p) => {
            if let Ok(v) = p.value() {
                // generalPractitioner, managingOrganization.
                references(&v, &mut refs);
            }
            w.resource(&p)?;
        }
        Err(Failure::Skip { code, reason }) => {
            w.skip(skip_of("Patient", Some(patient), None, code, reason));
        }
        Err(f) => return Err(f.into_error()),
    }

    for (resource_type, extra, dated) in SEARCHES {
        let p = params(patient, extra, *dated, since);
        let search = describe(resource_type, &p);
        let mut n = 0usize;
        let result = fhir.search_each(resource_type, &p, |r| {
            let value = r.value().ok();
            // A resource another search already wrote has had its files
            // fetched after it.
            if !w.resource(&r)? {
                return Ok(());
            }
            n += 1;
            if let Some(v) = &value {
                references(v, &mut refs);
                if matches!(
                    r.resource_type.as_str(),
                    "DocumentReference" | "DiagnosticReport"
                ) {
                    binaries(fhir, w, v, &mut tried_binaries)?;
                }
            }
            Ok(())
        });
        match result {
            Ok(summary) => {
                if summary.malformed > 0 {
                    w.skip(skip_of(
                        resource_type,
                        None,
                        Some(search.clone()),
                        "upstream",
                        format!(
                            "{} search entries were not FHIR resources",
                            summary.malformed
                        ),
                    ));
                }
                if summary.truncated {
                    w.skip(skip_of(
                        resource_type,
                        None,
                        Some(search.clone()),
                        "page_cap",
                        format!("stopped after {} pages", summary.pages),
                    ));
                }
                note(format!("{search}: {n}"));
            }
            Err(Failure::Skip { code, reason }) => {
                note(format!("{search}: skipped ({code})"));
                w.skip(skip_of(resource_type, None, Some(search), code, reason));
            }
            Err(f) => return Err(f.into_error()),
        }
    }

    let mut read = 0;
    for (t, id) in refs {
        if w.written(&t, &id) {
            continue;
        }
        if read == REFERENCE_CAP {
            w.skip(skip_of(
                &t,
                None,
                None,
                "page_cap",
                format!("stopped after {REFERENCE_CAP} reads by reference"),
            ));
            break;
        }
        read += 1;
        match fhir.read(&format!("{t}/{id}")) {
            Ok(r) => {
                w.resource(&r)?;
            }
            Err(Failure::Skip { code, reason }) => {
                w.skip(skip_of(&t, Some(&id), None, code, reason))
            }
            Err(f) => return Err(f.into_error()),
        }
    }
    if read > 0 {
        note(format!("referenced resources: {read}"));
    }
    Ok(())
}

/// Each attachment's Binary, written right after the resource naming it.
/// A Binary is fetched once per export: one that failed stays a skip rather
/// than landing later after some other resource.
fn binaries<T: Transport, W: std::io::Write>(
    fhir: &Fhir<'_, T>,
    w: &mut RecordsWriter<W>,
    v: &Value,
    tried: &mut HashSet<String>,
) -> Result<(), Failure> {
    for url in attachment_urls(v) {
        let Some(id) = binary_id(&url) else {
            w.skip(skip_of(
                "Binary",
                None,
                None,
                "unsupported",
                "an attachment URL that is not a Binary on this server".into(),
            ));
            continue;
        };
        if !tried.insert(id.clone()) {
            continue;
        }
        match fhir.binary(&url) {
            Ok(b) => {
                // A Binary without an id is written with the one its
                // attachment URL names. A Binary holds no decimal, so the
                // round trip through a Value changes nothing else.
                let b = if b.id.is_empty() {
                    let mut v = b.value()?;
                    v["id"] = id.clone().into();
                    RawResource::from_value(&v)?
                } else {
                    b
                };
                w.resource(&b)?;
            }
            Err(Failure::Skip { code, reason }) => {
                w.skip(skip_of("Binary", Some(&id), None, code, reason))
            }
            Err(f) => return Err(f),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unbounded_types_are_the_undated_searches_and_the_reads() {
        let types = since_unbounded();
        let mut sorted = types.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(types, sorted, "sorted, once each");
        assert!(types.contains(&"Patient".to_string()));
        for (t, _, dated) in SEARCHES {
            // A type searched both ways is still unbounded by its undated search.
            let undated = SEARCHES.iter().any(|(u, _, d)| u == t && !d);
            assert_eq!(types.contains(&t.to_string()), undated || !dated, "{t}");
        }
        for t in REFERENCED {
            assert!(types.contains(&t.to_string()), "{t}");
        }
        assert!(!types.contains(&"Binary".to_string()));
    }

    #[test]
    fn binary_ids_come_from_relative_and_absolute_urls() {
        assert_eq!(binary_id("Binary/abc").as_deref(), Some("abc"));
        assert_eq!(
            binary_id("https://fhir.example.org/api/FHIR/R4/Binary/e1.2-x").as_deref(),
            Some("e1.2-x")
        );
        assert_eq!(binary_id("https://elsewhere.example/doc.pdf"), None);
        assert_eq!(binary_id("Binary/"), None);
    }

    #[test]
    fn references_are_collected_for_the_followed_types_only() {
        let v: Value = serde_json::json!({
            "medicationReference": {"reference": "Medication/m1"},
            "requester": {"reference": "Practitioner/p1"},
            "subject": {"reference": "Patient/x"},
            "performer": [{"actor": {"reference": "Organization/o1"}}],
        });
        let mut out = BTreeSet::new();
        references(&v, &mut out);
        let got: Vec<String> = out.iter().map(|(t, i)| format!("{t}/{i}")).collect();
        assert_eq!(got, ["Medication/m1", "Organization/o1", "Practitioner/p1"]);
    }

    #[test]
    fn since_narrows_dated_searches_only() {
        assert_eq!(
            params("p", &[("category", "laboratory")], true, Some("2026-01-01")),
            [
                ("patient".into(), "p".into()),
                ("category".into(), "laboratory".into()),
                ("date".into(), "ge2026-01-01".into())
            ]
        );
        assert_eq!(params("p", &[], false, Some("2026-01-01")).len(), 1);
    }
}
