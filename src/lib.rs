//! SimQ – kö/tracker för simuleringsförfrågor.
//!
//! All logik ligger i biblioteket så att den kan testas utan gränssnitt:
//! - `model`  – domäntyper (status, platstyper, härlett ärende)
//! - `hlc`    – hybrid logical clock
//! - `ops`    – händelseformatet samt läsning/skrivning av filer i den delade mappen
//! - `replay` – den deterministiska statusmaskinen
//! - `store`  – lokal SQLite-cache
//! - `sync`   – mappläsning, inskick av egna händelser och bakgrundstråden
//! - `config` – lokal konfiguration och identitet
//! - `ui`     – egui-gränssnittet

pub mod config;
pub mod github;
pub mod hlc;
pub mod i18n;
pub mod model;
pub mod ops;
pub mod replay;
pub mod store;
pub mod sync;
pub mod ui;

#[cfg(test)]
pub(crate) mod testutil;
