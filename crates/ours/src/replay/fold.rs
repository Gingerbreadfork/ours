//! Reading a group's journal for a fold that never writes: the state to
//! start from and the logged inputs in order, a page at a time.

use super::matcher::Matcher;
use crate::runtime::journal::{INPUT_KIND, JournalError};
use ours_core::{GroupState, Input, step};
use ours_store::EventLog;
use ours_types::{GroupId, Instant, Kind};

/// Records read per lock hold.
const PAGE: usize = 1000;

/// One input as the journal holds it.
#[derive(Debug)]
pub(crate) struct Logged {
    pub seq: u64,
    pub at: Instant,
    pub input: Input,
}

/// A page of the inputs with seq in `from..=to`, and where the next page
/// starts, or None once the log or the range is used up.
#[derive(Debug)]
pub(crate) struct Page {
    pub inputs: Vec<Logged>,
    /// The highest seq read, inputs or not.
    pub last: Option<u64>,
    pub next: Option<u64>,
}

pub(crate) fn page(log: &dyn EventLog, from: u64, to: u64) -> Result<Page, JournalError> {
    let records = log.read(from, PAGE)?;
    let full = records.len() == PAGE;
    let mut page = Page {
        inputs: Vec::new(),
        last: None,
        next: None,
    };
    for record in records {
        if record.seq > to {
            return Ok(page);
        }
        page.last = Some(record.seq);
        if record.kind != INPUT_KIND {
            continue;
        }
        let input =
            serde_json::from_slice(&record.payload).map_err(|source| JournalError::Decode {
                seq: record.seq,
                source,
            })?;
        page.inputs.push(Logged {
            seq: record.seq,
            at: record.at,
            input,
        });
    }
    if full {
        page.next = page.last.map(|seq| seq + 1).filter(|next| *next <= to);
    }
    Ok(page)
}

/// Where a fold starts: the state and the seq it is current to.
#[derive(Debug)]
pub(crate) struct Start {
    pub state: GroupState,
    pub seq: u64,
    /// The seq of the snapshot it came from, if it came from one.
    pub snapshot: Option<u64>,
    /// Matches the journal's answers to what this fold asks for.
    pub matcher: Matcher,
}

/// The newest snapshot taken before `before`, or a new group. A new group
/// takes `kind`, else the newest snapshot's kind, because the runner never
/// logs the kind a group was created with.
pub(crate) fn start(
    log: &dyn EventLog,
    group: &GroupId,
    kind: Option<Kind>,
    before: u64,
) -> Result<Start, JournalError> {
    let decoded = match log.load_latest_snapshot()? {
        Some(snapshot) => {
            let state: GroupState = serde_json::from_slice(&snapshot.state).map_err(|source| {
                JournalError::Snapshot {
                    seq: snapshot.seq,
                    source,
                }
            })?;
            Some((snapshot.seq, snapshot.at, state))
        }
        None => None,
    };
    let mut matcher = Matcher::new(group.clone());
    match decoded {
        Some((seq, at, state)) if seq < before => {
            matcher.seed(&state, seq, at);
            Ok(Start {
                state,
                seq,
                snapshot: Some(seq),
                matcher,
            })
        }
        other => {
            let kind = kind
                .or_else(|| other.map(|(_, _, state)| state.kind))
                .unwrap_or_default();
            Ok(Start {
                state: GroupState::new(group.clone(), kind),
                seq: 0,
                snapshot: None,
                matcher,
            })
        }
    }
}

/// Folds the logged inputs after `start.seq` up to and including `upto`,
/// dropping their effects.
pub(crate) fn warm(log: &dyn EventLog, start: &mut Start, upto: u64) -> Result<(), JournalError> {
    let mut from = Some(start.seq + 1);
    while let Some(at) = from.filter(|f| *f <= upto) {
        let page = page(log, at, upto)?;
        for logged in page.inputs {
            let matched = start
                .matcher
                .translate(logged.at, logged.input, &start.state);
            if let Some(input) = matched.input {
                let fx = step(&mut start.state, input, logged.at);
                start.matcher.note(logged.seq, logged.at, &fx, &start.state);
            }
            let _ = start.matcher.expire(logged.at);
            start.seq = logged.seq;
        }
        from = page.next;
    }
    Ok(())
}

/// The counter in an id the engine minted for `group`, `<group>-<tag><n>`.
pub(crate) fn minted_number(group: &GroupId, id: &str) -> Option<u64> {
    let rest = id.strip_prefix(group.as_str())?.strip_prefix('-')?;
    let digits = rest.trim_start_matches(|c: char| c.is_ascii_lowercase());
    if digits.is_empty() || digits.len() == rest.len() {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_give_their_counter() {
        let group = GroupId::new("g-1");
        assert_eq!(minted_number(&group, "g-1-r17"), Some(17));
        assert_eq!(minted_number(&group, "g-1-rc4"), Some(4));
        assert_eq!(minted_number(&group, "g-1-17"), None);
        assert_eq!(minted_number(&group, "g-1-r"), None);
        assert_eq!(minted_number(&group, "other-r3"), None);
        assert_eq!(minted_number(&group, "01JABCDEF"), None);
    }
}
