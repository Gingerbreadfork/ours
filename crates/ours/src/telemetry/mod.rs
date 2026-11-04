//! The triage signal (DESIGN.md section 18): opt-in, off by default and
//! text-free. One record per message the classifier scored, built by
//! re-folding the journal; this is the exact shape, one JSON object per
//! line on disk and a JSON array of them when posted:
//!
//! ```json
//! {
//!   "host": "3f9c2a7d1e5b8c40",
//!   "group": "a81d4c0e9b7f2361",
//!   "kind": "friends",
//!   "dial": 1,
//!   "at_hour_utc": 18,
//!   "score": {
//!     "addressed": 0.0,
//!     "open_decision": 0.0,
//!     "plan_forming": 0.0,
//!     "unanswered_question": 0.6,
//!     "fact": 0.0,
//!     "commitment": 0.0,
//!     "banter": 0.0,
//!     "kind": "question",
//!     "answerable_from_context": true,
//!     "source": { "type": "small_model", "model": "gemma-4-e2b" }
//!   },
//!   "rules": {
//!     "addressed": 0.0,
//!     "open_decision": 0.0,
//!     "plan_forming": 0.0,
//!     "unanswered_question": 0.6,
//!     "fact": 0.0,
//!     "commitment": 0.0,
//!     "banter": 0.0,
//!     "kind": "question",
//!     "answerable_from_context": false,
//!     "source": { "type": "rules" }
//!   },
//!   "acted": "raised_hand",
//!   "outcome": {
//!     "positive_reactions": 2,
//!     "shushed": false,
//!     "undone": false,
//!     "answered_by_human": false,
//!     "window_hours": 24
//!   }
//! }
//! ```
//!
//! - `host`: a random id made once per host, 16 hex characters.
//! - `group`: the first 16 hex characters of SHA-256 over a random salt
//!   followed by the group id. The salt stays in the group's store, so the
//!   tag cannot be traced back to the group or matched across hosts.
//! - `kind`: friends, household, team, project, club, community, committee
//!   or cohort. `dial` is the persona dial, 0 to 3.
//! - `at_hour_utc`: the hour the message was scored, 0 to 23.
//! - `score`: the six signals, banter, the message kind (question,
//!   instruction, statement, greeting or other), whether it was answerable
//!   from context, and where the score came from: `{"type": "rules"}` or
//!   `{"type": "small_model", "model": <the model's name>}`.
//! - `rules`: the rules pass's score for the same message, same fields.
//! - `acted`: the most the agent did about it: nothing, reacted,
//!   raised_hand, opened_poll, chimed or spoke.
//! - `outcome`, over the `window_hours` after the message: reactions from
//!   people on what the agent said or, after a 🙋, on the question, other
//!   than 👎 and 🤫; whether anyone 🤫'd it or said "shut up"; whether what
//!   it did was undone; and whether another person answered the question
//!   before the agent did.
//!
//! No message text, message ids, user ids, room ids or names are ever in a
//! record.

mod join;

use crate::config::TelemetrySection;
use crate::replay::fold;
use crate::runtime::journal::JournalError;
use ours_store::{EventLog, StoreDir, StoreError};
use ours_types::{GroupId, Instant, Kind, Score};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// How long after a message its outcome is counted.
pub const WINDOW_HOURS: u8 = 24;
/// The group store's key for this host's telemetry id.
pub const HOST_ID_KEY: &str = "telemetry_host_id";
/// The group store's key for the last seq whose records were flushed.
pub const SEQ_KEY: &str = "telemetry_seq";
/// The group store's key for the random salt behind the group tag. The salt
/// is never sent.
pub const GROUP_SALT_KEY: &str = "telemetry_group_salt";

const POST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("telemetry file: {0}")]
    Io(#[from] std::io::Error),
    #[error("telemetry record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("telemetry is off")]
    Disabled,
    /// The lines were written to `written`; only the post failed.
    #[error("posting telemetry failed: {source}")]
    Post {
        written: PathBuf,
        #[source]
        source: reqwest::Error,
    },
    #[error("telemetry task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acted {
    Nothing,
    Reacted,
    Spoke,
    Chimed,
    RaisedHand,
    OpenedPoll,
}

impl Acted {
    /// Which of two acts on one message the record keeps.
    fn rank(self) -> u8 {
        match self {
            Acted::Nothing => 0,
            Acted::Reacted => 1,
            Acted::RaisedHand => 2,
            Acted::OpenedPoll => 3,
            Acted::Chimed => 4,
            Acted::Spoke => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub positive_reactions: u32,
    pub shushed: bool,
    pub undone: bool,
    pub answered_by_human: bool,
    pub window_hours: u8,
}

/// One scored message, without a word of it. The module comment has the shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub host: String,
    pub group: String,
    pub kind: Kind,
    pub dial: u8,
    pub at_hour_utc: u8,
    pub score: Score,
    pub rules: Score,
    pub acted: Acted,
    pub outcome: Outcome,
}

/// What a flush did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flushed {
    /// The day's file; left alone when there were no records.
    pub written: PathBuf,
    /// The endpoint's status, when there is an endpoint and records to send.
    pub posted: Option<u16>,
}

/// The records for messages scored after `since_seq` whose window has
/// closed by now, and the last seq they cover.
pub fn collect(
    store: &StoreDir,
    group: &GroupId,
    since_seq: u64,
) -> Result<(Vec<Record>, u64), TelemetryError> {
    collect_at(
        store,
        group,
        since_seq,
        Instant::from(jiff::Timestamp::now()),
    )
}

/// `collect` with the clock given, so windows close where a caller says.
pub fn collect_at(
    store: &StoreDir,
    group: &GroupId,
    since_seq: u64,
    now: Instant,
) -> Result<(Vec<Record>, u64), TelemetryError> {
    if !store.exists(group)? {
        return Ok((Vec::new(), since_seq));
    }
    let host = host_id(store, group)?;
    let log: Arc<dyn EventLog> = store.open_group(group)?;
    let salt = group_salt(log.as_ref())?;
    let mut start = fold::start(log.as_ref(), group, None, since_seq + 1)?;
    let mut join = join::Join::new(host, group_tag(&salt, group), since_seq);
    let mut last = start.seq.max(since_seq);
    let mut next = Some(start.seq + 1);
    while let Some(from) = next {
        let page = fold::page(log.as_ref(), from, u64::MAX)?;
        next = page.next;
        if let Some(seq) = page.last {
            last = last.max(seq);
        }
        for logged in page.inputs {
            // In the fold's own ids, so a journal from an older engine joins too.
            let matched = start
                .matcher
                .translate(logged.at, logged.input, &start.state);
            if let Some(input) = matched.input {
                let state = &mut start.state;
                let before = join.before(state, logged.seq, logged.at, &input);
                let effects = ours_core::step(state, input.clone(), logged.at);
                start.matcher.note(logged.seq, logged.at, &effects, state);
                join.after(state, &input, &effects, before, logged.at);
            }
            let _ = start.matcher.expire(logged.at);
        }
    }
    Ok(join.finish(now, last))
}

/// The first `n` records as pretty JSON, for `ours telemetry --show`.
pub fn sample(records: &[Record], n: usize) -> String {
    let shown = &records[..n.min(records.len())];
    serde_json::to_string_pretty(shown).unwrap_or_else(|_| "[]".to_owned())
}

/// Appends the records to `data/telemetry/<date>.jsonl` and posts them to
/// the endpoint when one is set, once. Does nothing while telemetry is off.
pub async fn flush(
    config: &TelemetrySection,
    data_dir: &Path,
    records: &[Record],
) -> Result<Flushed, TelemetryError> {
    if !config.enabled {
        return Err(TelemetryError::Disabled);
    }
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let written = data_dir.join("telemetry").join(format!("{today}.jsonl"));
    if records.is_empty() {
        return Ok(Flushed {
            written,
            posted: None,
        });
    }
    let mut lines = String::new();
    for record in records {
        lines.push_str(&serde_json::to_string(record)?);
        lines.push('\n');
    }
    {
        let path = written.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(lines.as_bytes())
        })
        .await??;
    }
    let Some(endpoint) = config.endpoint.as_deref().filter(|e| !e.trim().is_empty()) else {
        return Ok(Flushed {
            written,
            posted: None,
        });
    };
    let sent = async {
        let client = reqwest::Client::builder().timeout(POST_TIMEOUT).build()?;
        client.post(endpoint).json(records).send().await
    }
    .await;
    match sent {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                tracing::warn!(%status, endpoint, "the telemetry endpoint refused the batch");
            }
            Ok(Flushed {
                written,
                posted: Some(status.as_u16()),
            })
        }
        Err(source) => Err(TelemetryError::Post { written, source }),
    }
}

/// Collects and flushes every group once, remembering in each group's
/// store how far it got. Does nothing while telemetry is off.
pub async fn job_once(
    store: &Arc<StoreDir>,
    config: &TelemetrySection,
    data_dir: &Path,
    groups: &[GroupId],
) -> Vec<Result<Flushed, TelemetryError>> {
    if !config.enabled {
        return Vec::new();
    }
    let mut results = Vec::new();
    for group in groups {
        results.push(flush_group(store, config, data_dir, group).await);
    }
    results
}

/// Runs `job_once` every `every`, forever. Returns at once while telemetry
/// is off, having read and written nothing.
pub async fn job(
    store: Arc<StoreDir>,
    config: TelemetrySection,
    data_dir: PathBuf,
    groups: Vec<GroupId>,
    every: Duration,
) {
    if !config.enabled {
        tracing::debug!("telemetry is off");
        return;
    }
    let mut ticks = tokio::time::interval(every.max(Duration::from_millis(1)));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        for (group, result) in groups
            .iter()
            .zip(job_once(&store, &config, &data_dir, &groups).await)
        {
            match result {
                Ok(flushed) => {
                    tracing::debug!(%group, path = %flushed.written.display(), posted = ?flushed.posted, "telemetry flushed")
                }
                Err(error) => tracing::warn!(%group, %error, "telemetry flush failed"),
            }
        }
    }
}

async fn flush_group(
    store: &Arc<StoreDir>,
    config: &TelemetrySection,
    data_dir: &Path,
    group: &GroupId,
) -> Result<Flushed, TelemetryError> {
    let (records, covered) = {
        let store = Arc::clone(store);
        let group = group.clone();
        tokio::task::spawn_blocking(move || -> Result<_, TelemetryError> {
            if !store.exists(&group)? {
                return Ok((Vec::new(), 0));
            }
            let since = read_seq(store.open_group(&group)?.as_ref())?;
            collect(&store, &group, since)
        })
        .await??
    };
    let flushed = flush(config, data_dir, &records).await;
    if matches!(flushed, Ok(_) | Err(TelemetryError::Post { .. })) && covered > 0 {
        let store = Arc::clone(store);
        let group = group.clone();
        tokio::task::spawn_blocking(move || {
            store
                .open_group(&group)?
                .set(SEQ_KEY, covered.to_string().as_bytes())
        })
        .await??;
    }
    flushed
}

fn read_seq(log: &dyn EventLog) -> Result<u64, StoreError> {
    Ok(log
        .get(SEQ_KEY)?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0))
}

/// This host's telemetry id: the one any group's store already holds,
/// else a new one, kept in `group`'s store.
fn host_id(store: &StoreDir, group: &GroupId) -> Result<String, StoreError> {
    let own = store.open_group(group)?;
    if let Some(id) = valid_host_id(own.get(HOST_ID_KEY)?) {
        return Ok(id);
    }
    let mut found = None;
    for other in store.groups()? {
        if &other == group {
            continue;
        }
        if let Some(id) = valid_host_id(store.open_group(&other)?.get(HOST_ID_KEY)?) {
            found = Some(id);
            break;
        }
    }
    // The low 64 bits of a ULID are random.
    let id = found.unwrap_or_else(|| format!("{:016x}", ulid::Ulid::generate().0 as u64));
    own.set(HOST_ID_KEY, id.as_bytes())?;
    Ok(id)
}

fn valid_host_id(bytes: Option<Vec<u8>>) -> Option<String> {
    let id = String::from_utf8(bytes?).ok()?;
    (id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

/// The group's salt, made the first time it is needed: 160 random bits as hex.
fn group_salt(log: &dyn EventLog) -> Result<String, StoreError> {
    let kept = log
        .get(GROUP_SALT_KEY)?
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    if let Some(salt) = kept {
        return Ok(salt);
    }
    // A ULID's low 80 bits are random.
    let salt = format!(
        "{:020x}{:020x}",
        ulid::Ulid::generate().random(),
        ulid::Ulid::generate().random()
    );
    log.set(GROUP_SALT_KEY, salt.as_bytes())?;
    Ok(salt)
}

fn group_tag(salt: &str, group: &GroupId) -> String {
    let digest = Sha256::new()
        .chain_update(salt.as_bytes())
        .chain_update(group.as_str().as_bytes())
        .finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A record with every field filled, for the compliance page and the docs.
pub fn example() -> Record {
    use ours_types::{MessageKind, ScoreSource};
    let score = |source: ScoreSource| Score {
        addressed: 0.0,
        open_decision: 0.0,
        plan_forming: 0.0,
        unanswered_question: 0.6,
        fact: 0.0,
        commitment: 0.0,
        banter: 0.0,
        kind: MessageKind::Question,
        answerable_from_context: true,
        source,
    };
    Record {
        host: "3f9c2a7d1e5b8c40".to_owned(),
        group: "a81d4c0e9b7f2361".to_owned(),
        kind: Kind::Friends,
        dial: 1,
        at_hour_utc: 18,
        score: score(ScoreSource::SmallModel {
            model: "gemma-4-26b-a4b".to_owned(),
        }),
        rules: score(ScoreSource::Rules),
        acted: Acted::RaisedHand,
        outcome: Outcome {
            positive_reactions: 2,
            shushed: false,
            undone: false,
            answered_by_human: false,
            window_hours: WINDOW_HOURS,
        },
    }
}
