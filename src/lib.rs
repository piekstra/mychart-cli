//! `mychart`: a household member's own health records from any health system
//! running Epic (MyChart), through Epic's patient-access SMART on FHIR API.
//!
//! The binary is `src/main.rs`; this library holds everything it runs, so
//! the tests reach it without a network or a Keychain.

pub mod cli;
pub mod commands;
pub mod config;
pub mod export;
pub mod fhir;
pub mod fsutil;
pub mod key;
pub mod oauth;
pub mod orgs;
pub mod session;
pub mod smart;
pub mod stream;
pub mod vault;
