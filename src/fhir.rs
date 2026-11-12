//! A read-only FHIR R4 client over one organization's base URL: reads,
//! searches that follow `Bundle.link[next]`, and `Binary` fetches. Resources
//! stay as the server's text (minified), because a `serde_json::Value` would
//! rewrite a FHIR decimal such as `1.50`.
//!
//! Shaped like `pk_med_fhir::FhirClient` (medical-common), which replaces
//! this module once it is tagged.

use std::time::Duration;

use pk_cli_core::transport::{HttpRequest, HttpResponse, Transport};
use pk_cli_core::{block_on, CliError};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

/// Pages one search follows before it stops and says so.
pub const PAGE_CAP: usize = 100;
/// The longest stream line (a Binary carries its document inline).
pub const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
const RETRIES: usize = 2;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// One resource as the server sent it, minified.
#[derive(Debug, Clone)]
pub struct RawResource {
    pub resource_type: String,
    pub id: String,
    pub raw: String,
}

impl RawResource {
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        #[derive(Deserialize)]
        struct Head {
            #[serde(rename = "resourceType")]
            resource_type: String,
            #[serde(default)]
            id: String,
        }
        let head: Head = serde_json::from_str(raw).map_err(|e| {
            CliError::Upstream(format!(
                "the organization sent a resource that is not FHIR JSON: {e}"
            ))
        })?;
        Ok(RawResource {
            resource_type: head.resource_type,
            id: head.id,
            raw: minify(raw),
        })
    }

    pub fn value(&self) -> Option<Value> {
        serde_json::from_str(&self.raw).ok()
    }
}

/// JSON without insignificant whitespace, every token kept as written.
pub fn minify(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in json.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if !c.is_ascii_whitespace() {
            out.push(c);
        }
    }
    out
}

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

    /// One GET with the token. A request that does not arrive, a 429 and a
    /// 5xx are tried again, twice, within `Retry-After`: every request is a
    /// read, so a repeat is harmless.
    pub fn get(&self, path: &str, accept: &str) -> Result<HttpResponse, CliError> {
        let url = self.url(path)?;
        pk_cli_core::transport::require_secure_url(&url)?;
        let mut attempt = 0;
        loop {
            let req = HttpRequest::get(url.clone())
                .header("Accept", accept)
                .header("Authorization", format!("Bearer {}", self.bearer.token()?))
                .user_agent(self.user_agent.0, self.user_agent.1);
            let resp = match block_on(self.t.send(req)) {
                Ok(resp) => resp,
                Err(e) if attempt < RETRIES && !e.is_auth() => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_secs(attempt as u64));
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
                reason: format!("the search result is not a FHIR Bundle: {e}"),
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
    let Some(v) = r.value() else {
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

    #[test]
    fn minify_keeps_every_token_as_written() {
        let pretty = "{\n  \"resourceType\": \"Observation\",\n  \"valueQuantity\": { \"value\": 1.50 },\n  \"note\": [{\"text\": \"a  b \\\" c\"}]\n}";
        assert_eq!(
            minify(pretty),
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
