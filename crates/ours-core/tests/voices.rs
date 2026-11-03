//! The six starter voices and the hard rules that hold at every dial
//! (DESIGN.md section 16): choosing a voice by name or by sentence, what
//! the dial does when a voice is written for more than the kind allows, and
//! the rules that are code rather than prompt.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::leads::DEFERRAL_REPLY;
use ours_core::persona::HARD_RULES;
use ours_core::sim::{ModelScript, Sim, mail_plan, messages, model_requests, notices, paid, texts};
use ours_core::{
    Command, GroupState, Input, Persona, Plan, SettingChange, render_charter, starter, starters,
    system_prompt,
};
use ours_types::{Kind, PlatformRole, Role};

/// The product team: Priya is an admin and so a lead, Sam a member, Dana
/// the client's external account.
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
            ("priya", "Priya", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim
}

#[test]
fn six_starters_two_at_each_dial_written_to_be_read() {
    let all = starters();
    assert_eq!(all.len(), 6);
    for dial in 0..=2 {
        assert_eq!(all.iter().filter(|s| s.dial == dial).count(), 2, "{dial}");
    }
    let mut names: Vec<&str> = all.iter().map(|s| s.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 6);
    for s in &all {
        let p = &s.persona;
        assert!(
            s.name.chars().all(|c| c.is_ascii_lowercase()),
            "{}: one lower-case word",
            s.name
        );
        assert!((3..=4).contains(&p.traits.len()), "{}", s.name);
        assert!(!p.never_jokes_about.is_empty(), "{}", s.name);
        assert!(p.bits.is_empty(), "bits come from the group");
        assert!(!p.voice.trim().is_empty() && p.voice.ends_with('.'));
        assert!(
            s.one_line.starts_with(|c: char| c.is_lowercase()) && !s.one_line.ends_with('.'),
            "{}: reads after the name",
            s.name
        );
        assert!(!s.kinds.is_empty());
        let text = format!(
            "{} {} {} {}",
            p.voice,
            p.traits.join(" "),
            p.never_jokes_about.join(" "),
            s.one_line
        );
        assert!(!text.contains('\u{2014}'), "no em-dashes: {}", s.name);
        assert!(!text.contains(['*', '#', '_']), "plain text: {}", s.name);
    }

    // The dial-0 pair is for work and the dial-1 pair for everyone else.
    let work = [Kind::Team, Kind::Project, Kind::Committee];
    for s in all.iter().filter(|s| s.dial == 0) {
        assert_eq!(s.kinds, work.as_slice(), "{}", s.name);
    }
    for s in all.iter().filter(|s| s.dial == 1) {
        for kind in [
            Kind::Friends,
            Kind::Household,
            Kind::Club,
            Kind::Cohort,
            Kind::Community,
        ] {
            assert!(s.kinds.contains(&kind), "{} {kind:?}", s.name);
        }
    }
    assert_eq!(starter("CLERK").unwrap().name, "clerk");
    assert_eq!(starter(" the Host ").unwrap().name, "host");
    assert!(starter("butler").is_none());
}

#[test]
fn a_lead_picks_a_voice_by_sentence_and_the_prompt_and_charter_follow() {
    let mut sim = work();
    let said = sim.say("launch", "priya", "Q, be the clerk");
    assert_eq!(
        messages(&said.effects),
        vec![("Voice: clerk (dial 0).", Some(&said.id))]
    );
    assert!(model_requests(&said.effects).is_empty());
    assert_eq!(sim.state.persona_name.as_deref(), Some("clerk"));
    assert_eq!(sim.state.persona, starter("clerk").unwrap().persona);
    assert!(
        notices(&said.effects)
            .iter()
            .any(|n| n.starts_with("Setting: Voice: clerk (dial 0). Changed by Priya."))
    );

    let charter = render_charter(&sim.state);
    assert!(charter.contains("- Voice: clerk (dial 0)"), "{charter}");
    assert!(charter.contains("- Talks like: Quiet and exact, like a good minute-taker"));
    let json = serde_json::to_value(&sim.state).unwrap();
    assert_eq!(json["persona_name"], "clerk");

    sim.model_says("Thursday at 2.");
    let asked = sim.say("launch", "sam", "Q, when's the review?");
    let system = &model_requests(&asked.effects)[0].system;
    assert!(system.contains("You talk like this: Quiet and exact, like a good minute-taker"));
    assert!(
        system.contains(
            "You are precise, unhurried, discreet and exact about dates, names and numbers."
        ),
        "{system}"
    );
    assert!(system.contains("never joke about, whatever the mood: anyone's performance or pay"));

    // Other ways to say it, and back to the plain voice.
    for (sentence, name) in [
        ("Q, sound like the host", Some("host")),
        ("Q, use the clerk persona", Some("clerk")),
        ("Q, switch to the host voice", Some("host")),
        ("Q, use the plain default", None),
    ] {
        sim.say("launch", "priya", sentence);
        assert_eq!(sim.state.persona_name.as_deref(), name, "{sentence}");
    }
    assert_eq!(sim.state.persona, Persona::default());
    assert!(render_charter(&sim.state).contains("- Voice: plain default (dial 0)"));
}

#[test]
fn a_members_choice_waits_for_a_lead_and_a_guests_is_declined() {
    let mut sim = work();
    let member = sim.say("launch", "sam", "Q, be the host");
    assert_eq!(texts(&member.effects), vec![DEFERRAL_REPLY]);
    assert_eq!(sim.state.persona_name, None);
    let guest = sim.say("launch", "dana", "Q, be the host");
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert_eq!(sim.state.persona_name, None);

    // Anything that is not a starter's name is left to the model.
    sim.model_says("I'll keep it plain.");
    let other = sim.say("launch", "priya", "Q, be more sarcastic");
    assert_eq!(model_requests(&other.effects).len(), 1);
}

#[test]
fn a_dial_two_voice_for_a_team_keeps_the_dial_until_a_lead_raises_it() {
    let mut sim = work();
    sim.say("launch", "priya", "Q, you have a bit: the printer");
    let fx = sim.feed(Input::Command(Command::UsePersona { name: "Wag".into() }));
    assert_eq!(
        notices(&fx),
        vec![
            "Persona: Voice: wag, written for dial 2. The dial stays at 0 for a team until a lead says dial 2."
        ]
    );
    assert_eq!(sim.state.persona_name.as_deref(), Some("wag"));
    assert_eq!(sim.state.profile.dial, 0);
    assert_eq!(
        sim.state.persona.bits,
        vec!["the printer".to_owned()],
        "the group's bits stay"
    );
    assert!(render_charter(&sim.state).contains("- Voice: wag (dial 0)"));

    sim.say("launch", "priya", "Q, dial 2");
    assert_eq!(sim.state.profile.dial, 2);

    // A dial-1 voice moves the dial to 1 on its own.
    sim.feed(Input::Command(Command::UsePersona {
        name: "keeper".into(),
    }));
    assert_eq!(sim.state.profile.dial, 1);

    // A host's own persona has no starter's name.
    sim.feed(Input::Command(Command::SetPersona {
        persona: Persona {
            voice: "Brisk.".into(),
            ..Persona::default()
        },
    }));
    assert_eq!(sim.state.persona_name, None);
    assert!(render_charter(&sim.state).contains("- Voice: written by the host (dial 1)"));

    let unknown = sim.feed(Input::Command(Command::UsePersona {
        name: "butler".into(),
    }));
    assert!(notices(&unknown)[0].starts_with("Persona: I don't have a voice called butler."));
    assert_eq!(sim.state.persona.voice, "Brisk.");
}

#[test]
fn who_can_you_be_lists_the_six_in_plain_text() {
    let mut sim = friends();
    let asked = sim.say("dinner", "jo", "Q, who can you be?");
    assert!(model_requests(&asked.effects).is_empty());
    let text = texts(&asked.effects)[0];
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 7, "{text}");
    assert_eq!(
        lines[0],
        "clerk (dial 0): quiet and exact, it states what was decided and asks only what it needs."
    );
    for (line, s) in lines.iter().zip(starters()) {
        assert!(line.starts_with(&format!("{} (dial {}): ", s.name, s.dial)));
        assert!(line.contains(s.one_line));
    }
    assert_eq!(
        lines[6],
        "Say be the clerk, or any name above; right now it's my plain voice."
    );
    assert!(!text.contains(['*', '#']));

    sim.say("dinner", "jo", "Q, be the keeper");
    let again = sim.say("dinner", "sam", "Q, what voices do you have");
    assert!(texts(&again.effects)[0].ends_with("right now I'm the keeper."));
}

#[test]
fn a_message_that_carries_an_action_is_composed_in_code_at_any_dial() {
    // At dial 3 the model's ironic words never reach the room with a mail.
    let mut sim = friends();
    sim.say("dinner", "sam", "Q, dial 3");
    sim.script(ModelScript::Reply(Plan {
        message: Some("Booked, obviously. You're welcome, peasants.".into()),
        ..mail_plan(
            "bookings@baitong.com",
            "Table for five, Thursday 7:30",
            "Hello, a table for five at 7:30 on Thursday? Q for the group",
        )
    }));
    let asked = sim.say(
        "dinner",
        "jo",
        "Q, email bookings@baitong.com for a table for five on Thursday at 7:30",
    );
    assert_eq!(
        texts(&asked.effects),
        vec!["Emailing bookings@baitong.com in 2m: Table for five, Thursday 7:30. Undo?"]
    );
    // "Booked." is never ironic: the sent line is the engine's too.
    let sent = sim.advance(120);
    assert_eq!(
        texts(&sent),
        vec!["Sent to bookings@baitong.com: Table for five, Thursday 7:30. Undo?"]
    );
    let system = &model_requests(&asked.effects)[0].system;
    assert!(system.contains("Dial 3, chaos"));
    assert!(system.contains("Never ironic or sarcastic in a message that carries an action."));
    assert!(system.contains("the system posts the one line"));

    // The tab's line is the same words at dial 0 and dial 3, with no model.
    let line = |dial: u8| {
        let mut sim = friends();
        sim.feed(Input::Command(Command::ChangeSetting {
            change: SettingChange::DialTo { dial },
        }));
        for who in ["sam", "priya", "jo"] {
            sim.say("dinner", who, "hello");
        }
        sim.pays(vec![paid("Priya", 38.0, "wine")]);
        let wine = sim.say("dinner", "priya", "I got the wine, 38");
        assert!(model_requests(&wine.effects).is_empty());
        texts(&wine.effects)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let plain = line(0);
    assert_eq!(plain.len(), 1);
    assert!(
        plain[0].starts_with("Added. Tab: Priya 38 (wine)"),
        "{plain:?}"
    );
    assert_eq!(line(3), plain);
}

#[test]
fn a_guest_speaker_gets_the_no_nickname_rule_in_the_prompt() {
    let mut state = GroupState::new("g".into(), Kind::Friends);
    state.profile.dial = 2;
    state.persona = starter("wag").unwrap().persona;
    state.persona.bits = vec!["Dana's spreadsheets".into()];
    let now = ours_core::sim::START;
    let guest = system_prompt(&state, &"r".into(), &"dana".into(), Role::Guest, now);
    assert!(guest.contains("role is guest"));
    assert!(
        guest.contains("never use a nickname for a guest"),
        "{guest}"
    );
    assert!(guest.contains(
        "Call them by the name they show: never a nickname, a running bit or a joke at their expense."
    ));
    let member = system_prompt(&state, &"r".into(), &"sam".into(), Role::Member, now);
    assert!(!member.contains("Call them by the name they show"));

    // Every dial carries every hard rule.
    for dial in 0..=3 {
        state.profile.dial = dial;
        let prompt = system_prompt(&state, &"r".into(), &"dana".into(), Role::Guest, now);
        for rule in HARD_RULES {
            assert!(prompt.contains(rule), "dial {dial}: {rule}");
        }
    }
}
