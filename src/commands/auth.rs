//! `mychart auth login|status|logout`.

use std::time::Duration;

use pk_cli_auth::{AuthMethod, AuthStatus};
use pk_cli_core::transport::Transport;
use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::{orgs as orgs_cmd, rfc3339, transport, Ctx, BIN};
use crate::cli::{AuthCmd, LoginArgs, LogoutArgs};
use crate::config::REDIRECT_PATH;
use crate::key::SigningKey;
use crate::oauth::{self, LoopbackListener, Pkce};
use crate::orgs::{self, Org};
use crate::session::{self, now, Context, Session};
use crate::smart;
use crate::vault;

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
    // Discovery's answer names the endpoints the sign-in goes to, so it too
    // must come over https (or from this machine).
    pk_cli_core::transport::require_secure_url(&org.fhir_base)?;
    let lock = &ctx.lock;
    lock.before_signin()?;

    let t = transport()?;
    let endpoints = smart::discover(&t, &org.fhir_base, &client_id)?;
    let listener = LoopbackListener::bind(cfg.redirect_port(), REDIRECT_PATH)?;
    let redirect_uri = cfg.redirect_uri(listener.port());
    let pkce = Pkce::generate()?;
    let state = oauth::random_urlsafe(24)?;
    let url = smart::authorize_url(
        &endpoints,
        &client_id,
        &redirect_uri,
        cfg.scope(),
        &state,
        &pkce.challenge,
        org.aud(),
    );
    let opened = !args.no_browser && (ctx.browser)(&url);
    eprintln!(
        "Sign in to {} in the browser{}. If a list of people appears, pick the one to link.\n\
         If no browser opened, visit:\n  {url}",
        org.name,
        if opened { " window that opened" } else { "" }
    );
    let code = listener.wait_for_code(&state, Duration::from_secs(args.timeout * 60))?;
    let granted = smart::exchange_code(
        &t,
        &endpoints,
        &client_id,
        &code,
        &redirect_uri,
        &pkce.verifier,
    )?;
    let patient = granted.patient.clone().ok_or_else(|| {
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
    let fhir_user = granted.id_token.as_deref().and_then(smart::fhir_user);
    let at = now();
    let mut session = Session {
        version: 1,
        organization: org.id.clone(),
        fhir_base: org.fhir_base.clone(),
        token_endpoint: endpoints.token.clone(),
        client_id: client_id.clone(),
        dynamic_client_id: None,
        private_key: None,
        access_token: String::new(),
        expires_at: 0,
        refresh_token: None,
        scope: String::new(),
        patient: patient.clone(),
        fhir_user: fhir_user.clone(),
        signed_in_at: at,
        refused_at: None,
    };
    session.apply(&granted, at);

    match (&endpoints.register, cfg.dynamic_registration()) {
        (Some(register), true) => {
            ctx.note("registering this install with the organization, so access lasts past an hour");
            let key = SigningKey::generate()?;
            let dynamic = smart::register_dynamic_client(
                &t,
                register,
                &granted.access_token,
                &client_id,
                &key,
            )?;
            // The registration used up the one-time initial token, so from
            // here the dynamic client and its key are the sign-in: kept even
            // when the first grant fails, which the next command retries.
            session.private_key = Some(key.to_stored()?);
            session.dynamic_client_id = Some(dynamic.clone());
            match smart::jwt_bearer_grant(&t, &endpoints.token, &dynamic, &key, now()) {
                Ok(renewed) => session.apply(&renewed, now()),
                Err(e) => {
                    session.expires_at = 0;
                    ctx.note(format!(
                        "warning: this install's first token request failed ({}); the sign-in \
                         is stored and the next command asks again",
                        e.message()
                    ));
                }
            }
        }
        (None, true) if session.refresh_token.is_none() => ctx.note(format!(
            "warning: {} offers this app no dynamic client registration; this sign-in lasts until {}",
            org.name,
            rfc3339(session.expires_at)
        )),
        _ => {}
    }

    let context = Context {
        id: patient.clone(),
        organization: org.id.clone(),
        organization_name: org.name.clone(),
        fhir_base: org.fhir_base.clone(),
        label: args.label.clone(),
        relationship: smart::relationship(fhir_user.as_deref(), &patient).map(str::to_string),
        fhir_user,
        signed_in_at: rfc3339(at),
    };
    lock.store_signin(|| vault::set_json(ctx.vault.as_ref(), &context.account(), &session))?;
    ctx.index()?.upsert(context.clone())?;

    let verified = verify(&t, &session);
    let payload = json!({
        "schema": "auth-login/v1",
        "stored": true,
        "verified": verified,
        "patient": patient_dto(&context, Some(&session)),
        "renewable": session.renewable(),
        "expires_at": rfc3339(session.expires_at),
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
                format!("ends at {}", rfc3339(session.expires_at))
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

/// The token just granted, as a [`Bearer`](crate::fhir::Bearer).
struct Granted<'a>(&'a str);

impl crate::fhir::Bearer for Granted<'_> {
    fn token(&self) -> Result<String, CliError> {
        Ok(self.0.to_string())
    }
}

/// One read of the patient's own record, to prove the session works,
/// through the same origin and https checks as every other request. Only
/// the resource type is looked at.
fn verify<T: Transport>(t: &T, s: &Session) -> bool {
    let bearer = Granted(&s.access_token);
    crate::fhir::Fhir::new(t, &s.fhir_base, &bearer)
        .read(&format!("Patient/{}", s.patient))
        .is_ok_and(|r| r.resource_type == "Patient")
}

/// The profile's `PatientContext` DTO. `session` names the Keychain item
/// holding the sign-in, never its content; a stored session's state goes
/// in the extra `status` object.
pub fn patient_dto(c: &Context, session: Option<&Session>) -> Value {
    let mut pc = c.patient_context();
    pc.session = Some(c.account());
    // Strings and optional strings: nothing to fail on.
    let mut v = serde_json::to_value(pc).expect("a PatientContext serializes");
    v["organization_name"] = c.organization_name.clone().into();
    v["fhir_base"] = c.fhir_base.clone().into();
    v["signed_in_at"] = c.signed_in_at.clone().into();
    if let Some(s) = session {
        let usable = s.usable(now());
        v["status"] = json!({
            "authenticated": usable,
            "renewable": s.renewable(),
            "expires_at": rfc3339(s.expires_at),
        });
        if let Some(at) = s.refused_at {
            v["status"]["refused_at"] = rfc3339(at).into();
        }
    }
    v
}

/// Whether a `patient_dto` row's stored sign-in can make requests.
pub fn usable(row: &Value) -> bool {
    row.pointer("/status/authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// A context's stored session, or `None` when its Keychain item is gone.
pub fn stored(ctx: &Ctx, c: &Context) -> Result<Option<Session>, CliError> {
    vault::get_json::<Session>(ctx.vault.as_ref(), &c.account())
}

fn status(ctx: &Ctx) -> Result<(), CliError> {
    let contexts = ctx.contexts()?;
    let mut rows = Vec::new();
    let mut any_stored = false;
    let mut any_usable = false;
    for c in &contexts {
        let s = stored(ctx, c)?;
        any_stored |= s.is_some();
        any_usable |= s.as_ref().is_some_and(|s| s.usable(now()));
        let mut row = patient_dto(c, s.as_ref());
        row["credential_in_keychain"] = s.is_some().into();
        rows.push(row);
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
