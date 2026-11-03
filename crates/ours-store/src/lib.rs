//! The append-only, hash-chained event log and the snapshot store, one
//! SQLite file per group, behind the [`EventLog`] trait. Payloads are opaque
//! bytes tagged with a `kind`; the store knows nothing of the engine's types.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod dir;
mod error;
mod log;
mod record;
mod sqlite;

pub use dir::StoreDir;
pub use error::StoreError;
pub use log::{EventLog, Redactor};
pub use record::{GENESIS_HASH, Record, Snapshot, StoreStats, record_hash};
pub use sqlite::SqliteStore;
