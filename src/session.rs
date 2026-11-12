//! The patient contexts this install holds: one Keychain item per
//! (organization, patient) holding its `pk_med_smart::SmartSession`, and a
//! non-secret index beside the config listing them, so `patients list`
//! reads no Keychain item. [`Live`] is a signed-in context's token for a run
//! of requests, renewed through pk-med-smart's `Renewer` under the
//! family's refresh lock.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pk_cli_auth::oauth::{ClientAuth, OAuthClient};
use pk_cli_auth::refresh_lock::RefreshLock;
use pk_cli_core::transport::Transport;
use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::Secret;
use pk_med_smart::{DynamicClient, JwtSigner, Renewer, RsaKey, SessionStore, SmartSession};
use serde::{Deserialize, Serialize};

use crate::epic::EpicQuirks;
use crate::vault::{Sessions, Vault};

pub use pk_med_smart::account;

/// Treat an access token this close to its expiry as expired, so a command
/// does not start an export that fails part way.
pub const EXPIRY_SKEW_SECS: u64 = 120;

/// How long an access token lasts when its token answer has no
/// `expires_in`: an hour, as mychart 0.1 took it. Epic states one; a token
/// with no stated end would otherwise never be renewed before a 401, and
/// `expires_at` would have nothing to show.
pub const UNSTATED_LIFETIME_SECS: u64 = 3600;

/// When `tokens` stop working: as the organization said, or an hour after
/// they were issued.
pub fn expiry(tokens: &pk_cli_auth::oauth::TokenSet) -> u64 {
    tokens
        .expires_at
        .unwrap_or_else(|| tokens.obtained_at.saturating_add(UNSTATED_LIFETIME_SECS))
}

/// [`expiry`] as RFC 3339: what `expires_at` shows.
pub fn expires_at(tokens: &pk_cli_auth::oauth::TokenSet) -> String {
    pk_cli_core::dates::fmt_rfc3339(expiry(tokens) as i64)
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

    /// The profile's `PatientContext`, as the stream header carries it: no
    /// Keychain account (that is this machine's), no name (never read).
    pub fn patient_context(&self) -> pk_med_records::PatientContext {
        pk_med_records::PatientContext {
            id: self.id.clone(),
            organization: self.organization.clone(),
            label: self.label.clone(),
            relationship: self.relationship.clone(),
            kind: Some("patient".into()),
            member_role: None,
            fhir_user: self.fhir_user.clone(),
            name: None,
            session: None,
        }
    }

    /// The profile's `Organization` for the stream header.
    pub fn organization_dto(&self) -> pk_med_records::Organization {
        pk_med_records::Organization {
            id: self.organization.clone(),
            name: self.organization_name.clone(),
            fhir_base: self.fhir_base.clone(),
            vendor: Some(crate::orgs::VENDOR.into()),
            location: None,
            aliases: Vec::new(),
        }
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
                .map_err(|e| {
                    CliError::Other(format!(
                        "{} {}",
                        self.path.display(),
                        crate::fhir::describe_json_error(&e)
                    ))
                }),
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

fn unix_now() -> u64 {
    now().max(0) as u64
}

/// `mychart auth login --org <ORG> [--label <WHO>]` for this context.
pub fn login_command(ctx: &Context) -> String {
    format!(
        "mychart auth login --org {}{}",
        ctx.organization,
        ctx.label
            .as_ref()
            .map(|l| format!(" --label {l}"))
            .unwrap_or_default()
    )
}

/// The stored session for `ctx`, or exit 3 when the Keychain holds none.
pub fn load(sessions: &dyn SessionStore, ctx: &Context) -> Result<SmartSession, CliError> {
    let stored = sessions.load(&ctx.account()).map_err(|e| {
        if e.reason() == Some(Reason::CredentialUnreadable) {
            CliError::Auth(e.message().to_string())
                .because(Reason::CredentialUnreadable)
                .with_remedy(format!(
                    "sign in again: `{}` (it replaces the stored item; a sign-in mychart 0.1 \
                     stored does not read in 0.2)",
                    login_command(ctx)
                ))
        } else {
            e
        }
    })?;
    stored.ok_or_else(|| {
        CliError::Auth(format!(
            "no stored sign-in for patient {} at {}",
            ctx.display(),
            ctx.organization_name
        ))
        .because(Reason::NoCredential)
        .with_remedy(format!("run `{}`", login_command(ctx)))
    })
}

/// Whether a stored session's access token is good at `now`.
pub fn is_fresh(s: &SmartSession, now: i64) -> bool {
    s.refused_at.is_none() && !s.tokens.is_expired(now.max(0) as u64, EXPIRY_SKEW_SECS)
}

/// Whether a stored session can make requests: not refused, and fresh or
/// renewable without the person.
pub fn usable(s: &SmartSession, now: i64) -> bool {
    s.refused_at.is_none() && (is_fresh(s, now) || s.renewable())
}

/// "Access ... has ended" for a refusal, with the command that signs in
/// again.
fn ended(ctx: &Context, why: &str) -> CliError {
    CliError::Auth(format!(
        "access for patient {} at {} has ended ({why})",
        ctx.display(),
        ctx.organization_name
    ))
    .because(Reason::CredentialRefused)
    .with_remedy(format!("sign in again: `{}`", login_command(ctx)))
}

/// A renewal's refusal, said for this context. Other errors pass through.
fn explain(ctx: &Context, e: CliError) -> CliError {
    if e.is_auth() && e.reason() == Some(Reason::CredentialRefused) {
        ended(ctx, e.message())
    } else {
        e
    }
}

pub type Note = Arc<dyn Fn(&str) + Send + Sync>;

/// A signed-in context's bearer for a run of requests: renewed through the
/// refresh lock when it falls due, so an export longer than the token's
/// hour keeps going, and once after a 401. `Send + Sync + 'static`, so the
/// FHIR client's token source can own a clone.
pub struct Live<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Clone for Live<T> {
    fn clone(&self) -> Self {
        Live {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct Inner<T> {
    sessions: Sessions,
    lock: RefreshLock,
    oauth: OAuthClient<T>,
    /// The client the stored tokens were issued to, for a refresh grant.
    client: ClientAuth,
    context: Context,
    fhir_base: String,
    /// The token renewed after a 401, so a 401 to it too ends the sign-in.
    reissued: Mutex<Option<String>>,
    /// The token the source last handed out; `None` after it failed.
    last_sent: Mutex<Option<String>>,
    note: Option<Note>,
}

fn held<V: Clone>(m: &Mutex<V>) -> V {
    m.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

fn put<V>(m: &Mutex<V>, value: V) {
    *m.lock().unwrap_or_else(|p| p.into_inner()) = value;
}

impl<T: Transport + Clone + Send + Sync + 'static> Live<T> {
    /// The context's session, renewed now if it is due. The token goes only
    /// to the server it was issued for, as the Keychain records it: the
    /// index is a plain file anyone running as this user can edit. `note`
    /// is where a renewal after a 401 is reported, so it is never silent.
    pub fn start(
        t: &T,
        vault: Arc<dyn Vault>,
        lock: &RefreshLock,
        context: &Context,
        note: Option<Note>,
    ) -> Result<Self, CliError> {
        let sessions = Sessions::new(vault);
        let stored = load(&sessions, context)?;
        if crate::fhir::origin(&stored.fhir_base) != crate::fhir::origin(&context.fhir_base) {
            return Err(CliError::Auth(format!(
                "the stored sign-in for patient {} was made at {}, but the patient index names {}",
                context.display(),
                crate::fhir::origin(&stored.fhir_base).unwrap_or_default(),
                crate::fhir::origin(&context.fhir_base).unwrap_or_default()
            ))
            .with_remedy(format!("sign in again: `{}`", login_command(context))));
        }
        let live = Live {
            inner: Arc::new(Inner {
                sessions,
                lock: lock.clone(),
                oauth: OAuthClient::new(t.clone(), crate::commands::BIN, env!("CARGO_PKG_VERSION")),
                client: ClientAuth::public(stored.client_id.clone()),
                context: context.clone(),
                fhir_base: stored.fhir_base.clone(),
                reissued: Mutex::new(None),
                last_sent: Mutex::new(None),
                note,
            }),
        };
        live.fresh()?;
        Ok(live)
    }

    /// The base the Keychain's session was issued for: the only one its
    /// token is sent to.
    pub fn fhir_base(&self) -> &str {
        &self.inner.fhir_base
    }

    pub fn context(&self) -> &Context {
        &self.inner.context
    }

    /// One line for the person (stderr), unless the run is quiet.
    pub fn say(&self, line: &str) {
        if let Some(note) = &self.inner.note {
            note(line)
        }
    }

    fn with_renewer<R>(&self, f: impl FnOnce(&Renewer<'_, T>) -> R) -> R {
        let inner = &*self.inner;
        let quirks = EpicQuirks::default();
        let signer = |client: &DynamicClient| -> Result<Box<dyn JwtSigner>, CliError> {
            Ok(Box::new(RsaKey::for_client(client)?))
        };
        let mut renewer = Renewer::new(
            &inner.sessions,
            &inner.lock,
            &inner.oauth,
            &inner.client,
            &quirks,
        )
        .signer(&signer);
        renewer.skew_secs = EXPIRY_SKEW_SECS;
        f(&renewer)
    }

    /// The stored session with a usable token, renewed first when it is due
    /// or was refused.
    pub fn fresh(&self) -> Result<SmartSession, CliError> {
        let account = self.inner.context.account();
        self.with_renewer(|r| r.fresh(&account, unix_now()))
            .map_err(|e| explain(&self.inner.context, e))
    }

    /// A new token after the organization answered 401 to `refused`, under
    /// the refresh lock: a process that renewed meanwhile has stored a
    /// different token, and that one is used instead. A 401 to the token
    /// this run renewed to ends the sign-in ([`Self::refuse`]).
    pub fn after_refusal(&self, refused: &str) -> Result<SmartSession, CliError> {
        if held(&self.inner.reissued).as_deref() == Some(refused) {
            return Err(self.refuse(refused));
        }
        let account = self.inner.context.account();
        let renewed = self
            .with_renewer(|r| r.after_refusal(&account, refused, unix_now()))
            .map_err(|e| explain(&self.inner.context, e))?;
        put(
            &self.inner.reissued,
            Some(renewed.tokens.access_token.clone()),
        );
        self.say(&format!(
            "{} refused the access token (HTTP 401); renewed it, asking again",
            crate::fhir::origin(&self.inner.fhir_base).unwrap_or_default()
        ));
        Ok(renewed)
    }

    /// The token source for `pk_med_fhir::FhirClient::bearer_with`.
    pub fn token(&self, refused: Option<&Secret>) -> Result<Secret, CliError> {
        let session = match refused {
            None => self.fresh(),
            Some(t) => self.after_refusal(t.expose()),
        };
        put(
            &self.inner.last_sent,
            session.as_ref().ok().map(|s| s.tokens.access_token.clone()),
        );
        Ok(session?.bearer())
    }

    /// A request ended on a 401 after this run renewed: the organization
    /// refused the token it had just issued, so the access itself has ended.
    /// Recorded (`refused_at`) only while the Keychain still holds that
    /// token: a process that stored a newer one meanwhile keeps it
    /// unmarked, and this run stops with a plain auth error. `None` when
    /// this run renewed nothing (the 401 is the request's own answer).
    pub fn ended_after_renewal(&self) -> Option<CliError> {
        let reissued = held(&self.inner.reissued)?;
        (held(&self.inner.last_sent).as_deref() == Some(reissued.as_str()))
            .then(|| self.refuse(&reissued))
    }

    fn refuse(&self, token: &str) -> CliError {
        let inner = &*self.inner;
        let ctx = &inner.context;
        let marked = inner.lock.renew(
            || load(&inner.sessions, ctx),
            |latest| latest.refused_at.is_none() && latest.tokens.access_token == token,
            |latest| {
                let refused = SmartSession {
                    refused_at: Some(unix_now()),
                    ..latest
                };
                inner.sessions.save(&refused)?;
                Ok(refused)
            },
        );
        match marked {
            Ok(s) if s.tokens.access_token == token => ended(
                ctx,
                &format!(
                    "the organization refused it on {}",
                    crate::commands::rfc3339(s.refused_at.map_or_else(now, |at| at as i64))
                ),
            ),
            Ok(_) => CliError::Auth(format!(
                "the organization refused the token it had just issued for patient {} at {}; \
                 another process has stored a newer one",
                ctx.display(),
                ctx.organization_name
            ))
            .with_remedy("run the command again"),
            Err(e) => e,
        }
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
