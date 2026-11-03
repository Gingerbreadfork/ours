//! Knowing when to shut up: scores for every message, the 🙋 before an
//! unprompted answer, the interjection budget, and the 🤫 (DESIGN.md
//! section 7).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::attention::{RAISED_HAND, SHUSH, THUMBS_UP};
use ours_core::sim::{
    ClassifierScript, Sim, answerable_question, any_send, classify_requests, extract_requests,
    messages, model_requests, notices, reactions, texts, unreactions,
};
use ours_core::step::EYES;
use ours_core::{Input, rules_score};
use ours_types::{Kind, MessageKind, PlatformRole, Score, ScoreSource};

const WINDOW: i64 = 20 * 60;

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

fn left_today(sim: &Sim) -> u32 {
    sim.state.budget.left_today(
        sim.state.profile.budget_per_day,
        sim.now,
        &sim.state.timezone,
    )
}

#[test]
fn every_message_nobody_addressed_is_scored_with_its_context() {
    let mut sim = friends();
    sim.say("dinner", "priya", "Hobson's at half seven");
    sim.advance(90);
    sim.model_says("The 14th.");
    sim.say("dinner", "jo", "Q, what's the date?");
    sim.advance(30);
    sim.classify(ClassifierScript::Silent);
    let asked = sim.say("dinner", "sam", "what time was it again?");

    let requests = classify_requests(&asked.effects);
    assert_eq!(requests.len(), 1);
    let request = requests[0];
    assert_eq!(request.message, asked.id);
    assert_eq!(request.speaker, "Sam");
    assert_eq!(request.agent_name, "Q");
    assert_eq!(request.rules.kind, MessageKind::Question);
    assert_eq!(request.rules.unanswered_question, 0.6);
    assert!(!request.rules.answerable_from_context);
    assert_eq!(request.rules.source, ScoreSource::Rules);
    let context: Vec<(&str, &str, i64)> = request
        .context
        .iter()
        .map(|l| (l.speaker.as_str(), l.text.as_str(), l.seconds_before))
        .collect();
    assert_eq!(
        context,
        vec![
            ("Priya", "Hobson's at half seven", 120),
            ("Jo", "Q, what's the date?", 30),
            ("Q", "The 14th.", 30),
        ]
    );
    let stored = sim.state.rooms[&"dinner".into()].score_of(&asked.id);
    assert_eq!(stored, Some(&request.rules));

    // The addressed message took today's path: no classify request for it.
    assert_eq!(sim.classified.len(), 2);

    // A model's answer replaces the stored score.
    sim.feed(Input::Scored {
        room: "dinner".into(),
        message: asked.id.clone(),
        score: answerable_question(),
    });
    let stored = sim.state.rooms[&"dinner".into()].score_of(&asked.id);
    assert_eq!(stored, Some(&answerable_question()));
}

#[test]
fn an_unanswered_question_gets_a_hand_then_an_answer_after_a_reaction() {
    let mut sim = friends();
    sim.say("dinner", "priya", "Hobson's, half seven, thursday");
    sim.advance(60);
    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "sam", "what time was it again?");
    assert!(!any_send(&asked.effects));
    assert_eq!(sim.timers_besides_retention(), 1);

    assert!(sim.advance(WINDOW - 1).is_empty());
    let raised = sim.advance(1);
    assert_eq!(reactions(&raised), vec![(&asked.id, RAISED_HAND)]);
    assert!(texts(&raised).is_empty());
    assert!(model_requests(&raised).is_empty());
    assert_eq!(sim.state.budget.spent.len(), 1);

    sim.model_says("Half seven at Hobson's.");
    let invited = sim.react("dinner", &asked.id, "jo", "👍");
    let request = model_requests(&invited)[0];
    assert!(request.system.contains("Nobody addressed you"));
    let last = request.messages.last().unwrap();
    assert!(last.content.ends_with("Sam: what time was it again?"));
    assert_eq!(
        messages(&invited),
        vec![("Half seven at Hobson's.", Some(&asked.id))]
    );
    assert_eq!(unreactions(&invited), vec![(&asked.id, RAISED_HAND)]);
    assert!(!reactions(&invited).iter().any(|(_, e)| *e == EYES));
    assert_eq!(sim.state.budget.spent.len(), 2);
    assert!(sim.state.hands.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);
}

#[test]
fn a_raised_hand_answers_anyway_when_the_window_passes_again() {
    let mut sim = friends();
    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "sam", "anyone know the wifi password");
    sim.advance(WINDOW);
    // The asker adding to the question is not an answer.
    sim.classify(ClassifierScript::Rules);
    sim.say("dinner", "sam", "anyone?");
    sim.model_says("It's on the fridge: gull-harbour-42.");
    assert!(sim.advance(WINDOW - 1).is_empty());
    let answered = sim.advance(1);
    assert_eq!(
        messages(&answered),
        vec![("It's on the fridge: gull-harbour-42.", Some(&asked.id))]
    );
    assert_eq!(unreactions(&answered), vec![(&asked.id, RAISED_HAND)]);
    assert_eq!(sim.timers_besides_retention(), 0);
}

#[test]
fn a_human_answer_in_between_cancels_the_hand() {
    let mut sim = friends();
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "what time is kickoff?");
    sim.advance(60);
    sim.say("dinner", "jo", "8pm");
    let window = sim.advance(WINDOW);
    assert!(!any_send(&window), "{window:?}");
    assert!(sim.state.hands.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);

    sim.classify(ClassifierScript::Answerable);
    let after = sim.say("dinner", "alex", "where are we meeting?");
    let raised = sim.advance(WINDOW);
    assert_eq!(reactions(&raised), vec![(&after.id, RAISED_HAND)]);
    sim.say("dinner", "priya", "outside the station");
    let window = sim.advance(WINDOW);
    assert_eq!(unreactions(&window), vec![(&after.id, RAISED_HAND)]);
    assert!(texts(&window).is_empty());
    assert!(model_requests(&window).is_empty());

    // A reaction after a human has answered takes the hand down, nothing more.
    sim.classify(ClassifierScript::Answerable);
    let third = sim.say("dinner", "alex", "who's bringing the ball?");
    sim.advance(WINDOW);
    sim.say("dinner", "jo", "me");
    let reacted = sim.react("dinner", &third.id, "alex", "🙏");
    assert_eq!(unreactions(&reacted), vec![(&third.id, RAISED_HAND)]);
    assert!(model_requests(&reacted).is_empty());
}

#[test]
fn rules_alone_never_raise_a_hand() {
    let mut sim = friends();
    let asked = sim.say("dinner", "sam", "what time was it again?");
    assert_eq!(classify_requests(&asked.effects).len(), 1);
    assert_eq!(sim.timers_besides_retention(), 0);
    assert!(!any_send(&sim.advance(2 * WINDOW)));
}

#[test]
fn when_the_budget_runs_out_it_stays_silent() {
    let mut sim = friends();
    let mut asked = Vec::new();
    for text in [
        "what time was it?",
        "which bar was it?",
        "who booked?",
        "how much was it?",
    ] {
        sim.classify(ClassifierScript::Answerable);
        asked.push(sim.say("dinner", "sam", text).id);
        sim.advance(60);
    }
    let raised = sim.advance(WINDOW);
    let hands: Vec<_> = reactions(&raised).into_iter().map(|(m, _)| m).collect();
    assert_eq!(hands, vec![&asked[0], &asked[1], &asked[2]]);
    assert_eq!(sim.state.budget.this_hour(sim.now), 3);

    // The window passes again inside the same hour: no answers, hands down.
    let again = sim.advance(WINDOW);
    assert!(texts(&again).is_empty());
    assert!(model_requests(&again).is_empty());
    assert_eq!(unreactions(&again).len(), 3);
    assert!(sim.state.hands.is_empty());

    // Addressed questions are answered whatever the budget says.
    sim.model_says("Forty-two each.");
    let direct = sim.say("dinner", "sam", "Q, how much was it?");
    assert_eq!(texts(&direct.effects), vec!["Forty-two each."]);
}

#[test]
fn quiet_hours_keep_the_hand_down() {
    let mut sim = friends();
    sim.advance(4 * 60 * 60 - WINDOW / 2);
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "sam", "what was the address?");
    let late = sim.advance(WINDOW);
    assert!(!any_send(&late));
    assert!(sim.state.hands.is_empty());
}

#[test]
fn a_shush_ends_unprompted_speech_for_the_day_and_gets_only_a_thumb() {
    let mut sim = friends();
    sim.model_says("Thursday works for four of you.");
    sim.say("dinner", "jo", "Q, what do you reckon?");
    let answer = sim.last_sent_message().unwrap();
    assert_eq!(left_today(&sim), 10);

    let shushed = sim.react("dinner", &answer, "priya", SHUSH);
    assert_eq!(reactions(&shushed), vec![(&answer, THUMBS_UP)]);
    assert!(texts(&shushed).is_empty());
    assert_eq!(left_today(&sim), 0);
    assert!(sim.state.shushed.contains_key(&"priya".into()));
    let notice = notices(&shushed)[0];
    assert!(
        notice.starts_with("Shushed by Priya in #dinner: nothing unprompted for the rest of today"),
        "{notice}"
    );

    // An unanswered question gets no hand for the rest of the day.
    sim.classify(ClassifierScript::Answerable);
    sim.say("dinner", "alex", "what time was it?");
    let window = sim.advance(WINDOW);
    assert!(reactions(&window).is_empty(), "{window:?}");
    assert!(sim.state.hands.is_empty());

    // Words do the same, and an addressed question is still answered.
    let said = sim.say("dinner", "sam", "Q, shut up");
    assert_eq!(
        reactions(&said.effects),
        vec![(&said.id, EYES), (&said.id, THUMBS_UP)]
    );
    assert!(texts(&said.effects).is_empty());
    assert!(model_requests(&said.effects).is_empty());
    assert_eq!(left_today(&sim), 0);
    assert_eq!(notices(&said.effects).len(), 1);
    sim.model_says("Seven.");
    let asked = sim.say("dinner", "alex", "Q, what time?");
    assert_eq!(texts(&asked.effects), vec!["Seven."]);

    // A 🤫 on someone else's message changes nothing.
    let human = sim.say("dinner", "alex", "lol").id;
    assert!(sim.react("dinner", &human, "jo", SHUSH).is_empty());

    // Tomorrow the budget is whole again.
    sim.advance(24 * 60 * 60);
    assert_eq!(left_today(&sim), 10);
}

#[test]
fn a_guests_shush_stops_it_too() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    sim.advance_to(ours_core::sim::instant("2026-11-03T10:00:00Z"));
    sim.model_says("The review is Thursday at 2.");
    sim.say("launch", "dana", "Q, when's the review?");
    let answer = sim.last_sent_message().unwrap();
    let shushed = sim.react("launch", &answer, "dana", SHUSH);
    assert_eq!(reactions(&shushed), vec![(&answer, THUMBS_UP)]);
    assert!(sim.state.budget.shushed_today(sim.now, &sim.state.timezone));
    assert_eq!(left_today(&sim), 0);
}

#[test]
fn a_shush_takes_down_raised_hands_in_the_room() {
    let mut sim = friends();
    sim.model_says("Noted.");
    sim.say("dinner", "jo", "Q, noted?");
    let answer = sim.last_sent_message().unwrap();
    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "sam", "what time was it?");
    sim.advance(WINDOW);
    assert_eq!(sim.state.hands.len(), 1);
    let shushed = sim.react("dinner", &answer, "alex", "🤫");
    assert_eq!(unreactions(&shushed), vec![(&asked.id, RAISED_HAND)]);
    assert!(sim.state.hands.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);
    assert!(texts(&sim.advance(WINDOW)).is_empty());
}

#[test]
fn a_guest_cannot_shush_but_a_member_can() {
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
    let guest = sim.say("launch", "dana", "Q, quiet");
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert!(sim.state.budget.cut.is_none());
    let member = sim.say("launch", "sam", "Q, enough");
    assert!(texts(&member.effects).is_empty());
    assert!(sim.state.budget.cut.is_some());
}

#[test]
fn scores_lead_to_notices_and_nothing_in_the_room() {
    let mut sim = friends();
    sim.extracts(vec![ours_core::sim::self_draft(
        "Sam",
        "is",
        "vegetarian",
        "I'm vegetarian now",
    )]);
    let fact = sim.say("dinner", "sam", "I'm vegetarian now");
    assert_eq!(
        notices(&fact.effects),
        vec!["remembered: Sam is vegetarian, from #dinner"]
    );
    // A promise goes to the extractor, which found nothing to keep here.
    let promise = sim.say("dinner", "jo", "I'll get the tickets");
    assert_eq!(extract_requests(&promise.effects).len(), 1);
    assert!(notices(&promise.effects).is_empty());

    sim.classify(ClassifierScript::Score(Score {
        plan_forming: 0.9,
        source: ScoreSource::SmallModel {
            model: "sim".into(),
        },
        ..Score::default()
    }));
    let plan = sim.say("dinner", "alex", "dinner friday?");
    assert_eq!(
        notices(&plan.effects),
        vec!["Plan forming in #dinner; polls arrive with decisions."]
    );

    // Two replies put the rules at the plan-forming weight, which is enough
    // to try a poll; with no options to offer it stays a notice.
    sim.say("dinner", "priya", "yes");
    let joined = sim.say("dinner", "jo", "same");
    let rules = sim.state.rooms[&"dinner".into()]
        .score_of(&joined.id)
        .unwrap();
    assert_eq!(rules.plan_forming, 0.7);
    assert_eq!(
        notices(&joined.effects),
        vec!["Plan forming in #dinner; polls arrive with decisions."]
    );

    for effects in [&fact.effects, &promise.effects, &plan.effects] {
        assert!(!any_send(effects));
    }
}

#[test]
fn a_shushed_members_plans_stay_low_whatever_the_model_says() {
    let mut sim = friends();
    sim.model_says("Done.");
    sim.say("dinner", "jo", "Q, done?");
    let answer = sim.last_sent_message().unwrap();
    sim.react("dinner", &answer, "sam", SHUSH);

    sim.say("dinner", "sam", "pub tonight?");
    sim.say("dinner", "priya", "maybe");
    let message = sim.message("dinner", "jo", "in");
    let rules = rules_score(&sim.state, &"dinner".into(), &message);
    assert_eq!(rules.plan_forming, 0.35);
    sim.classify(ClassifierScript::Score(Score {
        plan_forming: 0.95,
        ..rules
    }));
    let posted = sim.post(message);
    assert!(notices(&posted.effects).is_empty());
    let stored = sim.state.rooms[&"dinner".into()]
        .score_of(&posted.id)
        .unwrap();
    assert_eq!(stored.plan_forming, 0.35);

    // A week later the weight is back.
    sim.advance(7 * 24 * 60 * 60 + 1);
    sim.say("dinner", "sam", "pub tonight?");
    sim.say("dinner", "priya", "maybe");
    let message = sim.message("dinner", "jo", "in");
    assert_eq!(
        rules_score(&sim.state, &"dinner".into(), &message).plan_forming,
        0.7
    );
}
