//! Non-secret settings (`~/.config/mychart/config.json`).
//!
//! The client ids are configuration, never code: each household registers
//! its own app with Epic (docs/registration.md) and sets the ids it was
//! issued. For a public client they are not secrets.

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

/// The loopback port the sign-in's redirect comes back to. Epic matches the
/// registered redirect URI, so the port is fixed (and configurable) rather
/// than picked at random per sign-in.
pub const DEFAULT_REDIRECT_PORT: u16 = 7468;
pub const REDIRECT_PATH: &str = "/callback";

/// The host the redirect URI names: `127.0.0.1` (RFC 8252's advice) or
/// `localhost`, whichever the app's registration lists. The listener binds
/// 127.0.0.1 either way.
pub const DEFAULT_REDIRECT_HOST: &str = "127.0.0.1";

/// The scopes a sign-in asks for, as Epic's tutorial gives them for a
/// standalone launch: Epic adds the resource scopes of the APIs the app was
/// registered for, and the patient context comes with a patient-facing
/// app. Dynamic registration, not `offline_access`, makes access outlast
/// the hour.
pub const DEFAULT_SCOPE: &str = "openid fhirUser";

pub const KEYS: &str = "client_id, sandbox_client_id, redirect_port, redirect_host, scope, \
                        directory_url, dynamic_registration";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The production client id Epic issued to this household's app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// The non-production client id, for Epic's sandbox (`--org sandbox`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Where `orgs refresh` downloads Epic's directory from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_url: Option<String>,
    /// Register a per-install dynamic client after sign-in, so access lasts
    /// past the first hour (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_registration: Option<bool>,
}

impl Config {
    pub fn redirect_port(&self) -> u16 {
        self.redirect_port.unwrap_or(DEFAULT_REDIRECT_PORT)
    }

    /// `http://<host>:<port>/callback`, the URI the app's registration lists.
    pub fn redirect_uri(&self, port: u16) -> String {
        format!(
            "http://{}:{port}{REDIRECT_PATH}",
            self.redirect_host
                .as_deref()
                .unwrap_or(DEFAULT_REDIRECT_HOST)
        )
    }

    pub fn scope(&self) -> &str {
        self.scope.as_deref().unwrap_or(DEFAULT_SCOPE)
    }

    pub fn dynamic_registration(&self) -> bool {
        self.dynamic_registration.unwrap_or(true)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), CliError> {
        match key {
            "client_id" => self.client_id = Some(client_id(value)?),
            "sandbox_client_id" => self.sandbox_client_id = Some(client_id(value)?),
            "redirect_port" => {
                let port: u16 = value.parse().ok().filter(|p| *p >= 1024).ok_or_else(|| {
                    CliError::Usage(format!("`{value}` is not a port from 1024 to 65535"))
                })?;
                self.redirect_port = Some(port);
            }
            "redirect_host" => match value {
                "127.0.0.1" | "localhost" => self.redirect_host = Some(value.to_string()),
                _ => {
                    return Err(CliError::Usage(
                        "redirect_host is 127.0.0.1 or localhost".into(),
                    ))
                }
            },
            "scope" => self.scope = Some(value.trim().to_string()),
            "directory_url" => {
                pk_cli_core::transport::require_secure_url(value)?;
                self.directory_url = Some(value.to_string());
            }
            "dynamic_registration" => {
                self.dynamic_registration = Some(
                    value
                        .parse()
                        .map_err(|_| CliError::Usage(format!("`{value}` is not true or false")))?,
                )
            }
            other => return Err(unknown(other)),
        }
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<(), CliError> {
        match key {
            "client_id" => self.client_id = None,
            "sandbox_client_id" => self.sandbox_client_id = None,
            "redirect_port" => self.redirect_port = None,
            "redirect_host" => self.redirect_host = None,
            "scope" => self.scope = None,
            "directory_url" => self.directory_url = None,
            "dynamic_registration" => self.dynamic_registration = None,
            other => return Err(unknown(other)),
        }
        Ok(())
    }
}

fn unknown(key: &str) -> CliError {
    CliError::Usage(format!("unknown config key `{key}` (known: {KEYS})"))
}

/// A client id is printable ASCII without spaces; anything else is a paste
/// gone wrong.
fn client_id(value: &str) -> Result<String, CliError> {
    let v = value.trim();
    if v.is_empty() || v.len() > 200 || !v.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(CliError::Usage(
            "a client id is the printable string Epic shows on the app's page, with no spaces"
                .into(),
        ));
    }
    Ok(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_set_and_unset() {
        let mut c = Config::default();
        c.set("client_id", " 00000000-0000-0000-0000-000000000001 ")
            .unwrap();
        assert_eq!(
            c.client_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000001")
        );
        c.set("redirect_port", "8123").unwrap();
        assert_eq!(c.redirect_port(), 8123);
        assert!(c.set("redirect_port", "80").is_err());
        assert!(c.set("client_id", "has space").is_err());
        assert!(c.set("directory_url", "http://example.com/x").is_err());
        c.set("directory_url", "http://127.0.0.1:9/brands").unwrap();
        c.set("dynamic_registration", "false").unwrap();
        assert!(!c.dynamic_registration());
        assert_eq!(c.redirect_uri(8123), "http://127.0.0.1:8123/callback");
        c.set("redirect_host", "localhost").unwrap();
        assert_eq!(c.redirect_uri(8123), "http://localhost:8123/callback");
        assert!(c.set("redirect_host", "example.com").is_err());
        c.unset("redirect_port").unwrap();
        assert_eq!(c.redirect_port(), DEFAULT_REDIRECT_PORT);
        assert_eq!(c.set("nope", "x").unwrap_err().exit_code(), 2);
    }
}
