//! A stand-in for one Epic organization on 127.0.0.1, for the end-to-end
//! tests and for dogfooding (`make mock`): Epic's directory, the
//! CapabilityStatement that names the OAuth endpoints only to a client that
//! sends `Epic-Client-ID`, a standalone launch that redirects straight back
//! (no MyChart page), the code exchange with PKCE, dynamic client
//! registration, the JWT-bearer grant checked against the registered key,
//! and FHIR reads and searches over synthetic resources.
//!
//! Every person and record here is invented.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha384};

pub const CLIENT_ID: &str = "00000000-0000-0000-0000-0000000c11e7";
pub const SELF_PATIENT: &str = "eSelf0000000000000000001";
pub const CHILD_PATIENT: &str = "eChild000000000000000002";
pub const PROXY_USER: &str = "RelatedPerson/eRelated00000000000000003";
pub const ORG_ID: &str = "9000-1";
pub const ORG_NAME: &str = "Mock Example Health";

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Who the next sign-in links: the signed-in user themselves, or their
/// child through proxy access (MyChart's "Yourself / Your child" picker).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Myself,
    Child,
}

#[derive(Default)]
struct Pending {
    challenge: String,
    redirect_uri: String,
    patient: String,
    fhir_user: String,
}

struct Issued {
    patient: String,
    expires_at: i64,
    /// The one-time initial access token, good for one registration.
    initial: bool,
}

pub struct State {
    pub pick: Pick,
    /// `expires_in` for tokens from the JWT-bearer grant.
    pub token_lifetime: i64,
    pub offer_registration: bool,
    pub refuse_jwt_bearer: bool,
    codes: HashMap<String, Pending>,
    tokens: HashMap<String, Issued>,
    /// dynamic client id → (patient, fhirUser, the registered JWK).
    clients: HashMap<String, (String, String, Value)>,
    pub jwt_bearer_grants: usize,
    pub registrations: usize,
    /// resourceType → resources, parsed and as the text served (decimals
    /// such as `13.50` kept).
    pub resources: HashMap<String, Vec<(Value, String)>>,
    /// Searches refused with a status: `Observation?category=sdoh` or a
    /// bare type.
    pub refuse: Vec<(String, u16)>,
    pub page_size: usize,
    /// Answers that replace a read's (`Binary/b1`, `Practitioner/p1`):
    /// a status and a body, for the given number of requests.
    pub faults: Vec<Fault>,
    /// Requests per FHIR path read or searched (`Binary/b1`, `Observation`).
    pub hits: HashMap<String, usize>,
    counter: u64,
    /// Per process, so a token from an earlier mock is unknown to this one.
    salt: u32,
}

impl State {
    /// Forget every access token issued, as a server that revoked them
    /// would; the registered clients stay.
    pub fn forget_tokens(&mut self) {
        self.tokens.clear();
    }

    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{:08x}{:016}", self.salt, self.counter)
    }
}

pub struct Fault {
    pub path: String,
    pub status: u16,
    pub body: String,
    pub times: usize,
}

impl Fault {
    /// `times` HTTP 500s for `path`, each saying to retry at once.
    pub fn failing(path: &str, times: usize) -> Self {
        Fault {
            path: path.into(),
            status: 500,
            body: "<html><body>Internal error</body></html>".into(),
            times,
        }
    }

    /// A 200 that is not FHIR JSON (a maintenance page), every time.
    pub fn not_fhir(path: &str) -> Self {
        Fault {
            path: path.into(),
            status: 200,
            body: "<html><body>Down for maintenance</body></html>".into(),
            times: usize::MAX,
        }
    }
}

pub struct MockEpic {
    pub port: u16,
    pub state: Arc<Mutex<State>>,
}

impl MockEpic {
    pub fn start() -> Self {
        Self::start_on(0)
    }

    pub fn start_on(port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            pick: Pick::Myself,
            token_lifetime: 3600,
            offer_registration: true,
            refuse_jwt_bearer: false,
            codes: HashMap::new(),
            tokens: HashMap::new(),
            clients: HashMap::new(),
            jwt_bearer_grants: 0,
            registrations: 0,
            resources: synthetic_resources(),
            refuse: vec![
                ("Observation?category=sdoh".into(), 400),
                ("Media".into(), 403),
            ],
            page_size: 2,
            faults: Vec::new(),
            hits: HashMap::new(),
            counter: 0,
            salt: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos(),
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let st = shared.clone();
                std::thread::spawn(move || {
                    let _ = serve(stream, port, &st);
                });
            }
        });
        MockEpic { port, state }
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn fhir_base(&self) -> String {
        format!("{}/api/FHIR/R4", self.origin())
    }

    pub fn directory_url(&self) -> String {
        format!("{}/Endpoints/Brands", self.origin())
    }

    pub fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap())
    }

    pub fn get<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.state.lock().unwrap())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).ok()?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    Some(Request {
        method,
        path,
        query,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn params(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn respond(stream: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &str) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn json_resp(stream: &mut TcpStream, status: u16, v: &Value) {
    respond(
        stream,
        status,
        &[("Content-Type", "application/json".into())],
        &v.to_string(),
    );
}

fn fhir_resp(stream: &mut TcpStream, status: u16, v: &Value) {
    respond(
        stream,
        status,
        &[("Content-Type", "application/fhir+json".into())],
        &v.to_string(),
    );
}

fn oauth_error(stream: &mut TcpStream, status: u16, error: &str, desc: &str) {
    json_resp(
        stream,
        status,
        &json!({ "error": error, "error_description": desc }),
    );
}

fn outcome(code: &str, diagnostics: &str) -> Value {
    json!({
        "resourceType": "OperationOutcome",
        "issue": [{ "severity": "error", "code": code, "diagnostics": diagnostics }]
    })
}

fn serve(mut stream: TcpStream, port: u16, state: &Arc<Mutex<State>>) -> Option<()> {
    let req = read_request(&mut stream)?;
    let origin = format!("http://127.0.0.1:{port}");
    let base = format!("{origin}/api/FHIR/R4");
    let q = params(&req.query);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/Endpoints/Brands") => {
            json_resp(&mut stream, 200, &brands(&base));
        }
        ("GET", "/api/FHIR/R4/metadata") => {
            let named = req.headers.get("epic-client-id").map(String::as_str) == Some(CLIENT_ID);
            let offer = state.lock().unwrap().offer_registration;
            let mut uris = vec![
                json!({ "url": "authorize", "valueUri": format!("{origin}/oauth2/authorize") }),
                json!({ "url": "token", "valueUri": format!("{origin}/oauth2/token") }),
            ];
            if named && offer {
                uris.push(
                    json!({ "url": "register", "valueUri": format!("{origin}/oauth2/register") }),
                );
            }
            fhir_resp(
                &mut stream,
                200,
                &json!({
                    "resourceType": "CapabilityStatement",
                    "fhirVersion": "4.0.1",
                    "rest": [{ "mode": "server", "security": { "extension": [{
                        "url": "http://fhir-registry.smarthealthit.org/StructureDefinition/oauth-uris",
                        "extension": uris
                    }]}}]
                }),
            );
        }
        ("GET", "/oauth2/authorize") => {
            let get = |k: &str| q.get(k).cloned().unwrap_or_default();
            if get("client_id") != CLIENT_ID {
                respond(&mut stream, 400, &[], "unknown client");
                return Some(());
            }
            if get("response_type") != "code"
                || get("code_challenge_method") != "S256"
                || get("aud") != base
            {
                respond(&mut stream, 400, &[], "bad authorize request");
                return Some(());
            }
            let redirect = get("redirect_uri");
            if !(redirect.starts_with("http://127.0.0.1:")
                || redirect.starts_with("http://localhost:"))
            {
                respond(&mut stream, 400, &[], "redirect_uri not registered");
                return Some(());
            }
            let mut st = state.lock().unwrap();
            let (patient, fhir_user) = match st.pick {
                Pick::Myself => (SELF_PATIENT.to_string(), format!("Patient/{SELF_PATIENT}")),
                Pick::Child => (CHILD_PATIENT.to_string(), PROXY_USER.to_string()),
            };
            let code = st.next("code");
            st.codes.insert(
                code.clone(),
                Pending {
                    challenge: get("code_challenge"),
                    redirect_uri: redirect.clone(),
                    patient,
                    fhir_user,
                },
            );
            drop(st);
            let sep = if redirect.contains('?') { '&' } else { '?' };
            let loc = format!(
                "{redirect}{sep}code={}&state={}",
                encode(&code),
                encode(&get("state"))
            );
            respond(&mut stream, 302, &[("Location", loc)], "");
        }
        ("POST", "/oauth2/token") => token(&mut stream, &origin, state, &params(&req.body)),
        ("POST", "/oauth2/register") => {
            let bearer = req
                .headers
                .get("authorization")
                .and_then(|h| h.strip_prefix("Bearer "))
                .unwrap_or_default()
                .to_string();
            let body: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
            let mut st = state.lock().unwrap();
            let Some(issued) = st.tokens.get(&bearer) else {
                drop(st);
                oauth_error(
                    &mut stream,
                    401,
                    "invalid_token",
                    "unknown initial access token",
                );
                return Some(());
            };
            if !issued.initial {
                drop(st);
                oauth_error(
                    &mut stream,
                    401,
                    "invalid_token",
                    "the initial access token was used already",
                );
                return Some(());
            }
            if body["software_id"] != CLIENT_ID
                || body.pointer("/jwks/keys/0/kty") != Some(&json!("RSA"))
            {
                drop(st);
                oauth_error(
                    &mut stream,
                    400,
                    "invalid_client_metadata",
                    "software_id or jwks",
                );
                return Some(());
            }
            let patient = issued.patient.clone();
            st.tokens.get_mut(&bearer).unwrap().initial = false;
            let fhir_user = st
                .codes
                .values()
                .find(|p| p.patient == patient)
                .map(|p| p.fhir_user.clone())
                .unwrap_or_default();
            let dynamic = st.next("G");
            st.clients.insert(
                dynamic.clone(),
                (patient, fhir_user, body["jwks"]["keys"][0].clone()),
            );
            st.registrations += 1;
            drop(st);
            json_resp(
                &mut stream,
                201,
                &json!({
                    "redirect_uris": [],
                    "token_endpoint_auth_method": "none",
                    "grant_types": ["urn:ietf:params:oauth:grant-type:jwt-bearer"],
                    "software_id": CLIENT_ID,
                    "client_id": dynamic,
                    "client_id_issued_at": now(),
                    "jwks": body["jwks"],
                }),
            );
        }
        ("GET", p) if p.starts_with("/api/FHIR/R4/") => fhir(&mut stream, &base, state, &req, &q),
        _ => respond(&mut stream, 404, &[], "not found"),
    }
    Some(())
}

fn token(
    stream: &mut TcpStream,
    origin: &str,
    state: &Arc<Mutex<State>>,
    p: &HashMap<String, String>,
) {
    let get = |k: &str| p.get(k).cloned().unwrap_or_default();
    let mut st = state.lock().unwrap();
    match get("grant_type").as_str() {
        "authorization_code" => {
            let Some(pending) = st.codes.remove(&get("code")) else {
                drop(st);
                return oauth_error(stream, 400, "invalid_grant", "unknown or used code");
            };
            let challenge = B64.encode(Sha256::digest(get("code_verifier").as_bytes()));
            if challenge != pending.challenge
                || get("redirect_uri") != pending.redirect_uri
                || get("client_id") != CLIENT_ID
            {
                drop(st);
                return oauth_error(
                    stream,
                    400,
                    "invalid_grant",
                    "PKCE, redirect_uri or client_id mismatch",
                );
            }
            // Keep the code's fhirUser for the registration that follows.
            let fhir_user = pending.fhir_user.clone();
            let patient = pending.patient.clone();
            let used = format!("used-{}", st.counter);
            st.codes.insert(used, pending);
            let token = st.next("initial");
            st.tokens.insert(
                token.clone(),
                Issued {
                    patient: patient.clone(),
                    expires_at: now() + 3600,
                    initial: true,
                },
            );
            drop(st);
            let id_token = format!(
                "{}.{}.sig",
                B64.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
                B64.encode(json!({ "sub": fhir_user, "fhirUser": format!("{origin}/api/FHIR/R4/{fhir_user}"), "exp": now() + 300 }).to_string())
            );
            json_resp(
                stream,
                200,
                &json!({
                    "access_token": token,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "scope": "openid fhirUser launch/patient offline_access patient/Patient.read",
                    "id_token": id_token,
                    "patient": patient,
                    "__epic.dstu2.patient": "dstu2-dummy",
                }),
            );
        }
        "urn:ietf:params:oauth:grant-type:jwt-bearer" => {
            let client = get("client_id");
            let Some((patient, _, jwk)) = st.clients.get(&client).cloned() else {
                drop(st);
                return oauth_error(stream, 400, "invalid_client", "unknown dynamic client");
            };
            if st.refuse_jwt_bearer {
                drop(st);
                return oauth_error(stream, 400, "invalid_grant", "the access period has ended");
            }
            if let Err(why) = check_assertion(
                &get("assertion"),
                &client,
                &format!("{origin}/oauth2/token"),
                &jwk,
            ) {
                drop(st);
                return oauth_error(stream, 400, "invalid_grant", &why);
            }
            st.jwt_bearer_grants += 1;
            let lifetime = st.token_lifetime;
            let token = st.next("access");
            st.tokens.insert(
                token.clone(),
                Issued {
                    patient: patient.clone(),
                    expires_at: now() + lifetime,
                    initial: false,
                },
            );
            drop(st);
            json_resp(
                stream,
                200,
                &json!({
                    "access_token": token,
                    "token_type": "Bearer",
                    "expires_in": lifetime,
                    "scope": "launch/patient offline_access patient/Patient.read",
                    "patient": patient,
                }),
            );
        }
        _ => {
            drop(st);
            oauth_error(stream, 400, "unsupported_grant_type", "")
        }
    }
}

/// Verify an RS384 client assertion against the registered JWK.
fn check_assertion(jwt: &str, client: &str, aud: &str, jwk: &Value) -> Result<(), String> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err("not a JWT".into());
    }
    let header: Value = serde_json::from_slice(&B64.decode(parts[0]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if header["alg"] != "RS384" {
        return Err("alg".into());
    }
    if claims["iss"] != client || claims["sub"] != client || claims["aud"] != aud {
        return Err("iss, sub or aud".into());
    }
    let (iat, exp) = (
        claims["iat"].as_i64().unwrap_or(0),
        claims["exp"].as_i64().unwrap_or(0),
    );
    if exp - iat > 300 || exp < now() {
        return Err("exp".into());
    }
    let n = rsa::BoxedUint::from_be_slice_vartime(
        &B64.decode(jwk["n"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?,
    );
    let e = rsa::BoxedUint::from_be_slice_vartime(
        &B64.decode(jwk["e"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?,
    );
    let public = rsa::PublicKey::new(n, e).map_err(|e| e.to_string())?;
    let digest = Sha384::digest(format!("{}.{}", parts[0], parts[1]).as_bytes());
    public
        .verify(
            rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha384>(),
            &digest,
            &B64.decode(parts[2]).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("signature: {e}"))
}

fn fhir(
    stream: &mut TcpStream,
    base: &str,
    state: &Arc<Mutex<State>>,
    req: &Request,
    q: &HashMap<String, String>,
) {
    let bearer = req
        .headers
        .get("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();
    let st = state.lock().unwrap();
    let Some(issued) = st.tokens.get(&bearer).filter(|t| t.expires_at > now()) else {
        drop(st);
        return fhir_resp(
            stream,
            401,
            &outcome("login", "invalid or expired access token"),
        );
    };
    let patient = issued.patient.clone();
    let rest = req.path.trim_start_matches("/api/FHIR/R4/");
    let mut st = st;
    *st.hits.entry(rest.to_string()).or_insert(0) += 1;
    if let Some(f) = st.faults.iter_mut().find(|f| f.path == rest && f.times > 0) {
        f.times -= 1;
        let (status, body) = (f.status, f.body.clone());
        drop(st);
        return respond(
            stream,
            status,
            &[
                ("Content-Type", "text/html".into()),
                ("Retry-After", "0".into()),
            ],
            &body,
        );
    }
    let mut segs = rest.splitn(2, '/');
    let kind = segs.next().unwrap_or_default().to_string();
    let id = segs.next().map(str::to_string);
    let all = st.resources.get(&kind).cloned().unwrap_or_default();
    let page_size = st.page_size;
    let refused = st
        .refuse
        .iter()
        .find(|(what, _)| match what.split_once("?category=") {
            Some((t, c)) => *t == kind && q.get("category").map(String::as_str) == Some(c),
            None => *what == kind && id.is_none(),
        })
        .map(|(_, status)| *status);
    drop(st);
    let raw = |stream: &mut TcpStream, status: u16, body: &str| {
        respond(
            stream,
            status,
            &[("Content-Type", "application/fhir+json".into())],
            body,
        )
    };
    match id {
        Some(id) => match all.into_iter().find(|(v, _)| v["id"] == id.as_str()) {
            Some((v, text)) if owner(&v) == patient || owner(&v).is_empty() => {
                raw(stream, 200, &text)
            }
            Some(_) => fhir_resp(
                stream,
                403,
                &outcome("forbidden", "not this patient's record"),
            ),
            None => fhir_resp(stream, 404, &outcome("not-found", "no such resource")),
        },
        None => {
            if let Some(status) = refused {
                return fhir_resp(
                    stream,
                    status,
                    &outcome("not-supported", "this search is not available to this app"),
                );
            }
            if q.get("patient").map(String::as_str) != Some(patient.as_str()) {
                return fhir_resp(stream, 403, &outcome("forbidden", "search another patient"));
            }
            let since = q
                .get("date")
                .and_then(|d| d.strip_prefix("ge"))
                .map(str::to_string);
            let mine: Vec<(Value, String)> = all
                .into_iter()
                .filter(|(r, _)| owner(r) == patient)
                .filter(|(r, _)| match q.get("category") {
                    Some(c) => {
                        r.pointer("/category/0/coding/0/code")
                            .and_then(Value::as_str)
                            == Some(c)
                    }
                    None => true,
                })
                .filter(|(r, _)| match &since {
                    Some(d) => [
                        "effectiveDateTime",
                        "date",
                        "occurrenceDateTime",
                        "performedDateTime",
                    ]
                    .iter()
                    .find_map(|k| r[*k].as_str())
                    .is_none_or(|v| v >= d.as_str()),
                    None => true,
                })
                .collect();
            let start: usize = q.get("start").and_then(|s| s.parse().ok()).unwrap_or(0);
            let mut link =
                vec![json!({ "relation": "self", "url": format!("{base}/{kind}?{}", req.query) })];
            if start + page_size < mine.len() {
                let mut next = q.clone();
                next.insert("start".into(), (start + page_size).to_string());
                let mut pairs: Vec<_> = next.into_iter().collect();
                pairs.sort();
                let qs = pairs
                    .iter()
                    .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
                    .collect::<Vec<_>>()
                    .join("&");
                link.push(json!({ "relation": "next", "url": format!("{base}/{kind}?{qs}") }));
            }
            let mut entries: Vec<String> = mine
                .iter()
                .skip(start)
                .take(page_size)
                .map(|(v, text)| {
                    format!(
                        "{{\n  \"fullUrl\": \"{base}/{kind}/{}\",\n  \"resource\": {text},\n  \"search\": {{ \"mode\": \"match\" }}\n}}",
                        v["id"].as_str().unwrap_or_default()
                    )
                })
                .collect();
            if mine.is_empty() {
                // Epic answers an empty search with an outcome entry.
                entries.push(format!(
                    "{{\"resource\": {}, \"search\": {{\"mode\": \"outcome\"}}}}",
                    json!({"resourceType": "OperationOutcome", "issue": [{"severity": "warning", "code": "processing", "details": {"text": "No results were found."}}]})
                ));
            }
            let body = format!(
                "{{\n \"resourceType\": \"Bundle\",\n \"type\": \"searchset\",\n \"total\": {},\n \"link\": {},\n \"entry\": [\n{}\n ]\n}}",
                mine.len(),
                Value::Array(link),
                entries.join(",\n")
            );
            raw(stream, 200, &body);
        }
    }
}

/// The patient a synthetic resource belongs to, from `subject`/`patient`
/// (or its own id for a Patient).
fn owner(r: &Value) -> String {
    if r["resourceType"] == "Patient" {
        return r["id"].as_str().unwrap_or_default().to_string();
    }
    for k in ["subject", "patient", "beneficiary"] {
        if let Some(s) = r[k]["reference"].as_str() {
            return s.trim_start_matches("Patient/").to_string();
        }
    }
    String::new()
}

fn brands(base: &str) -> Value {
    json!({
        "resourceType": "Bundle",
        "id": "epic-endpoint-brands-v1",
        "type": "collection",
        "timestamp": "2026-01-01T03:00:00-06:00",
        "entry": [
            { "fullUrl": "urn:uuid:00000000-0000-0000-0000-00000000a901", "resource": {
                "resourceType": "Organization", "id": "00000000-0000-0000-0000-00000000a901",
                "identifier": [{ "system": "https://open.epic.com/brand-identifier", "value": ORG_ID }],
                "active": true, "name": ORG_NAME,
                "address": [{ "state": "NY", "country": "US" }],
                "endpoint": [{ "reference": "urn:uuid:00000000-0000-0000-0000-00000000e901" }]
            }},
            { "fullUrl": "urn:uuid:00000000-0000-0000-0000-00000000b901", "resource": {
                "resourceType": "Organization", "id": "00000000-0000-0000-0000-00000000b901",
                "active": true, "name": "Mock Example Pediatrics",
                "address": [{ "city": "Exampleton", "state": "NY", "country": "US" }],
                "partOf": { "reference": "urn:uuid:00000000-0000-0000-0000-00000000a901" }
            }},
            { "fullUrl": "urn:uuid:00000000-0000-0000-0000-00000000e901", "resource": {
                "resourceType": "Endpoint", "id": "00000000-0000-0000-0000-00000000e901",
                "status": "active", "name": ORG_NAME,
                "managingOrganization": { "display": "Mock Example Health System" },
                "address": base
            }}
        ]
    })
}

/// The synthetic records: two patients (an adult and their child), a few
/// resources each, kept as the fixture's text.
pub fn synthetic_resources() -> HashMap<String, Vec<(Value, String)>> {
    let text = include_str!("../fixtures/mock_records.json");
    let all: Vec<Box<serde_json::value::RawValue>> =
        serde_json::from_str(text).expect("mock_records.json");
    let mut out: HashMap<String, Vec<(Value, String)>> = HashMap::new();
    for r in all {
        let v: Value = serde_json::from_str(r.get()).unwrap();
        let kind = v["resourceType"].as_str().unwrap_or_default().to_string();
        out.entry(kind).or_default().push((v, r.get().to_string()));
    }
    out
}
