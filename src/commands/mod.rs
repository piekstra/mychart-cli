//! Command implementations. `main` builds a [`Ctx`] and dispatches; the
//! tests build one over an in-memory vault, a scratch lock and cache, and a
//! stand-in browser.

pub mod api;
pub mod auth;
pub mod config;
pub mod documents;
pub mod orgs;
pub mod patients;
pub mod records;

use std::path::PathBuf;
use std::sync::Arc;

use pk_cli_auth::refresh_lock::RefreshLock;
use pk_cli_config::ConfigStore;
use pk_cli_core::{CliError, CommonArgs};
use pk_cli_http::BlockingTransport;
use pk_cli_secrets::CredentialStore;

use crate::config::Config;
use crate::session::{Context, Index};
use crate::vault::Vault;

pub const BIN: &str = "mychart";
pub const REPO: &str = "piekstra/mychart-cli";

/// Names the Keychain service a run uses instead of `piekstra.mychart`. For
/// the test suite and for dogfooding error paths: a service that holds
/// nothing reads as signed out without a Keychain prompt. It moves only the
/// Keychain; the config and index stay where they are.
pub const KEYCHAIN_SERVICE_ENV: &str = "MYCHART_KEYCHAIN_SERVICE";

pub struct Ctx {
    pub common: CommonArgs,
    pub store: ConfigStore,
    pub vault: Arc<dyn Vault>,
    pub lock: RefreshLock,
    pub cache_dir: PathBuf,
    /// Opens the sign-in URL; returns whether something was launched.
    pub browser: Box<dyn Fn(&str) -> bool>,
}

impl Ctx {
    /// The binary's context: the Keychain (or `MYCHART_KEYCHAIN_SERVICE`),
    /// the family's refresh lock, `~/.cache/mychart`, the system browser.
    pub fn for_binary(common: CommonArgs, config: Option<PathBuf>) -> Result<Self, CliError> {
        Ok(Ctx {
            common,
            store: ConfigStore::new(BIN).with_override(config),
            vault: Arc::new(credential_store()),
            lock: RefreshLock::for_binary(BIN)?,
            cache_dir: crate::orgs::cache_dir()?,
            browser: Box::new(crate::browser::open_browser),
        })
    }

    pub fn json(&self) -> bool {
        self.common.json
    }

    pub fn config(&self) -> Result<Config, CliError> {
        self.store.load()
    }

    pub fn index(&self) -> Result<Index, CliError> {
        Ok(Index::beside(&self.store.path()?))
    }

    pub fn contexts(&self) -> Result<Vec<Context>, CliError> {
        self.index()?.load()
    }

    /// The signed-in patient `who` names (the only one, when one is held).
    pub fn signed_in(&self, who: &crate::cli::PatientArgs) -> Result<Context, CliError> {
        let contexts = self.contexts()?;
        crate::session::pick(&contexts, who.patient.as_deref(), who.org.as_deref()).cloned()
    }

    pub fn note(&self, msg: impl AsRef<str>) {
        if !self.common.quiet {
            eprintln!("{}", msg.as_ref());
        }
    }

    /// [`Self::note`] as a closure a FHIR client can own.
    pub fn notes(&self) -> impl Fn(&str) + Send + Sync + 'static {
        let quiet = self.common.quiet;
        move |line: &str| {
            if !quiet {
                eprintln!("{line}")
            }
        }
    }

    /// `c`'s sign-in for a run of requests, renewed first when it is due.
    pub fn live(
        &self,
        t: &BlockingTransport,
        c: &Context,
    ) -> Result<crate::session::Live<BlockingTransport>, CliError> {
        crate::session::Live::start(
            t,
            self.vault.clone(),
            &self.lock,
            c,
            Some(std::sync::Arc::new(self.notes())),
        )
    }
}

pub fn credential_store() -> CredentialStore {
    match std::env::var(KEYCHAIN_SERVICE_ENV) {
        Ok(service) if !service.trim().is_empty() => CredentialStore::new(service),
        _ => CredentialStore::for_binary(BIN),
    }
}

/// The transport for an organization's FHIR and OAuth endpoints: no
/// redirects, the family's 60 s timeout.
pub fn transport() -> Result<BlockingTransport, CliError> {
    BlockingTransport::new()
}

/// A DTO as JSON for stdout; a failure is an error with an exit code, never
/// a `null` that reads as success.
pub fn to_json<T: serde::Serialize>(dto: &T) -> Result<serde_json::Value, CliError> {
    serde_json::to_value(dto).map_err(|e| CliError::Other(format!("serializing the output: {e}")))
}

/// RFC 3339 for unix seconds.
pub fn rfc3339(secs: i64) -> String {
    pk_cli_core::dates::fmt_rfc3339(secs)
}
