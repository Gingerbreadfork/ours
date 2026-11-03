//! The first conversation: questions by DM to whoever added the agent, then
//! one line to the room (DESIGN.md section 7).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::sim::{Sim, dms, member, messages, model_requests, notices, texts};
use ours_core::{Command, Effect, Input, LeadsRule, Stage, render_charter, settings_summary};
use ours_types::{
    InboundEvent, Kind, Member, OutboundAction, Platform, PlatformRole, PlatformRoomId, Room,
    RoomKind,
};

const NAME_QUESTION: &str = "Hi, I'm Q. What should people call me here? Say a name, or keep.";

/// Olu owns #general and has been asked the name question, which is returned.
fn asked_by_olu() -> (Sim, String) {
    let mut sim = Sim::new(Kind::Friends);
    let synced = sim.room(
        "general",
        "general",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
        ],
    );
    let sent = dms(&synced);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.as_str(), "olu");
    (sim, sent[0].1.to_owned())
}

/// Olu kept the name and has been asked the kind question, which is returned.
fn added_by_olu() -> (Sim, String) {
    let (mut sim, first) = asked_by_olu();
    assert_eq!(first, NAME_QUESTION);
    let kept = sim.dm("olu", "keep");
    let sent = texts(&kept.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
    (sim, sent[0].to_owned())
}

#[test]
fn the_name_is_asked_first_and_threads_into_what_follows() {
    let (mut sim, first) = asked_by_olu();
    assert_eq!(first, NAME_QUESTION);
    assert_eq!(sim.state.setup.stage, Stage::AskedName);

    let named = sim.dm("olu", "robin");
    let text = texts(&named.effects);
    assert_eq!(text.len(), 1);
    assert!(
        text[0].starts_with("Thanks, Robin it is. What kind of group is this?\n1. Friends:"),
        "{}",
        text[0]
    );
    assert_eq!(sim.state.agent_name, "Robin");
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
    assert!(
        notices(&named.effects).contains(&"First conversation: agent name set to Robin by Olu.")
    );
    assert!(model_requests(&named.effects).is_empty());

    sim.dm("olu", "1");
    sim.dm("olu", "keep");
    let done = sim.dm("olu", "keep");
    let room_line = texts(&done.effects)[1];
    assert!(
        room_line.starts_with("I'm Robin, this group's agent."),
        "{room_line}"
    );
    let general = PlatformRoomId::new("general");
    let summary = settings_summary(&sim.state, &general, sim.now);
    let top = summary.lines().next().unwrap();
    assert!(top.starts_with("A friends group; I'm Robin"), "{top}");
    assert!(render_charter(&sim.state).contains("- Agent name: Robin\n"));

    // The new name is what addresses it in the room.
    sim.model_says("Here.");
    let asked = sim.say("general", "sam", "Robin, are you there?");
    assert_eq!(model_requests(&asked.effects).len(), 1);
}

#[test]
fn keep_or_q_keeps_q() {
    for reply in ["keep", "Q", "q is fine", "skip"] {
        let (mut sim, _) = asked_by_olu();
        let kept = sim.dm("olu", reply);
        let text = texts(&kept.effects);
        assert!(
            text[0].starts_with("Thanks, Q it is. What kind of group is this?"),
            "{reply}: {text:?}"
        );
        assert_eq!(sim.state.agent_name, "Q");
        assert_eq!(sim.state.setup.stage, Stage::AskedKind);
        assert!(
            !notices(&kept.effects)
                .iter()
                .any(|n| n.contains("agent name"))
        );
    }
}

#[test]
fn a_reply_that_is_not_a_name_is_asked_about_once_then_q_stays() {
    let (mut sim, _) = asked_by_olu();
    let odd = sim.dm("olu", "something friendly but not too cute");
    assert_eq!(
        texts(&odd.effects),
        vec![
            "A short name works best: one or two words. What should people call me here? Say a name, or keep."
        ]
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedName);
    assert!(model_requests(&odd.effects).is_empty());

    let again = sim.dm("olu", "I really can't decide on one");
    let text = texts(&again.effects);
    assert!(
        text[0].starts_with("I'll stay Q for now. What kind of group is this?"),
        "{text:?}"
    );
    assert_eq!(sim.state.agent_name, "Q");
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);

    // A good name after one bad reply still takes.
    let (mut sim, _) = asked_by_olu();
    sim.dm("olu", "a name that is far too long for this");
    sim.dm("olu", "call you Pip");
    assert_eq!(sim.state.agent_name, "Pip");
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
}

#[test]
fn a_name_from_the_host_skips_the_name_question() {
    let mut sim = Sim::new(Kind::Friends);
    sim.named_by_host("Sarah");
    let synced = sim.room(
        "general",
        "general",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    let sent = dms(&synced);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0]
            .1
            .starts_with("Hi, I'm Sarah. What kind of group is this?"),
        "{}",
        sent[0].1
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
    let skipped = sim.dm("olu", "skip");
    let room_line = texts(&skipped.effects)[1];
    assert!(
        room_line.starts_with("I'm Sarah, this group's agent."),
        "{room_line}"
    );
    assert_eq!(sim.state.agent_name, "Sarah");
}

#[test]
fn the_first_conversation_runs_to_done_through_dms() {
    let (mut sim, first) = added_by_olu();
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 10, "{first}");
    assert_eq!(lines[0], "Thanks, Q it is. What kind of group is this?");
    assert_eq!(lines[1], "1. Friends: plans, the tab, what was said.");
    assert_eq!(
        lines[3],
        "3. Team at work: commitments, decisions with a record, a digest."
    );
    assert_eq!(
        lines[8],
        "8. Class or cohort: deadlines, scheduling, answers."
    );
    assert_eq!(lines[9], "Reply with a number, or skip.");
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);

    // Anything that is not an answer goes to the model as any DM would.
    sim.model_says("Pick the closest; you can change it later.");
    let unsure = sim.dm("olu", "not sure, what's the difference?");
    assert_eq!(model_requests(&unsure.effects).len(), 1);
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);

    let kind = sim.dm("olu", "3");
    assert_eq!(
        texts(&kind.effects),
        vec![
            "Two voices to pick from: clerk, quiet and exact, it states what was decided and asks only what it needs; host, warm and formal, with courteous full sentences and a welcome for anyone new. Say one, or 'keep' for the plain default."
        ]
    );
    assert_eq!(sim.state.kind, Kind::Team);
    assert_eq!(sim.state.setup.stage, Stage::AskedPersona);
    assert!(model_requests(&kind.effects).is_empty());

    let voice = sim.dm("olu", "Host");
    assert_eq!(
        texts(&voice.effects),
        vec![
            "Voice: host (dial 0). Who should I take instructions from? Right now: Olu (platform admins). Reply with names, 'everyone', or 'keep'."
        ]
    );
    assert_eq!(sim.state.persona_name.as_deref(), Some("host"));
    assert_eq!(
        sim.state.persona,
        ours_core::starter("host").unwrap().persona
    );
    assert_eq!(sim.state.profile.dial, 0);
    assert_eq!(sim.state.setup.stage, Stage::AskedLeads);

    let leads = sim.dm("olu", "Sam and Priya");
    let sent = messages(&leads.effects);
    assert_eq!(sent.len(), 2);
    let (summary, _) = sent[0];
    assert_eq!(summary.lines().count(), 5, "{summary}");
    assert!(summary.contains("at most 1 an hour and 4 a day, never between 22:00 and 08:00"));
    assert!(summary.ends_with("Change any of it with a sentence, any time."));
    let room_line = sent[1].0;
    assert_eq!(room_line.lines().count(), 1);
    assert_eq!(
        room_line,
        "I'm Q, this group's agent. I keep plans, decisions and commitments from these rooms, take instructions from Priya and Sam, and anyone can DM me to see or delete what I hold on them. Whoever runs me can see everything."
    );
    let general = PlatformRoomId::new("general");
    let to_room = leads.effects.iter().any(|e| {
        matches!(e, Effect::Send(o) if matches!(
            &o.action,
            OutboundAction::SendMessage { room, .. } if room == &general
        ))
    });
    assert!(to_room);
    assert_eq!(sim.state.setup.stage, Stage::Done);
    assert_eq!(sim.state.profile.leads, LeadsRule::Named);
    assert!(sim.state.leads.contains(&"sam".into()));
    assert!(sim.state.leads.contains(&"priya".into()));
    assert!(
        notices(&leads.effects)
            .iter()
            .any(|n| n.contains("Leads are now"))
    );

    // Done means done: later DMs are ordinary.
    sim.model_says("Hello.");
    let later = sim.dm("olu", "1");
    assert_eq!(texts(&later.effects), vec!["Hello."]);
    assert_eq!(sim.state.kind, Kind::Team);
}

#[test]
fn everyone_keep_and_names_that_do_not_resolve() {
    let (mut sim, _) = added_by_olu();
    sim.dm("olu", "household");
    assert_eq!(sim.state.kind, Kind::Household);
    let plain = sim.dm("olu", "keep");
    assert!(texts(&plain.effects)[0].starts_with("Who should I take instructions from?"));
    assert_eq!(sim.state.persona_name, None);
    let unknown = sim.dm("olu", "Zed");
    assert_eq!(
        texts(&unknown.effects),
        vec!["I can't find \"zed\" in this room."]
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedLeads);
    let kept = sim.dm("olu", "keep");
    assert_eq!(messages(&kept.effects).len(), 2);
    assert_eq!(sim.state.profile.leads, LeadsRule::Everyone);
    assert_eq!(sim.state.setup.stage, Stage::Done);

    let (mut sim, _) = added_by_olu();
    sim.dm("olu", "a committee");
    assert_eq!(sim.state.profile.leads, LeadsRule::Named);
    sim.dm("olu", "2");
    assert_eq!(sim.state.persona_name.as_deref(), Some("host"));
    sim.dm("olu", "everyone");
    assert_eq!(sim.state.profile.leads, LeadsRule::Everyone);
    assert_eq!(sim.state.setup.stage, Stage::Done);
}

#[test]
fn skip_ends_it_but_the_room_still_hears_who_it_is() {
    let (mut sim, _) = added_by_olu();
    let skipped = sim.dm("olu", "skip");
    assert_eq!(
        texts(&skipped.effects),
        vec![
            "Skipped. I'll use the defaults for a friends group; change any of it with a sentence.",
            "I'm Q, this group's agent. I keep plans, decisions and commitments from these rooms, take instructions from everyone here, and anyone can DM me to see or delete what I hold on them. Whoever runs me can see everything.",
        ]
    );
    assert_eq!(sim.state.setup.stage, Stage::Done);
    assert_eq!(sim.state.kind, Kind::Friends);
}

#[test]
fn a_host_is_asked_before_the_owner() {
    let mut sim = Sim::new(Kind::Friends);
    sim.feed(Input::Command(Command::SetHosts {
        users: vec!["hal".into()],
    }));
    let synced = sim.room(
        "general",
        "general",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("hal", "Hal", PlatformRole::Regular),
        ],
    );
    let to: Vec<&str> = dms(&synced).iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(to, vec!["hal"]);

    // The host's own DM is the one that answers.
    sim.model_says("Sure.");
    let olu = sim.dm("olu", "3");
    assert_eq!(texts(&olu.effects), vec!["Sure."]);
    assert_eq!(sim.state.kind, Kind::Friends);
    sim.dm("hal", "keep");
    sim.dm("hal", "club");
    assert_eq!(sim.state.kind, Kind::Club);
    sim.dm("hal", "the keeper");
    assert_eq!(sim.state.persona_name.as_deref(), Some("keeper"));
    let done = sim.dm("hal", "keep");
    let room_line = texts(&done.effects)[1];
    assert!(
        room_line.ends_with("Hal runs me and can see everything."),
        "{room_line}"
    );
}

#[test]
fn a_bot_owner_is_never_asked() {
    let mut sim = Sim::new(Kind::Friends);
    let room = Room {
        platform: Platform::Sim,
        id: "general".into(),
        name: Some("general".into()),
        kind: RoomKind::Channel,
        parent: None,
        container: None,
    };
    let members = vec![
        Member {
            is_bot: true,
            ..member("dicebot", "DiceBot", PlatformRole::Owner)
        },
        member("sam", "Sam", PlatformRole::Admin),
    ];
    let synced = sim.feed(Input::Inbound(InboundEvent::RoomSync { room, members }));
    assert!(dms(&synced).is_empty());
    assert_eq!(sim.state.setup.stage, Stage::NotStarted);
}

#[test]
fn the_voice_question_offers_the_kinds_two_and_takes_any_starter() {
    let (mut sim, _) = added_by_olu();
    let kind = sim.dm("olu", "friends");
    assert_eq!(
        texts(&kind.effects),
        vec![
            "Two voices to pick from: keeper, dry and brief, with a little warmth when things go right; regular, plain and dry, it says little and misses nothing. Say one, or 'keep' for the plain default."
        ]
    );

    // Anything else is an ordinary DM.
    sim.model_says("Either works.");
    let unsure = sim.dm("olu", "which is funnier?");
    assert_eq!(texts(&unsure.effects), vec!["Either works."]);
    assert_eq!(sim.state.setup.stage, Stage::AskedPersona);

    // A dial-2 voice may be picked, and the dial stays where the kind puts it.
    let mate = sim.dm("olu", "mate");
    let text = texts(&mate.effects)[0];
    assert!(
        text.starts_with(
            "Voice: mate, written for dial 2. The dial stays at 1 for a friends group until a lead says dial 2. Who should I take instructions from?"
        ),
        "{text}"
    );
    assert_eq!(sim.state.profile.dial, 1);
    assert!(
        notices(&mate.effects)
            .iter()
            .any(|n| n.starts_with("First conversation: Voice: mate"))
    );

    // Skip still ends it at the voice question.
    let (mut sim, _) = added_by_olu();
    sim.dm("olu", "3");
    let skipped = sim.dm("olu", "skip");
    assert_eq!(texts(&skipped.effects).len(), 2);
    assert_eq!(sim.state.setup.stage, Stage::Done);
    assert_eq!(sim.state.persona_name, None);
}

#[test]
fn a_refused_dm_asks_in_the_room_and_their_hello_gets_the_question() {
    let mut sim = Sim::new(Kind::Friends);
    sim.refuse_dms_to("olu");
    let synced = sim.room(
        "general",
        "general",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    assert_eq!(dms(&synced).len(), 1);
    let general = PlatformRoomId::new("general");
    let asked_in_room = synced.iter().any(|e| {
        matches!(e, Effect::Send(o) if matches!(
            &o.action,
            OutboundAction::SendMessage { room, text, .. } if room == &general
                && text == "Olu, open a chat with me and say hi so I can ask you a few things."
        ))
    });
    assert!(asked_in_room, "{synced:?}");
    assert_eq!(texts(&synced).len(), 1);
    assert_eq!(sim.state.setup.stage, Stage::AskedName);
    assert_eq!(sim.state.setup.pending_retry, Some("olu".into()));
    assert!(
        notices(&synced)
            .iter()
            .any(|n| n.contains("could not DM Olu"))
    );

    // Telegram's /start arrives as "hello"; anything from them will do.
    let hello = sim.dm("olu", "hello");
    let sent = dms(&hello.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.as_str(), "olu");
    assert_eq!(sent[0].1, NAME_QUESTION);
    assert!(texts(&hello.effects).is_empty());
    assert!(model_requests(&hello.effects).is_empty());
    assert_eq!(sim.state.setup.pending_retry, None);
    assert_eq!(sim.state.setup.asking, None);

    // Then it carries on as normal.
    sim.dm("olu", "keep");
    sim.dm("olu", "3");
    assert_eq!(sim.state.kind, Kind::Team);
    assert_eq!(sim.state.setup.stage, Stage::AskedPersona);
    sim.model_says("Hello again.");
    let chat = sim.dm("olu", "hello");
    assert!(dms(&chat.effects).is_empty());
    assert_eq!(texts(&chat.effects), vec!["Hello again."]);
}

/// Olu owns #launch and is asked; Priya leads by admin rights; Sam is a member.
fn launch_added_by_olu() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    assert_eq!(sim.state.setup.with, Some("olu".into()));
    sim
}

#[test]
fn a_lead_may_take_setup_over_once_the_adder_has_had_three_days() {
    let mut sim = launch_added_by_olu();
    let early = sim.dm("priya", "setup");
    assert_eq!(
        texts(&early.effects),
        vec![
            "I've asked Olu; if they haven't answered by 2026-11-05 18:00, say setup again and I'll ask you."
        ]
    );
    assert_eq!(sim.state.setup.with, Some("olu".into()));

    let member = sim.dm("sam", "setup");
    assert_eq!(texts(&member.effects), vec!["That's one for Olu or Priya."]);
    assert_eq!(sim.state.setup.with, Some("olu".into()));

    sim.advance(3 * 24 * 60 * 60);
    let taken = sim.dm("priya", "setup");
    let sent = dms(&taken.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0.as_str(), "priya");
    assert_eq!(sent[0].1, NAME_QUESTION);
    assert!(model_requests(&taken.effects).is_empty());
    assert_eq!(sim.state.setup.with, Some("priya".into()));
    assert_eq!(sim.state.setup.stage, Stage::AskedName);
    assert!(notices(&taken.effects).contains(&"First conversation taken over by Priya from Olu."));

    // Olu's late answer is an ordinary DM now; Priya's runs it to done.
    sim.model_says("Priya is setting me up now.");
    let late = sim.dm("olu", "3");
    assert_eq!(texts(&late.effects), vec!["Priya is setting me up now."]);
    sim.dm("priya", "keep");
    sim.dm("priya", "committee");
    assert_eq!(sim.state.kind, Kind::Committee);
    sim.dm("priya", "keep");
    let done = sim.dm("priya", "keep");
    assert_eq!(messages(&done.effects).len(), 2);
    assert_eq!(sim.state.setup.stage, Stage::Done);
    let after = sim.dm("priya", "setup");
    assert_eq!(
        texts(&after.effects),
        vec!["Setup is done. Change any of it with a sentence, or say settings to see it."]
    );
}

#[test]
fn setup_from_the_person_asked_repeats_the_question() {
    let mut sim = launch_added_by_olu();
    let again = sim.dm("olu", "setup");
    let sent = dms(&again.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, NAME_QUESTION);
    assert_eq!(sim.state.setup.stage, Stage::AskedName);
    sim.dm("olu", "Robin");
    let kind = sim.dm("olu", "setup");
    let sent = dms(&kind.effects);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0]
            .1
            .starts_with("Hi, I'm Robin. What kind of group is this?")
    );
    sim.dm("olu", "3");
    let voices = sim.dm("olu", "set up");
    assert!(
        texts(&voices.effects)[0].starts_with("Two voices to pick from:"),
        "{:?}",
        texts(&voices.effects)
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedPersona);
    sim.dm("olu", "keep");
    let leads = sim.dm("olu", "setup");
    assert!(texts(&leads.effects)[0].starts_with("Who should I take instructions from?"));
    assert_eq!(sim.state.setup.stage, Stage::AskedLeads);
}

#[test]
fn a_lead_can_start_setup_when_nobody_was_asked() {
    let mut sim = Sim::new(Kind::Friends);
    let room = Room {
        platform: Platform::Sim,
        id: "general".into(),
        name: Some("general".into()),
        kind: RoomKind::Channel,
        parent: None,
        container: None,
    };
    let members = vec![
        Member {
            is_bot: true,
            ..member("dicebot", "DiceBot", PlatformRole::Owner)
        },
        member("sam", "Sam", PlatformRole::Admin),
    ];
    sim.feed(Input::Inbound(InboundEvent::RoomSync { room, members }));
    assert_eq!(sim.state.setup.stage, Stage::NotStarted);
    let started = sim.dm("sam", "setup");
    let sent = dms(&started.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, NAME_QUESTION);
    assert_eq!(sim.state.setup.with, Some("sam".into()));
    assert_eq!(sim.state.setup.room, Some("general".into()));
    // Skip at the name keeps Q; skip at the kind ends it.
    sim.dm("sam", "skip");
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
    let skipped = sim.dm("sam", "skip");
    assert_eq!(texts(&skipped.effects).len(), 2);
    assert_eq!(sim.state.setup.stage, Stage::Done);
}

#[test]
fn a_fresh_group_is_named_noa_and_the_sim_names_it_q() {
    let state = ours_core::GroupState::new("g".into(), Kind::Friends);
    assert_eq!(state.agent_name, "Noa");
    assert_eq!(ours_core::state::DEFAULT_AGENT_NAME, "Noa");
    assert!(!state.name_from_host);
    let sim = Sim::new(Kind::Friends);
    assert_eq!(sim.state.agent_name, "Q");
    assert!(!sim.state.name_from_host);
}

#[test]
fn a_name_not_from_the_host_still_gets_the_name_question() {
    let mut sim = Sim::new(Kind::Friends);
    sim.feed(Input::Command(Command::SetAgentName {
        name: "Sarah".into(),
        by_host: false,
    }));
    assert!(!sim.state.name_from_host);
    let synced = sim.room("general", "general", &[("olu", "Olu", PlatformRole::Owner)]);
    let sent = dms(&synced);
    assert_eq!(
        sent[0].1,
        "Hi, I'm Sarah. What should people call me here? Say a name, or keep."
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedName);
}

#[test]
fn the_host_keeping_the_default_name_still_skips_the_question() {
    let mut sim = Sim::new(Kind::Friends);
    sim.named_by_host("Noa");
    assert!(sim.state.name_from_host);
    let synced = sim.room("general", "general", &[("olu", "Olu", PlatformRole::Owner)]);
    let sent = dms(&synced);
    assert!(
        sent[0]
            .1
            .starts_with("Hi, I'm Noa. What kind of group is this?"),
        "{}",
        sent[0].1
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
}

#[test]
fn a_takeover_after_a_name_was_chosen_skips_the_name_question() {
    let mut sim = launch_added_by_olu();
    sim.dm("olu", "Robin");
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
    sim.advance(3 * 24 * 60 * 60);
    let taken = sim.dm("priya", "setup");
    let sent = dms(&taken.effects);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0]
            .1
            .starts_with("Hi, I'm Robin. What kind of group is this?"),
        "{}",
        sent[0].1
    );
    assert_eq!(sim.state.setup.stage, Stage::AskedKind);
    assert!(!sim.state.name_from_host);
}

#[test]
fn an_old_set_agent_name_line_decodes_as_not_from_the_host() {
    let old = r#"{"command":{"type":"set_agent_name","name":"Sarah"}}"#;
    let input: Input = serde_json::from_str(old).unwrap();
    assert_eq!(
        input,
        Input::Command(Command::SetAgentName {
            name: "Sarah".into(),
            by_host: false,
        })
    );
    let line = serde_json::to_string(&input).unwrap();
    assert!(line.contains(r#""by_host":false"#), "{line}");
    assert_eq!(serde_json::from_str::<Input>(&line).unwrap(), input);

    let mut state = serde_json::to_value(Sim::new(Kind::Friends).state).unwrap();
    state.as_object_mut().unwrap().remove("name_from_host");
    let restored = ours_core::GroupState::restore(&state.to_string()).unwrap();
    assert!(!restored.name_from_host);
}
