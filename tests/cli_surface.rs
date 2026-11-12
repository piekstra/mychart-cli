//! Black-box tests of the binary's surface. None reaches the Keychain: the
//! service is an empty test one, the config and cache are temp dirs, and
//! every case either renders help or info, works on the empty index, or is
//! refused by argument checks before any credential read. The directory
//! commands run against the mock organization on 127.0.0.1.

#[path = "support/mock_epic.rs"]
mod mock_epic;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

/// A fresh scratch dir per call. The counter, not the clock, makes it
/// unique: parallel tests read the same instant often enough on macOS.
fn tmp() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mychart-cli-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn mychart(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("mychart").unwrap();
    cmd.env(
        "MYCHART_KEYCHAIN_SERVICE",
        "piekstra.mychart.hermetic-tests",
    )
    .env("XDG_CONFIG_HOME", home.join("config"))
    .env("XDG_CACHE_HOME", home.join("cache"))
    .env("MYCHART_BROWSER", "/usr/bin/false")
    .env_remove("MYCHART_CONFIG")
    .env_remove("NO_COLOR");
    cmd
}

fn json_of(out: &[u8]) -> Value {
    serde_json::from_slice(out).unwrap_or_else(|e| {
        panic!(
            "not one JSON document ({e}): {}",
            String::from_utf8_lossy(out)
        )
    })
}

#[test]
fn every_command_renders_help() {
    let home = tmp();
    for args in [
        vec!["--help"],
        vec!["orgs", "--help"],
        vec!["orgs", "refresh", "--help"],
        vec!["orgs", "list", "--help"],
        vec!["orgs", "search", "--help"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["auth", "status", "--help"],
        vec!["auth", "logout", "--help"],
        vec!["patients", "--help"],
        vec!["patients", "list", "--help"],
        vec!["records", "--help"],
        vec!["records", "export", "--help"],
        vec!["documents", "--help"],
        vec!["documents", "list", "--help"],
        vec!["documents", "download", "--help"],
        vec!["api", "--help"],
        vec!["config", "--help"],
        vec!["config", "path", "--help"],
        vec!["config", "show", "--help"],
        vec!["config", "set", "--help"],
        vec!["config", "unset", "--help"],
        vec!["self-update", "--help"],
        vec!["completions", "--help"],
        vec!["info", "--help"],
    ] {
        mychart(&home).args(&args).assert().success();
    }
}

#[test]
fn version_and_completions() {
    let home = tmp();
    mychart(&home)
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
    for shell in ["bash", "zsh", "fish"] {
        mychart(&home)
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("mychart"));
    }
}

#[test]
fn info_declares_both_profiles() {
    let home = tmp();
    let out = mychart(&home)
        .args(["info", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "mychart");
    let profiles: Vec<&str> = v["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        profiles.contains(&"health-records/v1") && profiles.contains(&"documents/v1"),
        "{profiles:?}"
    );
    assert_eq!(v["auth"]["method"], "browser-session");
}

#[test]
fn nothing_signed_in_reads_as_such() {
    let home = tmp();
    let out = mychart(&home)
        .args(["auth", "status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["schema"], "auth-status/v1");
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["patients"], serde_json::json!([]));

    let out = mychart(&home)
        .args(["patients", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["schema"], "patient-list/v1");
    assert_eq!(v["items"], serde_json::json!([]));

    for args in [
        vec!["records", "export", "--ndjson", "--json"],
        vec!["documents", "list", "--json"],
        vec!["api", "GET", "Patient/x", "--json"],
    ] {
        let out = mychart(&home)
            .args(&args)
            .assert()
            .code(3)
            .get_output()
            .stdout
            .clone();
        assert!(
            json_of(&out)["error"]["message"]
                .as_str()
                .unwrap()
                .contains("auth login"),
            "{args:?}"
        );
    }
    mychart(&home).args(["auth", "logout"]).assert().success();
}

#[test]
fn bad_arguments_are_refused_before_anything_else() {
    let home = tmp();
    for (args, code) in [
        (vec!["records", "export"], 2),
        (
            vec!["records", "export", "--ndjson", "--since", "2026-13-01"],
            2,
        ),
        (vec!["records", "export", "--ndjson", "-o", "x"], 2),
        (vec!["api", "POST", "Patient"], 2),
        (vec!["api", "DELETE", "Patient/x"], 2),
        (
            vec!["auth", "login", "--org", "sandbox", "--label", "has space"],
            2,
        ),
        (
            vec!["auth", "login", "--org", "sandbox", "--timeout", "0"],
            2,
        ),
        (vec!["auth", "login", "--org", "sandbox"], 2),
        (vec!["documents", "download"], 2),
        (
            vec![
                "documents",
                "list",
                "--since",
                "2026-02-01",
                "--until",
                "2026-01-01",
            ],
            2,
        ),
        (vec!["config", "set", "redirect_port", "80"], 2),
        (vec!["config", "set", "redirect_host", "example.com"], 2),
        (
            vec![
                "config",
                "set",
                "directory_url",
                "http://example.com/brands",
            ],
            2,
        ),
        (vec!["config", "set", "no_such_key", "x"], 2),
        (vec!["orgs", "list", "--state", "XYZ"], 2),
        (vec!["orgs", "list", "--limit", "0"], 2),
        (vec!["nonsense"], 2),
    ] {
        mychart(&home).args(&args).assert().code(code);
    }
    let out = mychart(&home)
        .args(["auth", "login", "--org", "sandbox", "--json"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["error"]["code"], "usage");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("sandbox_client_id"));
}

#[test]
fn config_round_trips() {
    let home = tmp();
    mychart(&home)
        .args([
            "config",
            "set",
            "client_id",
            "00000000-0000-0000-0000-000000000001",
        ])
        .assert()
        .success();
    mychart(&home)
        .args(["config", "set", "redirect_port", "8123"])
        .assert()
        .success();
    let out = mychart(&home)
        .args(["config", "show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["client_id"], "00000000-0000-0000-0000-000000000001");
    assert_eq!(v["redirect_port"], 8123);
    mychart(&home)
        .args(["config", "unset", "redirect_port"])
        .assert()
        .success();
    let out = mychart(&home)
        .args(["config", "path", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(json_of(&out)["path"]
        .as_str()
        .unwrap()
        .ends_with("mychart/config.json"));
    mychart(&home)
        .args(["auth", "logout", "--forget"])
        .assert()
        .success();
    let out = mychart(&home)
        .args(["config", "show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(json_of(&out).get("client_id").is_none());
}

#[test]
fn the_directory_downloads_once_and_resolves_organizations() {
    let mock = mock_epic::MockEpic::start();
    let home = tmp();
    mychart(&home)
        .args(["config", "set", "directory_url", &mock.directory_url()])
        .assert()
        .success();
    let out = mychart(&home)
        .args(["orgs", "refresh", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["schema"], "org-refresh/v1");
    assert_eq!(v["organizations"], 1);
    assert_eq!(v["refreshed"], true);
    let out = mychart(&home)
        .args(["orgs", "refresh", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json_of(&out)["refreshed"], false);

    let out = mychart(&home)
        .args(["orgs", "search", "pediatrics", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json_of(&out);
    assert_eq!(v["schema"], "org-list/v1");
    assert_eq!(v["items"][0]["id"], mock_epic::ORG_ID);
    assert_eq!(v["items"][0]["vendor"], "epic");
    assert!(v["items"][0]["matched"]
        .as_str()
        .unwrap()
        .contains("Pediatrics"));

    let out = mychart(&home)
        .args(["orgs", "list", "--json", "--state", "ny"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ids: Vec<String> = json_of(&out)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&mock_epic::ORG_ID.to_string()));
    mychart(&home)
        .args(["orgs", "search", "no such clinic anywhere"])
        .assert()
        .code(4);
    mychart(&home)
        .args(["orgs", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains(mock_epic::ORG_NAME));
}

#[test]
fn an_unreachable_directory_is_an_upstream_error() {
    let home = tmp();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    mychart(&home)
        .args([
            "config",
            "set",
            "directory_url",
            &format!("http://127.0.0.1:{port}/Endpoints/Brands"),
        ])
        .assert()
        .success();
    mychart(&home).args(["orgs", "list"]).assert().code(5);
}

#[test]
fn no_color_is_accepted() {
    let home = tmp();
    mychart(&home)
        .env("NO_COLOR", "1")
        .args(["patients", "list"])
        .assert()
        .success();
}
