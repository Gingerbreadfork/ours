//! A guest's instruction is declined in code, flagged to the leads by DM,
//! and run as the lead's own on a ✅ (DESIGN.md sections 5 and 6, "A guest").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::sim::{Sim, dms, messages, model_requests};
use ours_core::step::CHECK;
use ours_core::{Command, DECLINE_PREFIX, Input};
use ours_types::{Kind, PlatformRole, PlatformRoomId};

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
    sim.feed(Input::Command(Command::AddLead { user: "sam".into() }));
    sim
}

const ASK: &str = "Q, push the review to Friday and update the invite";

#[test]
fn a_guests_instruction_is_declined_and_flagged() {
    let mut sim = work();
    let ask = sim.say("launch", "dana", ASK);
    let fx = &ask.effects;

    assert!(model_requests(fx).is_empty());
    assert_eq!(
        messages(fx),
        vec![("That's one for Priya or Sam.", Some(&ask.id))]
    );
    assert!(messages(fx)[0].0.starts_with(DECLINE_PREFIX));
    let sent = dms(fx);
    assert_eq!(sent.len(), 2);
    for (_, text) in &sent {
        assert_eq!(
            *text,
            "Dana at Acme in #launch asked: push the review to Friday and update the invite. \
             React ✅ and I'll treat it as yours."
        );
    }
    let users: Vec<&str> = sent.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(users, vec!["priya", "sam"]);
}

#[test]
fn a_leads_check_runs_the_request_as_theirs() {
    let mut sim = work();
    sim.dm_room("sam");
    sim.dm_room("priya");
    sim.say("launch", "dana", ASK);
    let flag = sim.last_dm_to("sam").expect("flag sent");
    let other = sim.last_dm_to("priya").expect("flag sent");

    sim.model_says("Moved it.");
    let fx = sim.react("dm-sam", &flag, "sam", CHECK);
    let asks = model_requests(&fx);
    assert_eq!(asks.len(), 1);
    assert!(asks[0].system.contains("Sam"), "{}", asks[0].system);
    assert!(
        asks[0]
            .messages
            .last()
            .unwrap()
            .content
            .contains("Sam: push the review")
    );

    // The other lead's copy is spent.
    let again = sim.react("dm-priya", &other, "priya", CHECK);
    assert!(model_requests(&again).is_empty());
}

#[test]
fn a_taken_up_request_keeps_the_guests_words_as_theirs() {
    let mut sim = work();
    sim.dm_room("sam");
    let ask = sim.say("launch", "dana", ASK);
    let flag = sim.last_dm_to("sam").expect("flag sent");

    sim.model_says("Moved it.");
    let fx = sim.react("dm-sam", &flag, "sam", CHECK);
    let room = &sim.state.rooms[&PlatformRoomId::new("launch")];
    let held = room.messages.iter().find(|m| m.id == ask.id).unwrap();
    assert_eq!(held.author.as_str(), "dana");
    assert_eq!(held.text, ASK);

    let asks = model_requests(&fx);
    assert_eq!(asks.len(), 1);
    let last = &asks[0].messages.last().unwrap().content;
    assert!(
        last.ends_with(&format!(
            "Dana at Acme: {ASK}\nSam: push the review to Friday and update the invite"
        )),
        "{last}"
    );
    assert!(asks[0].system.contains("The last message is from Sam"));
}

#[test]
fn a_non_leads_check_does_nothing() {
    let mut sim = work();
    sim.dm_room("sam");
    sim.say("launch", "dana", ASK);
    let flag = sim.last_dm_to("sam").expect("flag sent");
    let fx = sim.react("dm-sam", &flag, "dana", CHECK);
    assert!(model_requests(&fx).is_empty());
}

#[test]
fn a_guests_question_still_reaches_the_model() {
    let mut sim = work();
    sim.model_says("Friday at 3.");
    let ask = sim.say("launch", "dana", "Q, when's the review again?");
    assert_eq!(model_requests(&ask.effects).len(), 1);
    assert!(dms(&ask.effects).is_empty());
    assert_eq!(messages(&ask.effects)[0].0, "Friday at 3.");
}

#[test]
fn a_guests_own_data_still_works() {
    let mut sim = work();
    sim.dm_room("dana");
    let ask = sim.dm("dana", "Q, what do you know about me");
    assert!(model_requests(&ask.effects).is_empty());
    assert!(!messages(&ask.effects).is_empty() || !dms(&ask.effects).is_empty());
    assert!(
        !messages(&ask.effects)
            .iter()
            .any(|(t, _)| t.starts_with(DECLINE_PREFIX))
    );
}
