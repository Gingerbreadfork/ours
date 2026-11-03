//! `ours replay`: reruns a group's journal through the engine without
//! sending anything and diffs what it would do (DESIGN.md section 18,
//! "Replay"). The logged run re-folds the journal, its answers matched to
//! the fold's own requests so a journal from an older engine still lines
//! up; the replayed run takes its model and small-stage answers from the
//! oracle.

mod describe;
pub(crate) mod fold;
mod matcher;
mod run;

use crate::runtime::journal::JournalError;
use crate::runtime::model::ChatSettings;
use ours_model::{Classifier, Extractor, LargeModel, TimeResolver};
use ours_store::{EventLog, StoreDir, StoreError};
use ours_types::{GroupId, Instant, Kind};
use serde::Serialize;
use std::fmt::Write as _;
use std::sync::Arc;
use thiserror::Error;

/// Divergence blocks a rendered report shows before counting the rest.
pub const DIVERGENCES_SHOWN: usize = 200;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("group {0} has no store in this data directory")]
    NoGroup(GroupId),
    #[error("from seq {from} is after to seq {to}")]
    Range { from: u64, to: u64 },
    #[error("replay task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// Who answers the replayed run's requests.
pub enum Oracle {
    /// The journal's own answers, so the replay is a pure re-fold. A content
    /// divergence is a determinism bug; the other kinds show where the engine
    /// that wrote the journal asked for other things.
    Logged,
    /// These stages, asked the same requests. Timers and outside results
    /// still come from the journal.
    Live {
        model: Arc<dyn LargeModel>,
        classifier: Arc<dyn Classifier>,
        extractor: Arc<dyn Extractor>,
        resolver: Arc<dyn TimeResolver>,
    },
}

impl std::fmt::Debug for Oracle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Oracle::Logged => f.write_str("Logged"),
            Oracle::Live {
                model,
                classifier,
                extractor,
                resolver,
            } => f
                .debug_struct("Live")
                .field("model", &model.name())
                .field("classifier", &classifier.name())
                .field("extractor", &extractor.name())
                .field("resolver", &resolver.name())
                .finish(),
        }
    }
}

#[derive(Debug)]
pub struct ReplayOptions {
    pub group: GroupId,
    /// The first seq to compare; earlier inputs only rebuild the state.
    pub from_seq: Option<u64>,
    /// The last seq to compare, inclusive.
    pub to_seq: Option<u64>,
    pub oracle: Oracle,
    pub chat: ChatSettings,
    pub strict_schema: bool,
    /// The kind the group was created with, for a replay from the start.
    /// None takes the newest snapshot's kind.
    pub kind: Option<Kind>,
}

impl ReplayOptions {
    /// The whole journal, re-folded with its own answers.
    pub fn logged(group: GroupId) -> ReplayOptions {
        ReplayOptions {
            group,
            from_seq: None,
            to_seq: None,
            oracle: Oracle::Logged,
            chat: ChatSettings::default(),
            strict_schema: false,
            kind: None,
        }
    }
}

/// What one run did that people would have seen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    /// Room messages, ballots included.
    pub messages: u64,
    pub reactions: u64,
    pub dms: u64,
    /// Room messages from steps that spent the interjection budget.
    pub unprompted_messages: u64,
    pub chime_ins: u64,
    /// Native polls and reaction ballots.
    pub polls: u64,
    pub mails: u64,
    pub notices: u64,
    /// Requests to the large model.
    pub asks: u64,
}

/// What a divergence is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceKind {
    /// The runs sent or noted different things after this input.
    Content,
    /// The log answers something the replay never asked for.
    UnmatchedAnswer,
    /// The replay asked for something the log never answered.
    Unanswered,
}

/// One input after which the two runs did different things, or where the
/// log and the replay disagree on what was asked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Divergence {
    pub seq: u64,
    pub at: Instant,
    pub kind: DivergenceKind,
    pub input_label: String,
    /// What came in, in one line.
    pub summary: String,
    pub logged: Vec<String>,
    pub replayed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlowInput {
    pub seq: u64,
    pub input_label: String,
    pub logged_us: u64,
    /// Live calls included.
    pub replayed_us: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Timing {
    pub logged_ms: u64,
    /// Live calls included.
    pub replayed_ms: u64,
    /// Time spent waiting on live stages.
    pub live_ms: u64,
    /// The inputs that took longest on the replayed side, slowest first.
    pub slowest: Vec<SlowInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayReport {
    pub group: GroupId,
    /// "logged" or "live".
    pub oracle: String,
    /// The snapshot the state was rebuilt from, when there was one.
    pub snapshot: Option<u64>,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub inputs: u64,
    pub logged: Tally,
    pub replayed: Tally,
    pub divergences: Vec<Divergence>,
    /// Live large-model calls.
    pub model_calls: u32,
    /// Live classifier, extractor and time resolver calls.
    pub stage_calls: u32,
    pub elapsed_ms: u64,
    pub timing: Timing,
}

/// Replays one group's journal. Reads the store and never writes to it.
pub async fn replay(store: &StoreDir, options: ReplayOptions) -> Result<ReplayReport, ReplayError> {
    let clock = std::time::Instant::now();
    let ReplayOptions {
        group,
        from_seq,
        to_seq,
        oracle,
        chat,
        strict_schema,
        kind,
    } = options;
    let from = from_seq.unwrap_or(1).max(1);
    let to = to_seq.unwrap_or(u64::MAX);
    if from > to {
        return Err(ReplayError::Range { from, to });
    }
    if !store.exists(&group)? {
        return Err(ReplayError::NoGroup(group));
    }
    let log: Arc<dyn EventLog> = store.open_group(&group)?;
    let start = {
        let log = Arc::clone(&log);
        let group = group.clone();
        tokio::task::spawn_blocking(move || -> Result<fold::Start, JournalError> {
            let mut start = fold::start(log.as_ref(), &group, kind, from)?;
            fold::warm(log.as_ref(), &mut start, from - 1)?;
            Ok(start)
        })
        .await??
    };
    let (oracle_name, live) = match oracle {
        Oracle::Logged => ("logged", None),
        Oracle::Live {
            model,
            classifier,
            extractor,
            resolver,
        } => (
            "live",
            Some(run::Live {
                model,
                classifier,
                extractor,
                resolver,
            }),
        ),
    };
    tracing::info!(%group, from, snapshot = ?start.snapshot, oracle = oracle_name, "replaying");
    let snapshot = start.snapshot;
    let mut run = run::Run::new(group.clone(), start, from, live, chat, strict_schema);
    let (mut first_seq, mut last_seq) = (None, None);
    let mut next = Some(from);
    while let Some(at) = next {
        let page = {
            let log = Arc::clone(&log);
            tokio::task::spawn_blocking(move || fold::page(log.as_ref(), at, to)).await??
        };
        next = page.next;
        for logged in page.inputs {
            first_seq.get_or_insert(logged.seq);
            last_seq = Some(logged.seq);
            run.input(logged.seq, logged.at, logged.input).await;
        }
    }
    let report = ReplayReport {
        group,
        oracle: oracle_name.to_owned(),
        snapshot,
        first_seq,
        last_seq,
        inputs: run.inputs,
        logged: run.logged,
        replayed: run.replayed,
        timing: run.timing(),
        divergences: run.divergences(),
        model_calls: run.model_calls,
        stage_calls: run.stage_calls,
        elapsed_ms: u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    tracing::info!(
        group = %report.group,
        inputs = report.inputs,
        divergences = report.divergences.len(),
        model_calls = report.model_calls,
        elapsed_ms = report.elapsed_ms,
        "replay done"
    );
    Ok(report)
}

/// The report as Markdown: a summary table, then the divergences.
pub fn render_markdown(report: &ReplayReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Replay of {}\n", report.group);
    let range = match (report.first_seq, report.last_seq) {
        (Some(first), Some(last)) => format!("seq {first} to {last}"),
        _ => "nothing in range".to_owned(),
    };
    let from = match report.snapshot {
        Some(seq) => format!("the snapshot at seq {seq}"),
        None => "the start of the log".to_owned(),
    };
    let _ = writeln!(
        out,
        "{} inputs, {range}, rebuilt from {from}. Answers from the {} oracle; {} model and {} small-stage calls. Took {} ms.\n",
        report.inputs, report.oracle, report.model_calls, report.stage_calls, report.elapsed_ms
    );
    let _ = writeln!(out, "| | logged | replayed |\n|---|---:|---:|");
    let (l, r) = (&report.logged, &report.replayed);
    for (name, a, b) in [
        ("Messages", l.messages, r.messages),
        (
            "Unprompted messages",
            l.unprompted_messages,
            r.unprompted_messages,
        ),
        ("Chime-ins", l.chime_ins, r.chime_ins),
        ("Reactions", l.reactions, r.reactions),
        ("DMs", l.dms, r.dms),
        ("Polls", l.polls, r.polls),
        ("Mails", l.mails, r.mails),
        ("Notices", l.notices, r.notices),
        ("Model requests", l.asks, r.asks),
    ] {
        let _ = writeln!(out, "| {name} | {a} | {b} |");
    }
    let _ = writeln!(out, "\n## Divergences\n");
    if report.divergences.is_empty() {
        let _ = writeln!(out, "None: both runs did the same after every input.");
        return out;
    }
    let count = |kind| report.divergences.iter().filter(|d| d.kind == kind).count();
    let _ = writeln!(
        out,
        "{} divergences: {} inputs where the runs differ, {} answers the replay never asked for, {} requests the log never answered.\n",
        report.divergences.len(),
        count(DivergenceKind::Content),
        count(DivergenceKind::UnmatchedAnswer),
        count(DivergenceKind::Unanswered),
    );
    for d in report.divergences.iter().take(DIVERGENCES_SHOWN) {
        let what = match d.kind {
            DivergenceKind::Content => "the runs differ",
            DivergenceKind::UnmatchedAnswer => "an answer the replay never asked for",
            DivergenceKind::Unanswered => "a request the log never answered",
        };
        let _ = writeln!(out, "### Seq {} at {}: {what}\n", d.seq, d.at);
        let _ = writeln!(
            out,
            "What came in ({}): {}\n",
            d.input_label,
            one_line(&d.summary)
        );
        for (side, lines) in [("Logged", &d.logged), ("Replayed", &d.replayed)] {
            let _ = writeln!(out, "{side}:\n");
            if lines.is_empty() {
                let _ = writeln!(out, "- nothing");
            }
            for line in lines {
                let _ = writeln!(out, "- {}", one_line(line));
            }
            let _ = writeln!(out);
        }
    }
    let rest = report.divergences.len().saturating_sub(DIVERGENCES_SHOWN);
    if rest > 0 {
        let _ = writeln!(out, "And {rest} more.");
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
