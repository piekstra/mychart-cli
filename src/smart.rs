//! SMART App Launch for a standalone patient app against Epic: endpoint
//! discovery, the authorization request, the code exchange, Epic's dynamic
//! client registration, and the JWT-bearer and refresh grants.
//!
//! Epic's behaviour each step depends on is in docs/api.md, quoted. The
//! shape follows `pk-med-smart` (medical-common), which this module moves
//! into once that crate is tagged.

use pk_cli_core::transport::{require_secure_url, HttpRequest, HttpResponse, Transport};
use pk_cli_core::{block_on, CliError};
use serde_json::{json, Value};

use crate::key::SigningKey;
use crate::oauth;

pub const BIN: &str = "mychart";

/// The OAuth endpoints an organization's FHIR server names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub authorize: String,
    pub token: String,
    /// Dynamic client registration, when the server offers it to this
    /// client id.
    pub register: Option<String>,
}

/// What a token endpoint answered. No `Debug`: it holds tokens.
#[derive(Clone)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: Option<i64>,
    pub scope: Option<String>,
    pub patient: Option<String>,
    pub id_token: Option<String>,
    pub refresh_token: Option<String>,
}

fn request(method: &str, url: &str) -> HttpRequest {
    HttpRequest::new(method, url).user_agent(BIN, env!("CARGO_PKG_VERSION"))
}

fn send<T: Transport>(t: &T, req: HttpRequest) -> Result<HttpResponse, CliError> {
    block_on(t.send(req))
}

/// Discover the endpoints for `fhir_base`.
///
/// Epic can override the endpoints per client id, and names the
/// registration endpoint only to a client allowed to register dynamic
/// clients, both only in the CapabilityStatement fetched with the
/// `Epic-Client-ID` header. So the metadata comes first, and
/// `.well-known/smart-configuration` is the fallback for a server that
/// answers metadata without the SMART extension.
pub fn discover<T: Transport>(
    t: &T,
    fhir_base: &str,
    client_id: &str,
) -> Result<Endpoints, CliError> {
    let base = fhir_base.trim_end_matches('/');
    let meta = send(
        t,
        request("GET", &format!("{base}/metadata"))
            .header("Accept", "application/fhir+json")
            .header("Epic-Client-ID", client_id),
    )?;
    let mut found = if meta.is_success() {
        serde_json::from_str::<Value>(&meta.body)
            .ok()
            .and_then(|v| endpoints_from_capability(&v))
    } else {
        None
    };
    if found.is_none() {
        let well_known = send(
            t,
            request("GET", &format!("{base}/.well-known/smart-configuration"))
                .header("Accept", "application/json"),
        )?;
        if well_known.is_success() {
            found = serde_json::from_str::<Value>(&well_known.body)
                .ok()
                .and_then(|v| endpoints_from_smart_configuration(&v));
        }
    }
    let endpoints = found.ok_or_else(|| {
        CliError::Upstream(format!(
            "{base} names no OAuth endpoints in its metadata or .well-known/smart-configuration"
        ))
    })?;
    for url in [&endpoints.authorize, &endpoints.token]
        .into_iter()
        .chain(endpoints.register.as_ref())
    {
        require_secure_url(url).map_err(|e| CliError::Upstream(e.message().to_string()))?;
    }
    Ok(endpoints)
}

/// The `oauth-uris` extension of a CapabilityStatement's REST security.
pub fn endpoints_from_capability(cap: &Value) -> Option<Endpoints> {
    let rests = cap.get("rest")?.as_array()?;
    for rest in rests {
        let Some(exts) = rest
            .pointer("/security/extension")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for ext in exts {
            if !ext["url"]
                .as_str()
                .is_some_and(|u| u.ends_with("/oauth-uris"))
            {
                continue;
            }
            let uri = |name: &str| {
                ext["extension"].as_array().and_then(|inner| {
                    inner
                        .iter()
                        .find(|e| e["url"] == name)
                        .and_then(|e| e["valueUri"].as_str())
                        .map(|s| s.trim().to_string())
                })
            };
            if let (Some(authorize), Some(token)) = (uri("authorize"), uri("token")) {
                return Some(Endpoints {
                    authorize,
                    token,
                    register: uri("register"),
                });
            }
        }
    }
    None
}

pub fn endpoints_from_smart_configuration(v: &Value) -> Option<Endpoints> {
    Some(Endpoints {
        authorize: v["authorization_endpoint"].as_str()?.to_string(),
        token: v["token_endpoint"].as_str()?.to_string(),
        register: v["registration_endpoint"].as_str().map(str::to_string),
    })
}

/// The authorization request for a standalone patient launch. `aud` is the
/// FHIR base as the directory spells it (Epic requires `aud` from its May
/// 2023 version on, and the directory's spellings differ in trailing slash
/// and case), so it is sent untouched.
pub fn authorize_url(
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    challenge: &str,
    aud: &str,
) -> String {
    oauth::url_with_query(
        &endpoints.authorize,
        &[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("scope", scope),
            ("state", state),
            ("aud", aud),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
    )
}

/// Exchange the authorization code (public client: `client_id` in the body,
/// no secret, the PKCE verifier).
pub fn exchange_code<T: Transport>(
    t: &T,
    endpoints: &Endpoints,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenResponse, CliError> {
    post_token(
        t,
        &endpoints.token,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier),
        ],
    )
}

/// Register a dynamic client bound to the signed-in user: `software_id` is
/// the app's Epic client id, `jwks` the per-install public key, and the
/// bearer is the one-time initial access token from the code exchange.
/// Returns the dynamic client id.
pub fn register_dynamic_client<T: Transport>(
    t: &T,
    register_endpoint: &str,
    initial_access_token: &str,
    software_id: &str,
    key: &SigningKey,
) -> Result<String, CliError> {
    let body = json!({ "software_id": software_id, "jwks": key.public_jwks() });
    let resp = send(
        t,
        request("POST", register_endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {initial_access_token}"))
            .body(body.to_string()),
    )?;
    if !resp.is_success() {
        let (slug, detail) = oauth_error(&resp.body);
        let why =
            format!(
            "the organization refused to register this install as a dynamic client (HTTP {}{}{})",
            resp.status,
            if slug.is_empty() { "" } else { ", " },
            [slug, detail].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(": ")
        );
        return Err(match resp.status {
            500..=599 => CliError::Upstream(why),
            _ => CliError::Auth(why).with_remedy(
                "check that the app's registration at fhir.epic.com allows dynamic clients \
                 (docs/registration.md), or turn this step off with \
                 `mychart config set dynamic_registration false` (access then ends after an hour)",
            ),
        });
    }
    let v: Value = serde_json::from_str(&resp.body)
        .map_err(|e| CliError::Upstream(format!("the registration response is not JSON: {e}")))?;
    v["client_id"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CliError::Upstream("the registration response names no client_id".into()))
}

/// An access token for a dynamic client through the JWT-bearer grant
/// (RFC 7523 §2.1), which Epic answers for as long as the user's chosen
/// access period lasts.
pub fn jwt_bearer_grant<T: Transport>(
    t: &T,
    token_endpoint: &str,
    dynamic_client_id: &str,
    key: &SigningKey,
    now: i64,
) -> Result<TokenResponse, CliError> {
    let assertion = key.client_assertion(dynamic_client_id, token_endpoint, now)?;
    post_token(
        t,
        token_endpoint,
        &[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &assertion),
            ("client_id", dynamic_client_id),
        ],
    )
}

/// A refresh-token grant. A dynamic client authenticates with a signed
/// assertion; any other client sends its id.
pub fn refresh_grant<T: Transport>(
    t: &T,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
    key: Option<&SigningKey>,
    now: i64,
) -> Result<TokenResponse, CliError> {
    let assertion = match key {
        Some(k) => Some(k.client_assertion(client_id, token_endpoint, now)?),
        None => None,
    };
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    if let Some(a) = &assertion {
        params.push((
            "client_assertion_type",
            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
        ));
        params.push(("client_assertion", a));
    }
    post_token(t, token_endpoint, &params)
}

fn post_token<T: Transport>(
    t: &T,
    endpoint: &str,
    params: &[(&str, &str)],
) -> Result<TokenResponse, CliError> {
    let resp = send(
        t,
        request("POST", endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(oauth::form_encode(params)),
    )?;
    token_response(&resp)
}

/// Read a token endpoint's answer. An OAuth error is an auth failure (exit
/// 3) naming the error code; a server error is upstream (exit 5).
pub fn token_response(resp: &HttpResponse) -> Result<TokenResponse, CliError> {
    if !resp.is_success() {
        let (slug, detail) = oauth_error(&resp.body);
        let msg = explain_token_error(&slug, &detail, resp.status);
        return Err(if resp.status >= 500 || resp.status == 429 {
            CliError::Upstream(msg)
        } else {
            CliError::Auth(msg)
        });
    }
    let v: Value = serde_json::from_str(&resp.body)
        .map_err(|e| CliError::Upstream(format!("the token response is not JSON: {e}")))?;
    let text = |k: &str| v[k].as_str().map(str::to_string).filter(|s| !s.is_empty());
    let access_token = text("access_token")
        .ok_or_else(|| CliError::Upstream("the token response holds no access_token".into()))?;
    let expires_in = v["expires_in"]
        .as_i64()
        .or_else(|| v["expires_in"].as_str().and_then(|s| s.parse().ok()));
    Ok(TokenResponse {
        access_token,
        expires_in,
        scope: text("scope").map(|s| s.trim().to_string()),
        patient: text("patient"),
        id_token: text("id_token"),
        refresh_token: text("refresh_token"),
    })
}

/// `(error, error_description)` of an OAuth error body, or empty strings.
pub fn oauth_error(body: &str) -> (String, String) {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let s = |k: &str| v[k].as_str().unwrap_or_default().trim().to_string();
    (s("error"), s("error_description"))
}

fn explain_token_error(slug: &str, detail: &str, status: u16) -> String {
    let base = match slug {
        "invalid_grant" => "the organization refused the grant: the sign-in expired, was used already, or access was revoked or ran out",
        "invalid_client" => "the organization does not recognise this client id",
        "unauthorized_client" => "this client id may not use that grant at this organization",
        "invalid_request" => "the organization refused the token request as malformed",
        "" if status >= 500 => "the organization's token endpoint failed",
        "" => "the organization refused the token request",
        _ => "the organization refused the token request",
    };
    let detail = match (slug.is_empty(), detail.is_empty()) {
        (true, _) => String::new(),
        (false, true) => format!(" [{slug}]"),
        (false, false) => format!(" [{slug}: {detail}]"),
    };
    format!("{base}{detail} (HTTP {status})")
}

/// The `fhirUser` claim of an id_token ("Patient/abc", "RelatedPerson/x"),
/// read without verification: it decides only how a context is labelled.
/// Epic may send it as an absolute URL; the type and id are kept.
pub fn fhir_user(id_token: &str) -> Option<String> {
    let claims = pk_cli_auth::token::claims(id_token)?;
    let raw = claims["fhirUser"].as_str()?;
    let mut parts = raw.trim_end_matches('/').rsplitn(3, '/');
    let id = parts.next()?;
    let kind = parts.next()?;
    (!id.is_empty() && kind.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
        .then(|| format!("{kind}/{id}"))
}

/// `self` when the signed-in user is the patient, `proxy` when it is someone
/// else (a parent with proxy access), unknown without a `fhirUser`.
pub fn relationship(fhir_user: Option<&str>, patient: &str) -> Option<&'static str> {
    let user = fhir_user?;
    Some(if user == format!("Patient/{patient}") {
        "self"
    } else {
        "proxy"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn epic_metadata_names_the_three_endpoints() {
        let e = endpoints_from_capability(&fixture("epic_metadata.json")).unwrap();
        assert_eq!(e.authorize, "https://fhir.example.org/oauth2/authorize");
        assert_eq!(e.token, "https://fhir.example.org/oauth2/token");
        assert_eq!(
            e.register.as_deref(),
            Some("https://fhir.example.org/oauth2/register")
        );
    }

    #[test]
    fn a_smart_configuration_is_the_fallback() {
        let e = endpoints_from_smart_configuration(&fixture("smart_configuration.json")).unwrap();
        assert_eq!(e.token, "https://fhir.example.org/oauth2/token");
        assert_eq!(e.register, None);
    }

    #[test]
    fn the_authorize_url_carries_pkce_aud_and_state() {
        let e = Endpoints {
            authorize: "https://fhir.example.org/oauth2/authorize".into(),
            token: "t".into(),
            register: None,
        };
        let url = authorize_url(
            &e,
            "client-1",
            "http://127.0.0.1:7468/callback",
            "openid fhirUser",
            "st",
            "ch",
            "https://fhir.example.org/api/FHIR/R4/",
        );
        assert_eq!(
            oauth::query_param(&url, "aud").unwrap(),
            "https://fhir.example.org/api/FHIR/R4/"
        );
        assert_eq!(
            oauth::query_param(&url, "code_challenge_method").unwrap(),
            "S256"
        );
        assert_eq!(
            oauth::query_param(&url, "redirect_uri").unwrap(),
            "http://127.0.0.1:7468/callback"
        );
        assert_eq!(oauth::query_param(&url, "response_type").unwrap(), "code");
    }

    #[test]
    fn token_errors_map_to_exit_codes() {
        let refused = HttpResponse::new(
            400,
            r#"{"error":"invalid_grant","error_description":"expired"}"#,
        );
        let e = token_response(&refused).err().unwrap();
        assert_eq!(e.exit_code(), 3);
        assert!(e.to_string().contains("[invalid_grant: expired]"), "{e}");
        assert_eq!(
            token_response(&HttpResponse::new(503, "down"))
                .err()
                .unwrap()
                .exit_code(),
            5
        );
        let ok = HttpResponse::new(
            200,
            r#"{"access_token":"a","expires_in":"3600","patient":"p1"}"#,
        );
        let t = token_response(&ok).unwrap();
        assert_eq!(t.expires_in, Some(3600));
        assert_eq!(t.patient.as_deref(), Some("p1"));
        assert!(token_response(&HttpResponse::new(200, "{}")).is_err());
    }

    fn id_token(claims: Value) -> String {
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("e30.{}.sig", b.encode(claims.to_string()))
    }

    #[test]
    fn fhir_user_and_relationship() {
        let own = id_token(json!({"fhirUser": "https://fhir.example.org/api/FHIR/R4/Patient/p1"}));
        assert_eq!(fhir_user(&own).as_deref(), Some("Patient/p1"));
        assert_eq!(relationship(fhir_user(&own).as_deref(), "p1"), Some("self"));
        let parent = id_token(json!({"fhirUser": "Patient/parent9"}));
        assert_eq!(
            relationship(fhir_user(&parent).as_deref(), "child1"),
            Some("proxy")
        );
        let related = id_token(json!({"fhirUser": "RelatedPerson/r1"}));
        assert_eq!(
            relationship(fhir_user(&related).as_deref(), "child1"),
            Some("proxy")
        );
        assert_eq!(fhir_user(&id_token(json!({"sub": "x"}))), None);
        assert_eq!(relationship(None, "p1"), None);
    }
}
