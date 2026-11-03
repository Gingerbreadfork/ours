//! Being addressed: the 👀, the model call, the reply, and silence when
//! nobody asked (DESIGN.md sections 6, 7 and 8).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::sim::{
    self, ModelScript, Sim, any_send, messages, model_requests, notices, reactions, texts,
    unreactions,
};
use ours_core::step::{EYES, FAILED_REPLY};
use ours_core::{Effect, Input, Plan, PlannedAction, TimerPurpose, TurnRole};
use ours_types::{InboundEvent, Kind, PlatformRole, Room, RoomKind};

fn friends() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
            ("alex", "Alex", PlatformRole::Regular),
        ],
    );
    sim
}

fn position(effects: &[Effect], wanted: impl Fn(&Effect) -> bool) -> usize {
    effects.iter().position(wanted).expect("effect present")
}

/// No timer is armed but the nightly retention sweep.
fn only_retention(sim: &Sim) -> bool {
    sim.state
        .timers
        .values()
        .all(|t| t.purpose == TimerPurpose::Retention)
}

#[test]
fn addressed_by_name_in_a_friends_group() {
    let mut sim = friends();
    sim.say("dinner", "sam", "dinner this week?");
    sim.say("dinner", "priya", "yes pls");
    sim.model_says("Thursday suits most people so far. Want a poll?");

    let asked = sim.say("dinner", "jo", "Q, what do you reckon?");
    let fx = &asked.effects;

    assert_eq!(reactions(fx), vec![(&asked.id, EYES)]);
    let request = model_requests(fx)[0];
    assert!(request.system.contains("You are Q"), "{}", request.system);
    assert!(request.system.contains("role is lead"));
    assert!(request.system.contains("Room: dinner"));
    assert!(request.system.contains("Monday 2026-11-02 18:00"));
    assert!(request.output_schema["properties"]["message"].is_object());
    let last = request.messages.last().unwrap();
    assert_eq!(last.role, TurnRole::User);
    assert!(last.content.contains("Sam: dinner this week?"));
    assert!(last.content.ends_with("Jo: Q, what do you reckon?"));

    assert_eq!(
        messages(fx),
        vec![(
            "Thursday suits most people so far. Want a poll?",
            Some(&asked.id)
        )]
    );
    assert_eq!(unreactions(fx), vec![(&asked.id, EYES)]);

    let set = fx.iter().find_map(|e| match e {
        Effect::SetTimer {
            id,
            at,
            purpose: TimerPurpose::ModelGuard { request: r },
        } => {
            assert_eq!(r, &request.id);
            assert_eq!(at.seconds_since(sim::START), 30);
            Some(id.clone())
        }
        _ => None,
    });
    let cancelled = fx.iter().find_map(|e| match e {
        Effect::CancelTimer { id } => Some(id.clone()),
        _ => None,
    });
    assert_eq!(set, cancelled);

    let eyes = position(fx, |e| !reactions(std::slice::from_ref(e)).is_empty());
    let ask = position(fx, |e| matches!(e, Effect::AskModel(_)));
    let reply = position(fx, |e| !texts(std::slice::from_ref(e)).is_empty());
    let uneyes = position(fx, |e| !unreactions(std::slice::from_ref(e)).is_empty());
    assert!(eyes < ask && ask < reply && reply < uneyes);

    assert!(sim.state.pending_requests.is_empty());
    assert!(only_retention(&sim));
    assert_eq!(sim.timers_besides_retention(), 0);
}

#[test]
fn forty_messages_about_the_football_get_no_reply() {
    let mut sim = friends();
    let lines = [
        "what a goal",
        "that was never offside",
        "quarter-final next week",
        "Q2 was rough for them",
        "ref's blind",
        "q-final tickets anyone?",
        "queue at the bar was mad",
        "Qatar again?",
    ];
    let speakers = ["sam", "priya", "jo", "alex"];
    let mut all = Vec::new();
    for i in 0..40 {
        let posted = sim.say("dinner", speakers[i % 4], lines[i % lines.len()]);
        all.extend(posted.effects);
        sim.advance(20);
    }
    assert!(!any_send(&all), "{all:?}");
    assert!(model_requests(&all).is_empty());
    assert_eq!(sim.state.rooms[&"dinner".into()].messages.len(), 40);
}

#[test]
fn a_reply_to_its_own_message_is_addressed() {
    let mut sim = friends();
    sim.model_says("Hobson's, 14 March, after Priya's birthday.");
    sim.say(
        "dinner",
        "alex",
        "Q, what was that bar in March with the weird cocktails",
    );
    let answer = sim.last_sent_message().unwrap();

    let human = sim.say("dinner", "sam", "the negroni was a war crime").id;
    let not_addressed = sim.reply("dinner", "jo", "ha, it was", &human);
    assert!(!any_send(&not_addressed.effects));

    sim.model_says("Want it on the list for next time?");
    let follow_up = sim.reply("dinner", "alex", "and the place after?", &answer);
    assert_eq!(reactions(&follow_up.effects), vec![(&follow_up.id, EYES)]);
    assert_eq!(model_requests(&follow_up.effects).len(), 1);
    assert_eq!(
        messages(&follow_up.effects),
        vec![("Want it on the list for next time?", Some(&follow_up.id))]
    );
    let context = &model_requests(&follow_up.effects)[0].messages;
    assert!(
        context
            .iter()
            .any(|t| t.role == TurnRole::Assistant && t.content.contains("Hobson's"))
    );
}

#[test]
fn a_model_that_never_answers_is_owned_up_to() {
    let mut sim = friends();
    sim.script(ModelScript::Silent);
    let asked = sim.say("dinner", "jo", "Q, what's the plan for thursday?");
    assert_eq!(model_requests(&asked.effects).len(), 1);
    assert!(texts(&asked.effects).is_empty());

    assert!(sim.advance(29).is_empty());
    let fired = sim.advance(1);
    assert_eq!(messages(&fired), vec![(FAILED_REPLY, Some(&asked.id))]);
    assert_eq!(unreactions(&fired), vec![(&asked.id, EYES)]);
    assert_eq!(notices(&fired).len(), 1);
    assert!(sim.state.pending_requests.is_empty());
    assert!(only_retention(&sim));

    let request = sim.requests[0].id.clone();
    let late = sim.feed(Input::ModelReply {
        request,
        reply: Ok(sim::plan("Thursday at 7:30.")),
    });
    assert!(late.is_empty());
}

#[test]
fn a_model_error_is_owned_up_to() {
    let mut sim = friends();
    sim.script(ModelScript::Fail("endpoint returned 503".into()));
    let asked = sim.say("dinner", "priya", "Q, how much was the wine?");
    assert_eq!(
        messages(&asked.effects),
        vec![(FAILED_REPLY, Some(&asked.id))]
    );
    assert_eq!(unreactions(&asked.effects), vec![(&asked.id, EYES)]);
    assert!(
        asked
            .effects
            .iter()
            .any(|e| matches!(e, Effect::CancelTimer { .. }))
    );
    assert!(notices(&asked.effects).iter().any(|n| n.contains("503")));
    assert!(sim.state.pending_requests.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);
}

#[test]
fn planned_actions_run_after_the_reply() {
    let mut sim = friends();
    sim.script(ModelScript::Reply(Plan {
        message: Some("Noted.".into()),
        actions: vec![
            PlannedAction::React {
                emoji: "👍".into()
            },
            PlannedAction::Nothing,
            PlannedAction::Say {
                text: "Thursday it is.".into(),
            },
        ],
        cites: Vec::new(),
    }));
    let asked = sim.say("dinner", "sam", "Q, thursday then");
    assert_eq!(
        messages(&asked.effects),
        vec![("Noted.", Some(&asked.id)), ("Thursday it is.", None)]
    );
    assert_eq!(
        reactions(&asked.effects),
        vec![(&asked.id, EYES), (&asked.id, "👍")]
    );
}

#[test]
fn mentions_and_at_names_address_it() {
    let mut sim = friends();
    let mut message = sim.message("dinner", "sam", "<@ours-bot> are we on?");
    message.mentions_me = true;
    let posted = sim.post(message);
    assert_eq!(model_requests(&posted.effects).len(), 1);

    let posted = sim.say("dinner", "sam", "@q undo");
    assert_eq!(
        texts(&posted.effects),
        vec![ours_core::undo::NOTHING_TO_UNDO]
    );
}

#[test]
fn its_own_messages_bots_and_repeats_are_not_answered() {
    let mut sim = friends();
    let own = sim.say("dinner", sim::ME, "Q, this is me talking");
    assert!(own.effects.is_empty());

    let bot = ours_types::Member {
        is_bot: true,
        ..sim::member("dicebot", "DiceBot", PlatformRole::Regular)
    };
    sim.feed(Input::Inbound(InboundEvent::MemberJoined {
        room: "dinner".into(),
        member: bot,
        at: sim.now,
    }));
    assert!(
        sim.say("dinner", "dicebot", "Q, roll a d20")
            .effects
            .is_empty()
    );

    sim.model_says("Friday.");
    let message = sim.message("dinner", "jo", "Q, which day?");
    let first = sim.post(message.clone());
    assert_eq!(texts(&first.effects), vec!["Friday."]);
    let again = sim.post(message);
    assert!(again.effects.is_empty());
}

#[test]
fn a_direct_message_is_always_addressed() {
    let mut sim = friends();
    let dm = Room {
        platform: ours_types::Platform::Sim,
        id: "dm-sam".into(),
        name: None,
        kind: RoomKind::Dm,
        parent: None,
        container: None,
    };
    sim.feed(Input::Inbound(InboundEvent::RoomSync {
        room: dm,
        members: vec![sim::member("sam", "Sam", PlatformRole::Regular)],
    }));
    sim.model_says("Dinner on Thursday.");
    let posted = sim.say("dm-sam", "sam", "anything on this week");
    assert_eq!(texts(&posted.effects), vec!["Dinner on Thursday."]);
}
