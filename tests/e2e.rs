//! Sign-in end to end against the mock Epic organization, in-process: the
//! directory download, discovery with `Epic-Client-ID`, the standalone
//! launch through a stand-in browser, the code exchange, dynamic client
//! registration, the JWT-bearer grant, renewal, proxy access and logout.
//! The vault is in memory, so no Keychain is read on any platform.

#[path = "support/mock_epic.rs"]
mod mock_epic;

use std::path::Path;

use mock_epic::{MockEpic, Pick, CHILD_PATIENT, CLIENT_ID, ORG_ID, PROXY_USER, SELF_PATIENT};
use mychart::cli::{AuthCmd, LoginArgs, LogoutArgs, PatientsCmd};
use mychart::commands::{self, Ctx};
use mychart::config::Config;
use mychart::session::{self, Session};
use mychart::vault::{self, MemoryVault};
use pk_cli_auth::refresh_lock::RefreshLock;
use pk_cli_config::ConfigStore;
use pk_cli_core::{CliError, CommonArgs};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A browser that follows the authorize redirect back to the loopback
/// listener, as MyChart would after the person signs in.
fn browser(url: &str) -> bool {
    let url = url.to_string();
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = client.get(&url).send().unwrap();
        assert_eq!(
            resp.status().as_u16(),
            302,
            "authorize refused: {}",
            resp.text().unwrap_or_default()
        );
        let back = resp.headers()["location"].to_str().unwrap().to_string();
        client.get(back).send().unwrap();
    });
    true
}

fn ctx(dir: &Path, mock: &MockEpic, configure: impl FnOnce(&mut Config)) -> Ctx {
    let store = ConfigStore::new("mychart").with_override(Some(dir.join("config.json")));
    let mut cfg = Config {
        client_id: Some(CLIENT_ID.into()),
        directory_url: Some(mock.directory_url()),
        redirect_port: Some(free_port()),
        ..Config::default()
    };
    configure(&mut cfg);
    store.save(&cfg).unwrap();
    Ctx {
        common: CommonArgs {
            json: true,
            quiet: true,
            ..CommonArgs::default()
        },
        store,
        vault: Box::new(MemoryVault::default()),
        lock: RefreshLock::at(dir.join("token-refresh.lock")),
        cache_dir: dir.join("cache"),
        browser: Box::new(browser),
    }
}

fn login(ctx: &Ctx, label: &str) {
    commands::auth::run(
        ctx,
        &AuthCmd::Login(LoginArgs {
            org: ORG_ID.into(),
            label: Some(label.into()),
            no_browser: false,
            timeout: 1,
        }),
    )
    .unwrap();
}

fn stored(ctx: &Ctx, patient: &str) -> Session {
    vault::get_json::<Session>(ctx.vault.as_ref(), &session::account(ORG_ID, patient))
        .unwrap()
        .expect("a stored sign-in")
}

#[test]
fn a_sign_in_registers_the_install_and_renews_without_the_browser() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");

    let s = stored(&ctx, SELF_PATIENT);
    assert!(s.dynamic_client_id.is_some() && s.private_key.is_some());
    assert!(s.renewable());
    assert_eq!(s.client_id, CLIENT_ID);
    assert_eq!(
        s.fhir_user.as_deref(),
        Some(format!("Patient/{SELF_PATIENT}").as_str())
    );
    assert_eq!(
        mock.get(|st| (st.registrations, st.jwt_bearer_grants)),
        (1, 1)
    );

    let contexts = ctx.contexts().unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].label.as_deref(), Some("adult1"));
    assert_eq!(contexts[0].relationship.as_deref(), Some("self"));
    assert_eq!(contexts[0].organization_name, mock_epic::ORG_NAME);

    // An expired access token renews through the JWT-bearer grant.
    let mut expired = s;
    expired.expires_at = 0;
    vault::set_json(ctx.vault.as_ref(), &contexts[0].account(), &expired).unwrap();
    let t = commands::transport().unwrap();
    let renewed = session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &contexts[0]).unwrap();
    assert!(renewed.is_fresh(session::now()));
    assert_ne!(renewed.access_token, expired.access_token);
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), 2);
    assert_eq!(
        stored(&ctx, SELF_PATIENT).access_token,
        renewed.access_token
    );

    // A fresh token is used as is.
    session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &contexts[0]).unwrap();
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), 2);

    // The profile's PatientContext: the Keychain account, never the token,
    // and the stored sign-in's state under `status`.
    let row = commands::auth::patient_dto(&contexts[0], Some(&stored(&ctx, SELF_PATIENT)));
    assert_eq!(
        row["session"],
        format!("smart/{}/{SELF_PATIENT}", mock_epic::ORG_ID)
    );
    assert_eq!(row["kind"], "patient");
    assert_eq!(row["relationship"], "self");
    assert_eq!(row.pointer("/status/authenticated"), Some(&true.into()));
    assert_eq!(row.pointer("/status/renewable"), Some(&true.into()));
    assert!(commands::auth::usable(&row));
    assert!(!row.to_string().contains(&renewed.access_token));
    let bare = commands::auth::patient_dto(&contexts[0], None);
    assert!(bare.get("status").is_none() && !commands::auth::usable(&bare));

    commands::patients::run(&ctx, &PatientsCmd::List { status: true }).unwrap();
    commands::auth::run(&ctx, &AuthCmd::Status).unwrap();
}

#[test]
fn a_parent_reaches_a_child_through_proxy_access() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    mock.set(|st| st.pick = Pick::Child);
    login(&ctx, "child1");

    let contexts = ctx.contexts().unwrap();
    assert_eq!(contexts.len(), 2);
    let child = session::pick(&contexts, Some("child1"), None).unwrap();
    assert_eq!(child.id, CHILD_PATIENT);
    assert_eq!(child.relationship.as_deref(), Some("proxy"));
    assert_eq!(child.fhir_user.as_deref(), Some(PROXY_USER));
    assert!(stored(&ctx, CHILD_PATIENT).renewable());
    // Two people held: a command that reads records must be told which.
    assert_eq!(
        session::pick(&contexts, None, None)
            .unwrap_err()
            .exit_code(),
        2
    );

    commands::auth::run(
        &ctx,
        &AuthCmd::Logout(LogoutArgs {
            org: None,
            patient: Some("child1".into()),
            forget: false,
        }),
    )
    .unwrap();
    let left = ctx.contexts().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, SELF_PATIENT);
    assert!(vault::get_json::<Session>(
        ctx.vault.as_ref(),
        &session::account(ORG_ID, CHILD_PATIENT)
    )
    .unwrap()
    .is_none());

    commands::auth::run(
        &ctx,
        &AuthCmd::Logout(LogoutArgs {
            org: None,
            patient: None,
            forget: true,
        }),
    )
    .unwrap();
    assert!(ctx.contexts().unwrap().is_empty());
    assert!(!dir.path().join("config.json").exists());
}

#[test]
fn an_ended_access_period_asks_for_a_new_sign_in() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let c = ctx.contexts().unwrap().remove(0);
    let mut s = stored(&ctx, SELF_PATIENT);
    s.expires_at = 0;
    vault::set_json(ctx.vault.as_ref(), &c.account(), &s).unwrap();
    mock.set(|st| st.refuse_jwt_bearer = true);
    let t = commands::transport().unwrap();
    let Err(e) = session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &c) else {
        panic!("a refused renewal returned a session")
    };
    assert_eq!(e.exit_code(), 3, "{e}");
    assert!(e.to_string().contains("auth login"), "{e}");
}

#[test]
fn without_dynamic_registration_the_sign_in_lasts_its_hour() {
    let mock = MockEpic::start();
    mock.set(|st| st.offer_registration = false);
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let s = stored(&ctx, SELF_PATIENT);
    assert!(s.dynamic_client_id.is_none());
    assert!(!s.renewable());
    assert!(s.is_fresh(session::now()));
    assert_eq!(mock.get(|st| st.registrations), 0);
}

#[test]
fn dynamic_registration_can_be_turned_off() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |c| c.dynamic_registration = Some(false));
    login(&ctx, "adult1");
    assert!(!stored(&ctx, SELF_PATIENT).renewable());
    assert_eq!(mock.get(|st| st.registrations), 0);
}

#[test]
fn a_sign_in_without_a_client_id_stops_before_the_network() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |c| c.client_id = None);
    let e = commands::auth::run(
        &ctx,
        &AuthCmd::Login(LoginArgs {
            org: "sandbox".into(),
            label: None,
            no_browser: true,
            timeout: 1,
        }),
    )
    .unwrap_err();
    assert_eq!(e.exit_code(), 2);
    assert!(format!("{e:?}").contains("sandbox_client_id"), "{e:?}");
}

#[test]
fn an_unknown_organization_is_not_found() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    let e = commands::auth::run(
        &ctx,
        &AuthCmd::Login(LoginArgs {
            org: "No Such Clinic".into(),
            label: None,
            no_browser: true,
            timeout: 1,
        }),
    )
    .unwrap_err();
    assert_eq!(e.exit_code(), 4, "{e}");
}

fn export_stream(
    ctx: &Ctx,
    patient: &str,
    since: Option<&str>,
) -> (Vec<serde_json::Value>, Vec<String>) {
    let c = ctx
        .contexts()
        .unwrap()
        .into_iter()
        .find(|c| c.id == patient)
        .unwrap();
    let t = commands::transport().unwrap();
    let live = session::Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c).unwrap();
    let fhir = mychart::fhir::Fhir::new(&t, &live.fhir_base(), &live);
    let mut buf = Vec::new();
    let mut w =
        pk_med_records::RecordsWriter::start(&mut buf, &commands::records::header(&c, since))
            .unwrap();
    mychart::export::run(&fhir, &mut w, &c.id, since, &|_| {}).unwrap();
    w.finish("2026-01-01T00:00:00Z", None).unwrap();
    let text = String::from_utf8(buf).unwrap();
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let values = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (values, lines)
}

#[test]
fn an_export_is_one_records_stream_with_documents_after_their_references() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let (values, lines) = export_stream(&ctx, SELF_PATIENT, None);

    let header = &values[0];
    assert_eq!(header["schema"], "records-stream/v1");
    assert_eq!(header["source"]["cli"], "mychart");
    assert_eq!(header["organization"]["id"], ORG_ID);
    assert_eq!(header["organization"]["vendor"], "epic");
    assert_eq!(header["patient"]["id"], SELF_PATIENT);
    assert_eq!(header["patient"]["label"], "adult1");
    assert_eq!(header["patient"]["relationship"], "self");
    assert_eq!(header["fhir_version"], "4.0.1");
    assert!(header.get("since").is_none());
    assert!(header.get("since_unbounded").is_none(), "{header}");

    let trailer = values.last().unwrap();
    assert_eq!(trailer["schema"], "records-stream-end/v1");
    assert_eq!(trailer["complete"], true);

    // Counts agree with the lines, by type, as medrec checks them.
    let body = &values[1..values.len() - 1];
    let mut counted = std::collections::BTreeMap::<String, u64>::new();
    for v in body {
        *counted
            .entry(v["resourceType"].as_str().unwrap().to_string())
            .or_default() += 1;
    }
    let trailer_counts: std::collections::BTreeMap<String, u64> =
        serde_json::from_value(trailer["counts"].clone()).unwrap();
    assert_eq!(counted, trailer_counts);
    let expect = [
        ("AllergyIntolerance", 1),
        ("Binary", 2),
        ("Condition", 1),
        ("DocumentReference", 1),
        ("Medication", 1),
        ("MedicationRequest", 1),
        ("Observation", 3),
        ("Patient", 1),
        ("Practitioner", 1),
    ];
    assert_eq!(
        trailer_counts,
        expect
            .iter()
            .map(|(k, v)| (k.to_string(), *v as u64))
            .collect()
    );

    // The server's text, minified: 13.50 stays 13.50.
    assert!(
        lines.iter().any(|l| l.contains(r#""value":13.50"#)),
        "decimal rewritten"
    );
    assert!(lines.iter().all(|l| !l.contains("\n  ")));
    // Each Binary follows its DocumentReference.
    let pos = |t: &str, id: &str| {
        body.iter()
            .position(|v| v["resourceType"] == t && v["id"] == id)
            .unwrap()
    };
    assert!(pos("DocumentReference", "doc-1") < pos("Binary", "bin-pdf-1"));
    assert!(pos("DocumentReference", "doc-1") < pos("Binary", "bin-html-1"));
    assert_eq!(body[0]["resourceType"], "Patient");
    // No other patient's records.
    assert!(body.iter().all(|v| !v.to_string().contains(CHILD_PATIENT)));

    // Refused searches are skips, not the end of the export.
    let skipped = trailer["skipped"].as_array().unwrap();
    let skip = |search: &str| {
        skipped
            .iter()
            .find(|s| s["search"] == search)
            .map(|s| s["code"].as_str().unwrap().to_string())
    };
    assert_eq!(
        skip("Observation?category=sdoh").as_deref(),
        Some("unsupported")
    );
    assert_eq!(skip("Media").as_deref(), Some("forbidden"));
}

#[test]
fn since_narrows_dated_searches() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let (values, _) = export_stream(&ctx, SELF_PATIENT, Some("2026-02-01"));
    assert_eq!(values[0]["since"], "2026-02-01");
    let unbounded: Vec<&str> = values[0]["since_unbounded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for t in [
        "Patient",
        "AllergyIntolerance",
        "Condition",
        "Medication",
        "Practitioner",
    ] {
        assert!(unbounded.contains(&t), "{t} in {unbounded:?}");
    }
    for t in ["Observation", "DocumentReference", "Binary", "Encounter"] {
        assert!(!unbounded.contains(&t), "{t} in {unbounded:?}");
    }
    let trailer = values.last().unwrap();
    assert_eq!(trailer["counts"]["Observation"], 2);
    assert_eq!(trailer["counts"]["AllergyIntolerance"], 1);
}

#[test]
fn an_export_to_a_directory_writes_one_private_file() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    mock.set(|st| st.pick = Pick::Child);
    login(&ctx, "child1");
    let out = dir.path().join("export");
    commands::records::run(
        &ctx,
        &mychart::cli::RecordsCmd::Export(mychart::cli::ExportArgs {
            who: mychart::cli::PatientArgs {
                patient: Some("child1".into()),
                org: None,
            },
            since: None,
            ndjson: false,
            output: Some(out.clone()),
        }),
    )
    .unwrap();
    let path = out.join("records.ndjson");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.lines().next().unwrap().contains(CHILD_PATIENT));
    assert!(text.contains(r#""relationship":"proxy""#));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn export_to_a_file_is_for_synthetic_servers_only() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    ctx.index()
        .unwrap()
        .upsert(session::Context {
            id: "pat-0001".into(),
            organization: "1000-1".into(),
            organization_name: "Example Health".into(),
            fhir_base: "https://fhir.example.org/api/FHIR/R4".into(),
            label: Some("adult9".into()),
            relationship: Some("self".into()),
            fhir_user: None,
            signed_in_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
    let e = commands::records::run(
        &ctx,
        &mychart::cli::RecordsCmd::Export(mychart::cli::ExportArgs {
            who: mychart::cli::PatientArgs {
                patient: Some("adult9".into()),
                org: None,
            },
            since: None,
            ndjson: false,
            output: Some(dir.path().join("x")),
        }),
    )
    .unwrap_err();
    assert_eq!(e.exit_code(), 2);
    assert!(format!("{e:?}").contains("medrec import"), "{e:?}");
    assert!(!dir.path().join("x").exists());
}

#[test]
fn documents_list_and_download_verify_the_bytes() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let who = mychart::cli::PatientArgs::default();
    let docs =
        commands::documents::list(&ctx, &who, &pk_cli_documents::RangeArgs::default()).unwrap();
    let ids: Vec<&str> = docs.iter().map(|d| d.document.id.as_str()).collect();
    assert_eq!(ids, ["doc-1", "doc-1#2"]);
    assert_eq!(docs[0].document.date.as_deref(), Some("2026-01-06"));

    let out = dir.path().join("docs");
    commands::documents::run(
        &ctx,
        &mychart::cli::DocumentsCmd::Download {
            id: None,
            all: true,
            output: out.clone(),
            who: who.clone(),
            range: pk_cli_documents::RangeArgs::default(),
        },
    )
    .unwrap();
    let pdf = std::fs::read(out.join("2026-01-06_doc-1.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    let html = std::fs::read_to_string(out.join("2026-01-06_doc-1_2.html")).unwrap();
    assert!(html.contains("Synthetic visit note"));

    let one = dir.path().join("one.html");
    commands::documents::run(
        &ctx,
        &mychart::cli::DocumentsCmd::Download {
            id: Some("doc-1#2".into()),
            all: false,
            output: one.clone(),
            who: who.clone(),
            range: pk_cli_documents::RangeArgs::default(),
        },
    )
    .unwrap();
    assert!(one.exists());
    let e = commands::documents::run(
        &ctx,
        &mychart::cli::DocumentsCmd::Download {
            id: Some("nope".into()),
            all: false,
            output: dir.path().join("nope"),
            who,
            range: pk_cli_documents::RangeArgs::default(),
        },
    )
    .unwrap_err();
    assert_eq!(e.exit_code(), 4);
}

#[test]
fn api_reads_and_refuses_writes_and_other_hosts() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let api = |method: &str, path: &str| {
        commands::api::run(
            &ctx,
            &mychart::cli::ApiArgs {
                method: method.into(),
                path: path.into(),
                who: Default::default(),
            },
        )
    };
    api("GET", &format!("Patient/{SELF_PATIENT}")).unwrap();
    assert_eq!(api("POST", "Patient").unwrap_err().exit_code(), 2);
    assert_eq!(
        api("GET", "https://elsewhere.example/Patient/x")
            .unwrap_err()
            .exit_code(),
        2
    );
    assert_eq!(
        api("GET", "Observation/no-such").unwrap_err().exit_code(),
        4
    );
    assert_eq!(
        api("GET", &format!("Patient/{CHILD_PATIENT}"))
            .unwrap_err()
            .exit_code(),
        3
    );
}

fn skips(trailer: &serde_json::Value) -> Vec<(String, String, String)> {
    trailer["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let text = |k: &str| s[k].as_str().unwrap_or_default().to_string();
            (text("resource_type"), text("id"), text("code"))
        })
        .collect()
}

#[test]
fn an_export_goes_past_files_elsewhere_and_answers_that_are_not_fhir() {
    use serde_json::json;
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    mock.set(|st| {
        let me = json!({"reference": format!("Patient/{SELF_PATIENT}")});
        // A note whose file is on another server.
        let far = json!({
            "resourceType": "DocumentReference", "id": "doc-far", "subject": me,
            "date": "2026-02-01T00:00:00Z",
            "category": [{"coding": [{"code": "clinical-note"}]}],
            "content": [{"attachment": {"contentType": "application/pdf",
                "url": "https://elsewhere.example.org/api/FHIR/R4/Binary/bin-far"}}]
        });
        st.resources
            .entry("DocumentReference".into())
            .or_default()
            .push((far.clone(), far.to_string()));
        // A search entry that is not a resource.
        let goal = json!({"id": "goal-x", "subject": me});
        st.resources
            .entry("Goal".into())
            .or_default()
            .push((goal.clone(), goal.to_string()));
        // An organization only the Patient names.
        let patients = st.resources.get_mut("Patient").unwrap();
        let (p, text) = patients
            .iter_mut()
            .find(|(p, _)| p["id"] == SELF_PATIENT)
            .unwrap();
        p["managingOrganization"] = json!({"reference": "Organization/org-1"});
        *text = p.to_string();
        let org = json!({"resourceType": "Organization", "id": "org-1", "name": "Example Clinic"});
        st.resources
            .entry("Organization".into())
            .or_default()
            .push((org.clone(), org.to_string()));
        st.faults
            .push(mock_epic::Fault::not_fhir("Practitioner/prac-1"));
    });

    let (values, _) = export_stream(&ctx, SELF_PATIENT, None);
    let trailer = values.last().unwrap();
    assert_eq!(trailer["complete"], true);
    let skipped = skips(trailer);
    assert!(
        skipped.contains(&("Binary".into(), "bin-far".into(), "no_file".into())),
        "{skipped:?}"
    );
    assert!(
        skipped.contains(&("Practitioner".into(), "prac-1".into(), "upstream".into())),
        "{skipped:?}"
    );
    assert!(
        skipped
            .iter()
            .any(|(t, _, c)| t == "Goal" && c == "upstream"),
        "{skipped:?}"
    );
    // The searches after the documents ran, and the Patient's reference was read.
    assert_eq!(trailer["counts"]["MedicationRequest"], 1);
    assert_eq!(trailer["counts"]["DocumentReference"], 2);
    assert_eq!(trailer["counts"]["Organization"], 1);
    assert!(trailer["counts"].get("Goal").is_none());
    // Nor was the file fetched from this server under its id, as a path
    // rewritten onto the base would have been.
    assert_eq!(mock.get(|st| st.hits.get("Binary/bin-far").copied()), None);
}

#[test]
fn a_failing_binary_is_retried_in_place_and_fetched_once() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");

    // One 500 is retried at once and the export carries the file.
    mock.set(|st| {
        st.faults
            .push(mock_epic::Fault::failing("Binary/bin-pdf-1", 1))
    });
    let (values, _) = export_stream(&ctx, SELF_PATIENT, None);
    let trailer = values.last().unwrap();
    assert_eq!(trailer["counts"]["Binary"], 2);
    assert!(skips(trailer).iter().all(|(t, _, _)| t != "Binary"));

    // A file that keeps failing is one skip; doc-1 coming back from the
    // second DocumentReference search does not fetch it again.
    mock.set(|st| {
        st.hits.clear();
        st.faults
            .push(mock_epic::Fault::failing("Binary/bin-pdf-1", 100));
    });
    let (values, _) = export_stream(&ctx, SELF_PATIENT, None);
    let trailer = values.last().unwrap();
    assert_eq!(trailer["counts"]["Binary"], 1);
    let binary_skips: Vec<_> = skips(trailer)
        .into_iter()
        .filter(|(t, _, _)| t == "Binary")
        .collect();
    assert_eq!(
        binary_skips,
        [(
            "Binary".to_string(),
            "bin-pdf-1".to_string(),
            "upstream".to_string()
        )]
    );
    assert_eq!(mock.get(|st| st.hits["Binary/bin-pdf-1"]), 3);
    assert_eq!(mock.get(|st| st.hits["Binary/bin-html-1"]), 1);
}

#[test]
fn a_failed_first_grant_keeps_the_registered_install() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    mock.set(|st| st.refuse_jwt_bearer = true);
    login(&ctx, "adult1");
    let s = stored(&ctx, SELF_PATIENT);
    assert!(s.dynamic_client_id.is_some() && s.private_key.is_some());
    assert_eq!(s.expires_at, 0);
    assert_eq!(mock.get(|st| st.registrations), 1);

    let c = ctx.contexts().unwrap().remove(0);
    let t = commands::transport().unwrap();
    let Err(e) = session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &c) else {
        panic!("a refused grant returned a session")
    };
    assert_eq!(e.exit_code(), 3);
    let refused = stored(&ctx, SELF_PATIENT);
    assert!(refused.refused_at.is_some());
    assert!(!refused.usable(session::now()));

    // A refusal is not final: the next use tries the grant once more.
    mock.set(|st| st.refuse_jwt_bearer = false);
    let renewed = session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &c).unwrap();
    assert!(renewed.is_fresh(session::now()));
    assert!(stored(&ctx, SELF_PATIENT).refused_at.is_none());
    assert_eq!(mock.get(|st| st.registrations), 1);
}

#[test]
fn a_revoked_token_is_renewed_once_and_the_export_goes_on() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let grants = mock.get(|st| st.jwt_bearer_grants);
    mock.set(|st| st.forget_tokens());
    let (values, _) = export_stream(&ctx, SELF_PATIENT, None);
    let trailer = values.last().unwrap();
    assert_eq!(trailer["complete"], true);
    assert_eq!(trailer["counts"]["Patient"], 1);
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), grants + 1);
    assert!(stored(&ctx, SELF_PATIENT).refused_at.is_none());
}

/// An export of `patient` that may fail: the error, if any.
fn export_result(ctx: &Ctx, patient: &str) -> Result<(), CliError> {
    let c = ctx
        .contexts()
        .unwrap()
        .into_iter()
        .find(|c| c.id == patient)
        .unwrap();
    let t = commands::transport().unwrap();
    let live = session::Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c)?;
    let fhir = mychart::fhir::Fhir::new(&t, &live.fhir_base(), &live);
    let mut buf = Vec::new();
    let mut w =
        pk_med_records::RecordsWriter::start(&mut buf, &commands::records::header(&c, None))
            .unwrap();
    mychart::export::run(&fhir, &mut w, &c.id, None, &|_| {})
}

#[test]
fn a_refused_renewal_shows_in_status_and_its_token_is_not_sent_again() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    mock.set(|st| {
        st.forget_tokens();
        st.refuse_jwt_bearer = true;
    });
    let e = export_result(&ctx, SELF_PATIENT).unwrap_err();
    assert_eq!(e.exit_code(), 3, "{e}");

    let c = ctx.contexts().unwrap().remove(0);
    let s = stored(&ctx, SELF_PATIENT);
    assert!(s.refused_at.is_some());
    let row = commands::auth::patient_dto(&c, Some(&s));
    assert_eq!(row["status"]["authenticated"], false, "{row}");
    assert!(row["status"]["refused_at"].is_string(), "{row}");

    // The next export asks for a grant first (refused again, so none is
    // counted) and sends no FHIR request.
    let reads: usize = mock.get(|st| st.hits.values().sum());
    let grants = mock.get(|st| st.jwt_bearer_grants);
    let e = export_result(&ctx, SELF_PATIENT).unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert_eq!(mock.get(|st| st.hits.values().sum::<usize>()), reads);
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), grants);
}

#[test]
fn a_401_to_a_just_renewed_token_ends_the_sign_in() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let grants = mock.get(|st| st.jwt_bearer_grants);
    mock.set(|st| {
        st.faults.push(mock_epic::Fault {
            path: format!("Patient/{SELF_PATIENT}"),
            status: 401,
            body: "{}".into(),
            times: 2,
        })
    });
    let e = export_result(&ctx, SELF_PATIENT).unwrap_err();
    assert_eq!(e.exit_code(), 3, "{e}");
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), grants + 1);
    assert!(stored(&ctx, SELF_PATIENT).refused_at.is_some());
}

#[test]
fn the_registered_install_renews_even_when_a_refresh_token_is_held() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let c = ctx.contexts().unwrap().remove(0);
    let mut s = stored(&ctx, SELF_PATIENT);
    // A refresh token from the code exchange, issued to the app's client id.
    s.refresh_token = Some("issued-to-the-app".into());
    s.expires_at = 0;
    vault::set_json(ctx.vault.as_ref(), &c.account(), &s).unwrap();
    let before = mock.get(|st| st.jwt_bearer_grants);
    let t = commands::transport().unwrap();
    let renewed = session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &c).unwrap();
    assert!(renewed.is_fresh(session::now()));
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), before + 1);
}

#[test]
fn a_patient_index_naming_another_server_is_refused() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let mut c = ctx.contexts().unwrap().remove(0);
    c.fhir_base = "https://elsewhere.example.org/api/FHIR/R4".into();
    ctx.index().unwrap().upsert(c).unwrap();
    let hits = mock.get(|st| st.hits.values().sum::<usize>());
    let Err(e) = commands::api::run(
        &ctx,
        &mychart::cli::ApiArgs {
            method: "GET".into(),
            path: format!("Patient/{SELF_PATIENT}"),
            who: mychart::cli::PatientArgs::default(),
        },
    ) else {
        panic!("a request went out under an edited index")
    };
    assert_eq!(e.exit_code(), 3);
    assert!(e.to_string().contains("patient index"), "{e}");
    assert_eq!(mock.get(|st| st.hits.values().sum::<usize>()), hits);
}

#[test]
fn an_organization_without_https_is_refused_before_the_network() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    let mut index = commands::orgs::index(&ctx, false).unwrap();
    let mut plain = index.orgs[0].clone();
    plain.id = "9000-9".into();
    plain.name = "Plain Example Clinic".into();
    plain.fhir_base = "http://fhir.example.org/api/FHIR/R4".into();
    plain.address = plain.fhir_base.clone();
    index.orgs.push(plain);
    mychart::fsutil::write_private(
        &mychart::orgs::index_path(&ctx.cache_dir),
        &serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    let Err(e) = commands::auth::run(
        &ctx,
        &AuthCmd::Login(LoginArgs {
            org: "9000-9".into(),
            label: None,
            no_browser: true,
            timeout: 1,
        }),
    ) else {
        panic!("a sign-in started over plain http")
    };
    assert_eq!(e.exit_code(), 2);
    assert!(e.to_string().contains("only https"), "{e}");
}

#[test]
fn a_401_to_a_token_another_process_replaced_uses_the_new_one_without_a_grant() {
    let mock = MockEpic::start();
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let c = ctx.contexts().unwrap().remove(0);
    let t = commands::transport().unwrap();
    let held = stored(&ctx, SELF_PATIENT).access_token;
    let grants = mock.get(|st| st.jwt_bearer_grants);

    // This process was refused an older token; the Keychain holds a newer one.
    let s =
        session::renew_refused(&t, ctx.vault.as_ref(), &ctx.lock, &c, "an-older-token").unwrap();
    assert_eq!(s.access_token, held);
    assert_eq!(mock.get(|st| st.jwt_bearer_grants), grants);

    // Nor is the newer token marked when the older one was just issued.
    let e = session::refuse(ctx.vault.as_ref(), &ctx.lock, &c, "an-older-token");
    assert_eq!(e.exit_code(), 3);
    let kept = stored(&ctx, SELF_PATIENT);
    assert!(kept.refused_at.is_none());
    assert!(kept.usable(session::now()));
}

#[test]
fn a_401_with_no_way_to_renew_marks_the_sign_in_refused() {
    let mock = MockEpic::start();
    mock.set(|st| st.offer_registration = false);
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx(dir.path(), &mock, |_| {});
    login(&ctx, "adult1");
    let s = stored(&ctx, SELF_PATIENT);
    assert!(!s.renewable());
    let c = ctx.contexts().unwrap().remove(0);
    let t = commands::transport().unwrap();

    let Err(e) = session::renew_refused(&t, ctx.vault.as_ref(), &ctx.lock, &c, &s.access_token)
    else {
        panic!("a refused token that cannot be renewed returned a session")
    };
    assert_eq!(e.exit_code(), 3);
    assert!(stored(&ctx, SELF_PATIENT).refused_at.is_some());

    // Not fresh and not renewable: refused before any request.
    let reads: usize = mock.get(|st| st.hits.values().sum());
    let Err(e) = session::access_token(&t, ctx.vault.as_ref(), &ctx.lock, &c) else {
        panic!("a refused sign-in returned a session")
    };
    assert_eq!(e.exit_code(), 3);
    assert!(e.message().contains("refused it on"), "{e}");
    assert_eq!(mock.get(|st| st.hits.values().sum::<usize>()), reads);
}
