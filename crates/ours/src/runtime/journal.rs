//! The engine's inputs as they are written to and read back from a group's
//! event log. The log holds bytes; this is where the engine's types meet
//! them.

use ours_store::{EventLog, StoreError};
use ours_types::Instant;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::marker::PhantomData;
use std::sync::Arc;
use thiserror::Error;

/// The record kind every engine input is appended under.
pub const INPUT_KIND: &str = "input";

/// How many inputs may follow the latest snapshot before a new one is taken.
pub const SNAPSHOT_EVERY: u64 = 200;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("record {seq} could not be decoded: {source}")]
    Decode {
        seq: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("snapshot at {seq} could not be decoded: {source}")]
    Snapshot {
        seq: u64,
        #[source]
        source: serde_json::Error,
    },
}

/// A typed view over one group's log: `I` is the engine input, `S` the
/// engine state that snapshots hold.
pub struct Journal<I, S> {
    log: Arc<dyn EventLog>,
    _types: PhantomData<fn() -> (I, S)>,
}

impl<I, S> std::fmt::Debug for Journal<I, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Journal")
    }
}

impl<I, S> Clone for Journal<I, S> {
    fn clone(&self) -> Self {
        Journal {
            log: Arc::clone(&self.log),
            _types: PhantomData,
        }
    }
}

/// What replay found: the state to start from and the seq it is current to.
#[derive(Debug)]
pub struct Recovered<S> {
    pub state: Option<S>,
    pub seq: u64,
}

impl<I, S> Journal<I, S>
where
    I: Serialize + DeserializeOwned,
    S: Serialize + DeserializeOwned,
{
    pub fn new(log: Arc<dyn EventLog>) -> Self {
        Journal {
            log,
            _types: PhantomData,
        }
    }

    /// Appends one input and returns its seq. Blocks on disk.
    pub fn append(&self, input: &I, at: Instant) -> Result<u64, JournalError> {
        let payload =
            serde_json::to_vec(input).map_err(|source| JournalError::Decode { seq: 0, source })?;
        Ok(self.log.append(INPUT_KIND, &payload, at)?.seq)
    }

    pub fn snapshot(&self, seq: u64, state: &S) -> Result<(), JournalError> {
        let bytes =
            serde_json::to_vec(state).map_err(|source| JournalError::Snapshot { seq, source })?;
        Ok(self.log.save_snapshot(seq, &bytes)?)
    }

    /// Rewrites the payloads `f` returns a replacement for, keeping the chain.
    pub fn redact(&self, f: &ours_store::Redactor<'_>) -> Result<u64, JournalError> {
        Ok(self.log.redact_where(f)?)
    }

    pub fn earliest_redacted(&self) -> Result<Option<u64>, JournalError> {
        Ok(self.log.earliest_redacted()?)
    }

    /// Drops the snapshots from `seq` on; they may hold text since forgotten.
    pub fn drop_snapshots_from(&self, seq: u64) -> Result<u64, JournalError> {
        Ok(self.log.drop_snapshots_from(seq)?)
    }

    /// Walks the hash chain and returns its newest seq and hash.
    pub fn verify_head(&self) -> Result<ours_store::ChainHead, JournalError> {
        Ok(self.log.verify_head()?)
    }

    pub fn stats(&self) -> Result<ours_store::StoreStats, JournalError> {
        Ok(self.log.stats()?)
    }

    /// A per-group secret for calendar feed links, made once and kept in the
    /// group's settings so links keep working across restarts.
    pub fn feed_secret(&self) -> Result<String, JournalError> {
        const KEY: &str = "feed_secret";
        if let Some(bytes) = self.log.get(KEY)? {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        let secret = ulid::Ulid::generate().to_string().to_lowercase();
        self.log.set(KEY, secret.as_bytes())?;
        Ok(secret)
    }

    /// The latest snapshot, decoded, and the seq it covers (0 when none).
    pub fn recover(&self) -> Result<Recovered<S>, JournalError> {
        match self.log.load_latest_snapshot()? {
            Some(snapshot) => {
                let state = serde_json::from_slice(&snapshot.state).map_err(|source| {
                    JournalError::Snapshot {
                        seq: snapshot.seq,
                        source,
                    }
                })?;
                Ok(Recovered {
                    state: Some(state),
                    seq: snapshot.seq,
                })
            }
            None => Ok(Recovered {
                state: None,
                seq: 0,
            }),
        }
    }

    /// Every input after `after_seq`, oldest first, with its seq and time,
    /// calling `each` for every one. Pages through the log so a long replay
    /// never holds the store's lock for long.
    pub fn replay(
        &self,
        after_seq: u64,
        mut each: impl FnMut(u64, Instant, I) -> Result<(), JournalError>,
    ) -> Result<u64, JournalError> {
        const PAGE: usize = 1000;
        let mut from = after_seq + 1;
        let mut last = after_seq;
        loop {
            let page = self.log.read(from, PAGE)?;
            let Some(tail) = page.last() else {
                return Ok(last);
            };
            from = tail.seq + 1;
            for record in page {
                if record.kind != INPUT_KIND {
                    continue;
                }
                let input = serde_json::from_slice(&record.payload).map_err(|source| {
                    JournalError::Decode {
                        seq: record.seq,
                        source,
                    }
                })?;
                each(record.seq, record.at, input)?;
                last = record.seq;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_store::SqliteStore;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct In(u32);

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct St {
        sum: u32,
    }

    fn journal() -> Journal<In, St> {
        Journal::new(Arc::new(
            SqliteStore::open_in_memory().expect("memory store"),
        ))
    }

    #[test]
    fn append_snapshot_and_replay_the_tail() {
        let journal = journal();
        let at = Instant::from_millis(1_700_000_000_000).expect("in range");
        for n in 1..=5 {
            journal.append(&In(n), at).expect("append");
        }
        journal.snapshot(3, &St { sum: 6 }).expect("snapshot");
        let recovered = journal.recover().expect("recover");
        assert_eq!(recovered.seq, 3);
        let mut state = recovered.state.expect("state");
        let last = journal
            .replay(recovered.seq, |_, _, In(n)| {
                state.sum += n;
                Ok(())
            })
            .expect("replay");
        assert_eq!(last, 5);
        assert_eq!(state, St { sum: 15 });
    }

    #[test]
    fn empty_log_recovers_to_nothing() {
        let journal = journal();
        let recovered = journal.recover().expect("recover");
        assert!(recovered.state.is_none());
        assert_eq!(recovered.seq, 0);
        assert_eq!(journal.replay(0, |_, _, _| Ok(())).expect("replay"), 0);
    }
}
