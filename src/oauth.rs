//! OAuth 2.0 authorization code with PKCE, for a native app (RFC 6749, 7636,
//! 8252): the code verifier and its S256 challenge, `state`, the
//! authorization URL, the one-shot loopback listener the browser is sent back
//! to, and the browser launch.
//!
//! Provider-free: nothing here knows Epic or SMART. It is the mechanism the
//! family is promoting into `pk_cli_auth::oauth` (cli-common), and goes away
//! from this crate once that lands.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use base64::Engine as _;
use pk_cli_core::CliError;
use sha2::{Digest, Sha256};

/// A PKCE verifier and its S256 challenge (RFC 7636).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A fresh 64-byte verifier (86 characters, inside RFC 7636's 43–128).
    pub fn generate() -> Result<Self, CliError> {
        let verifier = random_urlsafe(64)?;
        let challenge = s256_challenge(&verifier);
        Ok(Pkce {
            verifier,
            challenge,
        })
    }
}

/// `base64url(sha256(verifier))`, unpadded.
pub fn s256_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `bytes` of operating-system randomness, base64url-unpadded.
pub fn random_urlsafe(bytes: usize) -> Result<String, CliError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf)
        .map_err(|e| CliError::Other(format!("reading system randomness: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf))
}

/// `endpoint` with `params` appended as a percent-encoded query.
pub fn url_with_query(endpoint: &str, params: &[(&str, &str)]) -> String {
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{sep}{}", form_encode(params))
}

/// `application/x-www-form-urlencoded` body (also a query string).
pub fn form_encode(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-encode everything but RFC 3986's unreserved characters.
pub fn encode(s: &str) -> String {
    pk_cli_core::transport::percent_encode(s)
}

/// Decode one query component (`+` is a space, `%XX` a byte).
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi << 4 | lo);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The decoded value of `key` in a request target, URL or bare query.
pub fn query_param(input: &str, key: &str) -> Option<String> {
    let query = match input.split_once('?') {
        Some((_, q)) => q,
        None => input,
    };
    let query = query.split('#').next().unwrap_or_default();
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(k) == key).then(|| decode(v))
    })
}

/// What one request to the redirect path says.
#[derive(Debug)]
pub enum Callback {
    /// This sign-in's authorization code.
    Code(String),
    /// The authorization server ended this sign-in (`error`, with this
    /// sign-in's `state`).
    Refused(CliError),
    /// Not this sign-in's answer: no `state`, or another one (an old tab
    /// reloaded, another program, a web page). Answered and ignored.
    Foreign,
}

/// Classify a redirect's request target. Nothing in it counts until its
/// `state` matches, so no other page or program can end the sign-in.
pub fn code_from_callback(target: &str, expected_state: &str) -> Callback {
    if query_param(target, "state").as_deref() != Some(expected_state) {
        return Callback::Foreign;
    }
    if let Some(err) = query_param(target, "error") {
        let desc = printable(&query_param(target, "error_description").unwrap_or_default());
        return Callback::Refused(CliError::Auth(format!(
            "the sign-in was refused ({}{}{desc})",
            printable(&err),
            if desc.is_empty() { "" } else { ": " }
        )));
    }
    match query_param(target, "code") {
        Some(c) if !c.is_empty() => Callback::Code(c),
        _ => Callback::Refused(CliError::Auth(
            "the sign-in returned no authorization code".into(),
        )),
    }
}

/// Text from the redirect, fit for a terminal: control characters (escape
/// sequences included) dropped, at most 200 characters.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

/// A one-shot loopback listener for the redirect (RFC 8252 §7.3).
pub struct LoopbackListener {
    listener: TcpListener,
    port: u16,
    path: String,
}

impl LoopbackListener {
    /// Bind `127.0.0.1:port`. The port is fixed rather than ephemeral because
    /// an authorization server may match the redirect URI exactly, port
    /// included; `0` picks a free one for tests.
    pub fn bind(port: u16, path: &str) -> Result<Self, CliError> {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
            CliError::Other(format!(
                "could not listen on 127.0.0.1:{port} for the sign-in's redirect: {e}"
            ))
            .with_remedy(
                "close whatever holds that port, or pick another with \
                 `mychart config set redirect_port <PORT>` and register the matching redirect URI",
            )
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| CliError::Other(format!("reading the loopback address: {e}")))?
            .port();
        Ok(LoopbackListener {
            listener,
            port,
            path: path.to_string(),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn redirect_uri(&self) -> String {
        redirect_uri(self.port, &self.path)
    }

    /// Wait for the browser to come back to the redirect path, at most
    /// `timeout`, and return the validated code. A request to any other path
    /// (a browser asking for `/favicon.ico`) gets a 404 and the wait goes on.
    pub fn wait_for_code(
        &self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<String, CliError> {
        let deadline = Instant::now() + timeout;
        self.listener
            .set_nonblocking(true)
            .map_err(|e| CliError::Other(format!("configuring the loopback listener: {e}")))?;
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let Ok(target) = read_request_target(&mut stream) else {
                        continue;
                    };
                    let path = target.split('?').next().unwrap_or_default();
                    if path != self.path {
                        respond(&mut stream, 404, "Not found.");
                        continue;
                    }
                    match code_from_callback(&target, expected_state) {
                        Callback::Code(code) => {
                            respond(
                                &mut stream,
                                200,
                                "mychart: signed in. You can close this tab and return to the terminal.",
                            );
                            return Ok(code);
                        }
                        Callback::Refused(e) => {
                            respond(
                                &mut stream,
                                200,
                                "mychart: the sign-in did not complete. Return to the terminal for details.",
                            );
                            return Err(e);
                        }
                        Callback::Foreign => {
                            respond(
                                &mut stream,
                                400,
                                "mychart: this is not the sign-in mychart is waiting for.",
                            );
                            eprintln!(
                                "ignored a request to the redirect that did not carry this sign-in's state; still waiting"
                            );
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(CliError::Auth(format!(
                            "no sign-in came back within {} minutes",
                            timeout.as_secs() / 60
                        ))
                        .with_remedy(
                            "run `mychart auth login` again and finish the sign-in in the browser",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(CliError::Other(format!(
                        "accepting the sign-in's redirect: {e}"
                    )))
                }
            }
        }
    }
}

/// The loopback redirect URI for `port` and `path`.
pub fn redirect_uri(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

fn read_request_target(stream: &mut TcpStream) -> Result<String, CliError> {
    let mut buf = [0u8; 8192];
    let n = stream
        .read(&mut buf)
        .map_err(|e| CliError::Other(format!("reading the redirect request: {e}")))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    head.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .map(str::to_string)
        .ok_or_else(|| CliError::Other("malformed redirect request".into()))
}

fn respond(stream: &mut TcpStream, status: u16, msg: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><title>mychart</title><p>{msg}</p>");
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// Open `url` in a browser. `MYCHART_BROWSER` names the program to run
/// instead of the system's (`open`, `xdg-open`): its words are the program
/// and leading arguments, the URL is appended, and no shell is involved.
/// Returns whether a launcher ran; the URL is printed either way.
pub fn open_browser(url: &str) -> bool {
    let custom = std::env::var("MYCHART_BROWSER")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let (program, mut args): (String, Vec<String>) = match custom {
        Some(cmd) => {
            let mut words = cmd.split_whitespace().map(str::to_string);
            let Some(program) = words.next() else {
                return false;
            };
            (program, words.collect())
        }
        None if cfg!(target_os = "macos") => ("open".into(), vec![]),
        None if cfg!(target_os = "linux") => ("xdg-open".into(), vec![]),
        None => return false,
    };
    args.push(url.to_string());
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_challenge_matches_the_rfc_7636_vector() {
        assert_eq!(
            s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_generated_verifier_is_url_safe_and_long_enough() {
        let p = Pkce::generate().unwrap();
        assert!(
            (43..=128).contains(&p.verifier.len()),
            "{}",
            p.verifier.len()
        );
        assert!(p
            .verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(p.verifier, Pkce::generate().unwrap().verifier);
    }

    #[test]
    fn the_callback_yields_the_code_only_when_state_matches() {
        assert!(matches!(
            code_from_callback("/callback?code=abc%2Fdef&state=s1", "s1"),
            Callback::Code(c) if c == "abc/def"
        ));
        assert!(matches!(
            code_from_callback("/callback?code=abc&state=other", "s1"),
            Callback::Foreign
        ));
        assert!(matches!(
            code_from_callback("/callback?code=abc", "s1"),
            Callback::Foreign
        ));
        assert!(matches!(
            code_from_callback("/callback?state=s1", "s1"),
            Callback::Refused(e) if e.exit_code() == 3
        ));
    }

    #[test]
    fn an_error_counts_only_with_this_sign_ins_state_and_prints_safely() {
        let Callback::Refused(e) = code_from_callback(
            "/callback?error=access_denied&error_description=User+declined%1B%5D0%3Bx%07&state=s1",
            "s1",
        ) else {
            panic!("an error with the right state ends the sign-in")
        };
        assert_eq!(e.exit_code(), 3);
        let text = e.to_string();
        assert!(text.contains("access_denied: User declined]0;x"), "{text}");
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(matches!(
            code_from_callback("/callback?error=access_denied&state=other", "s1"),
            Callback::Foreign
        ));
        assert!(matches!(
            code_from_callback("/callback?error=access_denied", "s1"),
            Callback::Foreign
        ));
    }

    #[test]
    fn a_foreign_request_is_answered_and_the_wait_goes_on() {
        use std::io::{Read as _, Write as _};
        let l = LoopbackListener::bind(0, "/callback").unwrap();
        let port = l.port();
        let client = std::thread::spawn(move || {
            let send = |target: &str| {
                let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
                out
            };
            let first = send("/callback?error=x&error_description=gone&state=old");
            let second = send("/callback?code=good&state=s1");
            (first, second)
        });
        let code = l.wait_for_code("s1", Duration::from_secs(20)).unwrap();
        assert_eq!(code, "good");
        let (first, second) = client.join().unwrap();
        assert!(first.starts_with("HTTP/1.1 400"), "{first}");
        assert!(second.starts_with("HTTP/1.1 200"), "{second}");
    }

    #[test]
    fn query_components_round_trip() {
        let url = url_with_query(
            "https://auth.example.com/authorize",
            &[
                ("redirect_uri", "http://127.0.0.1:1/callback"),
                ("scope", "openid fhirUser"),
            ],
        );
        assert_eq!(
            query_param(&url, "redirect_uri").as_deref(),
            Some("http://127.0.0.1:1/callback")
        );
        assert_eq!(
            query_param(&url, "scope").as_deref(),
            Some("openid fhirUser")
        );
        assert_eq!(decode("a%2"), "a%2");
        assert_eq!(decode("100%"), "100%");
    }

    #[test]
    fn the_listener_ignores_other_paths_and_returns_the_code() {
        let l = LoopbackListener::bind(0, "/callback").unwrap();
        let port = l.port();
        let client = std::thread::spawn(move || {
            let get = |target: &str| {
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(s, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).unwrap();
                out
            };
            let missing = get("/favicon.ico");
            let ok = get("/callback?code=c1&state=st");
            (missing, ok)
        });
        let code = l.wait_for_code("st", Duration::from_secs(10)).unwrap();
        assert_eq!(code, "c1");
        let (missing, ok) = client.join().unwrap();
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
        assert!(ok.contains("signed in"), "{ok}");
    }
}
