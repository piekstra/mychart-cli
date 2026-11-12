//! `mychart auth login|status|logout`.

use std::time::Duration;

use pk_cli_auth::oauth::{ClientAuth, OAuthClient};
use pk_cli_auth::{AuthMethod, AuthStatus};
use pk_cli_core::transport::Transport;
use pk_cli_core::{block_on, output, CliError};
use pk_med_smart::{
    begin, decode_session, discover, register_dynamic_client, JwtSigner, LaunchRequest, Renewer,
    RsaKey, ScopeSet, SessionStore, SmartApp, SmartSession,
};
use serde_json::{json, Value};

use super::{orgs as orgs_cmd, rfc3339, transport, Ctx, BIN};
use crate::cli::{AuthCmd, LoginArgs, LogoutArgs};
use crate::epic::{self, EpicQuirks};
use crate::fhir::Fhir;
use crate::orgs::{self, Org};
use crate::session::{self, now, Context};
use crate::vault::Sessions;

/// The per-install key's size. Epic accepts RS384 with 2048 bits or more;
/// 3072 is NIST's size for keys used past 2030.
const KEY_BITS: usize = 3072;

fn now_u64() -> u64 {
    now().max(0) as u64
}

pub fn run(ctx: &Ctx, cmd: &AuthCmd) -> Result<(), CliError> {
    match cmd {
        AuthCmd::Login(args) => login(ctx, args),
        AuthCmd::Status => status(ctx),
        AuthCmd::Logout(args) => logout(ctx, args),
    }
}

/// A label names a person in this household; it travels in commands and in
/// a Keychain-free index, so it is short and plain.
fn validate_label(label: &str) -> Result<(), CliError> {
    let ok = !label.is_empty()
        && label.len() <= 40
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(CliError::Usage(
            "a label is 1-40 letters, digits, '-', '_' or '.' (for example adult1 or child1)"
                .into(),
        ))
    }
}

/// `--org`: `sandbox` needs no directory; anything else resolves against
/// the cached directory, downloading it when it is missing or a week old.
pub fn resolve_org(ctx: &Ctx, query: &str) -> Result<Org, CliError> {
    if query.trim().eq_ignore_ascii_case(orgs::SANDBOX_ID) {
        return Ok(Org::sandbox());
    }
    let index = orgs_cmd::index(ctx, false)?;
    let all = index.all();
    orgs::resolve(&all, query).cloned()
}

fn login(ctx: &Ctx, args: &LoginArgs) -> Result<(), CliError> {
    if let Some(l) = &args.label {
        validate_label(l)?;
    }
    if args.timeout == 0 || args.timeout > 60 {
        return Err(CliError::Usage("--timeout is 1 to 60 minutes".into()));
    }
    let cfg = ctx.config()?;
    let org = resolve_org(ctx, &args.org)?;
    let (client_id, key_name) = if org.is_sandbox() {
        (cfg.sandbox_client_id.clone(), "sandbox_client_id")
    } else {
        (cfg.client_id.clone(), "client_id")
    };
    let client_id = client_id.ok_or_else(|| {
        CliError::Usage(format!("no {key_name} is configured")).with_remedy(format!(
            "register this household's app with Epic (docs/registration.md), then \
             `{BIN} config set {key_name} <ID>`"
        ))
    })?;
    if let Some(host) = cfg.stale_redirect_host() {
        ctx.note(format!(
            "warning: config.json sets redirect_host `{host}`, which mychart 0.2 does not use: \
             this sign-in's redirect is {}. If the app at fhir.epic.com lists only \
             `{host}`, Epic refuses it; add this URI there, then `{BIN} config unset redirect_host`",
            cfg.redirect_uri()
        ));
    }
    // Discovery's answer names the endpoints the sign-in goes to, so it too
    // must come over https (or from this machine).
    pk_cli_core::transport::require_secure_url(&org.fhir_base)?;
    let lock = &ctx.lock;
    lock.before_signin()?;

    let t = transport()?;
    let quirks = EpicQuirks::for_client(&client_id);
    let version = env!("CARGO_PKG_VERSION");
    let config = block_on(discover(&t, &org.fhir_base, &quirks, BIN, version))?;
    let client = ClientAuth::public(client_id.clone());
    let app = SmartApp {
        app: BIN.into(),
        client: ClientAuth::public(client_id.clone()),
        ports: vec![cfg.redirect_port()],
    };
    let signin = begin(
        &app,
        LaunchRequest {
            config: &config,
            fhir_base: &org.fhir_base,
            aud: Some(org.aud()),
            scopes: ScopeSet::parse(cfg.scope()),
            quirks: &quirks,
        },
    )?;
    let url = signin.url().to_string();
    let opened = !args.no_browser && (ctx.browser)(&url);
    eprintln!(
        "Sign in to {} in the browser{}. If a list of people appears, pick the one to link.\n\
         If no browser opened, visit:\n  {url}",
        org.name,
        if opened { " window that opened" } else { "" }
    );
    let code = signin.wait_for_code(Duration::from_secs(args.timeout * 60), |why| {
        eprintln!("ignored an answer that is not this sign-in's: {why}")
    })?;
    let oauth = OAuthClient::new(t.clone(), BIN, version);
    let mut session = block_on(signin.exchange(&oauth, &client, &code, &org.id, now_u64()))?;
    session.tokens.expires_at = Some(session::expiry(&session.tokens));
    let patient = session.patient.clone().ok_or_else(|| {
        CliError::Upstream(
            "the organization granted access without a patient; the app is not registered \
             for patients (docs/registration.md)"
                .into(),
        )
    })?;
    if !is_fhir_id(&patient) {
        return Err(CliError::Upstream(
            "the organization granted access to a patient id that is not a FHIR id".into(),
        ));
    }
    let fhir_user = session
        .fhir_user
        .as_deref()
        .and_then(epic::fhir_user_reference);
    let at = now();

    let sessions = Sessions::new(ctx.vault.clone());
    let register = config.registration_endpoint.is_some() && cfg.dynamic_registration();
    if register {
        ctx.note("registering this install with the organization, so access lasts past an hour");
        let key = RsaKey::generate(KEY_BITS)?;
        session = block_on(register_dynamic_client(
            &oauth,
            &config,
            session,
            &client_id,
            key.jwks(),
            &key,
            key.to_pkcs8()?,
        ))?;
        // Epic's initial access token is for the registration only ("This
        // token is intentionally one-time-use"), so the session is due from
        // here: the first JWT-bearer grant, below or in the next command,
        // gets the token the reads use.
        session.tokens.expires_at = Some(0);
    }
    lock.store_signin(|| sessions.save(&session))?;
    let context = Context {
        id: patient.clone(),
        organization: org.id.clone(),
        organization_name: org.name.clone(),
        fhir_base: org.fhir_base.clone(),
        label: args.label.clone(),
        relationship: epic::relationship(fhir_user.as_deref(), &patient).map(str::to_string),
        fhir_user,
        signed_in_at: rfc3339(at),
    };
    ctx.index()?.upsert(context.clone())?;

    if register {
        let renewer_quirks = EpicQuirks::default();
        let signer = |c: &pk_med_smart::DynamicClient| -> Result<Box<dyn JwtSigner>, CliError> {
            Ok(Box::new(RsaKey::for_client(c)?))
        };
        let renewed = Renewer::new(&sessions, lock, &oauth, &client, &renewer_quirks)
            .signer(&signer)
            .renew(&context.account(), now_u64());
        match renewed {
            Ok(s) => session = s,
            Err(e) => ctx.note(format!(
                "warning: this install's first token request failed ({}); the sign-in \
                 is stored and the next command asks again",
                e.message()
            )),
        }
    } else if session.tokens.refresh_token.is_none() && cfg.dynamic_registration() {
        ctx.note(format!(
            "warning: {} offers this app no dynamic client registration; this sign-in lasts until {}",
            org.name,
            session::expires_at(&session.tokens)
        ));
    }

    let verified = verify(&t, &session);
    let expires_at = session::expires_at(&session.tokens);
    let payload = json!({
        "schema": "auth-login/v1",
        "stored": true,
        "verified": verified,
        "patient": patient_dto(&context, Some(&session)),
        "renewable": session.renewable(),
        "expires_at": expires_at,
    });
    if ctx.json() {
        output::json(&payload);
    } else {
        println!(
            "Signed in: patient {} at {} ({}){}",
            context.display(),
            org.name,
            context
                .relationship
                .as_deref()
                .unwrap_or("relationship unknown"),
            if verified {
                ""
            } else {
                "; the first read was refused"
            }
        );
        println!(
            "Access {}",
            if session.renewable() {
                "renews without a sign-in for the period chosen in MyChart".to_string()
            } else {
                format!("ends at {}", expires_at)
            }
        );
    }
    Ok(())
}

/// A FHIR `id`: 1 to 64 of `A-Z a-z 0-9 - .`. The patient id goes into
/// request paths and the Keychain account's name.
fn is_fhir_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
}

/// One read of the patient's own record, to prove the session works,
/// through the same origin and https checks as every other request. Only
/// the resource type is looked at. A session still waiting for its first
/// grant has no token to read with.
fn verify<T: Transport + Clone + Send + Sync + 'static>(t: &T, s: &SmartSession) -> bool {
    let (Some(patient), false) = (&s.patient, s.tokens.is_expired(now_u64(), 0)) else {
        return false;
    };
    Fhir::with_token(t, &s.fhir_base, s.bearer()).is_ok_and(|f| {
        f.read(&format!("Patient/{patient}"))
            .is_ok_and(|r| r.resource_type == "Patient")
    })
}

/// What the Keychain holds for a context.
pub enum Stored {
    Missing,
    /// An item this release cannot read (one mychart 0.1 stored, or a
    /// damaged one): it needs a new sign-in, and `auth logout` removes it.
    Unreadable(CliError),
    Session(Box<SmartSession>),
}

impl Stored {
    pub fn session(&self) -> Option<&SmartSession> {
        match self {
            Stored::Session(s) => Some(s),
            _ => None,
        }
    }

    pub fn in_keychain(&self) -> bool {
        !matches!(self, Stored::Missing)
    }
}

/// The profile's `PatientContext` DTO. `session` names the Keychain item
/// holding the sign-in, never its content; a stored session's state goes
/// in the extra `status` object.
pub fn patient_dto(c: &Context, session: Option<&SmartSession>) -> Value {
    let mut pc = c.patient_context();
    pc.session = Some(c.account());
    // Strings and optional strings: nothing to fail on.
    let mut v = serde_json::to_value(pc).expect("a PatientContext serializes");
    v["organization_name"] = c.organization_name.clone().into();
    v["fhir_base"] = c.fhir_base.clone().into();
    v["signed_in_at"] = c.signed_in_at.clone().into();
    if let Some(s) = session {
        v["status"] = json!({
            "authenticated": session::usable(s, now()),
            "renewable": s.renewable(),
            "expires_at": session::expires_at(&s.tokens),
        });
        if let Some(at) = s.refused_at {
            v["status"]["refused_at"] = rfc3339(at as i64).into();
        }
    }
    v
}

/// [`patient_dto`] for whatever the Keychain holds.
pub fn status_row(c: &Context, stored: &Stored) -> Value {
    let mut row = patient_dto(c, stored.session());
    if let Stored::Unreadable(e) = stored {
        row["status"] = json!({
            "authenticated": false,
            "renewable": false,
            "unreadable": e.message(),
        });
    }
    row["credential_in_keychain"] = stored.in_keychain().into();
    row
}

/// Whether a `patient_dto` row's stored sign-in can make requests.
pub fn usable(row: &Value) -> bool {
    row.pointer("/status/authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// A context's stored session. A Keychain that refuses the read is the
/// error; an item that does not read is [`Stored::Unreadable`].
pub fn stored(ctx: &Ctx, c: &Context) -> Result<Stored, CliError> {
    let Some(text) = ctx.vault.read(&c.account())? else {
        return Ok(Stored::Missing);
    };
    Ok(match decode_session(&text) {
        Ok(s) => Stored::Session(Box::new(s)),
        Err(e) => Stored::Unreadable(e),
    })
}

fn status(ctx: &Ctx) -> Result<(), CliError> {
    let contexts = ctx.contexts()?;
    let mut rows = Vec::new();
    let mut any_stored = false;
    let mut any_usable = false;
    for c in &contexts {
        let s = stored(ctx, c)?;
        any_stored |= s.in_keychain();
        any_usable |= s.session().is_some_and(|s| session::usable(s, now()));
        rows.push(status_row(c, &s));
    }
    let mut status = AuthStatus::new(true, any_usable, AuthMethod::BrowserSession);
    status.credential_in_keychain = Some(any_stored);
    status.session_valid = Some(any_usable);
    let mut v = status.to_json();
    v["patients"] = Value::Array(rows.clone());
    if ctx.json() {
        output::json(&v);
    } else {
        status.render();
        if rows.is_empty() {
            println!("Patients:      none signed in (`{BIN} auth login --org <ORG>`)");
        } else {
            println!();
            let view: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "patient": r["label"].as_str().unwrap_or(r["id"].as_str().unwrap_or_default()),
                        "organization": r["organization_name"],
                        "relationship": r["relationship"],
                        "usable": usable(r),
                        "renewable": r.pointer("/status/renewable").cloned().unwrap_or(false.into()),
                        "token_expires": r.pointer("/status/expires_at").cloned().unwrap_or_default(),
                        "refused_at": r.pointer("/status/refused_at").cloned().unwrap_or_default(),
                    })
                })
                .collect();
            output::table(&view);
        }
    }
    Ok(())
}

fn logout(ctx: &Ctx, args: &LogoutArgs) -> Result<(), CliError> {
    let contexts = ctx.contexts()?;
    let chosen: Vec<&Context> = match (&args.patient, &args.org) {
        (None, None) => contexts.iter().collect(),
        (patient, org) => {
            let org_id = match org {
                Some(o) if o.eq_ignore_ascii_case(orgs::SANDBOX_ID) => {
                    Some(orgs::SANDBOX_ID.to_string())
                }
                Some(o) => Some(
                    contexts
                        .iter()
                        .find(|c| {
                            c.organization == *o || c.organization_name.eq_ignore_ascii_case(o)
                        })
                        .map(|c| c.organization.clone())
                        .unwrap_or_else(|| o.clone()),
                ),
                None => None,
            };
            match patient {
                Some(p) => vec![session::pick(&contexts, Some(p), org_id.as_deref())?],
                None => contexts
                    .iter()
                    .filter(|c| Some(&c.organization) == org_id.as_ref())
                    .collect(),
            }
        }
    };
    let index = ctx.index()?;
    let mut removed = Vec::new();
    for c in chosen {
        let account = c.account();
        ctx.lock.forget(|| ctx.vault.delete(&account))?;
        index.remove(&c.organization, &c.id)?;
        removed.push(json!({ "id": c.id, "organization": c.organization }));
    }
    if args.forget {
        ctx.store.clear()?;
    }
    let payload = json!({
        "removed": removed,
        "config_cleared": args.forget,
    });
    output::emit(ctx.json(), "auth-logout", payload, |p| {
        let n = p["removed"].as_array().map_or(0, Vec::len);
        println!(
            "signed out of {n} patient context{}{}",
            if n == 1 { "" } else { "s" },
            if args.forget { "; config cleared" } else { "" }
        );
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_patient_id_must_be_a_fhir_id() {
        assert!(super::is_fhir_id("eq081-VQEgP8drUUqCWzHfw3"));
        assert!(super::is_fhir_id("a.b-1"));
        for bad in ["", "x/../y", "x?y", "x y", &"a".repeat(65)] {
            assert!(!super::is_fhir_id(bad), "{bad}");
        }
    }

    use super::*;

    #[test]
    fn labels_are_short_and_plain() {
        for ok in ["adult1", "child1", "adult-2", "a.b_c"] {
            validate_label(ok).unwrap();
        }
        for bad in ["", "has space", "slash/inside", &"x".repeat(41)] {
            assert_eq!(validate_label(bad).unwrap_err().exit_code(), 2, "{bad:?}");
        }
    }
}
