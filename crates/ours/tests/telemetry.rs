//! The text-free triage signal, joined from a journal the real engine
//! wrote: what it did about each scored message and what people did in the
//! day after (DESIGN.md section 18, "The triage signal").
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours::config::TelemetrySection;
use ours::runtime::journal::Journal;
use ours::telemetry::{
    Acted, GROUP_SALT_KEY, HOST_ID_KEY, Record, SEQ_KEY, TelemetryError, collect, collect_at,
    flush, job, job_once, sample,
};
use ours_core::sim::{ClassifierScript, ME, ModelScript, START, Sim, banter, plan};
use ours_core::{Command, GroupState, Input, Profile, SettingChange};
use ours_store::{EventLog, StoreDir};
use ours_types::{
    ActionId, Capabilities, GroupId, InboundEvent, Instant, Kind, MessageKind, Platform,
    PlatformRole, PlatformUserId, RequestId, TimerId,
};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GROUP: &str = "g-the-six";
const WINDOW: i64 = 20 * 60;
const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

const TEXTS: [&str; 10] = [
    "thurs or fri",
    "Thursday works",
    "Hobson",
    "what time was it again",
    "Half seven",
    "wifi password",
    "on the fridge",
    "WE GOT THE HOUSE",
    "Called it",
    "lol",
];
const IDS: [&str; 9] = [
    GROUP, "sam", "priya", "alex", "dinner", "guild-1", ME, "\"m3\"", "sent",
];

/// The six in #dinner on Discord, watching nothing, a month back.
fn the_six() -> Sim {
    let mut sim = Sim::with_state(GroupState::new(GroupId::new(GROUP), Kind::Friends));
    // A month back, so the wall clock has closed every window.
    sim.now = start();
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
            dms: true,
            delete: true,
            ..Capabilities::default()
        },
    }));
    sim
}

/// The six's evening: a 🙋 that was welcome, a question a person answered,
/// a chime that got a 🤫 and an undo, and chatter.
fn evening() -> Sim {
    let mut sim = the_six();
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
    sim.react("dinner", &answer, "priya", "😂");

    sim.advance(120);
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "alex", "anyone know the wifi password?");
    sim.say("dinner", "jo", "it's on the fridge");
    sim.advance(WINDOW);

    sim.advance(HOUR);
    sim.feed(Input::Command(Command::ChangeSetting {
        change: SettingChange::DialTo { dial: 2 },
    }));
    sim.script(ModelScript::Reply(plan("Called it.")));
    sim.classify(ClassifierScript::Score(banter(0.9)));
    sim.say("dinner", "sam", "WE GOT THE HOUSE");
    let chime = sim.last_sent_message().unwrap();
    sim.react("dinner", &chime, "priya", "🤫");
    sim.advance(30);
    sim.say("dinner", "alex", "Q, undo");

    sim.advance(60);
    sim.say("dinner", "jo", "lol");
    sim
}

fn write_journal(dir: &Path, sim: &Sim) -> StoreDir {
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
fn write_offset_journal(dir: &Path, sim: &Sim) -> StoreDir {
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

/// The scored messages' texts, in the order their records come.
fn scored_texts(sim: &Sim) -> Vec<String> {
    sim.log
        .iter()
        .filter_map(|(_, input)| match input {
            Input::Scored { room, message, .. } => sim.state.rooms[room]
                .messages
                .iter()
                .find(|m| &m.id == message)
                .map(|m| m.text.clone()),
            _ => None,
        })
        .collect()
}

fn record_for<'a>(sim: &Sim, records: &'a [Record], text: &str) -> &'a Record {
    let at = scored_texts(sim).iter().position(|t| t == text).unwrap();
    &records[at]
}

fn start() -> Instant {
    START.plus_seconds(-30 * DAY)
}

fn later() -> Instant {
    start().plus_seconds(3 * DAY)
}

fn on(endpoint: Option<String>) -> TelemetrySection {
    TelemetrySection {
        enabled: true,
        endpoint,
    }
}

#[test]
fn each_scored_message_is_joined_to_what_happened_next() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);

    let (records, covered) = collect_at(&store, &group, 0, later()).unwrap();
    assert_eq!(covered, sim.log.len() as u64);
    assert_eq!(records.len(), scored_texts(&sim).len());
    assert_eq!(records.len(), 6, "{:?}", scored_texts(&sim));

    // The 🙋 was welcome: an invite on the question and two on the answer.
    let welcome = record_for(&sim, &records, "what time was it again?");
    assert_eq!(welcome.acted, Acted::Spoke);
    assert_eq!(welcome.outcome.positive_reactions, 3);
    assert!(!welcome.outcome.shushed);
    assert!(!welcome.outcome.undone);
    assert!(!welcome.outcome.answered_by_human);
    assert_eq!(welcome.score.kind, MessageKind::Question);
    assert!(welcome.score.answerable_from_context);
    assert!(!welcome.rules.answerable_from_context);
    assert_eq!(welcome.kind, Kind::Friends);
    assert_eq!(welcome.dial, 1);
    assert_eq!(welcome.at_hour_utc, 18);
    assert_eq!(welcome.outcome.window_hours, 24);

    // The chime got a 🤫 and was undone.
    let chime = record_for(&sim, &records, "WE GOT THE HOUSE");
    assert_eq!(chime.acted, Acted::Chimed);
    assert_eq!(chime.dial, 2);
    assert!(chime.outcome.shushed);
    assert!(chime.outcome.undone);
    assert_eq!(chime.outcome.positive_reactions, 0);
    assert!((chime.score.banter - 0.9).abs() < 1e-6);

    // Jo answered before any hand went up.
    let answered = record_for(&sim, &records, "anyone know the wifi password?");
    assert_eq!(answered.acted, Acted::Nothing);
    assert!(answered.outcome.answered_by_human);

    let chatter = record_for(&sim, &records, "lol");
    assert_eq!(chatter.acted, Acted::Nothing);
    assert_eq!(chatter.outcome.positive_reactions, 0);
    assert!(!chatter.outcome.shushed);

    // One host, one group hash, on every record.
    assert!(records.iter().all(|r| r.host == records[0].host));
    assert!(records.iter().all(|r| r.group == records[0].group));
    for id in [&records[0].host, &records[0].group] {
        assert_eq!(id.len(), 16);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert_ne!(records[0].host, records[0].group);
}

#[test]
fn a_shut_up_lands_on_the_last_thing_it_said_unprompted() {
    let mut sim = the_six();
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "what time was it again?");
    sim.advance(WINDOW);
    sim.model_says("Half seven.");
    sim.advance(WINDOW);
    sim.advance(60);
    sim.say("dinner", "priya", "Q, shut up");
    sim.advance(60);
    sim.say("dinner", "jo", "lol");
    assert!(
        sim.state
            .shushed
            .contains_key(&PlatformUserId::new("priya")),
        "the engine took it as a shush"
    );

    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let (records, _) = collect_at(&store, &GroupId::new(GROUP), 0, later()).unwrap();
    let answered = record_for(&sim, &records, "what time was it again?");
    assert_eq!(answered.acted, Acted::Spoke);
    assert!(answered.outcome.shushed);
    assert_eq!(answered.outcome.positive_reactions, 0);
    let after = record_for(&sim, &records, "lol");
    assert!(!after.outcome.shushed);
}

#[test]
fn a_journal_whose_ids_are_off_by_one_gives_the_same_records() {
    let sim = evening();
    let group = GroupId::new(GROUP);
    let plain_dir = tempfile::tempdir().unwrap();
    let plain = write_journal(plain_dir.path(), &sim);
    let (expected, covered) = collect_at(&plain, &group, 0, later()).unwrap();

    let offset_dir = tempfile::tempdir().unwrap();
    let offset = write_offset_journal(offset_dir.path(), &sim);
    let salt = plain
        .open_group(&group)
        .unwrap()
        .get(GROUP_SALT_KEY)
        .unwrap()
        .unwrap();
    let log = offset.open_group(&group).unwrap();
    log.set(HOST_ID_KEY, expected[0].host.as_bytes()).unwrap();
    log.set(GROUP_SALT_KEY, &salt).unwrap();
    let (records, end) = collect_at(&offset, &group, 0, later()).unwrap();
    assert_eq!(records, expected);
    assert_eq!(end, covered);

    // The joins that follow ids survived the shift.
    let welcome = record_for(&sim, &records, "what time was it again?");
    assert_eq!(welcome.acted, Acted::Spoke);
    assert_eq!(welcome.outcome.positive_reactions, 3);
    let chime = record_for(&sim, &records, "WE GOT THE HOUSE");
    assert!(chime.outcome.shushed && chime.outcome.undone);
}

#[test]
fn records_carry_no_text_and_no_ids() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let (records, _) = collect_at(&store, &GroupId::new(GROUP), 0, later()).unwrap();
    let json = serde_json::to_string(&records).unwrap();
    for text in TEXTS {
        assert!(!json.contains(text), "{text} leaked: {json}");
    }
    for id in IDS {
        assert!(!json.contains(id), "{id} leaked: {json}");
    }
    let shown = sample(&records, 2);
    let parsed: Vec<Record> = serde_json::from_str(&shown).unwrap();
    assert_eq!(parsed, records[..2].to_vec());
    let keys: Vec<String> = serde_json::to_value(&records[0])
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        [
            "host",
            "group",
            "kind",
            "dial",
            "at_hour_utc",
            "score",
            "rules",
            "acted",
            "outcome"
        ]
    );
}

#[test]
fn the_host_id_is_made_once_and_shared_by_the_hosts_groups() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);
    let (first, _) = collect_at(&store, &group, 0, later()).unwrap();
    let kept = store.open_group(&group).unwrap().get(HOST_ID_KEY).unwrap();
    assert_eq!(kept, Some(first[0].host.as_bytes().to_vec()));
    let (again, _) = collect(&store, &group, 0).unwrap();
    assert!(again.iter().all(|r| r.host == first[0].host));

    // A second group on the host gets the same host id and its own hash.
    let other = GroupId::new("g-the-other");
    let log: Arc<dyn EventLog> = store.open_group(&other).unwrap();
    let journal = Journal::<Input, GroupState>::new(log);
    for (at, input) in &sim.log {
        journal.append(input, *at).unwrap();
    }
    let (theirs, _) = collect_at(&store, &other, 0, later()).unwrap();
    assert_eq!(theirs[0].host, first[0].host);
    assert_ne!(theirs[0].group, first[0].group);
}

#[test]
fn the_group_tag_rests_on_a_salt_that_never_leaves_the_host() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);
    let (first, _) = collect_at(&store, &group, 0, later()).unwrap();
    let salt = store
        .open_group(&group)
        .unwrap()
        .get(GROUP_SALT_KEY)
        .unwrap()
        .expect("the salt is kept in the group's store");
    let salt = String::from_utf8(salt).unwrap();
    assert_eq!(salt.len(), 40);
    let json = serde_json::to_string(&first).unwrap();
    assert!(!json.contains(&salt));

    // The tag is stable, and a guess from what a record carries misses it.
    let (again, _) = collect_at(&store, &group, 0, later()).unwrap();
    assert_eq!(again[0].group, first[0].group);
    let guess: String = Sha256::new()
        .chain_update(first[0].host.as_bytes())
        .chain_update(GROUP.as_bytes())
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_ne!(first[0].group, guess);

    // The same group id on another host gets another tag.
    let other_dir = tempfile::tempdir().unwrap();
    let other = write_journal(other_dir.path(), &sim);
    other
        .open_group(&group)
        .unwrap()
        .set(HOST_ID_KEY, first[0].host.as_bytes())
        .unwrap();
    let (theirs, _) = collect_at(&other, &group, 0, later()).unwrap();
    assert_eq!(theirs[0].host, first[0].host);
    assert_ne!(theirs[0].group, first[0].group);
}

#[test]
fn records_wait_until_their_window_closes() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(dir.path(), &sim);
    let group = GroupId::new(GROUP);
    let first_scored = sim
        .log
        .iter()
        .position(|(_, input)| matches!(input, Input::Scored { .. }))
        .unwrap() as u64
        + 1;

    let (early, covered) = collect_at(&store, &group, 0, sim.now).unwrap();
    assert!(early.is_empty());
    assert_eq!(covered, first_scored - 1);

    let (all, end) = collect_at(&store, &group, covered, later()).unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(end, sim.log.len() as u64);
    let (none, same) = collect_at(&store, &group, end, later()).unwrap();
    assert!(none.is_empty());
    assert_eq!(same, end);
}

#[tokio::test]
async fn flush_appends_json_lines_and_posts_the_batch_once() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = write_journal(&dir.path().join("groups"), &sim);
    let (records, _) = collect_at(&store, &GroupId::new(GROUP), 0, later()).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/triage"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;
    let config = on(Some(format!("{}/v1/triage", server.uri())));

    let flushed = flush(&config, dir.path(), &records).await.unwrap();
    assert_eq!(flushed.posted, Some(202));
    assert!(flushed.written.starts_with(dir.path().join("telemetry")));
    assert!(flushed.written.to_string_lossy().ends_with(".jsonl"));
    let lines: Vec<Record> = std::fs::read_to_string(&flushed.written)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines, records);

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let posted: Vec<Record> = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(posted, records);

    // Lines are appended, never rewritten.
    let local = on(None);
    let again = flush(&local, dir.path(), &records[..1]).await.unwrap();
    assert_eq!(again.posted, None);
    let count = std::fs::read_to_string(&again.written)
        .unwrap()
        .lines()
        .count();
    assert_eq!(count, records.len() + 1);
}

#[tokio::test]
async fn a_failed_post_is_reported_after_the_lines_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}/v1/triage", listener.local_addr().unwrap());
    drop(listener);
    let sim = evening();
    let store = write_journal(&dir.path().join("groups"), &sim);
    let (records, _) = collect_at(&store, &GroupId::new(GROUP), 0, later()).unwrap();

    match flush(&on(Some(closed)), dir.path(), &records).await {
        Err(TelemetryError::Post { written, .. }) => {
            let count = std::fs::read_to_string(written).unwrap().lines().count();
            assert_eq!(count, records.len());
        }
        other => panic!("expected a post error, got {other:?}"),
    }
}

#[tokio::test]
async fn nothing_is_written_or_sent_while_telemetry_is_off() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(write_journal(&dir.path().join("groups"), &sim));
    let group = GroupId::new(GROUP);
    let off = TelemetrySection::default();
    assert!(!off.enabled);

    let records = vec![];
    assert!(matches!(
        flush(&off, dir.path(), &records).await,
        Err(TelemetryError::Disabled)
    ));
    assert!(
        job_once(&store, &off, dir.path(), std::slice::from_ref(&group))
            .await
            .is_empty()
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        job(
            Arc::clone(&store),
            off,
            dir.path().to_path_buf(),
            vec![group.clone()],
            Duration::from_millis(1),
        ),
    )
    .await
    .expect("the job returns at once when off");

    assert!(!dir.path().join("telemetry").exists());
    let log = store.open_group(&group).unwrap();
    assert_eq!(log.get(SEQ_KEY).unwrap(), None);
    assert_eq!(log.get(HOST_ID_KEY).unwrap(), None);
    assert_eq!(log.get(GROUP_SALT_KEY).unwrap(), None);
}

#[tokio::test]
async fn the_job_remembers_how_far_it_got() {
    let sim = evening();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(write_journal(&dir.path().join("groups"), &sim));
    let group = GroupId::new(GROUP);
    let config = on(None);

    // The evening was a month ago by the wall clock.
    let results = job_once(&store, &config, dir.path(), std::slice::from_ref(&group)).await;
    let flushed = results.into_iter().next().unwrap().unwrap();
    let written = std::fs::read_to_string(&flushed.written).unwrap();
    assert_eq!(written.lines().count(), 6);
    let seq = store.open_group(&group).unwrap().get(SEQ_KEY).unwrap();
    assert_eq!(seq, Some(sim.log.len().to_string().into_bytes()));

    let results = job_once(&store, &config, dir.path(), &[group]).await;
    assert!(results[0].is_ok());
    let unchanged = std::fs::read_to_string(&flushed.written).unwrap();
    assert_eq!(unchanged, written);
}
