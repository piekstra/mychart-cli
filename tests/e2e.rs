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
use pk_cli_core::CommonArgs;

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
    let fhir = mychart::fhir::Fhir::new(&t, &c.fhir_base, &live);
    let mut buf = Vec::new();
    let mut w =
        mychart::stream::RecordsWriter::new(&mut buf, &commands::records::header(&c, since))
            .unwrap();
    mychart::export::run(&fhir, &mut w, &c.id, since, &|_| {}).unwrap();
    w.finish("2026-01-01T00:00:00Z".into(), None).unwrap();
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
