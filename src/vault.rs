//! Where sign-ins are kept: the OS Keychain in the binary, an in-memory map
//! in the tests. Values are whole JSON items (one per credential set).

use std::cell::RefCell;
use std::collections::HashMap;

use pk_cli_core::CliError;
use pk_cli_secrets::{CredentialStore, Secret};
use serde::{de::DeserializeOwned, Serialize};

pub trait Vault {
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

/// A present item that does not parse is an error naming the account,
/// never `None`: treating it as absent would read as an unexplained logout.
pub fn get_json<T: DeserializeOwned>(v: &dyn Vault, account: &str) -> Result<Option<T>, CliError> {
    match v.read(account)? {
        None => Ok(None),
        Some(s) => serde_json::from_str(&s).map(Some).map_err(|e| {
            CliError::Keychain(format!(
                "the Keychain item {account} in {} is not a sign-in this version can read ({e})",
                v.service()
            ))
            .with_remedy("sign in again with `mychart auth login`; the new sign-in replaces it")
        }),
    }
}

pub fn set_json<T: Serialize>(v: &dyn Vault, account: &str, value: &T) -> Result<(), CliError> {
    let s = serde_json::to_string(value)
        .map_err(|e| CliError::Other(format!("serializing the sign-in: {e}")))?;
    v.write(account, &s)
}

/// An in-memory vault for tests.
#[derive(Default)]
pub struct MemoryVault {
    items: RefCell<HashMap<String, String>>,
}

impl MemoryVault {
    pub fn len(&self) -> usize {
        self.items.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }

    pub fn accounts(&self) -> Vec<String> {
        let mut a: Vec<String> = self.items.borrow().keys().cloned().collect();
        a.sort();
        a
    }
}

impl Vault for MemoryVault {
    fn read(&self, account: &str) -> Result<Option<String>, CliError> {
        Ok(self.items.borrow().get(account).cloned())
    }
    fn write(&self, account: &str, value: &str) -> Result<(), CliError> {
        self.items.borrow_mut().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<bool, CliError> {
        Ok(self.items.borrow_mut().remove(account).is_some())
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
        let v = MemoryVault::default();
        assert_eq!(get_json::<serde_json::Value>(&v, "a").unwrap(), None);
        v.write("a", "not json").unwrap();
        let e = get_json::<serde_json::Value>(&v, "a").unwrap_err();
        assert!(e.to_string().contains("sign in again"), "{e}");
        set_json(&v, "a", &serde_json::json!({"k": 1})).unwrap();
        assert_eq!(
            get_json::<serde_json::Value>(&v, "a").unwrap().unwrap()["k"],
            1
        );
        assert!(v.delete("a").unwrap());
        assert!(!v.delete("a").unwrap());
    }
}
