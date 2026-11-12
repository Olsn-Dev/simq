//! SimQ – a queue/tracker for simulation requests.
//!
//! All logic lives in the library so that it can be tested without a user interface:
//! - `model`  – domain types (status, location kinds, derived request)
//! - `hlc`    – hybrid logical clock
//! - `ops`    – the event format and reading/writing files in the shared folder
//! - `replay` – the deterministic state machine
//! - `store`  – local SQLite cache
//! - `sync`   – folder reading, submitting own events and the background thread
//! - `config` – local configuration and identity
//! - `ui`     – the egui user interface

pub mod config;
pub mod github;
pub mod hlc;
pub mod i18n;
pub mod model;
pub mod ops;
pub mod replay;
pub mod store;
pub mod sync;
pub mod theme;
pub mod ui;

#[cfg(test)]
pub(crate) mod testutil;
