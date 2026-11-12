//! The `records-stream/v1` writer: a header line, one FHIR resource per
//! line, and a `records-stream-end/v1` trailer with counts and skips.
//!
//! Field for field the format medrec-cli reads and medical-common's
//! `pk_med_records::stream` defines; this module goes when that crate is
//! pinned.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use crate::fhir::{RawResource, MAX_LINE_BYTES};

pub const STREAM_SCHEMA: &str = "records-stream/v1";
pub const END_SCHEMA: &str = "records-stream-end/v1";
pub const FHIR_VERSION: &str = "4.0.1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub schema: String,
    pub source: SourceCli,
    pub organization: StreamOrganization,
    pub patient: StreamPatient,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub pulled_at: String,
    /// Always written; a reader takes an absent one as 4.0.1.
    #[serde(default = "default_fhir_version")]
    pub fhir_version: String,
}

fn default_fhir_version() -> String {
    FHIR_VERSION.into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCli {
    pub cli: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOrganization {
    pub id: String,
    pub name: String,
    pub fhir_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPatient {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    /// `patient`: mychart reads providers, never insurers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The signed-in user's `fhirUser` (`Patient/…`, or `RelatedPerson/…`
    /// for a parent's proxy access).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_user: Option<String>,
}

/// One thing the export could not carry, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Skip {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The search that was refused (`Observation?category=survey`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    pub reason: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trailer {
    pub schema: String,
    pub complete: bool,
    pub counts: BTreeMap<String, u64>,
    pub skipped: Vec<Skip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TrailerError>,
}

/// Why a stream stopped: piekstra-cli/1's error code and the exit code the
/// CLI ended with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrailerError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl TrailerError {
    pub fn from_error(e: &CliError) -> Self {
        TrailerError {
            code: e.code().into(),
            message: e.message().to_string(),
            exit_code: Some(e.exit_code()),
        }
    }
}

pub struct RecordsWriter<W: Write> {
    out: W,
    counts: BTreeMap<String, u64>,
    skipped: Vec<Skip>,
    seen: HashSet<(String, String)>,
}

fn io(e: std::io::Error) -> CliError {
    CliError::Other(format!("writing the records stream: {e}"))
}

impl<W: Write> RecordsWriter<W> {
    /// Write the header line.
    pub fn new(mut out: W, header: &Header) -> Result<Self, CliError> {
        let line = serde_json::to_string(header).map_err(|e| CliError::Other(e.to_string()))?;
        writeln!(out, "{line}").map_err(io)?;
        Ok(RecordsWriter {
            out,
            counts: BTreeMap::new(),
            skipped: Vec::new(),
            seen: HashSet::new(),
        })
    }

    /// Write one resource unless it was written already (by type and id).
    /// Returns whether it was written.
    pub fn resource(&mut self, r: &RawResource) -> Result<bool, CliError> {
        if !r.id.is_empty() && !self.seen.insert((r.resource_type.clone(), r.id.clone())) {
            return Ok(false);
        }
        if r.raw.len() > MAX_LINE_BYTES {
            self.skip(Skip {
                resource_type: Some(r.resource_type.clone()),
                id: Some(r.id.clone()).filter(|i| !i.is_empty()),
                search: None,
                reason: format!(
                    "{} bytes, over the {MAX_LINE_BYTES}-byte line limit",
                    r.raw.len()
                ),
                code: "too_large".into(),
            });
            return Ok(false);
        }
        self.out.write_all(r.raw.as_bytes()).map_err(io)?;
        self.out.write_all(b"\n").map_err(io)?;
        *self.counts.entry(r.resource_type.clone()).or_insert(0) += 1;
        Ok(true)
    }

    pub fn written(&self, resource_type: &str, id: &str) -> bool {
        self.seen
            .contains(&(resource_type.to_string(), id.to_string()))
    }

    pub fn skip(&mut self, s: Skip) {
        if !self.skipped.contains(&s) {
            self.skipped.push(s);
        }
    }

    pub fn counts(&self) -> &BTreeMap<String, u64> {
        &self.counts
    }

    /// Write the trailer. `error` makes the stream incomplete.
    pub fn finish(
        mut self,
        finished_at: String,
        error: Option<&CliError>,
    ) -> Result<Trailer, CliError> {
        let trailer = Trailer {
            schema: END_SCHEMA.into(),
            complete: error.is_none(),
            counts: std::mem::take(&mut self.counts),
            skipped: std::mem::take(&mut self.skipped),
            finished_at: Some(finished_at),
            error: error.map(TrailerError::from_error),
        };
        let line = serde_json::to_string(&trailer).map_err(|e| CliError::Other(e.to_string()))?;
        writeln!(self.out, "{line}").map_err(io)?;
        self.out.flush().map_err(io)?;
        Ok(trailer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn header() -> Header {
        Header {
            schema: STREAM_SCHEMA.into(),
            source: SourceCli {
                cli: "mychart".into(),
                version: "0.0.0".into(),
            },
            organization: StreamOrganization {
                id: "1000-1".into(),
                name: "Example Health".into(),
                fhir_base: "https://fhir.example.org/api/FHIR/R4".into(),
                vendor: Some("epic".into()),
            },
            patient: StreamPatient {
                id: "pat-0001".into(),
                organization: Some("1000-1".into()),
                label: None,
                relationship: Some("self".into()),
                kind: Some("patient".into()),
                fhir_user: None,
            },
            since: None,
            pulled_at: "2026-01-01T00:00:00Z".into(),
            fhir_version: FHIR_VERSION.into(),
        }
    }

    #[test]
    fn a_stream_is_header_resources_trailer_with_duplicates_dropped() {
        let mut buf = Vec::new();
        let mut w = RecordsWriter::new(&mut buf, &header()).unwrap();
        let obs = RawResource::parse(
            r#"{ "resourceType": "Observation", "id": "o1", "valueQuantity": {"value": 1.50} }"#,
        )
        .unwrap();
        assert!(w.resource(&obs).unwrap());
        assert!(!w.resource(&obs).unwrap());
        w.skip(Skip {
            resource_type: Some("Observation".into()),
            id: None,
            search: Some("Observation?category=survey".into()),
            reason: "HTTP 400".into(),
            code: "unsupported".into(),
        });
        let t = w.finish("2026-01-01T00:01:00Z".into(), None).unwrap();
        assert!(t.complete);
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].starts_with(r#"{"schema":"records-stream/v1","source":{"cli":"mychart""#),
            "{}",
            lines[0]
        );
        assert_eq!(
            lines[1],
            r#"{"resourceType":"Observation","id":"o1","valueQuantity":{"value":1.50}}"#
        );
        let trailer: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(trailer["schema"], END_SCHEMA);
        assert_eq!(trailer["counts"]["Observation"], 1);
        assert_eq!(trailer["skipped"][0]["code"], "unsupported");
        assert!(trailer.get("error").is_none());
    }

    #[test]
    fn a_fatal_error_ends_the_stream_incomplete() {
        let mut buf = Vec::new();
        let w = RecordsWriter::new(&mut buf, &header()).unwrap();
        let e = CliError::Auth("the organization no longer accepts this sign-in".into());
        let t = w.finish("2026-01-01T00:01:00Z".into(), Some(&e)).unwrap();
        assert!(!t.complete);
        let text = String::from_utf8(buf).unwrap();
        let trailer: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(trailer["complete"], false);
        assert_eq!(trailer["error"]["code"], "auth");
        assert_eq!(trailer["error"]["exit_code"], 3);
    }

    #[test]
    fn a_keychain_failure_keeps_its_own_code() {
        let mut buf = Vec::new();
        let w = RecordsWriter::new(&mut buf, &header()).unwrap();
        let e = CliError::Keychain("the Keychain refused the read".into());
        w.finish("2026-01-01T00:01:00Z".into(), Some(&e)).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let trailer: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(trailer["error"]["code"], "keychain");
        assert_eq!(trailer["error"]["exit_code"], e.exit_code());
    }

    #[test]
    fn a_header_without_fhir_version_reads_as_4_0_1() {
        let line = serde_json::to_string(&header()).unwrap();
        assert!(line.contains(r#""fhir_version":"4.0.1""#), "{line}");
        let mut v: Value = serde_json::from_str(&line).unwrap();
        v.as_object_mut().unwrap().remove("fhir_version");
        let back: Header = serde_json::from_value(v).unwrap();
        assert_eq!(back.fhir_version, FHIR_VERSION);
    }
}
