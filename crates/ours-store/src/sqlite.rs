use crate::error::StoreError;
use crate::log::{EventLog, Redactor};
use crate::record::{ChainHead, GENESIS_HASH, Record, Snapshot, StoreStats, record_hash};
use ours_types::Instant;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tracing::Span;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Records fetched per lock hold while verifying or exporting.
const PAGE: usize = 1000;

/// Migration `n` takes `user_version` from `n - 1` to `n`.
const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE events (
        seq       INTEGER PRIMARY KEY CHECK (seq >= 1),
        kind      TEXT    NOT NULL,
        at        INTEGER NOT NULL,
        payload   BLOB    NOT NULL,
        prev_hash BLOB    NOT NULL CHECK (length(prev_hash) = 32),
        hash      BLOB    NOT NULL CHECK (length(hash) = 32)
    );
    CREATE TABLE snapshots (
        seq   INTEGER PRIMARY KEY REFERENCES events (seq),
        at    INTEGER NOT NULL,
        state BLOB    NOT NULL
    );
    CREATE TABLE kv (
        key   TEXT PRIMARY KEY,
        value BLOB NOT NULL
    ) WITHOUT ROWID;
",
    "
    ALTER TABLE events ADD COLUMN redacted INTEGER NOT NULL DEFAULT 0;
",
];

/// Made on every open rather than by a migration, so an older build can
/// still open the file.
const INDEXES: &str = "
    CREATE INDEX IF NOT EXISTS events_at_redacted ON events (at, redacted);
";

/// Answered from `events_at_redacted` alone, so no payload is read.
const STATS_SQL: &str = "SELECT COUNT(*), COALESCE(SUM(redacted), 0), MIN(at), MAX(at) FROM events";

/// The newest snapshot and one before it, to fall back on when a redaction
/// drops the newest.
const KEPT_SNAPSHOTS: i64 = 2;

const EVENT_COLUMNS: &str = "seq, kind, at, payload, prev_hash, hash, redacted";

/// One group's log in one SQLite file. A single connection sits behind a
/// mutex, so the store is `Send + Sync` and appends are serialised.
#[derive(Debug)]
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Opens the file, creating it and migrating the schema as needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::from_connection(Connection::open(path)?)
    }

    /// A store that lives only as long as the value; for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Self, StoreError> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        conn.execute_batch(INDEXES)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves at worst a rolled-back
        // transaction, so the connection is still usable.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let supported = MIGRATIONS.len() as u32;
    let found: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if found > supported {
        return Err(StoreError::SchemaTooNew { found, supported });
    }
    for (version, sql) in (1u32..).zip(MIGRATIONS) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Another process may have migrated while this one waited.
        let current: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if current >= version {
            continue;
        }
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

struct RawEvent {
    seq: i64,
    kind: String,
    at: i64,
    payload: Vec<u8>,
    prev_hash: Vec<u8>,
    hash: Vec<u8>,
    redacted: bool,
}

fn raw_event(row: &Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok(RawEvent {
        seq: row.get(0)?,
        kind: row.get(1)?,
        at: row.get(2)?,
        payload: row.get(3)?,
        prev_hash: row.get(4)?,
        hash: row.get(5)?,
        redacted: row.get(6)?,
    })
}

impl RawEvent {
    fn into_record(self) -> Result<Record, StoreError> {
        let seq = u64::try_from(self.seq).map_err(|_| corrupt(0, "negative seq"))?;
        let at = Instant::from_millis(self.at).ok_or_else(|| corrupt(seq, "time out of range"))?;
        Ok(Record {
            seq,
            kind: self.kind,
            at,
            payload: self.payload,
            prev_hash: hash_from(seq, "prev_hash", self.prev_hash)?,
            hash: hash_from(seq, "hash", self.hash)?,
            redacted: self.redacted,
        })
    }
}

fn hash_from(seq: u64, column: &str, bytes: Vec<u8>) -> Result<[u8; 32], StoreError> {
    let len = bytes.len();
    bytes
        .try_into()
        .map_err(|_| corrupt(seq, format!("{column} is {len} bytes, not 32")))
}

fn corrupt(seq: u64, reason: impl Into<String>) -> StoreError {
    StoreError::Corrupt {
        seq,
        reason: reason.into(),
    }
}

fn db_int(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The instant as the store keeps it: whole milliseconds.
fn to_millis(at: Instant) -> Result<(i64, Instant), StoreError> {
    let ms = at.as_millis();
    let rounded = Instant::from_millis(ms)
        .ok_or_else(|| StoreError::Serialization(format!("time {at} is out of range")))?;
    Ok((ms, rounded))
}

fn read_page(conn: &Connection, from_seq: u64, limit: usize) -> Result<Vec<Record>, StoreError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {EVENT_COLUMNS} FROM events WHERE seq >= ?1 ORDER BY seq LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![db_int(from_seq), db_int(limit as u64)], raw_event)?;
    rows.map(|row| row?.into_record()).collect()
}

impl EventLog for SqliteStore {
    #[tracing::instrument(
        level = "debug",
        skip(self, payload),
        fields(bytes = payload.len(), seq = tracing::field::Empty)
    )]
    fn append(&self, kind: &str, payload: &[u8], at: Instant) -> Result<Record, StoreError> {
        if kind.is_empty() || kind.contains('\0') {
            return Err(StoreError::Serialization(
                "kind must be non-empty and contain no NUL".into(),
            ));
        }
        let (at_ms, at) = to_millis(at)?;

        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let last = tx
            .query_row(
                "SELECT seq, hash FROM events ORDER BY seq DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        let (seq, prev_hash) = match last {
            Some((last_seq, last_hash)) => {
                let last_seq = u64::try_from(last_seq).map_err(|_| corrupt(0, "negative seq"))?;
                (last_seq + 1, hash_from(last_seq, "hash", last_hash)?)
            }
            None => (1, GENESIS_HASH),
        };
        let hash = record_hash(&prev_hash, seq, kind, payload, at);
        tx.execute(
            "INSERT INTO events (seq, kind, at, payload, prev_hash, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![db_int(seq), kind, at_ms, payload, prev_hash, hash],
        )?;
        tx.commit()?;

        Span::current().record("seq", seq);
        Ok(Record {
            seq,
            kind: kind.to_owned(),
            at,
            payload: payload.to_vec(),
            prev_hash,
            hash,
            redacted: false,
        })
    }

    fn read(&self, from_seq: u64, limit: usize) -> Result<Vec<Record>, StoreError> {
        read_page(&self.conn(), from_seq, limit)
    }

    fn latest(&self) -> Result<Option<Record>, StoreError> {
        let conn = self.conn();
        let raw = conn
            .query_row(
                &format!("SELECT {EVENT_COLUMNS} FROM events ORDER BY seq DESC LIMIT 1"),
                [],
                raw_event,
            )
            .optional()?;
        raw.map(RawEvent::into_record).transpose()
    }

    #[tracing::instrument(level = "info", skip(self), fields(verified = tracing::field::Empty))]
    fn verify_head(&self) -> Result<ChainHead, StoreError> {
        let mut expected_seq = 1u64;
        let mut prev_hash = GENESIS_HASH;
        loop {
            let page = self.read(expected_seq, PAGE)?;
            if page.is_empty() {
                break;
            }
            for record in page {
                if record.seq != expected_seq {
                    return Err(corrupt(
                        expected_seq,
                        format!("expected seq {expected_seq}, found {}", record.seq),
                    ));
                }
                if record.prev_hash != prev_hash {
                    return Err(corrupt(
                        record.seq,
                        "prev_hash does not match the previous record",
                    ));
                }
                // A redacted record keeps its place in the chain but its
                // original content is gone, so only unredacted ones are rehashed.
                if !record.redacted {
                    let want = record_hash(
                        &record.prev_hash,
                        record.seq,
                        &record.kind,
                        &record.payload,
                        record.at,
                    );
                    if record.hash != want {
                        return Err(corrupt(record.seq, "hash does not match the contents"));
                    }
                }
                prev_hash = record.hash;
                expected_seq += 1;
            }
        }
        let verified = expected_seq - 1;
        Span::current().record("verified", verified);
        Ok(ChainHead {
            seq: verified,
            hash: prev_hash,
        })
    }

    fn save_snapshot(&self, seq: u64, state: &[u8]) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "INSERT INTO snapshots (seq, at, state)
             SELECT seq, at, ?2 FROM events WHERE seq = ?1
             ON CONFLICT (seq) DO UPDATE SET at = excluded.at, state = excluded.state",
            params![db_int(seq), state],
        )?;
        if changed == 0 {
            return Err(StoreError::NoSuchEvent { seq });
        }
        // The freed pages go on SQLite's free list and the next snapshot
        // reuses them, so the file stops growing.
        tx.execute(
            "DELETE FROM snapshots WHERE seq < (
                 SELECT seq FROM snapshots ORDER BY seq DESC LIMIT 1 OFFSET ?1
             )",
            [KEPT_SNAPSHOTS - 1],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn load_latest_snapshot(&self) -> Result<Option<Snapshot>, StoreError> {
        let conn = self.conn();
        let raw = conn
            .query_row(
                "SELECT seq, at, state FROM snapshots ORDER BY seq DESC LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((seq, at, state)) = raw else {
            return Ok(None);
        };
        let seq = u64::try_from(seq).map_err(|_| corrupt(0, "negative snapshot seq"))?;
        let at =
            Instant::from_millis(at).ok_or_else(|| corrupt(seq, "snapshot time out of range"))?;
        Ok(Some(Snapshot { seq, at, state }))
    }

    fn export(&self) -> Result<Vec<Record>, StoreError> {
        let mut all = Vec::new();
        let mut from = 1;
        loop {
            let page = self.read(from, PAGE)?;
            let Some(last) = page.last() else {
                return Ok(all);
            };
            from = last.seq + 1;
            all.extend(page);
        }
    }

    #[tracing::instrument(level = "info", skip(self, f), fields(redacted = tracing::field::Empty))]
    fn redact_where(&self, f: &Redactor<'_>) -> Result<u64, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut count = 0u64;
        let mut from = 1u64;
        loop {
            let page = read_page(&tx, from, PAGE)?;
            let Some(last) = page.last() else { break };
            from = last.seq + 1;
            for record in &page {
                if let Some(payload) = f(record.seq, &record.kind, &record.payload) {
                    tx.execute(
                        "UPDATE events SET payload = ?2, redacted = 1 WHERE seq = ?1",
                        params![db_int(record.seq), payload],
                    )?;
                    count += 1;
                }
            }
        }
        tx.commit()?;
        Span::current().record("redacted", count);
        Ok(count)
    }

    fn earliest_redacted(&self) -> Result<Option<u64>, StoreError> {
        let seq: Option<i64> = self.conn().query_row(
            "SELECT MIN(seq) FROM events WHERE redacted = 1",
            [],
            |row| row.get(0),
        )?;
        seq.map(|s| u64::try_from(s).map_err(|_| corrupt(0, "negative seq")))
            .transpose()
    }

    fn drop_snapshots_from(&self, seq: u64) -> Result<u64, StoreError> {
        let removed = self
            .conn()
            .execute("DELETE FROM snapshots WHERE seq >= ?1", [db_int(seq)])?;
        Ok(removed as u64)
    }

    fn stats(&self) -> Result<StoreStats, StoreError> {
        let conn = self.conn();
        let (records, redacted, first, last): (i64, i64, Option<i64>, Option<i64>) = conn
            .query_row(STATS_SQL, [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
        let snapshots: i64 = conn.query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get(0))?;
        let instant = |ms: Option<i64>| {
            ms.map(|ms| Instant::from_millis(ms).ok_or_else(|| corrupt(0, "time out of range")))
                .transpose()
        };
        Ok(StoreStats {
            records: u64::try_from(records).unwrap_or(0),
            redacted: u64::try_from(redacted).unwrap_or(0),
            snapshots: u64::try_from(snapshots).unwrap_or(0),
            first_at: instant(first)?,
            last_at: instant(last)?,
        })
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    const BASE_MS: i64 = 1_793_613_600_000;

    fn at(offset_ms: i64) -> Instant {
        Instant::from_millis(BASE_MS + offset_ms).unwrap()
    }

    fn fill(store: &SqliteStore, n: u64) -> Vec<Record> {
        (1..=n)
            .map(|i| {
                let payload = format!(r#"{{"n":{i}}}"#);
                store
                    .append("msg", payload.as_bytes(), at(i as i64 * 1000))
                    .unwrap()
            })
            .collect()
    }

    fn raw(path: &Path) -> Connection {
        Connection::open(path).unwrap()
    }

    #[test]
    fn append_then_read_returns_the_same_records_in_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let written = fill(&store, 5);
        let read = store.read(1, 100).unwrap();
        assert_eq!(read, written);
        let seqs: Vec<u64> = read.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5]);
        assert_eq!(read[2].payload, br#"{"n":3}"#);
        assert_eq!(read[2].kind, "msg");
        assert_eq!(read[2].at, at(3000));
    }

    #[test]
    fn read_pages_from_a_seq() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 10);
        let page = store.read(4, 3).unwrap();
        let seqs: Vec<u64> = page.iter().map(|r| r.seq).collect();
        assert_eq!(seqs, [4, 5, 6]);
        assert!(store.read(11, 3).unwrap().is_empty());
        assert!(store.read(1, 0).unwrap().is_empty());
        assert_eq!(store.read(0, 100).unwrap().len(), 10);
    }

    #[test]
    fn latest_after_n_appends_is_n() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.latest().unwrap(), None);
        let written = fill(&store, 7);
        let latest = store.latest().unwrap().unwrap();
        assert_eq!(latest.seq, 7);
        assert_eq!(latest, written[6]);
    }

    #[test]
    fn hashes_chain() {
        let store = SqliteStore::open_in_memory().unwrap();
        let written = fill(&store, 3);
        assert_eq!(written[0].prev_hash, GENESIS_HASH);
        assert_eq!(written[1].prev_hash, written[0].hash);
        assert_eq!(written[2].prev_hash, written[1].hash);
        assert_ne!(written[0].hash, written[1].hash);
    }

    #[test]
    fn verify_chain_counts_a_good_log() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.verify_chain().unwrap(), 0);
        fill(&store, 2500);
        assert_eq!(store.verify_chain().unwrap(), 2500);
    }

    #[test]
    fn verify_chain_names_a_tampered_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let store = SqliteStore::open(&path).unwrap();
        fill(&store, 5);

        raw(&path)
            .execute("UPDATE events SET payload = x'00' WHERE seq = 3", [])
            .unwrap();

        match store.verify_chain() {
            Err(StoreError::Corrupt { seq, .. }) => assert_eq!(seq, 3),
            other => panic!("expected Corrupt at 3, got {other:?}"),
        }
    }

    #[test]
    fn verify_chain_names_a_broken_link() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let store = SqliteStore::open(&path).unwrap();
        fill(&store, 5);

        raw(&path)
            .execute(
                "UPDATE events SET prev_hash = zeroblob(32) WHERE seq = 4",
                [],
            )
            .unwrap();

        match store.verify_chain() {
            Err(StoreError::Corrupt { seq, .. }) => assert_eq!(seq, 4),
            other => panic!("expected Corrupt at 4, got {other:?}"),
        }
    }

    #[test]
    fn verify_chain_names_a_deleted_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let store = SqliteStore::open(&path).unwrap();
        fill(&store, 5);

        raw(&path)
            .execute("DELETE FROM events WHERE seq = 2", [])
            .unwrap();

        match store.verify_chain() {
            Err(StoreError::Corrupt { seq, .. }) => assert_eq!(seq, 2),
            other => panic!("expected Corrupt at 2, got {other:?}"),
        }
    }

    #[test]
    fn verify_chain_reports_the_first_of_two_bad_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let store = SqliteStore::open(&path).unwrap();
        fill(&store, 6);

        let conn = raw(&path);
        conn.execute("UPDATE events SET kind = 'other' WHERE seq = 5", [])
            .unwrap();
        conn.execute("UPDATE events SET payload = x'01' WHERE seq = 2", [])
            .unwrap();

        match store.verify_chain() {
            Err(StoreError::Corrupt { seq, .. }) => assert_eq!(seq, 2),
            other => panic!("expected Corrupt at 2, got {other:?}"),
        }
    }

    #[test]
    fn append_rejects_a_bad_kind() {
        let store = SqliteStore::open_in_memory().unwrap();
        for kind in ["", "a\0b"] {
            assert!(matches!(
                store.append(kind, b"x", at(0)),
                Err(StoreError::Serialization(_))
            ));
        }
        assert_eq!(store.latest().unwrap(), None);
    }

    #[test]
    fn time_is_kept_to_the_millisecond() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let nanos = i128::from(BASE_MS) * 1_000_000 + 123_456;
        let sub_ms = Instant(jiff::Timestamp::from_nanosecond(nanos).unwrap());
        let written = {
            let store = SqliteStore::open(&path).unwrap();
            store.append("msg", b"x", sub_ms).unwrap()
        };
        assert_eq!(written.at, at(0));

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.read(1, 1).unwrap(), vec![written]);
        assert_eq!(store.verify_chain().unwrap(), 1);
    }

    #[test]
    fn snapshot_save_and_load_returns_the_newest() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.load_latest_snapshot().unwrap(), None);
        fill(&store, 6);

        store.save_snapshot(4, b"state-4").unwrap();
        store.save_snapshot(2, b"state-2").unwrap();
        let newest = store.load_latest_snapshot().unwrap().unwrap();
        assert_eq!(newest.seq, 4);
        assert_eq!(newest.state, b"state-4");
        assert_eq!(newest.at, at(4000));

        store.save_snapshot(6, b"state-6").unwrap();
        assert_eq!(store.load_latest_snapshot().unwrap().unwrap().seq, 6);
    }

    #[test]
    fn saving_a_snapshot_twice_replaces_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 2);
        store.save_snapshot(2, b"first").unwrap();
        store.save_snapshot(2, b"second").unwrap();
        let snapshot = store.load_latest_snapshot().unwrap().unwrap();
        assert_eq!(snapshot.state, b"second");
    }

    #[test]
    fn a_snapshot_needs_an_event() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 2);
        for seq in [0, 3] {
            assert!(matches!(
                store.save_snapshot(seq, b"x"),
                Err(StoreError::NoSuchEvent { seq: s }) if s == seq
            ));
        }
        assert_eq!(store.load_latest_snapshot().unwrap(), None);
    }

    #[test]
    fn kv_round_trips() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(store.get("tz").unwrap(), None);
        store.set("tz", b"Europe/London").unwrap();
        assert_eq!(store.get("tz").unwrap().unwrap(), b"Europe/London");
        store.set("tz", b"UTC").unwrap();
        assert_eq!(store.get("tz").unwrap().unwrap(), b"UTC");
        store.set("bin", &[0, 255, 0, 7]).unwrap();
        assert_eq!(store.get("bin").unwrap().unwrap(), [0, 255, 0, 7]);
        store.set("empty", b"").unwrap();
        assert_eq!(store.get("empty").unwrap().unwrap(), b"");
    }

    #[test]
    fn export_returns_everything_across_pages() {
        let store = SqliteStore::open_in_memory().unwrap();
        let written = fill(&store, PAGE as u64 + 17);
        assert_eq!(store.export().unwrap(), written);
        assert!(
            SqliteStore::open_in_memory()
                .unwrap()
                .export()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reopening_keeps_the_data_and_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let written = {
            let store = SqliteStore::open(&path).unwrap();
            let written = fill(&store, 3);
            store.save_snapshot(2, b"state").unwrap();
            store.set("k", b"v").unwrap();
            written
        };

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.export().unwrap(), written);
        assert_eq!(store.verify_chain().unwrap(), 3);
        assert_eq!(store.load_latest_snapshot().unwrap().unwrap().seq, 2);
        assert_eq!(store.get("k").unwrap().unwrap(), b"v");

        let next = store.append("msg", b"after", at(9000)).unwrap();
        assert_eq!(next.seq, 4);
        assert_eq!(next.prev_hash, written[2].hash);
        assert_eq!(store.verify_chain().unwrap(), 4);
    }

    #[test]
    fn the_file_uses_wal_and_schema_version_2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let _store = SqliteStore::open(&path).unwrap();
        let conn = raw(&path);
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        assert_eq!(version, 2);
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        drop(SqliteStore::open(&path).unwrap());
        raw(&path).pragma_update(None, "user_version", 99).unwrap();
        assert!(matches!(
            SqliteStore::open(&path),
            Err(StoreError::SchemaTooNew {
                found: 99,
                supported: 2
            })
        ));
    }

    fn redact_payload(seqs: &'static [u64]) -> impl Fn(u64, &str, &[u8]) -> Option<Vec<u8>> {
        move |seq, _, _| seqs.contains(&seq).then(|| b"[forgotten]".to_vec())
    }

    #[test]
    fn redaction_keeps_the_chain_and_marks_records() {
        let store = SqliteStore::open_in_memory().unwrap();
        let written = fill(&store, 10);
        let n = store.redact_where(&redact_payload(&[2, 5, 9])).unwrap();
        assert_eq!(n, 3);
        assert_eq!(store.verify_chain().unwrap(), 10);
        assert_eq!(store.earliest_redacted().unwrap(), Some(2));

        for (record, before) in store.export().unwrap().into_iter().zip(&written) {
            if [2, 5, 9].contains(&record.seq) {
                assert!(record.redacted);
                assert_eq!(record.payload, b"[forgotten]");
                assert_eq!(record.hash, before.hash);
                assert_eq!(record.prev_hash, before.prev_hash);
            } else {
                assert_eq!(&record, before);
            }
        }
    }

    #[test]
    fn a_redacted_record_is_not_rehashed_but_an_unredacted_one_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let store = SqliteStore::open(&path).unwrap();
        fill(&store, 10);
        store.redact_where(&redact_payload(&[4])).unwrap();

        let conn = raw(&path);
        conn.execute("UPDATE events SET payload = x'aa' WHERE seq = 4", [])
            .unwrap();
        assert_eq!(store.verify_chain().unwrap(), 10);

        conn.execute("UPDATE events SET payload = x'aa' WHERE seq = 7", [])
            .unwrap();
        match store.verify_chain() {
            Err(StoreError::Corrupt { seq, .. }) => assert_eq!(seq, 7),
            other => panic!("expected Corrupt at 7, got {other:?}"),
        }
    }

    #[test]
    fn redacting_nothing_changes_nothing() {
        let store = SqliteStore::open_in_memory().unwrap();
        let written = fill(&store, 10);
        assert_eq!(store.redact_where(&|_, _, _| None).unwrap(), 0);
        assert_eq!(store.export().unwrap(), written);
        assert_eq!(store.earliest_redacted().unwrap(), None);
    }

    #[test]
    fn redaction_sees_every_record_oldest_first() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 5);
        let seen = std::sync::Mutex::new(Vec::new());
        store
            .redact_where(&|seq, kind, payload| {
                assert_eq!(kind, "msg");
                assert!(!payload.is_empty());
                seen.lock().unwrap().push(seq);
                None
            })
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn an_old_file_without_the_column_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let first = {
            let conn = raw(&path);
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            let hash = record_hash(&GENESIS_HASH, 1, "msg", b"old", at(0));
            conn.execute(
                "INSERT INTO events (seq, kind, at, payload, prev_hash, hash)
                 VALUES (1, 'msg', ?1, x'6f6c64', ?2, ?3)",
                params![BASE_MS, GENESIS_HASH, hash],
            )
            .unwrap();
            hash
        };

        let store = SqliteStore::open(&path).unwrap();
        let records = store.export().unwrap();
        assert_eq!(records.len(), 1);
        assert!(!records[0].redacted);
        assert_eq!(records[0].hash, first);
        assert_eq!(store.verify_chain().unwrap(), 1);
        assert_eq!(store.redact_where(&|_, _, _| Some(vec![])).unwrap(), 1);
        assert_eq!(store.verify_chain().unwrap(), 1);
    }

    #[test]
    fn drop_snapshots_from_removes_the_right_ones() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 8);
        for seq in [4, 8] {
            store.save_snapshot(seq, b"s").unwrap();
        }
        assert_eq!(store.drop_snapshots_from(5).unwrap(), 1);
        assert_eq!(store.load_latest_snapshot().unwrap().unwrap().seq, 4);
        assert_eq!(store.drop_snapshots_from(5).unwrap(), 0);
        assert_eq!(store.drop_snapshots_from(1).unwrap(), 1);
        assert_eq!(store.load_latest_snapshot().unwrap(), None);
    }

    fn snapshot_seqs(store: &SqliteStore) -> Vec<u64> {
        let conn = store.conn();
        let mut stmt = conn
            .prepare("SELECT seq FROM snapshots ORDER BY seq")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .map(|seq| seq.unwrap() as u64)
            .collect()
    }

    #[test]
    fn only_the_newest_two_snapshots_are_kept() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 8);
        store.save_snapshot(2, b"s").unwrap();
        assert_eq!(snapshot_seqs(&store), [2]);
        for seq in [4, 6, 8] {
            store.save_snapshot(seq, b"s").unwrap();
        }
        assert_eq!(snapshot_seqs(&store), [6, 8]);
        store.save_snapshot(3, b"older").unwrap();
        assert_eq!(snapshot_seqs(&store), [6, 8]);
        store.save_snapshot(8, b"again").unwrap();
        assert_eq!(snapshot_seqs(&store), [6, 8]);
        assert_eq!(store.stats().unwrap().snapshots, 2);
    }

    #[test]
    fn a_redaction_falls_back_to_the_older_snapshot() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 10);
        store.save_snapshot(4, b"at 4").unwrap();
        store.save_snapshot(8, b"at 8").unwrap();
        store.redact_where(&redact_payload(&[6])).unwrap();

        let from = store.earliest_redacted().unwrap().unwrap();
        assert_eq!(store.drop_snapshots_from(from).unwrap(), 1);
        let fallback = store.load_latest_snapshot().unwrap().unwrap();
        assert_eq!((fallback.seq, fallback.state.as_slice()), (4, &b"at 4"[..]));

        store.save_snapshot(10, b"clean").unwrap();
        assert_eq!(snapshot_seqs(&store), [4, 10]);
    }

    #[test]
    fn space_from_dropped_snapshots_is_reused() {
        const STATE: usize = 64 * 1024;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let store = SqliteStore::open(&path).unwrap();
        fill(&store, 40);
        for seq in 1..=40u8 {
            store.save_snapshot(u64::from(seq), &[seq; STATE]).unwrap();
        }
        let conn = raw(&path);
        let pages: i64 = conn
            .pragma_query_value(None, "page_count", |r| r.get(0))
            .unwrap();
        let page_size: i64 = conn
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .unwrap();
        let bytes = (pages * page_size) as usize;
        assert!(bytes < 5 * STATE, "{bytes} bytes for two kept snapshots");
    }

    #[test]
    fn stats_reads_the_index_not_the_table() {
        let store = SqliteStore::open_in_memory().unwrap();
        fill(&store, 3);
        let conn = store.conn();
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {STATS_SQL}"))
            .unwrap()
            .query_map([], |row| row.get(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert!(
            plan[0].contains("USING COVERING INDEX events_at_redacted"),
            "{plan:?}"
        );
    }

    #[test]
    fn an_existing_file_gets_the_index_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        drop(SqliteStore::open(&path).unwrap());
        let has_index = || -> bool {
            raw(&path)
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name = 'events_at_redacted'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        };
        raw(&path)
            .execute_batch("DROP INDEX events_at_redacted")
            .unwrap();
        assert!(!has_index());

        let store = SqliteStore::open(&path).unwrap();
        assert!(has_index());
        let version: u32 = raw(&path)
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2);
        fill(&store, 2);
        assert_eq!(store.stats().unwrap().records, 2);
    }

    #[test]
    fn verify_head_names_the_newest_seq_and_hash() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(
            store.verify_head().unwrap(),
            ChainHead {
                seq: 0,
                hash: GENESIS_HASH
            }
        );
        let written = fill(&store, PAGE as u64 + 3);
        let head = store.verify_head().unwrap();
        assert_eq!(head.seq, PAGE as u64 + 3);
        assert_eq!(head.hash, written.last().unwrap().hash);

        store.redact_where(&redact_payload(&[2])).unwrap();
        assert_eq!(store.verify_head().unwrap(), head);

        store
            .conn()
            .execute("UPDATE events SET payload = x'00' WHERE seq = 5", [])
            .unwrap();
        assert!(matches!(
            store.verify_head(),
            Err(StoreError::Corrupt { seq: 5, .. })
        ));
    }

    #[test]
    fn stats_count_records_redactions_and_snapshots() {
        let store = SqliteStore::open_in_memory().unwrap();
        let empty = store.stats().unwrap();
        assert_eq!(
            empty,
            StoreStats {
                records: 0,
                redacted: 0,
                snapshots: 0,
                first_at: None,
                last_at: None
            }
        );

        fill(&store, 6);
        store.save_snapshot(3, b"s").unwrap();
        store.save_snapshot(6, b"s").unwrap();
        store.redact_where(&redact_payload(&[1, 2])).unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.records, 6);
        assert_eq!(stats.redacted, 2);
        assert_eq!(stats.snapshots, 2);
        assert_eq!(stats.first_at, Some(at(1000)));
        assert_eq!(stats.last_at, Some(at(6000)));
    }

    #[test]
    fn concurrent_appends_from_threads_keep_one_chain() {
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    for i in 0..50 {
                        let payload = format!("{t}-{i}");
                        store.append("msg", payload.as_bytes(), at(0)).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.verify_chain().unwrap(), 400);
        assert_eq!(store.latest().unwrap().unwrap().seq, 400);
    }

    #[test]
    fn two_handles_on_one_file_keep_one_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.db");
        let a = Arc::new(SqliteStore::open(&path).unwrap());
        let b = Arc::new(SqliteStore::open(&path).unwrap());
        let handles: Vec<_> = [a.clone(), b.clone()]
            .into_iter()
            .map(|store| {
                thread::spawn(move || {
                    for i in 0..100 {
                        store
                            .append("msg", i.to_string().as_bytes(), at(0))
                            .unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(a.verify_chain().unwrap(), 200);
        assert_eq!(b.latest().unwrap().unwrap().seq, 200);
    }

    #[test]
    fn the_store_is_usable_as_a_trait_object() {
        let store: Arc<dyn EventLog> = Arc::new(SqliteStore::open_in_memory().unwrap());
        let record = store.append("msg", b"x", at(0)).unwrap();
        assert_eq!(store.latest().unwrap().unwrap(), record);
    }
}
