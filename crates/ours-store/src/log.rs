use crate::error::StoreError;
use crate::record::{ChainHead, Record, Snapshot, StoreStats};
use ours_types::Instant;

/// Decides whether a record is redacted: given `(seq, kind, payload)`,
/// returns the replacement payload or `None` to leave it alone.
pub type Redactor<'a> = dyn Fn(u64, &str, &[u8]) -> Option<Vec<u8>> + 'a;

/// A group's durable state: the event log, its snapshots and its settings.
/// Calls block on disk; async callers wrap them in `spawn_blocking`.
pub trait EventLog: Send + Sync {
    /// Adds a record at the end of the log. `kind` must be non-empty and
    /// free of NUL. `at` is rounded down to the millisecond.
    fn append(&self, kind: &str, payload: &[u8], at: Instant) -> Result<Record, StoreError>;

    /// Up to `limit` records with `seq >= from_seq`, oldest first.
    fn read(&self, from_seq: u64, limit: usize) -> Result<Vec<Record>, StoreError>;

    /// The newest record, if any.
    fn latest(&self) -> Result<Option<Record>, StoreError>;

    /// Walks the whole chain and returns where it ends, or `Corrupt` naming
    /// the first bad seq. It reads a page at a time, so appends still go
    /// through while it runs.
    fn verify_head(&self) -> Result<ChainHead, StoreError>;

    /// Walks the whole chain and returns how many records it checked, or
    /// `Corrupt` naming the first bad seq.
    fn verify_chain(&self) -> Result<u64, StoreError> {
        self.verify_head().map(|head| head.seq)
    }

    /// Stores `state` as the snapshot at `seq`, replacing one already there,
    /// then drops all but the newest two snapshots. The event at `seq` must
    /// exist.
    fn save_snapshot(&self, seq: u64, state: &[u8]) -> Result<(), StoreError>;

    /// The snapshot with the highest seq.
    fn load_latest_snapshot(&self) -> Result<Option<Snapshot>, StoreError>;

    /// Calls `f(seq, kind, payload)` for every record, oldest first. Where
    /// it returns `Some(new_payload)` the payload is replaced and the record
    /// is marked redacted; its hashes are left as they were. Runs in one
    /// transaction and returns how many records changed.
    fn redact_where(&self, f: &Redactor<'_>) -> Result<u64, StoreError>;

    /// The lowest seq of any redacted record.
    fn earliest_redacted(&self) -> Result<Option<u64>, StoreError>;

    /// Removes every snapshot at or after `seq` and returns how many.
    fn drop_snapshots_from(&self, seq: u64) -> Result<u64, StoreError>;

    /// Every record, oldest first. Redacted records carry `redacted: true`.
    fn export(&self) -> Result<Vec<Record>, StoreError>;

    /// Record, redaction and snapshot counts and the time span of the log.
    fn stats(&self) -> Result<StoreStats, StoreError>;

    /// Sets a per-group setting.
    fn set(&self, key: &str, value: &[u8]) -> Result<(), StoreError>;

    /// Reads a per-group setting.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError>;
}
