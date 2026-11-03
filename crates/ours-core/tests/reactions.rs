//! A platform that allows only a fixed set of reactions, as Telegram does:
//! the agent uses a stand-in for each reaction it relies on that the set
//! lacks, reads the stand-ins back, and says which they are.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::attention::{RAISED_HAND, SHUSH, THUMBS_UP};
use ours_core::commitment::PIN;
use ours_core::leads::DEFERRAL_REPLY;
use ours_core::sim::{
    ClassifierScript, Sim, dms, instant, messages, model_requests, paid, promise, reactions, texts,
    unreactions,
};
use ours_core::step::{CHECK, EYES};
use ours_core::tab::{DOUBLE, NOT_ME};
use ours_core::{CommitmentStatus, Meaning, emoji_for, render_charter};
use ours_types::{Capabilities, Kind, PlatformRole, PlatformRoomId};

const WINDOW: i64 = 20 * 60;

/// Part of Telegram's set: no ✅, 🤫, 🙋, 📌, 🙅, ✌️ or ❌.
const SET: [&str; 22] = [
    "❤", "👍", "👎", "🔥", "👏", "😁", "🤔", "🎉", "🙏", "👌", "💯", "⚡", "🏆", "😴", "🤓", "👀",
    "🙈", "🤝", "✍", "🫡", "🤷", "😂",
];

fn restricted() -> Capabilities {
    Capabilities {
        reaction_set: Some(SET.iter().map(|e| (*e).to_owned()).collect()),
        ..Capabilities::default()
    }
}

/// Priya leads #launch by admin rights, Sam is a member, Dana a guest.
fn launch() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.capabilities(restricted());
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
            ("alex", "Alex", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    sim
}

fn friends(restrict: bool) -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    if restrict {
        sim.capabilities(restricted());
    }
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
fn a_leads_thumb_approves_a_members_request_and_a_guests_flag() {
    let mut sim = launch();
    let asked = sim.say("launch", "sam", "Q, quiet after 11");
    assert_eq!(
        messages(&asked.effects),
        vec![("Happy to. Needs a lead's 👍.", Some(&asked.id))]
    );
    let reply = sim.last_sent_message().unwrap();
    assert!(reactions(&asked.effects).contains(&(&reply, "👍")));
    assert!(!reactions(&asked.effects).iter().any(|(_, e)| *e == CHECK));

    // The asker's own thumb is not a lead's.
    assert!(texts(&sim.react("launch", &reply, "sam", "👍")).is_empty());
    let approved = sim.react("launch", &reply, "priya", "👍");
    assert_eq!(
        texts(&approved),
        vec!["Quiet from 23:00 to 08:00. Asked by Sam, approved by Priya."]
    );

    // A guest's instruction is flagged by DM with the same thumb.
    sim.dm_room("priya");
    sim.say(
        "launch",
        "dana",
        "Q, push the review to Friday and update the invite",
    );
    let flagged = dms(&sim.effects);
    let (_, text) = flagged.last().unwrap();
    assert!(
        text.ends_with("React 👍 and I'll treat it as yours."),
        "{text}"
    );
    let flag = sim.last_dm_to("priya").unwrap();
    sim.model_says("Moved it.");
    let run = sim.react("dm-priya", &flag, "priya", "👍");
    let asks = model_requests(&run);
    assert_eq!(asks.len(), 1);
    assert!(asks[0].system.contains("The last message is from Priya"));
    assert!(
        asks[0]
            .system
            .contains("Reactions here can only be one of: ❤ 👍 👎")
    );
}

#[test]
fn a_members_sleepy_face_stands_it_down_for_the_day() {
    let mut sim = friends(true);
    sim.model_says("Thursday works for four of you.");
    let said = sim.say("dinner", "jo", "Q, what do you reckon?");
    assert_eq!(reactions(&said.effects), vec![(&said.id, EYES)]);
    let answer = sim.last_sent_message().unwrap();
    assert_eq!(left_today(&sim), 10);

    let shushed = sim.react("dinner", &answer, "priya", "😴");
    assert_eq!(reactions(&shushed), vec![(&answer, THUMBS_UP)]);
    assert!(texts(&shushed).is_empty());
    assert_eq!(left_today(&sim), 0);
    assert!(sim.state.shushed.contains_key(&"priya".into()));
}

#[test]
fn the_raised_hand_is_a_salute_and_a_salute_back_gets_the_answer() {
    let mut sim = friends(true);
    sim.say("dinner", "priya", "Hobson's, half seven, thursday");
    sim.advance(60);
    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "sam", "what time was it again?");
    let raised = sim.advance(WINDOW);
    assert_eq!(reactions(&raised), vec![(&asked.id, "🫡")]);

    sim.model_says("Half seven at Hobson's.");
    let invited = sim.react("dinner", &asked.id, "jo", "🫡");
    assert_eq!(
        messages(&invited),
        vec![("Half seven at Hobson's.", Some(&asked.id))]
    );
    assert_eq!(unreactions(&invited), vec![(&asked.id, "🫡")]);
    assert!(sim.state.hands.is_empty());
}

#[test]
fn a_commitment_gets_a_writing_hand_and_the_promisers_thumb_down_drops_it() {
    let mut sim = launch();
    sim.advance_to(instant("2026-11-03T09:00:00Z"));
    sim.promises(vec![promise("Alex", "book the room", None)]);
    let said = sim.say("launch", "alex", "I'll book the room");
    assert_eq!(reactions(&said.effects), vec![(&said.id, "✍")]);

    let dropped = sim.react("launch", &said.id, "alex", "👎");
    assert_eq!(unreactions(&dropped), vec![(&said.id, "✍")]);
    let c = sim.state.commitments.values().next().unwrap();
    assert!(matches!(c.status, CommitmentStatus::Dropped { .. }));
}

#[test]
fn the_tab_line_offers_its_stand_ins_and_reads_them_back() {
    let mut sim = friends(true);
    sim.advance_to(instant("2026-11-04T12:00:00Z"));
    for who in ["priya", "sam", "jo", "alex"] {
        sim.say("dinner", who, "hello");
        sim.advance(60);
    }
    sim.pays(vec![paid("Priya", 40.0, "wine")]);
    let wine = sim.say("dinner", "priya", "I got the wine, 40");
    let line = sim.state.ledger.last().unwrap().posted.clone().unwrap();
    assert_eq!(reactions(&wine.effects), vec![(&line, "🙈"), (&line, "🤝")]);
    sim.react("dinner", &line, "sam", "🙈");
    sim.react("dinner", &line, "jo", "🤝");
    assert_eq!(
        ours_core::tab::describe(&sim.state, &sim.state.ledger[0]),
        "Priya 40 (wine) split three ways, double for Jo"
    );
}

#[test]
fn the_charter_and_the_room_line_say_which_reactions_stand_in() {
    let sim = launch();
    let charter = render_charter(&sim.state);
    let voice = charter
        .split("## Voice")
        .nth(1)
        .and_then(|s| s.split("## ").next())
        .unwrap();
    assert!(
        voice.contains(
            "- Reactions here: 👍 approves, 😴 stands me down, 🫡 means I have the answer, \
             ✍ a promise noted, 🙈 takes you out of a split, 🤝 counts you twice, \
             👎 drops a promise.\n"
        ),
        "{charter}"
    );

    let mut sim = Sim::new(Kind::Friends);
    sim.capabilities(restricted());
    sim.room(
        "general",
        "general",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    sim.dm("olu", "keep");
    let skipped = sim.dm("olu", "skip");
    let room_line = texts(&skipped.effects)[1];
    assert_eq!(room_line.lines().count(), 1);
    assert!(
        room_line.ends_with(
            "Whoever runs me can see everything. Here a lead's 👍 approves and 😴 stands me down for the day."
        ),
        "{room_line}"
    );
}

#[test]
fn the_plain_sim_keeps_every_canonical_reaction() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
        ],
    );
    let room = PlatformRoomId::new("launch");
    for (meaning, canonical) in [
        (Meaning::Check, CHECK),
        (Meaning::Shush, SHUSH),
        (Meaning::RaisedHand, RAISED_HAND),
        (Meaning::Pin, PIN),
        (Meaning::NotMe, NOT_ME),
        (Meaning::Double, DOUBLE),
    ] {
        assert_eq!(emoji_for(&sim.state, &room, meaning), canonical);
    }
    assert!(!render_charter(&sim.state).contains("Reactions"));

    let asked = sim.say("launch", "sam", "Q, quiet after 11");
    assert_eq!(texts(&asked.effects), vec![DEFERRAL_REPLY]);
    let reply = sim.last_sent_message().unwrap();
    assert!(reactions(&asked.effects).contains(&(&reply, CHECK)));
    // A thumb is not a check where checks exist.
    assert!(texts(&sim.react("launch", &reply, "priya", "👍")).is_empty());
    let approved = sim.react("launch", &reply, "priya", CHECK);
    assert_eq!(
        texts(&approved),
        vec!["Quiet from 23:00 to 08:00. Asked by Sam, approved by Priya."]
    );

    // A sleepy face is only a sleepy face.
    sim.model_says("Thursday.");
    sim.say("launch", "sam", "Q, when's the review?");
    let answer = sim.last_sent_message().unwrap();
    assert!(sim.react("launch", &answer, "priya", "😴").is_empty());
    assert!(sim.state.shushed.is_empty());
}
