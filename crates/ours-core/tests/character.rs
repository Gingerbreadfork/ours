//! Making her fun: the persona in the prompt, chiming in on banter at dial 2
//! and 3, typing while she writes, reactions as answers and the running
//! bits (DESIGN.md sections 3, 7, 8 and 16).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::attention::SHUSH;
use ours_core::chime::CHIME_PROMPT;
use ours_core::leads::DEFERRAL_REPLY;
use ours_core::sim::{
    ClassifierScript, ModelScript, Sim, any_send, banter, classify_requests, messages,
    model_requests, notices, plan, promise, reactions, reacts, texts, typing, unreactions,
};
use ours_core::step::EYES;
use ours_core::{ActionClass, Command, Effect, Input, Plan, PlannedAction, SettingChange};
use ours_types::{Capabilities, Kind, PlatformRole, PlatformRoomId};

const HOUR: i64 = 60 * 60;

/// Friends in #dinner with the dial at 2. Dana is an outside guest. The
/// clock starts Monday 18:00 UTC; quiet hours are 22:00 to 08:00.
fn mates() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
            ("dana", "Dana", PlatformRole::External),
        ],
    );
    sim.feed(Input::Command(Command::ChangeSetting {
        change: SettingChange::DialTo { dial: 2 },
    }));
    sim
}

fn chimes(sim: &Sim) -> u32 {
    sim.state.budget.chimes_today(sim.now, &sim.state.timezone)
}

/// Posts banter at `level` from `user`, with the model's answer scripted.
fn banter_from(sim: &mut Sim, user: &str, text: &str, level: f32) -> ours_core::sim::Posted {
    sim.classify(ClassifierScript::Score(banter(level)));
    sim.say("dinner", user, text)
}

#[test]
fn banter_at_dial_two_gets_a_chime_and_the_reaction_is_applied_and_counted() {
    let mut sim = mates();
    sim.script(ModelScript::Reply(reacts("🎉")));
    let news = banter_from(&mut sim, "sam", "WE GOT THE HOUSE", 0.9);
    let fx = &news.effects;

    let requests = model_requests(fx);
    assert_eq!(requests.len(), 1);
    let request = requests[0];
    assert!(request.system.ends_with(CHIME_PROMPT), "{}", request.system);
    assert!(request.system.contains("Dial 2, a mate."));
    let last = request.messages.last().unwrap();
    assert!(last.content.ends_with("Sam: WE GOT THE HOUSE"));

    assert_eq!(reactions(fx), vec![(&news.id, "🎉")]);
    assert!(texts(fx).is_empty());
    assert!(unreactions(fx).is_empty(), "no 👀 on a chime");
    assert_eq!(chimes(&sim), 1);
    assert_eq!(sim.state.budget.spent.len(), 1);
    assert!(
        notices(fx)
            .iter()
            .any(|n| n.contains("Chimed in on Sam's message in #dinner: banter 0.90, 1 of 3")),
        "{fx:?}"
    );
    assert!(sim.state.pending_requests.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);

    // The reaction is on the record, nobody's request, and undo takes it off.
    let record = sim.state.actions.back().unwrap();
    assert_eq!(record.class, ActionClass::Reaction);
    assert_eq!(record.summary, "my 🎉 on Sam's message");
    assert_eq!(record.requested_by, None);
    let undone = sim.say("dinner", "jo", "Q, undo");
    assert!(unreactions(&undone.effects).contains(&(&news.id, "🎉")));
}

#[test]
fn a_chime_line_is_threaded_and_why_reads_its_trace() {
    let mut sim = mates();
    sim.script(ModelScript::Reply(Plan {
        message: Some("Called it in March.".into()),
        actions: vec![PlannedAction::Mail {
            to: "a@b.c".into(),
            subject: "x".into(),
            body: "y".into(),
        }],
        cites: Vec::new(),
    }));
    let news = banter_from(&mut sim, "priya", "she said yes!!", 0.82);
    assert_eq!(
        messages(&news.effects),
        vec![("Called it in March.", Some(&news.id))]
    );
    assert!(sim.state.holds.is_empty(), "nobody asked for mail");
    let line = sim.last_sent_message().unwrap();
    let trace = sim.state.rooms[&PlatformRoomId::new("dinner")]
        .trace_of(&line)
        .unwrap();
    assert_eq!(trace.rule, "chimed in: banter 0.82");

    sim.script(ModelScript::Silent);
    let why = sim.reply("dinner", "sam", "Q, why", &line);
    assert_eq!(
        texts(&why.effects),
        vec!["Trigger: Priya in #dinner: \"she said yes!!\"\nRule: chimed in: banter 0.82"]
    );
}

#[test]
fn a_question_or_a_speech_from_the_model_is_not_a_chime() {
    let mut sim = mates();
    sim.script(ModelScript::Reply(plan(
        "Who's bringing the cake? And when?",
    )));
    let posted = banter_from(&mut sim, "sam", "birthday tomorrow!!", 0.9);
    assert_eq!(model_requests(&posted.effects).len(), 1);
    assert!(!any_send(&posted.effects));
    assert_eq!(chimes(&sim), 0);
    assert!(sim.state.budget.spent.is_empty());
}

#[test]
fn the_fourth_chime_in_a_day_is_not_made() {
    let mut sim = mates();
    for (i, line) in ["first!", "second!", "third!"].iter().enumerate() {
        sim.script(ModelScript::Reply(reacts("😂")));
        let posted = banter_from(&mut sim, "sam", line, 0.9);
        assert_eq!(reactions(&posted.effects).len(), 1, "{i}");
        sim.advance(HOUR + 60);
    }
    assert_eq!(chimes(&sim), 3);
    sim.script(ModelScript::Reply(reacts("😂")));
    let fourth = banter_from(&mut sim, "sam", "fourth!", 0.95);
    assert_eq!(classify_requests(&fourth.effects).len(), 1);
    assert!(model_requests(&fourth.effects).is_empty());
    assert!(!any_send(&fourth.effects));

    // A new day brings new chimes.
    sim.advance_to(ours_core::sim::instant("2026-11-03T12:00:00Z"));
    sim.script(ModelScript::Reply(reacts("😂")));
    let next = banter_from(&mut sim, "sam", "new day!", 0.9);
    assert_eq!(reactions(&next.effects).len(), 1);
}

#[test]
fn dial_one_never_chimes_and_low_banter_never_does() {
    let mut sim = mates();
    let low = banter_from(&mut sim, "sam", "ha", 0.69);
    assert!(model_requests(&low.effects).is_empty());

    sim.feed(Input::Command(Command::ChangeSetting {
        change: SettingChange::DialTo { dial: 1 },
    }));
    let mut profile = sim.state.profile.clone();
    profile.chimes_per_day = Some(5);
    sim.feed(Input::Command(Command::SetProfile { profile }));
    let posted = banter_from(&mut sim, "sam", "WE GOT THE HOUSE", 1.0);
    assert!(model_requests(&posted.effects).is_empty());
    assert!(!any_send(&posted.effects));
}

#[test]
fn a_guests_message_never_gets_a_chime() {
    let mut sim = mates();
    let posted = banter_from(&mut sim, "dana", "LOL this group", 1.0);
    assert!(model_requests(&posted.effects).is_empty());
    assert!(!any_send(&posted.effects));

    sim.script(ModelScript::Reply(reacts("😂")));
    let member = banter_from(&mut sim, "jo", "LOL this group", 1.0);
    assert_eq!(reactions(&member.effects).len(), 1);
}

#[test]
fn quiet_hours_block_a_chime() {
    let mut sim = mates();
    sim.advance(4 * HOUR + 30 * 60);
    let late = banter_from(&mut sim, "sam", "WE GOT THE HOUSE", 1.0);
    assert!(model_requests(&late.effects).is_empty());

    sim.advance_to(ours_core::sim::instant("2026-11-03T09:00:00Z"));
    sim.script(ModelScript::Reply(reacts("🏠")));
    let morning = banter_from(&mut sim, "sam", "still can't believe it", 0.9);
    assert_eq!(reactions(&morning.effects).len(), 1);
}

#[test]
fn a_shush_stops_chimes_for_the_rest_of_the_day() {
    let mut sim = mates();
    sim.model_says("Told you the blue one.");
    banter_from(&mut sim, "sam", "the sofa arrived and it's PERFECT", 0.9);
    let line = sim.last_sent_message().unwrap();
    sim.react("dinner", &line, "jo", SHUSH);
    assert_eq!(sim.state.budget.left_today(10, sim.now, "UTC"), 0);

    sim.advance(10 * 60);
    let after = banter_from(&mut sim, "priya", "omg the cat is on it already", 1.0);
    assert!(model_requests(&after.effects).is_empty());

    // A chime already asked for lands nowhere once the room says hush.
    sim.advance_to(ours_core::sim::instant("2026-11-03T10:00:00Z"));
    sim.script(ModelScript::Silent);
    let asked = banter_from(&mut sim, "sam", "new day new sofa photos", 0.9);
    let request = model_requests(&asked.effects)[0].id.clone();
    sim.say("dinner", "jo", "Q, shut up");
    let late = sim.feed(Input::ModelReply {
        request,
        reply: Ok(reacts("🛋️")),
    });
    assert!(reactions(&late).is_empty());
    assert!(texts(&late).is_empty());
    assert_eq!(chimes(&sim), 0);
}

#[test]
fn money_and_slipping_promises_never_get_a_chime() {
    let mut sim = mates();
    for text in [
        "I got the wine, 38 lol",
        "settle up time hahaha",
        "who owes me for the pizza 😂",
        "£20 well spent",
    ] {
        let posted = banter_from(&mut sim, "sam", text, 1.0);
        assert!(model_requests(&posted.effects).is_empty(), "{text}");
    }

    sim.promises(vec![promise("Jo", "the tickets", None)]);
    sim.say("dinner", "jo", "I'll get the tickets");
    assert_eq!(sim.state.commitments.len(), 1);
    let slip = banter_from(&mut sim, "jo", "lol I totally forgot again 😂", 1.0);
    assert!(model_requests(&slip.effects).is_empty());

    // The same words from someone who promised nothing are only banter.
    sim.script(ModelScript::Reply(reacts("😂")));
    let other = banter_from(&mut sim, "priya", "lol I totally forgot again 😂", 1.0);
    assert_eq!(reactions(&other.effects).len(), 1);
}

#[test]
fn typing_follows_the_eyes_only_where_the_platform_has_it() {
    let mut sim = mates();
    sim.model_says("Thursday.");
    let plain = sim.say("dinner", "sam", "Q, what do you reckon?");
    assert!(typing(&plain.effects).is_empty());

    sim.capabilities(Capabilities {
        reactions: true,
        typing: true,
        ..Capabilities::default()
    });
    sim.model_says("Friday, then.");
    let asked = sim.say("dinner", "jo", "Q, what about friday?");
    let fx = &asked.effects;
    assert_eq!(typing(fx), vec![&PlatformRoomId::new("dinner")]);
    let at = |wanted: &dyn Fn(&Effect) -> bool| fx.iter().position(wanted).unwrap();
    let eyes = at(&|e| reactions(std::slice::from_ref(e)) == vec![(&asked.id, EYES)]);
    let shown = at(&|e| !typing(std::slice::from_ref(e)).is_empty());
    let ask = at(&|e| matches!(e, Effect::AskModel(_)));
    assert_eq!(shown, eyes + 1);
    assert!(shown < ask);
    assert_eq!(texts(fx), vec!["Friday, then."]);
    assert!(sim.state.pending_actions.is_empty());

    // A chime shows typing too.
    sim.script(ModelScript::Reply(reacts("🎉")));
    let news = banter_from(&mut sim, "priya", "PROMOTED", 0.9);
    assert_eq!(typing(&news.effects).len(), 1);
}

#[test]
fn a_reaction_is_a_complete_answer_to_an_addressed_message() {
    let mut sim = mates();
    sim.script(ModelScript::Reply(reacts("❤️")));
    let thanks = sim.say("dinner", "sam", "Q, thanks for sorting saturday");
    let fx = &thanks.effects;
    assert_eq!(reactions(fx), vec![(&thanks.id, EYES), (&thanks.id, "❤️")]);
    assert_eq!(unreactions(fx), vec![(&thanks.id, EYES)]);
    assert!(texts(fx).is_empty(), "{fx:?}");
    assert!(
        sim.state.budget.spent.is_empty(),
        "an answer is not unprompted"
    );

    // Neither a message nor a reaction is still silence.
    sim.script(ModelScript::Reply(Plan::default()));
    let nothing = sim.say("dinner", "jo", "Q, ok");
    assert_eq!(reactions(&nothing.effects), vec![(&nothing.id, EYES)]);
    assert_eq!(unreactions(&nothing.effects), vec![(&nothing.id, EYES)]);
    assert!(texts(&nothing.effects).is_empty());
}

#[test]
fn the_dial_two_prompt_has_its_examples_and_the_bits() {
    let mut sim = mates();
    sim.say(
        "dinner",
        "sam",
        "Q, you have a bit: the negroni was a war crime",
    );
    sim.model_says("Morning.");
    let asked = sim.say("dinner", "priya", "Q, morning");
    let system = &model_requests(&asked.effects)[0].system;
    for wanted in [
        "Who you are:",
        "You talk like this: Plain and brief",
        "Running bits the group gave you: \"the negroni was a war crime\".",
        "at most once a day, never twice in a row",
        "Your register: Dial 2, a mate.",
        "roast back once",
        "you're basically a spreadsheet with a name",
        "[a photo] finally finished the bookshelf\" You: a 🔥 reaction, message null.",
        "thai or pizza on saturday?",
        "reacting is a complete answer",
        "Brief is not vague.",
        "Never ironic or sarcastic in a message that carries an action.",
        "\"you have a bit: the negroni\"",
    ] {
        assert!(system.contains(wanted), "{wanted}\n{system}");
    }
    assert!(!system.contains('\u{2014}'), "no em-dashes");
}

#[test]
fn a_lead_gives_a_bit_and_settings_lists_it() {
    let mut sim = mates();
    let given = sim.say("dinner", "sam", "Q, you have a bit: Sam's parking");
    assert_eq!(
        texts(&given.effects),
        vec!["New bit: \"Sam's parking\". I'll bring it out when it fits, once a day at most."]
    );
    assert_eq!(sim.state.persona.bits, vec!["Sam's parking".to_owned()]);

    let settings = sim.say("dinner", "jo", "Q, settings");
    let text = texts(&settings.effects)[0];
    assert!(text.lines().count() <= 6, "{text}");
    assert!(
        text.contains(
            "Dial at 2: a mate, chiming in 3 times a day at most. Bits: \"Sam's parking\"."
        ),
        "{text}"
    );
    let charter = ours_core::render_charter(&sim.state);
    assert!(
        charter.contains("- Running bits: Sam's parking"),
        "{charter}"
    );
    assert!(charter.contains("- Traits: none"));
    assert!(charter.contains("- Chime-ins: 3 a day, on banter of 0.70 or more"));

    let dropped = sim.say("dinner", "sam", "Q, drop the bit about parking");
    assert_eq!(
        texts(&dropped.effects),
        vec!["Dropped the bit about parking."]
    );
    assert!(sim.state.persona.bits.is_empty());

    // A guest is declined and "be more sarcastic" goes to the model.
    let guest = sim.say("dinner", "dana", "Q, you have a bit: Dana rules");
    assert!(sim.state.persona.bits.is_empty());
    assert_eq!(texts(&guest.effects).len(), 1);
    sim.model_says("I'll see what I can do.");
    let ask = sim.say("dinner", "sam", "Q, be more sarcastic");
    assert_eq!(model_requests(&ask.effects).len(), 1);
}

#[test]
fn in_a_club_a_members_bit_waits_for_a_lead() {
    let mut sim = Sim::new(Kind::Club);
    sim.room(
        "club",
        "club",
        &[
            ("lee", "Lee", PlatformRole::Regular),
            ("ana", "Ana", PlatformRole::Regular),
        ],
    );
    sim.feed(Input::Command(Command::AddLead { user: "ana".into() }));
    let asked = sim.say("club", "lee", "Q, you have a bit: the minibus");
    assert_eq!(texts(&asked.effects), vec![DEFERRAL_REPLY]);
    assert!(sim.state.persona.bits.is_empty());
    let ask = sim.last_sent_message().unwrap();
    sim.react("club", &ask, "ana", "✅");
    assert_eq!(sim.state.persona.bits, vec!["the minibus".to_owned()]);
}

#[test]
fn a_chime_in_flight_survives_a_snapshot_and_replays() {
    let mut sim = mates();
    sim.capabilities(Capabilities {
        typing: true,
        ..Capabilities::default()
    });
    sim.say("dinner", "sam", "Q, you have a bit: the negroni");
    sim.script(ModelScript::Reply(reacts("🎉")));
    banter_from(&mut sim, "sam", "engaged!!", 0.9);
    sim.script(ModelScript::Silent);
    let asked = banter_from(&mut sim, "priya", "and I got the job", 0.9);
    let request = model_requests(&asked.effects)[0].id.clone();

    let json = sim.state.snapshot().unwrap();
    let mut restored = ours_core::GroupState::restore(&json).unwrap();
    assert_eq!(restored, sim.state);
    assert_eq!(
        restored.pending_requests[&request].answering,
        ours_core::Answering::Chime
    );
    let fx = ours_core::step(
        &mut restored,
        Input::ModelReply {
            request,
            reply: Ok(reacts("🥂")),
        },
        sim.now,
    );
    assert_eq!(reactions(&fx), vec![(&asked.id, "🥂")]);
    assert_eq!(restored.budget.chimes.len(), 2);

    let mut replayed = ours_core::GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(ours_core::step(&mut replayed, input, at));
    }
    assert_eq!(replayed, sim.state);
    assert_eq!(effects, sim.effects);
}
