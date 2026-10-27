//! Dolmen core: database connections, schema introspection and schema versioning.
//!
//! Shared by the desktop app (`dolmen-app`) and the command-line tool (`dolmen`).

pub mod ai;
pub mod connections;
mod error;
pub mod git;
pub mod history;
pub mod postgres;
pub mod schema;
pub mod snapshot;
pub mod sql;

pub use error::{AiFailure, Error, Refusal};

/// Dolmen version, shared by the app and the CLI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
