//! The patient contexts this install holds: one Keychain item per
//! (organization, patient) holding its tokens and per-install key, and a
//! non-secret index beside the config listing them, so `patients list`
//! reads no Keychain item.

use std::path::{Path, PathBuf};

use pk_cli_auth::refresh_lock::RefreshLock;
use pk_cli_core::transport::Transport;
use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use crate::key::SigningKey;
use crate::smart::{self, TokenResponse};
use crate::vault::{self, Vault};

/// Treat an access token this close to its expiry as expired, so a command
/// does not start an export that fails part way.
pub const EXPIRY_SKEW_SECS: i64 = 120;

/// What one sign-in stored. One Keychain JSON item; no `Debug`, it holds
/// tokens and a private key.
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub organization: String,
    pub fhir_base: String,
    pub token_endpoint: String,
    /// The app's Epic client id (the dynamic client's `software_id`).
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_client_id: Option<String>,
    /// PKCS#8 DER, base64: the dynamic client's private key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    pub access_token: String,
    /// Unix seconds.
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: String,
    pub patient: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_user: Option<String>,
    pub signed_in_at: i64,
}

impl Session {
    pub fn is_fresh(&self, now: i64) -> bool {
        self.expires_at - EXPIRY_SKEW_SECS > now
    }

    /// Whether this session can get a new access token without a sign-in.
    pub fn renewable(&self) -> bool {
        self.refresh_token.is_some()
            || (self.dynamic_client_id.is_some() && self.private_key.is_some())
    }

    pub fn apply(&mut self, t: &TokenResponse, now: i64) {
        self.access_token = t.access_token.clone();
        self.expires_at = now + t.expires_in.unwrap_or(3600);
        if let Some(rt) = &t.refresh_token {
            self.refresh_token = Some(rt.clone());
        }
        if let Some(s) = &t.scope {
            self.scope = s.clone();
        }
    }
}

/// The Keychain account for a context.
pub fn account(organization: &str, patient: &str) -> String {
    format!("smart/{organization}/{patient}")
}

/// One row of the index: everything about a context but its secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Context {
    /// The patient's FHIR id at the organization (`--patient`).
    pub id: String,
    pub organization: String,
    pub organization_name: String,
    pub fhir_base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_user: Option<String>,
    /// RFC 3339.
    pub signed_in_at: String,
}

impl Context {
    pub fn account(&self) -> String {
        account(&self.organization, &self.id)
    }

    /// What a `--patient` reference may match: the FHIR id, the label, and
    /// `<label>@<org>` style names come from `display`.
    pub fn display(&self) -> String {
        self.label.clone().unwrap_or_else(|| self.id.clone())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    contexts: Vec<Context>,
}

/// The index file, `contexts.json` beside the config file.
pub struct Index {
    path: PathBuf,
}

impl Index {
    pub fn beside(config_path: &Path) -> Self {
        let dir = config_path.parent().unwrap_or(Path::new("."));
        Index {
            path: dir.join("contexts.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<Context>, CliError> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => serde_json::from_str::<IndexFile>(&s)
                .map(|f| f.contexts)
                .map_err(|e| CliError::Other(format!("parsing {}: {e}", self.path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(CliError::Other(format!(
                "reading {}: {e}",
                self.path.display()
            ))),
        }
    }

    pub fn save(&self, contexts: &[Context]) -> Result<(), CliError> {
        let body = serde_json::to_string_pretty(&IndexFile {
            contexts: contexts.to_vec(),
        })
        .map_err(|e| CliError::Other(format!("serializing the index: {e}")))?;
        crate::fsutil::write_private(&self.path, body.as_bytes())
    }

    /// Insert or replace the row for the same (organization, patient).
    pub fn upsert(&self, ctx: Context) -> Result<(), CliError> {
        let mut all = self.load()?;
        all.retain(|c| !(c.organization == ctx.organization && c.id == ctx.id));
        all.push(ctx);
        self.save(&all)
    }

    pub fn remove(&self, organization: &str, patient: &str) -> Result<(), CliError> {
        let mut all = self.load()?;
        all.retain(|c| !(c.organization == organization && c.id == patient));
        self.save(&all)
    }
}

/// Resolve `--patient` against the held contexts: with none given and one
/// held, that one. A reference matches the FHIR id or the label, then a
/// unique part of the label (`pk_cli_core::resolve::pick`); `--org`
/// narrows first.
pub fn pick<'a>(
    contexts: &'a [Context],
    patient: Option<&str>,
    org: Option<&str>,
) -> Result<&'a Context, CliError> {
    let narrowed: Vec<&Context> = contexts
        .iter()
        .filter(|c| org.is_none_or(|o| c.organization == o || c.organization_name == o))
        .collect();
    if narrowed.is_empty() {
        return Err(CliError::Auth(if contexts.is_empty() {
            "no patient is signed in".into()
        } else {
            format!("no patient is signed in at {}", org.unwrap_or_default())
        })
        .with_remedy("run `mychart auth login --org <ORG> --label <WHO>`"));
    }
    match patient {
        None if narrowed.len() == 1 => Ok(narrowed[0]),
        None => Err(CliError::Usage(format!(
            "{} patients are signed in; name one with --patient ({})",
            narrowed.len(),
            narrowed
                .iter()
                .map(|c| c.display())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
        Some(q) => {
            let owned: Vec<Context> = narrowed.into_iter().cloned().collect();
            let hit = pk_cli_core::resolve::pick(
                &owned,
                q,
                |c| vec![c.id.clone()],
                |c| c.label.as_deref().unwrap_or(&c.id),
                "patient",
            )?;
            contexts
                .iter()
                .find(|c| c.organization == hit.organization && c.id == hit.id)
                .ok_or_else(|| CliError::Other("resolved a patient that is not held".into()))
        }
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// The stored session for `ctx`, or exit 3 when the Keychain holds none.
pub fn load(creds: &dyn Vault, ctx: &Context) -> Result<Session, CliError> {
    vault::get_json::<Session>(creds, &ctx.account())?.ok_or_else(|| {
        CliError::Auth(format!(
            "no stored sign-in for patient {} at {}",
            ctx.display(),
            ctx.organization_name
        ))
        .with_remedy(format!(
            "run `mychart auth login --org {}{}`",
            ctx.organization,
            ctx.label
                .as_ref()
                .map(|l| format!(" --label {l}"))
                .unwrap_or_default()
        ))
    })
}

/// A usable access token for `ctx`, renewing it under the refresh lock when
/// it is about to expire. Only one process renews; the others read what it
/// stored.
pub fn access_token<T: Transport>(
    t: &T,
    creds: &dyn Vault,
    lock: &RefreshLock,
    ctx: &Context,
) -> Result<Session, CliError> {
    let stored = load(creds, ctx)?;
    if stored.is_fresh(now()) {
        return Ok(stored);
    }
    if !stored.renewable() {
        return Err(expired(ctx, "it was stored without a way to renew it"));
    }
    lock.renew(
        || load(creds, ctx),
        |latest| !latest.is_fresh(now()),
        |mut latest| {
            let now = now();
            let key = match &latest.private_key {
                Some(k) => Some(SigningKey::from_stored(k)?),
                None => None,
            };
            let granted = match (&latest.refresh_token, &latest.dynamic_client_id, &key) {
                (Some(rt), dynamic, key) => smart::refresh_grant(
                    t,
                    &latest.token_endpoint,
                    dynamic.as_deref().unwrap_or(&latest.client_id),
                    rt,
                    key.as_ref(),
                    now,
                ),
                (None, Some(dynamic), Some(key)) => {
                    smart::jwt_bearer_grant(t, &latest.token_endpoint, dynamic, key, now)
                }
                _ => return Err(expired(ctx, "it was stored without a way to renew it")),
            }
            .map_err(|e| {
                if e.is_auth() {
                    expired(ctx, e.message())
                } else {
                    e
                }
            })?;
            latest.apply(&granted, now);
            vault::set_json(creds, &ctx.account(), &latest)?;
            Ok(latest)
        },
    )
}

fn expired(ctx: &Context, why: &str) -> CliError {
    CliError::Auth(format!(
        "access for patient {} at {} has ended ({why})",
        ctx.display(),
        ctx.organization_name
    ))
    .with_remedy(format!(
        "sign in again: `mychart auth login --org {}{}`",
        ctx.organization,
        ctx.label
            .as_ref()
            .map(|l| format!(" --label {l}"))
            .unwrap_or_default()
    ))
}

/// A signed-in context's bearer token for a run of requests: renewed through
/// the refresh lock when it falls due, so an export longer than the token's
/// hour keeps going.
pub struct Live<'a, T: Transport> {
    t: &'a T,
    vault: &'a dyn Vault,
    lock: &'a RefreshLock,
    context: &'a Context,
    session: std::cell::RefCell<Session>,
}

impl<'a, T: Transport> Live<'a, T> {
    pub fn start(
        t: &'a T,
        vault: &'a dyn Vault,
        lock: &'a RefreshLock,
        context: &'a Context,
    ) -> Result<Self, CliError> {
        let session = access_token(t, vault, lock, context)?;
        Ok(Live {
            t,
            vault,
            lock,
            context,
            session: std::cell::RefCell::new(session),
        })
    }

    pub fn fhir_base(&self) -> String {
        self.session.borrow().fhir_base.clone()
    }
}

impl<T: Transport> crate::fhir::Bearer for Live<'_, T> {
    fn token(&self) -> Result<String, CliError> {
        if !self.session.borrow().is_fresh(now()) {
            let renewed = access_token(self.t, self.vault, self.lock, self.context)?;
            *self.session.borrow_mut() = renewed;
        }
        Ok(self.session.borrow().access_token.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(id: &str, org: &str, label: Option<&str>) -> Context {
        Context {
            id: id.into(),
            organization: org.into(),
            organization_name: format!("{org} Health"),
            fhir_base: "https://fhir.example.org/api/FHIR/R4".into(),
            label: label.map(str::to_string),
            relationship: None,
            fhir_user: None,
            signed_in_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn one_held_context_needs_no_patient_flag() {
        let held = vec![ctx("p1", "org-a", Some("adult1"))];
        assert_eq!(pick(&held, None, None).unwrap().id, "p1");
        assert_eq!(pick(&[], None, None).unwrap_err().exit_code(), 3);
    }

    #[test]
    fn several_need_a_reference_by_label_or_id() {
        let held = vec![
            ctx("p1", "org-a", Some("adult1")),
            ctx("p2", "org-a", Some("child1")),
            ctx("p3", "org-b", Some("adult1")),
        ];
        assert_eq!(pick(&held, None, None).unwrap_err().exit_code(), 2);
        assert_eq!(pick(&held, Some("child1"), None).unwrap().id, "p2");
        assert_eq!(pick(&held, Some("p3"), None).unwrap().id, "p3");
        // Two contexts share the label: ambiguous until --org narrows it.
        assert_eq!(
            pick(&held, Some("adult1"), None).unwrap_err().exit_code(),
            4
        );
        assert_eq!(pick(&held, Some("adult1"), Some("org-b")).unwrap().id, "p3");
        assert_eq!(pick(&held, None, Some("org-c")).unwrap_err().exit_code(), 3);
    }

    #[test]
    fn the_index_round_trips_and_replaces_a_context() {
        let dir = tempfile::tempdir().unwrap();
        let index = Index::beside(&dir.path().join("config.json"));
        assert!(index.load().unwrap().is_empty());
        index.upsert(ctx("p1", "org-a", Some("adult1"))).unwrap();
        index.upsert(ctx("p1", "org-a", Some("renamed"))).unwrap();
        index.upsert(ctx("p2", "org-a", None)).unwrap();
        let all = index.load().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label.as_deref(), Some("renamed"));
        index.remove("org-a", "p1").unwrap();
        assert_eq!(index.load().unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(index.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
