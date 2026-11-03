//! The Ours binary's library: configuration, the runtime that turns the
//! pure engine into a process, and the maintenance commands.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod bench;
pub mod config;
pub mod replay;
pub mod run;
pub mod runtime;
pub mod schema_cmd;
pub mod telemetry;

/// The crate version with the commit it was built from.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("OURS_GIT_SHA"), ")");
