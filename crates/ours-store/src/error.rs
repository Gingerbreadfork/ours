use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The log is not what the chain says it should be, starting at `seq`.
    #[error("corrupt log at seq {seq}: {reason}")]
    Corrupt { seq: u64, reason: String },

    /// A value cannot be written in the store's encoding.
    #[error("serialization: {0}")]
    Serialization(String),

    /// A snapshot was requested for a seq that is not in the log.
    #[error("no event at seq {seq}")]
    NoSuchEvent { seq: u64 },

    /// The file was written by a newer version of the store.
    #[error("schema version {found} is newer than the supported {supported}")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("invalid group id {0:?}")]
    InvalidGroupId(String),
}
