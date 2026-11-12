//! Where sign-ins are kept: the OS Keychain in the binary, an in-memory map
//! in the tests. Each item is one `pk_med_smart::SmartSession`, as JSON.
//!
//! [`Sessions`] is the `SessionStore` pk-med-smart's renewer reads and
//! writes through. It is `Send + Sync` and owns its vault, so a FHIR
//! client's token source can hold it for a whole export.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::{CredentialStore, Secret};
use pk_med_smart::{decode_session, SessionStore, SmartSession, SESSION_FORMAT};

/// What to do about a stored sign-in that does not read.
pub const UNREADABLE_REMEDY: &str = "sign in again with `mychart auth login --org <ORG>`; the new \
     sign-in replaces it (a sign-in mychart 0.1 stored does not read in 0.2)";

pub trait Vault: Send + Sync {
    fn read(&self, account: &str) -> Result<Option<String>, CliError>;
    fn write(&self, account: &str, value: &str) -> Result<(), CliError>;
    /// `true` when something was removed.
    fn delete(&self, account: &str) -> Result<bool, CliError>;
    fn service(&self) -> &str;
}

impl Vault for CredentialStore {
    fn read(&self, account: &str) -> Result<Option<String>, CliError> {
        Ok(self.get(account)?.map(|s| s.expose().to_string()))
    }

    fn write(&self, account: &str, value: &str) -> Result<(), CliError> {
        self.set(account, &Secret::new(value))
    }

    fn delete(&self, account: &str) -> Result<bool, CliError> {
        CredentialStore::delete(self, account)
    }

    fn service(&self) -> &str {
        CredentialStore::service(self)
    }
}

/// The sign-ins in a [`Vault`], one `SmartSession` per account. A present
/// item that does not read is an error (pk-med-smart's, which names a line
/// and a column and never a value), never `None`: treating it as absent
/// would read as an unexplained logout.
#[derive(Clone)]
pub struct Sessions(pub Arc<dyn Vault>);

impl Sessions {
    pub fn new(vault: Arc<dyn Vault>) -> Self {
        Sessions(vault)
    }
}

impl SessionStore for Sessions {
    fn load(&self, account: &str) -> Result<Option<SmartSession>, CliError> {
        match self.0.read(account)? {
            None => Ok(None),
            // Exit 3 like any sign-in that no longer works: the remedy is
            // a new sign-in, which replaces the item.
            Some(text) => decode_session(&text).map(Some).map_err(|e| {
                CliError::Auth(e.message().to_string())
                    .because(Reason::CredentialUnreadable)
                    .with_remedy(UNREADABLE_REMEDY)
            }),
        }
    }

    fn save(&self, session: &SmartSession) -> Result<(), CliError> {
        let mut value = serde_json::to_value(session)
            .map_err(|_| CliError::Other("a sign-in did not serialize".into()))?;
        // What is written is this release's shape, and its stamp says so.
        value["format"] = SESSION_FORMAT.into();
        self.0.write(&session.account(), &value.to_string())
    }

    fn delete(&self, account: &str) -> Result<bool, CliError> {
        self.0.delete(account)
    }
}

/// An in-memory vault for tests.
#[derive(Default)]
pub struct MemoryVault {
    items: Mutex<HashMap<String, String>>,
}

impl MemoryVault {
    fn items(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.items.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn len(&self) -> usize {
        self.items().len()
    }

    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }

    pub fn accounts(&self) -> Vec<String> {
        let mut a: Vec<String> = self.items().keys().cloned().collect();
        a.sort();
        a
    }
}

impl Vault for MemoryVault {
    fn read(&self, account: &str) -> Result<Option<String>, CliError> {
        Ok(self.items().get(account).cloned())
    }

    fn write(&self, account: &str, value: &str) -> Result<(), CliError> {
        self.items().insert(account.into(), value.into());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<bool, CliError> {
        Ok(self.items().remove(account).is_some())
    }

    fn service(&self) -> &str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_item_is_an_error_not_a_logout() {
        let v: Arc<dyn Vault> = Arc::new(MemoryVault::default());
        let sessions = Sessions::new(v.clone());
        assert!(sessions.load("smart/o/p").unwrap().is_none());
        // A mychart 0.1 item: another shape entirely.
        v.write("smart/o/p", r#"{"version":1,"access_token":"x"}"#)
            .unwrap();
        let e = sessions.load("smart/o/p").unwrap_err();
        assert_eq!(e.exit_code(), 3);
        assert!(e.to_string().contains("sign in again"), "{e}");
        assert!(!e.to_string().contains("\"x\""), "{e}");
        assert!(sessions.delete("smart/o/p").unwrap());
        assert!(!sessions.delete("smart/o/p").unwrap());
    }
}
