//! The ring buffer and the snapshot (DESIGN.md section 14).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::sim::{
    ClassifierScript, ExtractorScript, ModelScript, Sim, citing, draft, extract_requests,
    self_draft,
};
use ours_core::state::RING_CAPACITY;
use ours_core::{Answering, GroupState, Input, Stage, TimerPurpose, step};
use ours_types::{InboundEvent, Kind, PlatformRole, PlatformRoomId, TimerId};
use serde_json::Value;

/// Every key this milestone added to the snapshot, at any depth.
const NEW_KEYS: [&str; 14] = [
    "observe",
    "observed",
    "setup",
    "shushed",
    "hands",
    "pending_dms",
    "cut",
    "answer_window_seconds",
    "observe_seconds",
    "scores",
    "traces",
    "trace",
    "answering",
    "score",
];

/// Every key milestone three added, at any depth.
const MEMORY_KEYS: [&str; 5] = ["facts", "extractions", "backfills", "patterns", "recalled"];

/// Every key milestone four added, at any depth.
const DECISION_KEYS: [&str; 9] = [
    "zones",
    "last_seen",
    "proposals",
    "decisions",
    "calendar",
    "pending_polls",
    "pending_times",
    "decision_rule",
    "capabilities",
];

/// Every key milestone six added, at any depth.
const ACTION_KEYS: [&str; 4] = ["actions", "holds", "record", "unprompted_outbound"];

/// Every key the character pass added, at any depth.
const CHARACTER_KEYS: [&str; 4] = ["bits", "chime_threshold", "chimes_per_day", "chimes"];

fn strip(value: &mut Value) {
    strip_keys(value, &NEW_KEYS);
    strip_keys(value, &MEMORY_KEYS);
    strip_keys(value, &DECISION_KEYS);
}

fn strip_keys(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in keys {
                map.remove(*key);
            }
            map.values_mut().for_each(|v| strip_keys(v, keys));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| strip_keys(v, keys)),
        _ => {}
    }
}

fn held(sim: &Sim) -> Vec<String> {
    sim.state.rooms[&PlatformRoomId::new("chat")]
        .messages
        .iter()
        .map(|m| m.id.to_string())
        .collect()
}

#[test]
fn the_ring_buffer_keeps_the_last_two_hundred() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room("chat", "chat", &[("sam", "Sam", PlatformRole::Regular)]);
    for i in 1..=250 {
        sim.say("chat", "sam", &format!("message {i}"));
    }
    let ids = held(&sim);
    assert_eq!(ids.len(), RING_CAPACITY);
    assert_eq!(ids.first().unwrap(), "m51");
    assert_eq!(ids.last().unwrap(), "m250");

    let mut edited = sim.message("chat", "sam", "message 100, fixed");
    edited.id = "m100".into();
    edited.edited = true;
    sim.feed(Input::Inbound(InboundEvent::MessageEdited(edited)));
    let room = &sim.state.rooms[&PlatformRoomId::new("chat")];
    assert_eq!(room.messages.len(), RING_CAPACITY);
    let at = room
        .messages
        .iter()
        .position(|m| m.id.as_str() == "m100")
        .unwrap();
    assert_eq!(at, 49);
    assert_eq!(room.messages[at].text, "message 100, fixed");
    assert!(room.messages[at].edited);

    let mut gone = sim.message("chat", "sam", "too old to keep");
    gone.id = "m10".into();
    sim.feed(Input::Inbound(InboundEvent::MessageEdited(gone)));
    assert!(!held(&sim).contains(&"m10".to_owned()));

    sim.feed(Input::Inbound(InboundEvent::MessageDeleted {
        room: "chat".into(),
        id: "m250".into(),
    }));
    assert_eq!(held(&sim).last().unwrap(), "m249");
}

#[test]
fn history_goes_in_front_in_order() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room("chat", "chat", &[("sam", "Sam", PlatformRole::Regular)]);
    sim.say("chat", "sam", "now");
    let mut old = Vec::new();
    for i in 0..3 {
        let mut m = sim.message("chat", "sam", &format!("old {i}"));
        m.id = format!("h{i}").into();
        old.push(m);
    }
    old.push(sim.state.rooms[&PlatformRoomId::new("chat")].messages[0].clone());
    sim.feed(Input::Inbound(InboundEvent::History {
        room: "chat".into(),
        messages: old,
    }));
    assert_eq!(held(&sim), vec!["h0", "h1", "h2", "m1"]);
}

#[test]
fn state_round_trips_through_json_mid_scenario() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("marcus", "Marcus", PlatformRole::Regular),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    sim.say("launch", "marcus", "Q, Sam is a lead");
    sim.model_says("Thursday 2pm.");
    sim.say("launch", "sam", "Q, when's the review?");
    sim.script(ModelScript::Silent);
    sim.say("launch", "marcus", "Q, and the print run?");
    assert_eq!(sim.state.deferrals.len(), 1);
    assert_eq!(sim.state.pending_requests.len(), 1);
    let guards: Vec<TimerId> = sim
        .state
        .timers
        .iter()
        .filter(|(_, t)| matches!(t.purpose, TimerPurpose::ModelGuard { .. }))
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(guards.len(), 1);
    assert_eq!(sim.timers_besides_retention(), 1);

    let json = sim.state.snapshot().unwrap();
    let mut restored = GroupState::restore(&json).unwrap();
    assert_eq!(restored, sim.state);
    assert_eq!(serde_json::to_string(&restored).unwrap(), json);

    let timer = guards[0].clone();
    let later = sim.now.plus_seconds(30);
    let original = step(
        &mut sim.state,
        Input::TimerFired { id: timer.clone() },
        later,
    );
    let replayed = step(&mut restored, Input::TimerFired { id: timer }, later);
    assert_eq!(original, replayed);
    assert_eq!(restored, sim.state);
}

#[test]
fn replaying_the_log_rebuilds_the_state() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "chat",
        "chat",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim.model_says("Friday.");
    sim.say("chat", "jo", "Q, which day?");
    sim.say("chat", "sam", "Q, Jo is a lead");

    let mut state = GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(step(&mut state, input, at));
    }
    assert_eq!(state, sim.state);
    assert_eq!(effects, sim.effects);
}

#[test]
fn a_milestone_one_snapshot_still_loads() {
    let fresh = GroupState::new("group".into(), Kind::Team);
    let mut json = serde_json::to_value(&fresh).unwrap();
    strip(&mut json);
    for key in NEW_KEYS.iter().chain(&MEMORY_KEYS).chain(&DECISION_KEYS) {
        assert!(!json.to_string().contains(&format!("\"{key}\"")), "{key}");
    }
    let old = GroupState::restore(&json.to_string()).unwrap();
    let mut expected = fresh;
    expected.profile.answer_window_seconds = 1200;
    expected.profile.observe_seconds = 0;
    assert_eq!(old, expected);

    // Mid-scenario: rooms, a pending model call, a deferral and a timer.
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("marcus", "Marcus", PlatformRole::Regular),
        ],
    );
    sim.say("launch", "marcus", "what time is the review?");
    sim.say("launch", "marcus", "Q, Marcus is a lead");
    sim.script(ModelScript::Silent);
    sim.say("launch", "marcus", "Q, and the print run?");
    let mut json = serde_json::to_value(&sim.state).unwrap();
    strip(&mut json);
    let mut restored = GroupState::restore(&json.to_string()).unwrap();
    assert!(restored.observe.is_none() && restored.observed.is_none());
    assert_eq!(restored.setup.stage, Stage::NotStarted);
    assert!(restored.budget.cut.is_none());
    let room = &restored.rooms[&PlatformRoomId::new("launch")];
    assert!(room.scores.is_empty() && room.traces.is_empty());
    let pending = restored.pending_requests.values().next().unwrap();
    assert_eq!(pending.answering, Answering::Addressed);
    assert!(pending.score.is_none());

    // It keeps folding: the guard fires and the failure is owned up to.
    let guard = restored
        .timers
        .iter()
        .find(|(_, t)| matches!(t.purpose, TimerPurpose::ModelGuard { .. }))
        .map(|(id, _)| id.clone())
        .unwrap();
    let fx = step(
        &mut restored,
        Input::TimerFired { id: guard },
        sim.now.plus_seconds(30),
    );
    assert_eq!(
        ours_core::sim::texts(&fx),
        vec![ours_core::step::FAILED_REPLY]
    );
}

#[test]
fn replaying_a_week_of_shutting_up_rebuilds_the_state() {
    let mut sim = Sim::observing(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim.dm("olu", "keep");
    sim.dm("olu", "1");
    sim.dm("olu", "keep");
    sim.dm("olu", "keep");
    assert_eq!(sim.state.setup.stage, Stage::Done);
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "what time was it again?");
    sim.advance(20 * 60);
    let line = sim.last_dm_to("jo").unwrap();
    sim.react("dm-jo", &line, "jo", "👍");
    sim.dm("olu", "go");
    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "jo", "where was it?");
    sim.advance(20 * 60);
    sim.model_says("Hobson's.");
    sim.react("dinner", &asked.id, "sam", "👀");
    let answer = sim.last_sent_message().unwrap();
    sim.react("dinner", &answer, "jo", "🤫");
    sim.say("dinner", "sam", "Q, why");
    sim.advance(24 * 60 * 60);

    let mut state = GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(step(&mut state, input, at));
    }
    assert_eq!(state, sim.state);
    assert_eq!(effects, sim.effects);
    let json = state.snapshot().unwrap();
    assert_eq!(GroupState::restore(&json).unwrap(), state);
}

/// Friends in #dinner with facts kept, a correction waiting on the
/// extractor, a backfill waiting on its second page, and a week of patterns.
fn remembering() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim.extracts(vec![self_draft(
        "Sam",
        "is allergic to",
        "peanuts",
        "allergic to peanuts",
    )]);
    let said = sim.say("dinner", "sam", "I'm allergic to peanuts");
    sim.advance(3600);
    sim.script(ModelScript::Reply(citing(
        "Not the satay place, then.",
        &[said.id.as_str()],
    )));
    sim.say("dinner", "jo", "Q, where can we take Sam?");
    let answer = sim.last_sent_message().unwrap();
    sim.extract(ExtractorScript::Silent);
    let wrong = sim.reply("dinner", "sam", "that's wrong, it's only cashews", &answer);
    assert_eq!(extract_requests(&wrong.effects).len(), 1);
    let page = vec![
        sim.old_message(
            "dinner",
            "priya",
            "my new number is 07700 900123",
            4 * 86_400,
        ),
        sim.old_message("dinner", "jo", "ok", 3 * 86_400),
    ];
    sim.history_page(page);
    sim.extracts(vec![draft("Priya", "has the number", "07700 900123")]);
    sim.say("dinner", "priya", "Q, backfill");
    sim.say("dinner", "priya", "lol");
    sim
}

#[test]
fn memory_round_trips_through_a_snapshot_and_keeps_folding() {
    let mut sim = remembering();
    assert_eq!(sim.state.facts.len(), 1);
    assert_eq!(sim.state.extractions.len(), 1);
    let backfill = &sim.state.backfills[&PlatformRoomId::new("dinner")];
    assert!(backfill.paging);
    assert_eq!(backfill.held.len(), 2);
    assert!(!sim.state.patterns.members.is_empty());

    let json = sim.state.snapshot().unwrap();
    let mut restored = GroupState::restore(&json).unwrap();
    assert_eq!(restored, sim.state);
    assert_eq!(serde_json::to_string(&restored).unwrap(), json);

    // Both copies take the correction's facts and the last page alike.
    let inputs = [
        Input::Extracted {
            room: "dinner".into(),
            message: sim.state.extractions[0].message.clone(),
            extraction: ours_core::sim::facts(vec![self_draft(
                "Sam",
                "is allergic to",
                "cashews",
                "only cashews",
            )]),
        },
        Input::Inbound(InboundEvent::History {
            room: "dinner".into(),
            messages: Vec::new(),
        }),
    ];
    let later = sim.now.plus_seconds(5);
    for input in inputs {
        let original = step(&mut sim.state, input.clone(), later);
        let replayed = step(&mut restored, input, later);
        assert_eq!(original, replayed);
    }
    assert_eq!(restored, sim.state);
    assert_eq!(restored.facts.len(), 2);
}

#[test]
fn replaying_a_log_with_memory_rebuilds_the_state() {
    let sim = remembering();
    let mut state = GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(step(&mut state, input, at));
    }
    assert_eq!(state, sim.state);
    assert_eq!(effects, sim.effects);
}

#[test]
fn a_milestone_two_snapshot_still_loads() {
    let sim = remembering();
    let mut json = serde_json::to_value(&sim.state).unwrap();
    strip_keys(&mut json, &MEMORY_KEYS);
    let mut restored = GroupState::restore(&json.to_string()).unwrap();
    assert!(restored.facts.is_empty());
    assert!(restored.extractions.is_empty() && restored.backfills.is_empty());
    assert!(restored.patterns.members.is_empty() && restored.patterns.halved.is_none());
    let room = &restored.rooms[&PlatformRoomId::new("dinner")];
    assert!(room.traces.iter().all(|(_, t)| t.facts.is_empty()));

    // Answers for reads it no longer knows of change nothing; new ones work.
    let fx = step(
        &mut restored,
        Input::Extracted {
            room: "dinner".into(),
            message: "m1".into(),
            extraction: ours_core::sim::facts(vec![draft("Sam", "drives", "")]),
        },
        sim.now,
    );
    assert!(fx.is_empty());
    assert!(restored.facts.is_empty());
}

#[test]
fn a_milestone_three_snapshot_still_loads() {
    let sim = remembering();
    let mut json = serde_json::to_value(&sim.state).unwrap();
    strip_keys(&mut json, &DECISION_KEYS);
    for key in DECISION_KEYS {
        assert!(!json.to_string().contains(&format!("\"{key}\"")), "{key}");
    }
    let restored = GroupState::restore(&json.to_string()).unwrap();
    assert!(restored.proposals.is_empty() && restored.decisions.is_empty());
    assert!(restored.calendar.is_empty() && restored.zones.is_empty());
    assert!(restored.last_seen.is_empty() && restored.pending_times.is_empty());
    assert!(restored.decision_rule.is_none());
    assert!(!restored.me[0].capabilities.polls);
    let mut expected = sim.state.clone();
    expected.last_seen.clear();
    assert_eq!(restored, expected);

    // It keeps folding: a poll opens, and a vote counts once members post.
    let mut sim = Sim::with_state(restored);
    sim.now = ours_core::sim::instant("2026-11-03T10:00:00Z");
    // A new harness counts message ids from one again, so these get their own.
    let lines = [
        ("sam", "hello"),
        ("priya", "hello"),
        ("jo", "hello"),
        ("sam", "Q, poll thursday vs friday"),
    ];
    for (n, (user, text)) in lines.into_iter().enumerate() {
        let mut message = sim.message("dinner", user, text);
        message.id = format!("after{n}").into();
        sim.post(message);
    }
    let p = sim.state.proposals.values().next().unwrap();
    assert!(p.is_open());
    let ballot = p.ballot.message().unwrap().clone();
    sim.react("dinner", &ballot, "jo", "2️⃣");
    sim.react("dinner", &ballot, "priya", "2️⃣");
    assert_eq!(sim.state.decisions.len(), 1);
}

#[test]
fn a_decision_scenario_replays_from_the_log() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    for user in ["sam", "priya", "jo"] {
        sim.say("dinner", user, "hello");
    }
    sim.say("dinner", "sam", "dinner this week?");
    sim.say("dinner", "priya", "yes");
    sim.classify(ClassifierScript::Score(ours_core::sim::plan_score()));
    sim.say("dinner", "jo", "thurs or fri");
    let ballot = sim
        .state
        .proposals
        .values()
        .next()
        .unwrap()
        .ballot
        .message()
        .unwrap()
        .clone();
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.advance(13 * 60 * 60);
    sim.dm("priya", "1");
    let mut state = GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(step(&mut state, input, at));
    }
    assert_eq!(state, sim.state);
    assert_eq!(effects, sim.effects);
    assert_eq!(state.decisions.len(), 1);
    let json = state.snapshot().unwrap();
    assert_eq!(GroupState::restore(&json).unwrap(), state);
}

#[test]
fn a_milestone_five_snapshot_still_loads() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    sim.model_says("Thursday at 2.");
    sim.say("launch", "sam", "Q, when's the review?");
    sim.say("launch", "sam", "Q, Sam is a lead");
    sim.script(ModelScript::Silent);
    sim.say("launch", "sam", "Q, and the print run?");
    assert!(!sim.state.actions.is_empty());
    let mut json = serde_json::to_value(&sim.state).unwrap();
    strip_keys(&mut json, &ACTION_KEYS);
    for key in ACTION_KEYS {
        assert!(!json.to_string().contains(&format!("\"{key}\"")), "{key}");
    }
    let mut restored = GroupState::restore(&json.to_string()).unwrap();
    assert!(restored.actions.is_empty() && restored.holds.is_empty());
    assert_eq!(
        restored.profile.unprompted_outbound,
        ours_core::Outbound::Propose
    );
    let mut expected = sim.state.clone();
    expected.actions.clear();
    assert_eq!(restored, expected);

    // It keeps folding, and the next reply is recorded.
    let guard = restored.timers.keys().next().unwrap().clone();
    let fx = step(
        &mut restored,
        Input::TimerFired { id: guard },
        sim.now.plus_seconds(30),
    );
    assert_eq!(
        ours_core::sim::texts(&fx),
        vec![ours_core::step::FAILED_REPLY]
    );
}

#[test]
fn a_milestone_six_snapshot_still_loads() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim.model_says("Thursday.");
    sim.say("dinner", "sam", "Q, when's dinner?");
    sim.script(ModelScript::Silent);
    sim.say("dinner", "jo", "Q, and who's booking?");
    let mut json = serde_json::to_value(&sim.state).unwrap();
    strip_keys(&mut json, &CHARACTER_KEYS);
    for key in CHARACTER_KEYS {
        assert!(!json.to_string().contains(&format!("\"{key}\"")), "{key}");
    }
    let mut restored = GroupState::restore(&json.to_string()).unwrap();
    assert!(restored.persona.bits.is_empty());
    assert_eq!(restored.profile.chime_threshold, 0.7);
    assert_eq!(restored.profile.chimes_per_day, None);
    assert_eq!(restored.profile.chimes_a_day(), 0);
    assert!(restored.budget.chimes.is_empty());
    assert_eq!(restored, sim.state);

    // It keeps folding: a lead turns the dial up and the chimes follow it.
    let fx = step(
        &mut restored,
        Input::Command(ours_core::Command::ChangeSetting {
            change: ours_core::SettingChange::DialTo { dial: 3 },
        }),
        sim.now,
    );
    assert!(!fx.is_empty());
    assert_eq!(restored.profile.chimes_a_day(), 6);
}
