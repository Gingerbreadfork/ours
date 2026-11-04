//! Replay against a journal written by the real engine: a Discord-like
//! dinner group with addressed messages, a 🙋 on an unanswered question,
//! a chime-in and a 🤫 (DESIGN.md section 18, "Replay").
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use ours::replay::{
    DivergenceKind, Oracle, ReplayError, ReplayOptions, Tally, render_markdown, replay,
};
use ours::runtime::journal::Journal;
use ours_core::sim::{ClassifierScript, ME, ModelScript, Sim, TimeScript, banter, instant, plan};
use ours_core::{Command, Effect, GroupState, Input, Profile, SettingChange, step};
use ours_model::{
    ChatRequest, ChatResponse, Classifier, LargeModel, ModelError, NoExtractor, NoTimeResolver,
    TimeResolver, Usage,
};
use ours_store::{EventLog, StoreDir};
use ours_types::{
    ActionId, Capabilities, ClassifyRequest, GroupId, InboundEvent, Kind, OutboundAction, Platform,
    PlatformRole, PlatformUserId, RequestId, Score, ScoreSource, TimeMeaning, TimeRequest,
    TimeResolution, TimerId,
};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

const GROUP: &str = "dinner-club";
const WINDOW: i64 = 20 * 60;
const HOUR: i64 = 60 * 60;
const DIFFERENT: &str = "Something else entirely.";

/// The dinner group's evening, as the engine lived it.
fn evening() -> Sim {
    let mut sim = dinner();
    sim.model_says("Thursday works for four of you.");
    sim.say("dinner", "jo", "Q, what do you reckon?");
    sim.advance(60);
    sim.say("dinner", "priya", "Hobson's, half seven, thursday");
    sim.advance(60);

    sim.classify(ClassifierScript::Answerable);
    let question = sim.say("dinner", "sam", "what time was it again?").id;
    sim.advance(WINDOW);
    sim.model_says("Half seven at Hobson's.");
    sim.react("dinner", &question, "jo", "👍");
    let answer = sim.last_sent_message().unwrap();
    sim.react("dinner", &answer, "alex", "❤️");

    sim.advance(HOUR);
    sim.feed(Input::Command(Command::ChangeSetting {
        change: SettingChange::DialTo { dial: 2 },
    }));
    sim.script(ModelScript::Reply(plan("Called it.")));
    sim.classify(ClassifierScript::Score(banter(0.9)));
    sim.say("dinner", "sam", "WE GOT THE HOUSE");
    let chime = sim.last_sent_message().unwrap();
    sim.react("dinner", &chime, "priya", "🤫");

    sim.advance(60);
    sim.model_says("Bring a jacket.");
    sim.say("dinner", "priya", "Q, what should I bring?");
    sim.advance(60);
    sim.say("dinner", "alex", "lol");
    sim
}

/// The dinner group's room on Discord, ready, with nothing said yet.
fn dinner() -> Sim {
    let mut sim = Sim::with_state(GroupState::new(GroupId::new(GROUP), Kind::Friends));
    let profile = Profile {
        observe_seconds: 0,
        ..Profile::for_kind(Kind::Friends)
    };
    sim.feed(Input::Command(Command::SetProfile { profile }));
    sim.discord_room(
        "dinner",
        "dinner",
        "guild-1",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
            ("alex", "Alex", PlatformRole::Admin),
        ],
    );
    sim.feed(Input::Inbound(InboundEvent::Ready {
        platform: Platform::Discord,
        me: PlatformUserId::new(ME),
        me_name: "Q".to_owned(),
        capabilities: Capabilities {
            reactions: true,
            threads: true,
            dms: true,
            edit: true,
            delete: true,
            history: true,
            ..Capabilities::default()
        },
    }));
    sim
}

fn write_journal(dir: &std::path::Path, sim: &Sim) -> StoreDir {
    let store = StoreDir::open(dir).unwrap();
    let log: Arc<dyn EventLog> = store.open_group(&GroupId::new(GROUP)).unwrap();
    let journal = Journal::<Input, GroupState>::new(log);
    for (at, input) in &sim.log {
        journal.append(input, *at).unwrap();
    }
    store
}

/// The evening as an engine that minted one id more ahead of each answer
/// would have logged it: every id the log answers is off by one.
fn write_offset_journal(dir: &std::path::Path, sim: &Sim) -> StoreDir {
    let bump = |id: &str| {
        let rest = id.strip_prefix(&format!("{GROUP}-")).unwrap();
        let digits = rest.trim_start_matches(|c: char| c.is_ascii_lowercase());
        let tag = &rest[..rest.len() - digits.len()];
        format!("{GROUP}-{tag}{}", digits.parse::<u64>().unwrap() + 1)
    };
    let store = StoreDir::open(dir).unwrap();
    let log: Arc<dyn EventLog> = store.open_group(&GroupId::new(GROUP)).unwrap();
    let journal = Journal::<Input, GroupState>::new(log);
    for (at, input) in &sim.log {
        let shifted = match input.clone() {
            Input::ModelReply { request, reply } => Input::ModelReply {
                request: RequestId::new(bump(request.as_str())),
                reply,
            },
            Input::TimerFired { id } => Input::TimerFired {
                id: TimerId::new(bump(id.as_str())),
            },
            Input::Inbound(InboundEvent::ActionResult {
                id,
                ok,
                message,
                error,
            }) => Input::Inbound(InboundEvent::ActionResult {
                id: ActionId::new(bump(id.as_str())),
                ok,
                message,
                error,
            }),
            other => other,
        };
        journal.append(&shifted, *at).unwrap();
    }
    store
}

/// The seq of every logged input that `matches`, the log starting at 1.
fn seqs(sim: &Sim, matches: impl Fn(&Input) -> bool) -> Vec<u64> {
    sim.log
        .iter()
        .enumerate()
        .filter(|(_, (_, input))| matches(input))
        .map(|(i, _)| i as u64 + 1)
        .collect()
}

fn message_seq(sim: &Sim, text: &str) -> u64 {
    seqs(
        sim,
        |input| matches!(input, Input::Inbound(InboundEvent::Message(m)) if m.text == text),
    )[0]
}

/// What the sim itself did, counted the way the report counts.
fn sim_tally(sim: &Sim) -> Tally {
    let mut tally = Tally::default();
    for effect in &sim.effects {
        match effect {
            Effect::Send(out) => match out.action {
                OutboundAction::SendMessage { .. } => tally.messages += 1,
                OutboundAction::React { .. } => tally.reactions += 1,
                OutboundAction::SendDm { .. } => tally.dms += 1,
                OutboundAction::CreatePoll { .. } => tally.polls += 1,
                _ => {}
            },
            Effect::AskModel(_) => tally.asks += 1,
            Effect::Outside { .. } => tally.mails += 1,
            Effect::Notice(_) => tally.notices += 1,
            _ => {}
        }
    }
    tally
}

struct DifferentModel {
    calls: AtomicU32,
}

#[async_trait]
impl LargeModel for DifferentModel {
    async fn complete(&self, _: &GroupId, _: ChatRequest) -> Result<ChatResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ChatResponse {
            json: json!({ "message": DIFFERENT, "actions": [] }),
            usage: Usage::default(),
            model: "different".to_owned(),
            latency_ms: 1,
        })
    }

    fn name(&self) -> &str {
        "different"
    }
}

/// Scores as the evening's script did, so only the large model differs.
struct ScriptedClassifier;

#[async_trait]
impl Classifier for ScriptedClassifier {
    async fn classify(&self, _: &GroupId, request: ClassifyRequest) -> Result<Score, ModelError> {
        Ok(match request.text.as_str() {
            "what time was it again?" => Score {
                answerable_from_context: true,
                source: ScoreSource::SmallModel {
                    model: "sim".to_owned(),
                },
                ..request.rules
            },
            "WE GOT THE HOUSE" => banter(0.9),
            _ => request.rules,
        })
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

fn live(model: Arc<DifferentModel>) -> Oracle {
    Oracle::Live {
        model,
        classifier: Arc::new(ScriptedClassifier),
        extractor: Arc::new(NoExtractor),
        resolver: Arc::new(NoTimeResolver),
    }
}

#[tokio::test]
async fn a_logged_replay_matches_the_log_and_counts_what_was_said() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);
    let before = store.open_group(&group).unwrap().latest().unwrap();

    let raised = sim.sent.iter().any(
        |s| matches!(&s.outbound.action, OutboundAction::React { emoji, .. } if emoji == "🙋"),
    );
    assert!(raised, "the evening has a raised hand");
    let shushed = sim
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Notice(n) if n.starts_with("Shushed by Priya")));
    assert!(shushed, "and a shush");

    let report = replay(&store, ReplayOptions::logged(group.clone()))
        .await
        .unwrap();

    assert!(report.divergences.is_empty(), "{:#?}", report.divergences);
    assert_eq!(report.inputs, sim.log.len() as u64);
    assert_eq!(report.first_seq, Some(1));
    assert_eq!(report.last_seq, Some(sim.log.len() as u64));
    assert_eq!(report.snapshot, None);
    assert_eq!(report.model_calls, 0);

    let mut expected = sim_tally(&sim);
    // The 🙋 answer and the chime line spent the budget; the addressed replies did not.
    expected.unprompted_messages = 2;
    expected.chime_ins = 1;
    assert_eq!(report.logged, expected);
    assert_eq!(report.replayed, expected);
    assert_eq!(report.logged.messages, 4);
    assert_eq!(report.logged.asks, 4);

    let markdown = render_markdown(&report);
    assert!(
        markdown.contains("| Unprompted messages | 2 | 2 |"),
        "{markdown}"
    );
    assert!(markdown.contains("None: both runs did the same"));

    // Nothing was written.
    let log = store.open_group(&group).unwrap();
    assert_eq!(log.latest().unwrap(), before);
    assert!(log.load_latest_snapshot().unwrap().is_none());
}

#[tokio::test]
async fn a_live_replay_diverges_where_the_model_answers_differently() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let model = Arc::new(DifferentModel {
        calls: AtomicU32::new(0),
    });
    let options = ReplayOptions {
        oracle: live(Arc::clone(&model)),
        ..ReplayOptions::logged(GroupId::new(GROUP))
    };

    let report = replay(&store, options).await.unwrap();

    let replies = seqs(&sim, |input| matches!(input, Input::ModelReply { .. }));
    assert_eq!(replies.len(), 4);
    let diverged: Vec<u64> = report.divergences.iter().map(|d| d.seq).collect();
    assert_eq!(diverged, replies, "{:#?}", report.divergences);
    assert_eq!(report.model_calls, 4);
    assert_eq!(model.calls.load(Ordering::SeqCst), 4);
    assert!(report.stage_calls > 0);

    let first = &report.divergences[0];
    assert_eq!(first.input_label, "model_reply");
    assert_eq!(
        first.logged[0],
        "said in #dinner replying to m1: Thursday works for four of you."
    );
    assert_eq!(
        first.replayed[0],
        format!("said in #dinner replying to m1: {DIFFERENT}")
    );
    assert_eq!(first.logged[1..], first.replayed[1..]);

    // The same things happened, in other words.
    assert_eq!(report.logged.messages, report.replayed.messages);
    assert_eq!(report.logged.chime_ins, 1);
    assert_eq!(report.replayed.chime_ins, 1);
    assert_eq!(report.replayed.unprompted_messages, 2);

    let markdown = render_markdown(&report);
    assert!(
        markdown.contains("4 divergences: 4 inputs where the runs differ, 0 answers"),
        "{markdown}"
    );
    for seq in &replies {
        assert!(
            markdown.contains(&format!("### Seq {seq} at ")),
            "{markdown}"
        );
    }
    assert!(markdown.contains(&format!("- said in #dinner replying to m1: {DIFFERENT}")));
}

#[tokio::test]
async fn from_and_to_seq_bound_the_run() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);

    let asked = message_seq(&sim, "Q, what should I bring?");
    let reply = seqs(&sim, |input| matches!(input, Input::ModelReply { .. }))
        .into_iter()
        .find(|seq| *seq > asked)
        .unwrap();

    let bounded = ReplayOptions {
        from_seq: Some(asked),
        to_seq: Some(reply),
        ..ReplayOptions::logged(group.clone())
    };
    let report = replay(&store, bounded).await.unwrap();
    assert!(report.divergences.is_empty());
    assert_eq!(report.inputs, reply - asked + 1);
    assert_eq!(
        (report.first_seq, report.last_seq),
        (Some(asked), Some(reply))
    );
    assert_eq!(report.logged.asks, 1);
    assert_eq!(report.logged.messages, 1);

    let model = Arc::new(DifferentModel {
        calls: AtomicU32::new(0),
    });
    let bounded_live = ReplayOptions {
        from_seq: Some(asked),
        to_seq: Some(reply),
        oracle: live(Arc::clone(&model)),
        ..ReplayOptions::logged(group.clone())
    };
    let report = replay(&store, bounded_live).await.unwrap();
    let diverged: Vec<u64> = report.divergences.iter().map(|d| d.seq).collect();
    assert_eq!(diverged, vec![reply]);
    assert_eq!(report.model_calls, 1);

    let backwards = ReplayOptions {
        from_seq: Some(reply),
        to_seq: Some(asked),
        ..ReplayOptions::logged(group)
    };
    assert!(matches!(
        replay(&store, backwards).await,
        Err(ReplayError::Range { .. })
    ));
}

#[tokio::test]
async fn a_replay_starts_from_the_snapshot_before_its_window() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);

    let at = message_seq(&sim, "WE GOT THE HOUSE") - 1;
    let mut state = GroupState::new(group.clone(), Kind::Friends);
    for (now, input) in sim.log.iter().take(at as usize) {
        let _ = step(&mut state, input.clone(), *now);
    }
    let log: Arc<dyn EventLog> = store.open_group(&group).unwrap();
    Journal::<Input, GroupState>::new(log)
        .snapshot(at, &state)
        .unwrap();

    let after = ReplayOptions {
        from_seq: Some(at + 1),
        ..ReplayOptions::logged(group.clone())
    };
    let report = replay(&store, after).await.unwrap();
    assert_eq!(report.snapshot, Some(at));
    assert!(report.divergences.is_empty(), "{:#?}", report.divergences);
    assert_eq!(report.logged.chime_ins, 1);
    assert_eq!(report.inputs, sim.log.len() as u64 - at);

    // A window that starts at or before the snapshot rebuilds from nothing.
    let whole = ReplayOptions {
        from_seq: Some(at),
        ..ReplayOptions::logged(group)
    };
    let report = replay(&store, whole).await.unwrap();
    assert_eq!(report.snapshot, None);
    assert!(report.divergences.is_empty());
}

#[tokio::test]
async fn an_answer_to_a_request_the_engine_never_made_is_reported_by_seq() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);
    let log: Arc<dyn EventLog> = store.open_group(&group).unwrap();
    let stray = Journal::<Input, GroupState>::new(log)
        .append(
            &Input::ModelReply {
                request: RequestId::new(format!("{GROUP}-r999")),
                reply: Ok(plan("From nowhere.")),
            },
            sim.now,
        )
        .unwrap();

    let report = replay(&store, ReplayOptions::logged(group)).await.unwrap();
    assert_eq!(report.divergences.len(), 1, "{:#?}", report.divergences);
    let bug = &report.divergences[0];
    assert_eq!(bug.seq, stray);
    assert_eq!(bug.kind, DivergenceKind::UnmatchedAnswer);
    assert_eq!(
        bug.logged,
        vec![format!("the log answers model request {GROUP}-r999")]
    );
    assert_eq!(bug.replayed, vec!["this replay never asked for it"]);
}

#[tokio::test]
async fn a_group_without_a_store_is_an_error_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoreDir::open(dir.path()).unwrap();
    let missing = GroupId::new("nobody");
    let result = replay(&store, ReplayOptions::logged(missing.clone())).await;
    assert!(matches!(result, Err(ReplayError::NoGroup(_))));
    assert!(!store.exists(&missing).unwrap());
}

#[tokio::test]
async fn ids_an_older_engine_minted_differently_still_pair_in_order() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_offset_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);
    let shifted = seqs(&sim, |input| {
        matches!(
            input,
            Input::ModelReply { .. }
                | Input::TimerFired { .. }
                | Input::Inbound(InboundEvent::ActionResult { .. })
        )
    });
    assert!(shifted.len() > 10, "the evening has answers to shift");

    let report = replay(&store, ReplayOptions::logged(group.clone()))
        .await
        .unwrap();
    assert!(report.divergences.is_empty(), "{:#?}", report.divergences);
    let mut expected = sim_tally(&sim);
    expected.unprompted_messages = 2;
    expected.chime_ins = 1;
    assert_eq!(report.logged, expected);
    assert_eq!(report.replayed, expected);

    // Live, only the model's own words differ.
    let model = Arc::new(DifferentModel {
        calls: AtomicU32::new(0),
    });
    let options = ReplayOptions {
        oracle: live(Arc::clone(&model)),
        ..ReplayOptions::logged(group)
    };
    let report = replay(&store, options).await.unwrap();
    let diverged: Vec<u64> = report.divergences.iter().map(|d| d.seq).collect();
    let replies = seqs(&sim, |input| matches!(input, Input::ModelReply { .. }));
    assert_eq!(diverged, replies, "{:#?}", report.divergences);
    assert!(
        report
            .divergences
            .iter()
            .all(|d| d.kind == DivergenceKind::Content)
    );
    assert_eq!(report.model_calls, 4);
}

#[tokio::test]
async fn a_request_the_log_never_answered_is_reported_where_it_was_made() {
    let mut sim = evening();
    let asked = message_seq(&sim, "Hobson's, half seven, thursday");
    let message = match &sim.log[asked as usize - 1].1 {
        Input::Inbound(InboundEvent::Message(m)) => m.id.clone(),
        other => panic!("not a message: {other:?}"),
    };
    sim.log
        .retain(|(_, input)| !matches!(input, Input::Scored { message: m, .. } if *m == message));
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);

    let report = replay(&store, ReplayOptions::logged(GroupId::new(GROUP)))
        .await
        .unwrap();
    assert_eq!(report.divergences.len(), 1, "{:#?}", report.divergences);
    let gap = &report.divergences[0];
    assert_eq!(gap.kind, DivergenceKind::Unanswered);
    assert_eq!(gap.seq, asked);
    assert_eq!(gap.input_label, "classify");
    assert!(gap.logged.is_empty());
    assert_eq!(
        gap.replayed,
        vec![format!(
            "asked to classify {message} in #dinner; the log never answered"
        )]
    );
    assert!(render_markdown(&report).contains("1 requests the log never answered"));
}

/// What each time phrase means, for the log and the live resolver alike.
fn meaning_of(phrase: &str) -> (TimeMeaning, Option<String>) {
    if phrase.to_lowercase().contains("holidays") {
        (
            TimeMeaning::Span {
                start: instant("2027-01-04T00:00:00Z"),
                end: instant("2027-01-11T00:00:00Z"),
            },
            Some("the week of 4 January".to_owned()),
        )
    } else {
        (
            TimeMeaning::Instant {
                at: instant("2026-12-24T18:00:00Z"),
                has_time: true,
            },
            Some("Christmas Eve".to_owned()),
        )
    }
}

struct PhraseResolver;

#[async_trait]
impl TimeResolver for PhraseResolver {
    async fn resolve(
        &self,
        _: &GroupId,
        request: TimeRequest,
    ) -> Result<TimeResolution, ModelError> {
        let (meaning, echo) = meaning_of(&request.text);
        Ok(TimeResolution {
            room: request.room,
            message: request.message,
            phrase: Some(request.text),
            meaning,
            echo,
        })
    }

    fn name(&self) -> &str {
        "phrases"
    }
}

#[tokio::test]
async fn two_times_in_one_message_each_get_their_own_live_answer() {
    let mut sim = dinner();
    sim.resolves(TimeScript::Silent);
    sim.resolves(TimeScript::Silent);
    let asked = sim.say(
        "dinner",
        "alex",
        "Q, poll: after the holidays or before christmas",
    );
    let requests: Vec<TimeRequest> = asked
        .effects
        .iter()
        .filter_map(|e| match e {
            Effect::ResolveTime(r) => Some(r.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(requests.len(), 2, "{:#?}", asked.effects);
    for request in requests {
        sim.advance(5);
        let (meaning, echo) = meaning_of(&request.text);
        sim.feed(Input::TimeResolved(TimeResolution {
            room: request.room,
            message: request.message,
            phrase: Some(request.text),
            meaning,
            echo,
        }));
    }
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let options = ReplayOptions {
        oracle: Oracle::Live {
            model: Arc::new(DifferentModel {
                calls: AtomicU32::new(0),
            }),
            classifier: Arc::new(ScriptedClassifier),
            extractor: Arc::new(NoExtractor),
            resolver: Arc::new(PhraseResolver),
        },
        ..ReplayOptions::logged(GroupId::new(GROUP))
    };

    let report = replay(&store, options).await.unwrap();
    assert!(report.divergences.is_empty(), "{:#?}", report.divergences);
    assert_eq!(report.logged, report.replayed);
}
