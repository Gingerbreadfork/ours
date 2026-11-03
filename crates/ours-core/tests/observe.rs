//! Observe mode: the first days in a group, watching and telling the leads
//! what it would have done (DESIGN.md section 18, "Proofs").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::attention::RAISED_HAND;
use ours_core::leads::DEFERRAL_REPLY;
use ours_core::sim::{
    ClassifierScript, Sim, any_send, dms, messages, model_requests, notices, reactions, texts,
};
use ours_core::step::CHECK;
use ours_core::{Command, Effect, Input, TimerPurpose, observers};
use ours_types::{Kind, PlatformRole, PlatformUserId};

const WINDOW: i64 = 20 * 60;
const DAY: i64 = 24 * 60 * 60;

fn watching_friends() -> Sim {
    let mut sim = Sim::observing(Kind::Friends);
    let synced = sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
        ],
    );
    assert!(
        notices(&synced)
            .iter()
            .any(|n| n.starts_with("Observing for 1 day"))
    );
    assert!(synced.iter().any(|e| matches!(
        e,
        Effect::SetTimer {
            purpose: TimerPurpose::ObserveEnd,
            ..
        }
    )));
    sim
}

fn to(effects: &[Effect]) -> Vec<(&str, &str)> {
    dms(effects)
        .into_iter()
        .map(|(u, t)| (u.as_str(), t))
        .collect()
}

#[test]
fn observing_turns_the_hand_into_a_dm_and_counts_thumbs() {
    let mut sim = watching_friends();
    assert!(sim.state.is_observing());

    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "what time was it again?");
    let raised = sim.advance(WINDOW);
    assert!(reactions(&raised).is_empty());
    assert!(texts(&raised).is_empty());
    let line = "In #dinner I would have reacted 🙋 to Sam: what time was it again?\n👍 if I should have, 👎 if not.";
    assert_eq!(to(&raised), vec![("priya", line), ("sam", line)]);
    let observe = sim.state.observe.as_ref().unwrap();
    assert_eq!(observe.would_have, 1);
    assert_eq!(observe.targets.len(), 2);

    // The person it went to gives it a 👍; nobody else's thumb counts, and
    // one vote per line.
    let to_priya = sim.last_dm_to("priya").unwrap();
    assert!(sim.react("dm-priya", &to_priya, "sam", "👍").is_empty());
    let thumb = sim.react("dm-priya", &to_priya, "priya", "👍");
    assert_eq!(
        notices(&thumb),
        vec!["Priya gave a would-have line 👍; 1 👍 and 0 👎 so far."]
    );
    assert!(!any_send(&thumb));
    assert!(sim.react("dm-priya", &to_priya, "priya", "👎").is_empty());

    // The window passes again: the answer goes to the leads too, with the text.
    sim.model_says("Half seven at Hobson's.");
    let answered = sim.advance(WINDOW);
    assert_eq!(model_requests(&answered).len(), 1);
    assert!(messages(&answered).is_empty());
    let reply = "In #dinner I would have replied to Sam: what time was it again?\nI'd have said: Half seven at Hobson's.\n👍 if I should have, 👎 if not.";
    assert_eq!(to(&answered), vec![("priya", reply), ("sam", reply)]);
    let to_sam = sim.last_dm_to("sam").unwrap();
    sim.react("dm-sam", &to_sam, "sam", "👎");
    let observe = sim.state.observe.as_ref().unwrap();
    assert_eq!((observe.would_have, observe.up, observe.down), (2, 1, 1));
}

#[test]
fn addressed_replies_ignore_observe_mode() {
    let mut sim = watching_friends();
    sim.model_says("Thursday.");
    let asked = sim.say("dinner", "priya", "Q, which day?");
    assert_eq!(
        messages(&asked.effects),
        vec![("Thursday.", Some(&asked.id))]
    );
    assert!(dms(&asked.effects).is_empty());
}

#[test]
fn after_the_period_it_asks_once_and_go_ends_it() {
    let mut sim = watching_friends();
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "where was it?");
    sim.advance(WINDOW);
    let to_priya = sim.last_dm_to("priya").unwrap();
    sim.react("dm-priya", &to_priya, "priya", "👍");

    let ended = sim.advance(DAY);
    let summary = "Watched for 1 day: 1 time I would have spoken, 1 👍, 0 👎. Say go and I'll start speaking unprompted, or say observe to watch longer.";
    assert_eq!(to(&ended), vec![("priya", summary), ("sam", summary)]);
    assert!(sim.state.is_observing(), "it keeps watching until told");
    assert!(sim.advance(DAY).is_empty());

    let go = sim.dm("priya", "go");
    assert_eq!(
        texts(&go.effects),
        vec!["Done watching. I'll speak up when it helps, within the budget."]
    );
    assert!(
        notices(&go.effects)
            .iter()
            .any(|n| n.contains("Changed by Priya"))
    );
    assert!(!sim.state.is_observing());
    let ended = sim.state.observed.as_ref().unwrap();
    assert_eq!((ended.would_have, ended.up, ended.down), (1, 1, 0));

    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "sam", "what time?");
    let raised = sim.advance(WINDOW);
    assert_eq!(reactions(&raised), vec![(&asked.id, RAISED_HAND)]);
    assert!(dms(&raised).is_empty());
}

#[test]
fn go_from_a_room_drops_hands_raised_only_in_dms() {
    let mut sim = watching_friends();
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "where was it?");
    sim.advance(WINDOW);
    assert_eq!(sim.state.hands.len(), 1);
    let go = sim.say("dinner", "sam", "Q, go");
    assert_eq!(
        texts(&go.effects),
        vec!["Done watching. I'll speak up when it helps, within the budget."]
    );
    assert!(sim.state.hands.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);
    assert!(!any_send(&sim.advance(2 * WINDOW)));
}

#[test]
fn observe_again_restarts_the_watch() {
    let mut sim = watching_friends();
    sim.say("dinner", "priya", "Q, go");
    assert!(!sim.state.is_observing());

    let again = sim.say("dinner", "priya", "Q, watch for 3 more days");
    assert_eq!(
        texts(&again.effects),
        vec!["Watching for 3 days; nothing unprompted until a lead says go."]
    );
    let observe = sim.state.observe.as_ref().unwrap();
    assert_eq!(observe.until.seconds_since(sim.now), 3 * DAY);
    assert_eq!(observe.would_have, 0);

    let longer = sim.say("dinner", "sam", "Q, keep watching");
    assert_eq!(
        texts(&longer.effects),
        vec!["Watching for 1 day; nothing unprompted until a lead says go."]
    );
    let ends = sim
        .state
        .timers
        .values()
        .filter(|t| t.purpose == TimerPurpose::ObserveEnd)
        .count();
    assert_eq!(ends, 1);
}

#[test]
fn at_work_only_leads_and_hosts_hear_and_a_members_go_waits() {
    let mut sim = Sim::observing(Kind::Team);
    sim.feed(Input::Command(Command::SetHosts {
        users: vec!["olu".into()],
    }));
    sim.feed(Input::Command(Command::AddLead { user: "jo".into() }));
    let synced = sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("jo", "Jo", PlatformRole::Regular),
            ("sam", "Sam", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    assert!(
        notices(&synced)
            .iter()
            .any(|n| n.starts_with("Observing for 3 days"))
    );
    let heard: Vec<PlatformUserId> = vec!["jo".into(), "olu".into(), "priya".into()];
    assert_eq!(observers(&sim.state), heard);

    let asked = sim.say("launch", "sam", "Q, go");
    assert_eq!(
        messages(&asked.effects),
        vec![(DEFERRAL_REPLY, Some(&asked.id))]
    );
    assert!(sim.state.is_observing());
    let reply = sim.last_sent_message().unwrap();
    let approved = sim.react("launch", &reply, "priya", CHECK);
    assert_eq!(
        texts(&approved),
        vec![
            "Done watching. I'll speak up when it helps, within the budget. Asked by Sam, approved by Priya."
        ]
    );
    assert!(!sim.state.is_observing());

    let guest = sim.say("launch", "dana", "Q, observe");
    assert_eq!(texts(&guest.effects), vec!["That's one for Jo or Priya."]);
    assert!(!sim.state.is_observing());
}

#[test]
fn a_group_that_watched_once_does_not_start_again_on_a_new_room() {
    let mut sim = watching_friends();
    sim.say("dinner", "sam", "Q, go");
    let synced = sim.room("trips", "trips", &[("sam", "Sam", PlatformRole::Regular)]);
    assert!(notices(&synced).is_empty());
    assert!(!sim.state.is_observing());
}
