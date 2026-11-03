use ours_types::Instant;
use sha2::{Digest, Sha256};

/// The `prev_hash` of the first record in a log.
pub const GENESIS_HASH: [u8; 32] = [0; 32];

/// One entry of the log. `at` has millisecond precision. A redacted record
/// has had its payload replaced; its hashes are the original ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub seq: u64,
    pub kind: String,
    pub at: Instant,
    pub payload: Vec<u8>,
    pub prev_hash: [u8; 32],
    pub hash: [u8; 32],
    pub redacted: bool,
}

/// The engine state as of the event at `seq`; `at` is that event's time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub seq: u64,
    pub at: Instant,
    pub state: Vec<u8>,
}

/// Counts and time span of a log, for the compliance page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStats {
    pub records: u64,
    pub redacted: u64,
    pub snapshots: u64,
    pub first_at: Option<Instant>,
    pub last_at: Option<Instant>,
}

/// SHA-256 over `prev_hash || seq (big-endian) || kind || 0x00 || payload
/// || at as RFC 3339`.
pub fn record_hash(
    prev_hash: &[u8; 32],
    seq: u64,
    kind: &str,
    payload: &[u8],
    at: Instant,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash);
    hasher.update(seq.to_be_bytes());
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    hasher.update(payload);
    hasher.update(at.to_string().as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn hash_matches_a_fixed_vector() {
        let at = Instant::from_millis(1_793_613_600_000).unwrap();
        let hash = record_hash(&GENESIS_HASH, 1, "msg", br#"{"a":1}"#, at);
        assert_eq!(
            hex(&hash),
            "546a3de27d9379968b602080550ca0537887f3c73c52576595ccf708bc49f48f"
        );
    }

    #[test]
    fn hash_keeps_fractional_seconds() {
        let at = Instant::from_millis(1_793_613_600_250).unwrap();
        let hash = record_hash(&GENESIS_HASH, 1, "msg", br#"{"a":1}"#, at);
        assert_eq!(
            hex(&hash),
            "523f984afe7e7b6b25f02340df3187398a2bbf491dd3ff9aafe315efad81e85c"
        );
    }

    #[test]
    fn every_field_changes_the_hash() {
        let at = Instant::from_millis(1_793_613_600_000).unwrap();
        let base = record_hash(&GENESIS_HASH, 1, "msg", b"x", at);
        let other_at = Instant::from_millis(1_793_613_600_001).unwrap();
        assert_ne!(base, record_hash(&[1; 32], 1, "msg", b"x", at));
        assert_ne!(base, record_hash(&GENESIS_HASH, 2, "msg", b"x", at));
        assert_ne!(base, record_hash(&GENESIS_HASH, 1, "msg2", b"x", at));
        assert_ne!(base, record_hash(&GENESIS_HASH, 1, "msg", b"y", at));
        assert_ne!(base, record_hash(&GENESIS_HASH, 1, "msg", b"x", other_at));
    }
}
