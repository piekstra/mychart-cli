//! A read-only FHIR R4 client over one organization's base URL: reads,
//! searches that follow `Bundle.link[next]`, and `Binary` fetches. Resources
//! stay as the server's text (`pk_med_fhir::RawResource`, minified), because
//! a `serde_json::Value` would rewrite a FHIR decimal such as `1.50`.
//!
//! The request layer stays here until `pk_med_fhir::FhirClient` takes a
//! token source (an export outlives Epic's hour-long access token), tells a
//! network failure from a 5xx, reports its retries, and reads a page around
//! an entry that is not a resource.

use std::time::Duration;

use pk_cli_core::transport::{HttpRequest, HttpResponse, Transport};
use pk_cli_core::{block_on, CliError};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

/// Pages one search follows before it stops and says so.
pub const PAGE_CAP: usize = 100;
/// The longest stream line (a Binary carries its document inline).
pub const MAX_LINE_BYTES: usize = pk_med_records::MAX_LINE_BYTES;
const RETRIES: usize = 2;
const SEND_RETRIES: usize = 1;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

pub use pk_med_fhir::{describe_json_error, RawResource};

/// Supplies the bearer token, renewing it when it is due.
pub trait Bearer {
    fn token(&self) -> Result<String, CliError>;
}

/// How one request failed, for a caller that records a skip and goes on.
#[derive(Debug)]
pub enum Failure {
    /// The sign-in no longer works: stop.
    Auth(CliError),
    /// The network, the server or this machine is unusable: stop.
    Fatal(CliError),
    /// This request was refused; the next may work.
    Skip { code: &'static str, reason: String },
}

impl Failure {
    pub fn into_error(self) -> CliError {
        match self {
            Failure::Auth(e) | Failure::Fatal(e) => e,
            Failure::Skip { code, reason } => match code {
                "not_found" => CliError::NotFound(reason),
                "forbidden" => CliError::Auth(reason),
                "unsupported" => CliError::Usage(reason),
                _ => CliError::Upstream(reason),
            },
        }
    }
}

impl From<CliError> for Failure {
    fn from(e: CliError) -> Self {
        if e.is_auth() {
            Failure::Auth(e)
        } else {
            Failure::Fatal(e)
        }
    }
}

pub struct Fhir<'a, T: Transport> {
    t: &'a T,
    base: String,
    bearer: &'a dyn Bearer,
    user_agent: (&'static str, &'static str),
    pub page_cap: usize,
    note: Option<&'a dyn Fn(String)>,
}

/// The scheme, host and port, lower-cased: the directory spells some hosts
/// in capitals while the server's own links do not.
pub fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

impl<'a, T: Transport> Fhir<'a, T> {
    pub fn new(t: &'a T, base: &str, bearer: &'a dyn Bearer) -> Self {
        Fhir {
            t,
            base: base.trim_end_matches('/').to_string(),
            bearer,
            user_agent: ("mychart", env!("CARGO_PKG_VERSION")),
            page_cap: PAGE_CAP,
            note: None,
        }
    }

    /// Where a retry is reported, so a wait is never silent.
    pub fn with_note(mut self, note: &'a dyn Fn(String)) -> Self {
        self.note = Some(note);
        self
    }

    fn tell(&self, line: String) {
        if let Some(note) = self.note {
            note(line)
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// `path` relative to the base (`Patient/123`, `/Observation?x=y`), or an
    /// absolute URL on the base's origin. The token never goes elsewhere.
    pub fn url(&self, path: &str) -> Result<String, CliError> {
        if path.contains("://") {
            if origin(path).is_none() || origin(path) != origin(&self.base) {
                return Err(CliError::Usage(format!(
                    "`{path}` is not on this organization's server ({}); the sign-in is sent only there",
                    self.base
                )));
            }
            return Ok(path.to_string());
        }
        Ok(format!("{}/{}", self.base, path.trim_start_matches('/')))
    }

    /// One GET with the token. Every request is a read, so a repeat is
    /// harmless: a 429 or 5xx is tried again twice within `Retry-After`, and
    /// a request that got no answer once (the transport's own timeout is a
    /// minute, so a second wait is the most worth spending). Each retry is
    /// reported through the note.
    pub fn get(&self, path: &str, accept: &str) -> Result<HttpResponse, CliError> {
        let url = self.url(path)?;
        pk_cli_core::transport::require_secure_url(&url)?;
        let (mut attempt, mut unanswered) = (0, 0);
        loop {
            let req = HttpRequest::get(url.clone())
                .header("Accept", accept)
                .header("Authorization", format!("Bearer {}", self.bearer.token()?))
                .user_agent(self.user_agent.0, self.user_agent.1);
            let resp = match block_on(self.t.send(req)) {
                Ok(resp) => resp,
                Err(e) if unanswered < SEND_RETRIES && !e.is_auth() => {
                    unanswered += 1;
                    self.tell(format!("{}; trying once more", e.message()));
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
                Err(e) => return Err(e),
            };
            if matches!(resp.status, 429 | 500 | 502 | 503 | 504) && attempt < RETRIES {
                attempt += 1;
                let wait = resp
                    .header_value("retry-after")
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(2 * attempt as u64))
                    .min(MAX_RETRY_WAIT);
                self.tell(format!(
                    "{} answered HTTP {}; trying again in {}s ({attempt}/{RETRIES})",
                    origin(&url).unwrap_or_default(),
                    resp.status,
                    wait.as_secs()
                ));
                std::thread::sleep(wait);
                continue;
            }
            return Ok(resp);
        }
    }

    /// A GET whose failure is the caller's to record and go past: a URL on
    /// another server (skipped as `off_origin`, the token not sent), a
    /// refusal by status. Only the sign-in, the network or this machine
    /// failing stops the caller.
    fn checked(
        &self,
        path: &str,
        accept: &str,
        off_origin: &'static str,
    ) -> Result<HttpResponse, Failure> {
        if self.url(path).is_err() {
            return Err(Failure::Skip {
                code: off_origin,
                reason: format!(
                    "{} is not this organization's server ({}); the sign-in is not sent there",
                    origin(path).unwrap_or_else(|| "the URL".into()),
                    origin(&self.base).unwrap_or_default()
                ),
            });
        }
        let resp = self.get(path, accept)?;
        if resp.is_success() {
            return Ok(resp);
        }
        Err(status_failure(&resp))
    }

    /// One resource by `Type/id`. A body that is not FHIR JSON (a
    /// maintenance page) is a skip.
    pub fn read(&self, path: &str) -> Result<RawResource, Failure> {
        let resp = self.checked(path, "application/fhir+json", "not_found")?;
        parsed(&resp.body)
    }

    /// A `Binary` as a FHIR resource (`data` in base64).
    pub fn binary(&self, reference: &str) -> Result<RawResource, Failure> {
        let resp = self.checked(reference, "application/fhir+json", "no_file")?;
        if resp.body.len() > MAX_LINE_BYTES {
            return Err(Failure::Skip {
                code: "too_large",
                reason: format!(
                    "{} bytes, over the stream's {MAX_LINE_BYTES}-byte line limit",
                    resp.body.len()
                ),
            });
        }
        let r = parsed(&resp.body)?;
        if r.resource_type != "Binary" {
            return Err(Failure::Skip {
                code: "upstream",
                reason: format!("asked for a Binary and got a {}", r.resource_type),
            });
        }
        Ok(r)
    }

    /// A search, page by page: `each` sees every resource in server order.
    /// Returns the OperationOutcome messages the server attached and
    /// whether the page cap stopped it.
    pub fn search_each(
        &self,
        resource_type: &str,
        params: &[(String, String)],
        mut each: impl FnMut(RawResource) -> Result<(), Failure>,
    ) -> Result<SearchSummary, Failure> {
        let query = crate::oauth::form_encode(
            &params
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<Vec<_>>(),
        );
        let mut next = Some(format!("{resource_type}?{query}"));
        let mut summary = SearchSummary::default();
        while let Some(url) = next.take() {
            if summary.pages == self.page_cap {
                summary.truncated = true;
                break;
            }
            let resp = self.checked(&url, "application/fhir+json", "upstream")?;
            summary.pages += 1;
            let page: Page = serde_json::from_str(&resp.body).map_err(|e| Failure::Skip {
                code: "upstream",
                reason: format!(
                    "the search result is not a FHIR Bundle: it {}",
                    describe_json_error(&e)
                ),
            })?;
            if page.resource_type != "Bundle" {
                return Err(Failure::Skip {
                    code: "upstream",
                    reason: format!("the search returned a {}, not a Bundle", page.resource_type),
                });
            }
            for entry in page.entry {
                let Some(raw) = entry.resource else { continue };
                let Ok(r) = RawResource::parse(raw.get()) else {
                    summary.malformed += 1;
                    continue;
                };
                let outcome = entry.search.as_ref().and_then(|s| s.mode.as_deref())
                    == Some("outcome")
                    || (r.resource_type == "OperationOutcome"
                        && resource_type != "OperationOutcome");
                if outcome {
                    summary.outcomes.extend(outcome_messages(&r));
                    continue;
                }
                each(r)?;
            }
            next = page
                .link
                .into_iter()
                .find(|l| l.relation == "next")
                .map(|l| l.url)
                .filter(|u| !u.is_empty());
            if let Some(u) = &next {
                if origin(u) != origin(&self.base) {
                    return Err(Failure::Skip {
                        code: "upstream",
                        reason:
                            "the next page is on another server; not following it with the sign-in"
                                .into(),
                    });
                }
            }
        }
        Ok(summary)
    }
}

#[derive(Debug, Default)]
pub struct SearchSummary {
    pub pages: usize,
    pub outcomes: Vec<String>,
    pub truncated: bool,
    /// Entries left out because they were not FHIR resources.
    pub malformed: usize,
}

/// A response body as a resource; one that is not FHIR JSON is a skip.
fn parsed(body: &str) -> Result<RawResource, Failure> {
    RawResource::parse(body).map_err(|e| Failure::Skip {
        code: "upstream",
        reason: e.message().to_string(),
    })
}

#[derive(Deserialize)]
struct Page<'a> {
    #[serde(rename = "resourceType")]
    resource_type: String,
    #[serde(default)]
    link: Vec<Link>,
    #[serde(default, borrow)]
    entry: Vec<Entry<'a>>,
}

#[derive(Deserialize)]
struct Link {
    #[serde(default)]
    relation: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct Entry<'a> {
    #[serde(default, borrow)]
    resource: Option<&'a RawValue>,
    #[serde(default)]
    search: Option<EntrySearch>,
}

#[derive(Deserialize)]
struct EntrySearch {
    #[serde(default)]
    mode: Option<String>,
}

/// `issue[].details.text` or `diagnostics`, shortened: what an
/// OperationOutcome says, for a skip reason or an error message.
pub fn outcome_messages(r: &RawResource) -> Vec<String> {
    let Ok(v) = r.value() else {
        return Vec::new();
    };
    v["issue"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            i.pointer("/details/text")
                .and_then(Value::as_str)
                .or_else(|| i["diagnostics"].as_str())
                .or_else(|| i["code"].as_str())
                .map(|s| s.chars().take(200).collect::<String>())
        })
        .collect()
}

/// What a non-success status means for the caller.
pub fn status_failure(resp: &HttpResponse) -> Failure {
    let said = RawResource::parse(&resp.body)
        .ok()
        .filter(|r| r.resource_type == "OperationOutcome")
        .map(|r| outcome_messages(&r).join("; "))
        .filter(|s| !s.is_empty())
        .map(|s| format!(": {s}"))
        .unwrap_or_default();
    let reason = format!("HTTP {}{said}", resp.status);
    match resp.status {
        401 => Failure::Auth(
            CliError::Auth(format!(
                "the organization no longer accepts this sign-in ({reason})"
            ))
            .with_remedy("sign in again with `mychart auth login --org <ORG>`"),
        ),
        403 => Failure::Skip {
            code: "forbidden",
            reason,
        },
        404 | 410 => Failure::Skip {
            code: "not_found",
            reason,
        },
        400 | 405 | 422 | 501 => Failure::Skip {
            code: "unsupported",
            reason,
        },
        _ => Failure::Skip {
            code: "upstream",
            reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers from a script, one per request, counting them.
    struct Scripted(RefCell<Vec<Result<HttpResponse, CliError>>>, RefCell<usize>);

    impl Transport for Scripted {
        async fn send(&self, _: HttpRequest) -> Result<HttpResponse, CliError> {
            *self.1.borrow_mut() += 1;
            self.0.borrow_mut().remove(0)
        }
    }

    struct Token;
    impl Bearer for Token {
        fn token(&self) -> Result<String, CliError> {
            Ok("t".into())
        }
    }

    fn ok(status: u16, body: &str) -> Result<HttpResponse, CliError> {
        Ok(HttpResponse::new(status, body).header("retry-after", "0"))
    }

    fn unanswered() -> Result<HttpResponse, CliError> {
        Err(CliError::Upstream(
            "the request to https://fhir.example.org failed: operation timed out".into(),
        ))
    }

    const PATIENT: &str = r#"{"resourceType":"Patient","id":"p1"}"#;

    #[test]
    fn a_request_without_an_answer_is_tried_once_more_and_said_so() {
        let t = Scripted(
            RefCell::new(vec![unanswered(), ok(200, PATIENT)]),
            RefCell::new(0),
        );
        let notes = RefCell::new(Vec::new());
        let note = |s: String| notes.borrow_mut().push(s);
        let fhir = Fhir::new(&t, "https://fhir.example.org/R4", &Token).with_note(&note);
        assert_eq!(fhir.read("Patient/p1").unwrap().id, "p1");
        assert_eq!(*t.1.borrow(), 2);
        assert_eq!(notes.borrow().len(), 1);
        assert!(
            notes.borrow()[0].contains("trying once more"),
            "{:?}",
            notes.borrow()
        );

        let t = Scripted(
            RefCell::new(vec![unanswered(), unanswered()]),
            RefCell::new(0),
        );
        let fhir = Fhir::new(&t, "https://fhir.example.org/R4", &Token);
        assert!(matches!(fhir.read("Patient/p1"), Err(Failure::Fatal(_))));
        assert_eq!(*t.1.borrow(), 2);
    }

    #[test]
    fn a_busy_server_is_tried_twice_more_and_said_so() {
        let t = Scripted(
            RefCell::new(vec![ok(503, ""), ok(500, ""), ok(200, PATIENT)]),
            RefCell::new(0),
        );
        let notes = RefCell::new(Vec::new());
        let note = |s: String| notes.borrow_mut().push(s);
        let fhir = Fhir::new(&t, "https://fhir.example.org/R4", &Token).with_note(&note);
        assert!(fhir.read("Patient/p1").is_ok());
        assert_eq!(*t.1.borrow(), 3);
        assert_eq!(
            *notes.borrow(),
            [
                "https://fhir.example.org answered HTTP 503; trying again in 0s (1/2)",
                "https://fhir.example.org answered HTTP 500; trying again in 0s (2/2)",
            ]
        );

        let t = Scripted(
            RefCell::new(vec![ok(500, ""), ok(500, ""), ok(500, "")]),
            RefCell::new(0),
        );
        let fhir = Fhir::new(&t, "https://fhir.example.org/R4", &Token);
        assert!(matches!(
            fhir.read("Patient/p1"),
            Err(Failure::Skip {
                code: "upstream",
                ..
            })
        ));
        assert_eq!(*t.1.borrow(), 3);
    }

    #[test]
    fn a_parse_error_never_quotes_the_value() {
        for body in [
            r#"{"resourceType":"Patient","id":12345}"#,
            r#"{"resourceType":"Patient","id":true}"#,
            r#"{"resourceType":"Patient","id":"pat-0001""#,
            "<html>Sam Example</html>",
        ] {
            let e = RawResource::parse(body).unwrap_err();
            let text = e.to_string();
            for secret in ["12345", "true", "pat-0001", "Sam", "Example"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
    }

    #[test]
    fn a_maintenance_page_and_a_file_elsewhere_are_skips() {
        let t = Scripted(
            RefCell::new(vec![ok(200, "<html>down</html>")]),
            RefCell::new(0),
        );
        let fhir = Fhir::new(&t, "https://fhir.example.org/R4", &Token);
        assert!(matches!(
            fhir.read("Patient/p1"),
            Err(Failure::Skip {
                code: "upstream",
                ..
            })
        ));
        assert!(matches!(
            fhir.binary("https://elsewhere.example.org/R4/Binary/b1"),
            Err(Failure::Skip {
                code: "no_file",
                ..
            })
        ));
        assert_eq!(*t.1.borrow(), 1, "the file elsewhere was requested");
    }

    #[test]
    fn a_resource_keeps_every_token_as_written() {
        let pretty = "{\n  \"resourceType\": \"Observation\",\n  \"valueQuantity\": { \"value\": 1.50 },\n  \"note\": [{\"text\": \"a  b \\\" c\"}]\n}";
        assert_eq!(
            RawResource::parse(pretty).unwrap().raw,
            r#"{"resourceType":"Observation","valueQuantity":{"value":1.50},"note":[{"text":"a  b \" c"}]}"#
        );
    }

    #[test]
    fn origins_compare_without_case() {
        assert_eq!(
            origin("https://FHIR.KP.ORG/x/api/FHIR/R4"),
            origin("https://fhir.kp.org/y?z")
        );
        assert_ne!(
            origin("https://fhir.kp.org/x"),
            origin("https://evil.example/x")
        );
        assert_eq!(origin("https://user@fhir.example.org/x"), None);
    }
}
