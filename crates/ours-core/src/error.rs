//! Errors the engine can report. `step` itself never fails; these come from
//! loading state and from validating configuration.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("unknown time zone {0:?}")]
    UnknownTimezone(String),
    #[error("group state snapshot: {0}")]
    Snapshot(#[from] serde_json::Error),
}
