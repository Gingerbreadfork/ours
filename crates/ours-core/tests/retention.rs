//! It keeps only what it should, for as long as it should (DESIGN.md
//! section 9, "Retention"): the nightly sweep of old action records and
//! expired facts, and the legal hold that turns it off.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::leads::DEFERRAL_REPLY;
use ours_core::sim::{ClassifierScript, Posted, Sim, draft, instant, notices, texts};
use ours_core::{
    Command, GroupState, Input, SettingChange, TimerPurpose, render_charter, retention_days,
};
use ours_types::{FactDraft, Instant, Kind, MessageKind, PlatformRole, Score, ScoreSource};

const DAY: i64 = 24 * 60 * 60;

/// Priya leads #launch as an admin; Sam is a member; Dana is a guest.
fn work() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    sim
}

fn friends() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim
}

/// A message the small model scores as a fact, and what the extractor finds in it.
fn hear(sim: &mut Sim, room: &str, user: &str, text: &str, drafts: Vec<FactDraft>) -> Posted {
    sim.classify(ClassifierScript::Score(Score {
        fact: 0.8,
        kind: MessageKind::Statement,
        source: ScoreSource::SmallModel {
            model: "sim".into(),
        },
        ..Score::default()
    }));
    sim.extracts(drafts);
    sim.say(room, user, text)
}

fn away_until(day: &str) -> FactDraft {
    FactDraft {
        durable: false,
        expires: Some(day.to_owned()),
        ..draft("Sam", "is", "away")
    }
}

fn sweep_at(sim: &Sim) -> Option<Instant> {
    sim.state
        .timers
        .values()
        .find(|t| t.purpose == TimerPurpose::Retention)
        .map(|t| t.at)
}

#[test]
fn a_year_at_work_and_ninety_days_elsewhere() {
    for kind in [Kind::Team, Kind::Project, Kind::Committee] {
        assert_eq!(retention_days(kind), 365, "{kind:?}");
    }
    for kind in [
        Kind::Friends,
        Kind::Household,
        Kind::Club,
        Kind::Cohort,
        Kind::Community,
    ] {
        assert_eq!(retention_days(kind), 90, "{kind:?}");
    }
}

#[test]
fn the_nightly_sweep_drops_records_past_ninety_days_in_a_friends_group() {
    let mut sim = friends();
    assert_eq!(sweep_at(&sim), None, "nothing held, nothing to sweep");
    sim.model_says("Thursday.");
    sim.say("dinner", "jo", "Q, when's dinner?");
    let kept = sim.state.actions.len();
    assert!(kept > 0);
    // The first night after the oldest record turns ninety days old.
    assert_eq!(sweep_at(&sim), Some(instant("2027-02-01T04:00:00Z")));

    let early = sim.advance_to(instant("2027-01-31T12:00:00Z"));
    assert!(notices(&early).is_empty());
    assert_eq!(sim.state.actions.len(), kept);

    let swept = sim.advance_to(instant("2027-02-01T05:00:00Z"));
    assert!(sim.state.actions.is_empty());
    let line = if kept == 1 {
        "Retention: dropped 1 action record older than 90 days.".to_owned()
    } else {
        format!("Retention: dropped {kept} action records older than 90 days.")
    };
    assert_eq!(notices(&swept), vec![line.as_str()]);
    assert_eq!(sweep_at(&sim), None);
}

#[test]
fn a_team_keeps_its_records_a_year_and_the_sweep_follows_the_groups_zone() {
    let mut sim = work();
    sim.zone("Europe/London");
    sim.model_says("Thursday at 2.");
    sim.say("launch", "sam", "Q, when's the review?");
    let kept = sim.state.actions.len();
    sim.advance(100 * DAY);
    assert_eq!(sim.state.actions.len(), kept);
    let at = sweep_at(&sim).unwrap();
    assert_eq!(at, instant("2027-11-03T04:00:00Z"), "04:00 in London");
    sim.advance_to(at);
    assert!(sim.state.actions.is_empty());
}

#[test]
fn an_expired_fact_is_scrubbed_the_night_after_and_a_new_one_brings_the_sweep_forward() {
    let mut sim = work();
    sim.model_says("Noted.");
    sim.say("launch", "sam", "Q, noted?");
    let year = sweep_at(&sim).unwrap();
    hear(
        &mut sim,
        "launch",
        "priya",
        "sam's away till wednesday",
        vec![away_until("2026-11-04")],
    );
    let night = instant("2026-11-05T04:00:00Z");
    assert_eq!(sweep_at(&sim), Some(night));
    assert!(night < year);
    let swept = sim.advance_to(night);
    assert_eq!(notices(&swept), vec!["Retention: dropped 1 expired fact."]);
    let fact = sim.state.facts.values().next().unwrap();
    assert_eq!(fact.retracted.as_ref().unwrap().reason, "expired");
    assert!(fact.line().is_empty() && fact.sources.is_empty());
    // Back to waiting for the year to turn.
    assert_eq!(sweep_at(&sim), Some(year));
}

#[test]
fn legal_hold_keeps_everything_until_a_lead_lifts_it() {
    let mut sim = work();
    sim.model_says("Noted.");
    sim.say("launch", "sam", "Q, noted?");
    hear(
        &mut sim,
        "launch",
        "priya",
        "sam's away till wednesday",
        vec![away_until("2026-11-04")],
    );
    let kept = sim.state.actions.len();

    let on = sim.say("launch", "priya", "Q, legal hold on");
    let line = "Legal hold on since 2026-11-02: nothing expires and nothing is dropped until a lead says legal hold off.";
    assert_eq!(texts(&on.effects), vec![line]);
    assert!(
        notices(&on.effects)
            .iter()
            .any(|n| *n == format!("Setting: {line} Changed by Priya."))
    );
    assert!(sim.state.legal_hold);
    assert_eq!(sim.state.legal_hold_since, Some(ours_core::sim::START));
    assert_eq!(sweep_at(&sim), None);
    let charter = render_charter(&sim.state);
    assert!(
        charter.contains("- Legal hold: on since 2026-11-02; nothing expires"),
        "{charter}"
    );

    // Past the fact's expiry and past the year, nothing goes.
    let quiet = sim.advance(400 * DAY);
    assert!(notices(&quiet).is_empty(), "{quiet:?}");
    assert!(sim.state.facts.values().all(|f| f.retracted.is_none()));
    assert!(sim.state.actions.len() > kept);
    assert_eq!(sweep_at(&sim), None);

    // Saying it again keeps the first date.
    sim.say("launch", "priya", "Q, hold everything");
    assert_eq!(sim.state.legal_hold_since, Some(ours_core::sim::START));

    let off = sim.say("launch", "priya", "Q, stop holding everything");
    assert_eq!(
        texts(&off.effects),
        vec![
            "Legal hold off. The action log is kept 365 days again and expired facts are dropped."
        ]
    );
    assert!(!sim.state.legal_hold && sim.state.legal_hold_since.is_none());
    assert!(render_charter(&sim.state).contains("- Legal hold: off"));
    let swept = sim.advance(DAY);
    let notice = notices(&swept)[0];
    assert!(
        notice.starts_with("Retention: dropped ")
            && notice.ends_with(" older than 365 days and 1 expired fact."),
        "{notice}"
    );
}

#[test]
fn only_a_lead_or_the_host_turns_it_on() {
    let mut sim = work();
    let member = sim.say("launch", "sam", "Q, legal hold on");
    assert_eq!(texts(&member.effects), vec![DEFERRAL_REPLY]);
    assert!(!sim.state.legal_hold);
    let guest = sim.say("launch", "dana", "Q, legal hold on");
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert!(!sim.state.legal_hold);

    sim.advance(DAY);
    let fx = sim.feed(Input::Command(Command::SetLegalHold { on: true }));
    assert_eq!(
        notices(&fx),
        vec![
            "Legal hold on since 2026-11-03: nothing expires and nothing is dropped until a lead says legal hold off."
        ]
    );
    assert!(sim.state.legal_hold);
    sim.feed(Input::Command(Command::ChangeSetting {
        change: SettingChange::LegalHold { on: false },
    }));
    assert!(!sim.state.legal_hold);
}

#[test]
fn a_snapshot_from_before_retention_still_loads() {
    let mut sim = work();
    sim.say("launch", "priya", "Q, be the host");
    sim.say("launch", "priya", "Q, legal hold on");
    let mut json = serde_json::to_value(&sim.state).unwrap();
    let map = json.as_object_mut().unwrap();
    for key in ["persona_name", "legal_hold", "legal_hold_since"] {
        assert!(map.remove(key).is_some(), "{key}");
    }
    let restored = GroupState::restore(&json.to_string()).unwrap();
    assert_eq!(restored.persona_name, None);
    assert!(!restored.legal_hold && restored.legal_hold_since.is_none());
    assert_eq!(restored.persona, sim.state.persona);
}
