//! ours-admin: the admin page server. It serves one embedded page, the
//! JSON and SSE endpoints the page uses, and guards them with a token.
//! It reads the engine only through `AdminSource`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod auth;
mod compliance;
mod error;
mod qr;
mod routes;
mod server;
mod source;

pub use error::AdminError;
pub use qr::qr_lines_for;
pub use routes::router;
pub use server::{AdminConfig, AdminServer};
pub use source::{
    AdminEvent, AdminSource, ComplianceReport, Export, FactCounts, GroupFacts, GroupSummary,
    HostFacts, ModelFacts, TelemetryFacts,
};
