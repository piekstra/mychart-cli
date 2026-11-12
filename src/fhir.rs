//! FHIR requests for one signed-in patient, over `pk_med_fhir::FhirClient`:
//! the bearer from [`Live`] (renewed when due and once after a 401), the
//! base's origin only, retries reported, resources kept as the server's
//! text. This module turns the client's errors into what an export does
//! next ([`Failure`]): stop, or record a skip and go on.

use pk_cli_core::transport::Transport;
use pk_cli_core::{block_on, CliError, Reason};
use pk_cli_secrets::Secret;
use pk_med_fhir::{FhirClient, RetryPolicy, UNREACHABLE};

use crate::session::Live;

pub use pk_med_fhir::{describe_json_error, RawResource};

/// The longest stream line (a Binary carries its document inline).
pub const MAX_LINE_BYTES: usize = pk_med_records::MAX_LINE_BYTES;

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
                "forbidden" => CliError::Auth(reason).because(Reason::Forbidden),
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

/// What a search returned besides its resources.
#[derive(Debug, Default)]
pub struct SearchSummary {
    /// Pages read, and handed to the caller's `each`.
    pub pages: usize,
    /// What the OperationOutcomes the server attached say.
    pub outcomes: Vec<String>,
    pub truncated: bool,
    /// Entries left out because they were not FHIR resources.
    pub malformed: usize,
}

/// A transport a FHIR client and its token source can own across a run.
pub trait Wire: Transport + Clone + Send + Sync + 'static {}

impl<T: Transport + Clone + Send + Sync + 'static> Wire for T {}

pub struct Fhir<T> {
    client: FhirClient<T>,
    live: Option<Live<T>>,
}

impl<T: Wire> Fhir<T> {
    /// Requests as `live`'s patient, to the base its session was issued for.
    pub fn new(t: &T, live: &Live<T>) -> Result<Self, CliError> {
        let source = live.clone();
        let retries = live.clone();
        let client = FhirClient::new(
            t.clone(),
            live.fhir_base(),
            crate::commands::BIN,
            env!("CARGO_PKG_VERSION"),
        )?
        .bearer_with(move |refused| source.token(refused))
        .on_retry(move |line| retries.say(line));
        Ok(Fhir {
            client,
            live: Some(live.clone()),
        })
    }

    /// Requests with one token and no renewal: the read that proves a
    /// sign-in just made works.
    pub fn with_token(t: &T, base: &str, token: Secret) -> Result<Self, CliError> {
        let client = FhirClient::new(
            t.clone(),
            base,
            crate::commands::BIN,
            env!("CARGO_PKG_VERSION"),
        )?
        .bearer(token);
        Ok(Fhir { client, live: None })
    }

    /// How often an answer that says nothing (a 429, a 5xx) is asked again.
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.client = self.client.retry(policy);
        self
    }

    pub fn base(&self) -> &str {
        self.client.base()
    }

    /// One resource by `Type/id`.
    pub fn read(&self, path: &str) -> Result<RawResource, Failure> {
        let (resource_type, id) = path.split_once('/').unwrap_or((path, ""));
        block_on(self.client.read(resource_type, id)).map_err(|e| self.failure(e, "upstream"))
    }

    /// A `Binary` as a FHIR resource (`data` in base64), by `Binary/<id>` or
    /// an absolute URL on this server. A file elsewhere is a skip, and the
    /// token is not sent there.
    pub fn binary(&self, reference: &str) -> Result<RawResource, Failure> {
        if reference.contains("://") && origin(reference) != origin(self.base()) {
            return Err(Failure::Skip {
                code: "no_file",
                reason: format!(
                    "{} is not this organization's server ({}); the sign-in is not sent there",
                    origin(reference).unwrap_or_else(|| "the URL".into()),
                    origin(self.base()).unwrap_or_default()
                ),
            });
        }
        let b = block_on(self.client.binary(reference)).map_err(|e| self.failure(e, "no_file"))?;
        if b.raw.len() > MAX_LINE_BYTES {
            return Err(Failure::Skip {
                code: "too_large",
                reason: format!(
                    "{} bytes, over the stream's {MAX_LINE_BYTES}-byte line limit",
                    b.raw.len()
                ),
            });
        }
        Ok(b)
    }

    /// A search over every page (to the client's page cap), `each` seeing
    /// every resource in server order, page by page as it is read. A page
    /// that fails ends the search: the pages before it have been handed to
    /// `each` and stay, and the failure says how many there were. The
    /// summary covers the pages read either way; an `Err` from `each` stops
    /// the search with no further request.
    pub fn search_each(
        &self,
        resource_type: &str,
        params: &[(String, String)],
        mut each: impl FnMut(RawResource) -> Result<(), Failure>,
    ) -> (SearchSummary, Result<(), Failure>) {
        let params: Vec<(&str, String)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        let mut summary = SearchSummary::default();
        let mut stopped = None;
        let result = block_on(self.client.search_pages(resource_type, &params, |page| {
            summary.pages = page.number as usize;
            summary
                .outcomes
                .extend(page.outcomes.iter().map(|o| o.summary()));
            summary.malformed += page.unreadable.len();
            for r in page.resources.into_iter().chain(page.included) {
                if let Err(f) = each(r) {
                    stopped = Some(f);
                    // Only a signal to stop asking: `stopped` is what
                    // the caller gets.
                    return Err(CliError::Other("stopped".into()));
                }
            }
            Ok(())
        }));
        if let Some(f) = stopped {
            return (summary, Err(f));
        }
        let result = match result {
            Ok(end) => {
                summary.pages = end.pages as usize;
                summary.truncated = end.truncated;
                Ok(())
            }
            Err(e) => Err(match self.failure(e, "upstream") {
                Failure::Skip { code, reason } if summary.pages > 0 => Failure::Skip {
                    code,
                    reason: format!(
                        "page {} failed, after {} that are kept: {reason}",
                        summary.pages + 1,
                        summary.pages
                    ),
                },
                f => f,
            }),
        };
        (summary, result)
    }

    /// The body of a 2xx for a path under the base, or an absolute URL on
    /// this server (`api GET`).
    pub fn get_raw(&self, path: &str) -> Result<String, CliError> {
        block_on(self.client.get_raw(path)).map_err(|e| self.settle(e))
    }

    /// A 401 after this run renewed ends the sign-in, and says so.
    fn settle(&self, e: CliError) -> CliError {
        if e.is_auth() && e.reason() == Some(Reason::CredentialRefused) {
            if let Some(ended) = self.live.as_ref().and_then(Live::ended_after_renewal) {
                return ended;
            }
        }
        e
    }

    /// Stop on the sign-in, the network or this machine; skip anything the
    /// organization refused or answered with something unreadable.
    /// `refused_name` is the skip for a reference this client will not
    /// follow (a dot segment, a Binary URL that names no Binary).
    fn failure(&self, e: CliError, refused_name: &'static str) -> Failure {
        let e = self.settle(e);
        if e.is_auth() {
            return if e.reason() == Some(Reason::Forbidden) {
                Failure::Skip {
                    code: "forbidden",
                    reason: e.message().to_string(),
                }
            } else {
                Failure::Auth(e)
            };
        }
        // By exit code: a reason or a remedy wraps the variant.
        let code = match (e.exit_code(), e.reason()) {
            (5, Some(UNREACHABLE)) => return Failure::Fatal(e),
            (5, Some(Reason::BadRequest)) => "unsupported",
            (5, _) => "upstream",
            (4, _) => "not_found",
            (2, _) => refused_name,
            _ => return Failure::Fatal(e),
        };
        Failure::Skip {
            code,
            reason: e.message().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pk_cli_core::transport::{HttpRequest, HttpResponse};
    use std::sync::{Arc, Mutex};

    /// Answers from a script, one per request, counting them.
    #[derive(Clone, Default)]
    struct Scripted(
        Arc<Mutex<Vec<Result<HttpResponse, CliError>>>>,
        Arc<Mutex<usize>>,
    );

    impl Scripted {
        fn with(answers: Vec<Result<HttpResponse, CliError>>) -> Self {
            Scripted(Arc::new(Mutex::new(answers)), Arc::default())
        }

        fn sent(&self) -> usize {
            *self.1.lock().unwrap()
        }
    }

    impl Transport for Scripted {
        async fn send(&self, _: HttpRequest) -> Result<HttpResponse, CliError> {
            *self.1.lock().unwrap() += 1;
            self.0.lock().unwrap().remove(0)
        }
    }

    fn ok(status: u16, body: &str) -> Result<HttpResponse, CliError> {
        Ok(HttpResponse::new(status, body).header("Content-Type", "application/fhir+json"))
    }

    fn fhir(t: &Scripted) -> Fhir<Scripted> {
        Fhir::with_token(t, "https://fhir.example.org/R4", Secret::new("t"))
            .unwrap()
            .retry(RetryPolicy::none())
    }

    fn code(f: Result<RawResource, Failure>) -> &'static str {
        match f {
            Err(Failure::Skip { code, .. }) => code,
            Err(Failure::Auth(_)) => "auth",
            Err(Failure::Fatal(_)) => "fatal",
            Ok(_) => "ok",
        }
    }

    #[test]
    fn each_refusal_is_a_skip_and_only_the_sign_in_and_the_network_stop() {
        let outcome = r#"{"resourceType":"OperationOutcome","issue":[{"severity":"error","code":"processing"}]}"#;
        let answers = [
            (ok(200, "<html>down</html>"), "upstream"),
            (ok(403, outcome), "forbidden"),
            (ok(404, outcome), "not_found"),
            (ok(410, outcome), "not_found"),
            (ok(400, outcome), "unsupported"),
            (ok(405, outcome), "unsupported"),
            (ok(422, outcome), "unsupported"),
            (ok(500, outcome), "upstream"),
            (ok(501, outcome), "upstream"),
            (ok(503, outcome), "upstream"),
            (ok(401, outcome), "auth"),
            (
                Err(CliError::Upstream("connection refused".into())),
                "fatal",
            ),
        ];
        let want: Vec<&str> = answers.iter().map(|(_, w)| *w).collect();
        let t = Scripted::with(answers.into_iter().map(|(a, _)| a).collect());
        let f = fhir(&t);
        let got: Vec<&str> = (0..want.len())
            .map(|_| code(f.read("Patient/p1")))
            .collect();
        assert_eq!(got, want);
        // `unsupported` is a usage refusal (exit 2) when it ends a command.
        let e = Failure::Skip {
            code: "unsupported",
            reason: "r".into(),
        }
        .into_error();
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn a_file_elsewhere_is_a_skip_and_never_requested() {
        let t = Scripted::with(vec![]);
        assert_eq!(
            code(fhir(&t).binary("https://elsewhere.example.org/R4/Binary/b1")),
            "no_file"
        );
        assert_eq!(
            code(fhir(&t).binary("https://fhir.example.org/R4/Patient/p1")),
            "no_file"
        );
        assert_eq!(t.sent(), 0);
    }

    #[test]
    fn a_binary_over_the_line_limit_is_a_skip() {
        let data = "A".repeat(MAX_LINE_BYTES);
        let body = format!(r#"{{"resourceType":"Binary","id":"b1","data":"{data}"}}"#);
        let t = Scripted::with(vec![ok(200, &body)]);
        assert_eq!(code(fhir(&t).binary("Binary/b1")), "too_large");
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
