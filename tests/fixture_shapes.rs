//! The fixtures hold invented people only (tests/fixtures/README.md), and
//! carry the shapes the code reads.

use serde_json::Value;

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn mock_patients_are_invented_people() {
    let records = fixture("mock_records.json");
    let patients: Vec<&Value> = records
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["resourceType"] == "Patient")
        .collect();
    assert_eq!(patients.len(), 2);
    for p in patients {
        let id = p["id"].as_str().unwrap();
        assert!(
            id.starts_with('e') && id.contains("0000000000"),
            "patient id {id}"
        );
        for n in p["name"].as_array().unwrap() {
            assert_eq!(n["family"], "Example");
        }
        assert!(matches!(
            p["birthDate"].as_str(),
            Some("1990-01-01") | Some("2026-01-01")
        ));
        for absent in ["telecom", "address", "identifier", "contact"] {
            assert!(p.get(absent).is_none(), "{id} carries {absent}");
        }
    }
}

#[test]
fn every_mock_resource_has_a_type_and_an_id() {
    for r in fixture("mock_records.json").as_array().unwrap() {
        assert!(r["resourceType"].is_string() && r["id"].is_string(), "{r}");
        if let Some(s) = r
            .pointer("/subject/reference")
            .or(r.pointer("/patient/reference"))
            .and_then(Value::as_str)
        {
            assert!(s.starts_with("Patient/e"), "{s}");
        }
    }
}

#[test]
fn the_brands_bundle_has_the_directory_shape() {
    let b = fixture("brands_bundle.json");
    assert_eq!(b["resourceType"], "Bundle");
    let entries = b["entry"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|e| e["resource"]["resourceType"] == "Endpoint"));
    for e in entries {
        let r = &e["resource"];
        if r["resourceType"] == "Endpoint" {
            let url = r["address"].as_str().unwrap().to_ascii_lowercase();
            assert!(
                url.starts_with("https://")
                    && url
                        .split('/')
                        .nth(2)
                        .is_some_and(|h| h.ends_with(".example.org")),
                "{url}"
            );
        }
        for a in r["address"].as_array().into_iter().flatten() {
            if let Some(pc) = a["postalCode"].as_str() {
                assert_eq!(pc, "00000");
            }
        }
    }
}

#[test]
fn the_capability_statement_names_oauth_uris_on_example_hosts() {
    let m = fixture("epic_metadata.json");
    let uris = m
        .pointer("/rest/0/security/extension/0/extension")
        .unwrap()
        .as_array()
        .unwrap();
    let names: Vec<&str> = uris.iter().map(|u| u["url"].as_str().unwrap()).collect();
    assert_eq!(names, ["authorize", "token", "register"]);
    for u in uris {
        assert!(u["valueUri"]
            .as_str()
            .unwrap()
            .starts_with("https://fhir.example.org/"));
    }
    let s = fixture("smart_configuration.json");
    assert!(s.get("registration_endpoint").is_none());
    assert!(s["token_endpoint"]
        .as_str()
        .unwrap()
        .starts_with("https://fhir.example.org/"));
}
