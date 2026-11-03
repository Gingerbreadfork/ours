//! It keeps the promises and the tab: commitments, due nudges, what is
//! outstanding, the tab and settling up, the digest, the day-of line, the
//! pinned state message, the ICS feed and recurrences (DESIGN.md sections
//! 6, 8, 11 and 12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::commitment::PIN;
use ours_core::sim::{
    Sim, any_send, away_draft, dms, edits, extract_requests, instant, messages, model_requests,
    notices, paid, pinned, promise, reactions, texts,
};
use ours_core::tab::{DOUBLE, LINK_NOT_KEPT, LINK_SAVED, NOT_ME};
use ours_core::{
    Command, CommitmentStatus, Effect, GroupState, Input, LineKind, SettlementStatus, Transfer,
    balances, render_ics, settle, step,
};
use ours_types::{
    Capabilities, Extraction, Kind, PlatformMessageId, PlatformRole, PlatformRoomId, PlatformUserId,
};

const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn regular(id: &'static str, name: &'static str) -> (&'static str, &'static str, PlatformRole) {
    (id, name, PlatformRole::Regular)
}

fn user(id: &str) -> PlatformUserId {
    PlatformUserId::new(id)
}

/// DMs sent to `to`, in order.
fn dms_to<'a>(effects: &'a [Effect], to: &str) -> Vec<&'a str> {
    dms(effects)
        .into_iter()
        .filter(|(u, _)| u.as_str() == to)
        .map(|(_, t)| t)
        .collect()
}

/// A product team in #launch, the clock at Tuesday 09:00, Alex posting at
/// nine so nine is the hour Alex answers.
fn launch() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            regular("sam", "Sam"),
            regular("alex", "Alex"),
            regular("jo", "Jo"),
        ],
    );
    sim.advance_to(instant("2026-11-03T09:00:00Z"));
    for line in ["morning", "coffee first", "ok here"] {
        sim.say("launch", "alex", line);
    }
    sim
}

fn only_commitment(sim: &Sim) -> &ours_core::Commitment {
    assert_eq!(
        sim.state.commitments.len(),
        1,
        "{:?}",
        sim.state.commitments
    );
    sim.state.commitments.values().next().unwrap()
}

#[test]
fn the_tuesday_commitment_is_pinned_nudged_twice_moved_and_closed() {
    let mut sim = launch();
    sim.promises(vec![promise("Alex", "draft to legal", Some("2026-11-05"))]);
    let said = sim.say("launch", "alex", "I'll have the draft to legal by Thursday");
    // Nothing in the room but the 📌.
    assert_eq!(reactions(&said.effects), vec![(&said.id, PIN)]);
    assert!(texts(&said.effects).is_empty());
    let c = only_commitment(&sim).clone();
    assert_eq!(c.who, user("alex"));
    assert_eq!(c.what, "draft to legal");
    assert_eq!(c.due, Some(instant("2026-11-06T00:00:00Z")));
    assert!(!c.due_has_time);
    assert!((c.confidence - 0.9).abs() < f32::EPSILON);

    // Someone else's ❌ on the 📌 line does nothing.
    let other = sim.react("launch", &said.id, "sam", "❌");
    assert!(!any_send(&other));
    assert!(only_commitment(&sim).is_open());

    // Wednesday at nine, the day before: one DM.
    let tuesday = sim.advance_to(instant("2026-11-04T08:59:00Z"));
    assert!(dms_to(&tuesday, "alex").is_empty());
    let before = sim.advance_to(instant("2026-11-04T09:00:00Z"));
    assert_eq!(
        dms_to(&before, "alex"),
        vec!["Draft to legal is due tomorrow. Done, slipping, or drop it?"]
    );
    // Thursday at nine, at due: the second.
    let at = sim.advance_to(instant("2026-11-05T09:00:00Z"));
    assert_eq!(
        dms_to(&at, "alex"),
        vec!["Draft to legal is due today. Done, slipping, or drop it?"]
    );
    // The room hears of it only in Wednesday's digest.
    assert!(texts(&before).is_empty());
    assert_eq!(texts(&at), vec!["Due: Alex, draft to legal, tomorrow."]);

    // "Friday" in reply moves it, with the date echoed.
    let moved = sim.dm("alex", "Friday.");
    assert_eq!(texts(&moved.effects), vec!["Moved to Friday the 6th."]);
    let c = only_commitment(&sim);
    assert_eq!(c.due, Some(instant("2026-11-07T00:00:00Z")));
    assert!(matches!(c.status, CommitmentStatus::Moved { .. }));

    // Never a third nudge.
    let friday = sim.advance_to(instant("2026-11-06T17:59:00Z"));
    assert!(dms_to(&friday, "alex").is_empty());

    // "done" closes it.
    let done = sim.dm("alex", "done");
    assert_eq!(texts(&done.effects), vec!["Marked done: draft to legal."]);
    assert!(matches!(
        only_commitment(&sim).status,
        CommitmentStatus::Done { .. }
    ));
}

#[test]
fn the_person_who_made_it_can_drop_it_with_a_cross() {
    let mut sim = launch();
    sim.promises(vec![promise("Alex", "book the room", None)]);
    let said = sim.say("launch", "alex", "I'll book the room");
    sim.react("launch", &said.id, "alex", "❌");
    assert!(matches!(
        only_commitment(&sim).status,
        CommitmentStatus::Dropped { .. }
    ));
}

#[test]
fn a_cross_on_the_nudge_drops_it_and_a_day_said_in_the_room_moves_it() {
    let mut sim = launch();
    sim.promises(vec![promise("Alex", "draft to legal", Some("2026-11-05"))]);
    sim.say("launch", "alex", "I'll have the draft to legal by Thursday");
    // "I'll have it by Friday" in the room from the same person moves it.
    sim.extract(ours_core::sim::ExtractorScript::Found(Extraction {
        commitments: vec![promise("Alex", "have it", Some("2026-11-06"))],
        ..Extraction::default()
    }));
    let later = sim.say("launch", "alex", "actually I'll have it by Friday");
    assert_eq!(reactions(&later.effects), vec![(&later.id, PIN)]);
    assert_eq!(sim.state.commitments.len(), 1, "no new commitment");
    let c = only_commitment(&sim);
    assert_eq!(c.due, Some(instant("2026-11-07T00:00:00Z")));

    let nudge = sim.advance_to(instant("2026-11-05T09:00:00Z"));
    assert_eq!(
        dms_to(&nudge, "alex"),
        vec!["Draft to legal is due tomorrow. Done, slipping, or drop it?"]
    );
    let dm = sim.last_dm_to("alex").unwrap();
    let dropped = sim.react("dm-alex", &dm, "alex", "❌");
    assert_eq!(texts(&dropped), vec!["Dropped: draft to legal."]);
    assert!(matches!(
        only_commitment(&sim).status,
        CommitmentStatus::Dropped { .. }
    ));
    let after = sim.advance_to(instant("2026-11-06T18:00:00Z"));
    assert!(dms_to(&after, "alex").is_empty());
}

#[test]
fn whats_outstanding_is_answered_for_anyone_in_the_room() {
    let mut sim = launch();
    sim.promises(vec![promise("Alex", "draft to legal", Some("2026-11-05"))]);
    sim.say("launch", "alex", "I'll have the draft to legal by Thursday");
    sim.promises(vec![promise("Jo", "slides", Some("2026-11-12"))]);
    sim.say("launch", "jo", "I'll do the slides for next thursday");
    sim.promises(vec![promise("Sam", "check the venue", None)]);
    sim.say("launch", "sam", "I'll check the venue");
    // A commitment made in a DM stays with its person.
    sim.promises(vec![promise("Sam", "call the bank", Some("2026-11-04"))]);
    let private = sim.dm("sam", "Q, I'll call the bank tomorrow");
    assert_eq!(reactions(&private.effects).last().unwrap().1, PIN);
    assert_eq!(sim.state.commitments.len(), 4);

    let all = sim.say("launch", "priya", "Q, what's outstanding?");
    assert!(model_requests(&all.effects).is_empty());
    assert_eq!(
        texts(&all.effects),
        vec![
            "Alex: draft to legal, due Thursday the 5th\nJo: slides, due Thursday the 12th\nSam: check the venue"
        ]
    );
    let alex = sim.say("launch", "jo", "Q, what did Alex commit to?");
    assert_eq!(
        texts(&alex.effects),
        vec!["Alex: draft to legal, due Thursday the 5th"]
    );
    let week = sim.say("launch", "jo", "Q, what's due this week");
    assert_eq!(
        texts(&week.effects),
        vec!["Alex: draft to legal, due Thursday the 5th"]
    );
    let mine = sim.dm("sam", "what did I commit to");
    assert_eq!(
        texts(&mine.effects),
        vec!["Sam: call the bank, due Wednesday the 4th\nSam: check the venue"]
    );
    let nobody = sim.say("launch", "jo", "Q, what did Marcus commit to");
    assert_eq!(
        texts(&nobody.effects),
        vec!["I don't know anyone called Marcus here."]
    );
}

/// Five friends in #dinner, everyone active, Wednesday noon.
fn dinner() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("priya", "Priya"),
            regular("sam", "Sam"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
            regular("kim", "Kim"),
        ],
    );
    sim.advance_to(instant("2026-11-04T12:00:00Z"));
    for who in ["priya", "sam", "jo", "alex", "kim"] {
        sim.say("dinner", who, "hello");
        sim.advance(MIN);
    }
    sim
}

/// The agent's "Added." line, by its platform id.
fn tab_line(sim: &Sim) -> PlatformMessageId {
    sim.state.ledger.last().unwrap().posted.clone().unwrap()
}

#[test]
fn the_wednesday_tab_splits_adjusts_and_balances() {
    let mut sim = dinner();
    sim.pays(vec![paid("Priya", 38.0, "wine")]);
    let wine = sim.say("dinner", "priya", "I got the wine, 38");
    assert_eq!(extract_requests(&wine.effects).len(), 1);
    assert_eq!(
        messages(&wine.effects),
        vec![(
            "Added. Tab: Priya 38 (wine) split five ways.",
            Some(&wine.id)
        )]
    );
    let line = tab_line(&sim);
    assert_eq!(
        reactions(&wine.effects),
        vec![(&line, NOT_ME), (&line, DOUBLE)]
    );
    assert_eq!(sim.state.ledger[0].amount_cents, 3800);
    assert_eq!(sim.state.ledger[0].currency, "USD");

    // "not me" in reply takes Sam out.
    let out = sim.reply("dinner", "sam", "not me", &line);
    assert_eq!(
        messages(&out.effects),
        vec![("Tab: Priya 38 (wine) split four ways.", Some(&out.id))]
    );
    // A ✌️ counts Kim twice; the line itself is not reposted.
    let double = sim.react("dinner", &line, "kim", DOUBLE);
    assert!(texts(&double).is_empty());
    assert_eq!(
        ours_core::tab::describe(&sim.state, &sim.state.ledger[0]),
        "Priya 38 (wine) split four ways, double for Kim"
    );

    let net = balances(&sim.state);
    assert_eq!(net[&user("priya")], 3040);
    assert_eq!(net[&user("kim")], -1520);
    assert_eq!(net[&user("jo")], -760);
    assert_eq!(net[&user("alex")], -760);
    assert!(!net.contains_key(&user("sam")));
    assert_eq!(net.values().sum::<i64>(), 0);
    assert_eq!(
        settle(&sim.state),
        vec![
            Transfer {
                from: user("kim"),
                to: user("priya"),
                cents: 1520
            },
            Transfer {
                from: user("alex"),
                to: user("priya"),
                cents: 760
            },
            Transfer {
                from: user("jo"),
                to: user("priya"),
                cents: 760
            },
        ]
    );

    let tab = sim.say("dinner", "alex", "Q, what's the tab?");
    assert_eq!(
        texts(&tab.effects),
        vec!["Tab (USD): Priya +30.40, Kim -15.20, Alex -7.60, Jo -7.60."]
    );
    let owe = sim.say("dinner", "jo", "Q, what do I owe");
    assert_eq!(
        texts(&owe.effects),
        vec!["You owe 7.60.\nTab (USD): Priya +30.40, Kim -15.20, Alex -7.60, Jo -7.60."]
    );
    // Taking the ✌️ off undoes it.
    sim.unreact("dinner", &line, "kim", DOUBLE);
    assert_eq!(balances(&sim.state)[&user("kim")], -950);
}

#[test]
fn explicit_shares_and_the_groups_currency() {
    let mut sim = dinner();
    sim.feed(Input::Command(Command::SetCurrency { code: "aud".into() }));
    assert_eq!(sim.state.currency, "AUD");
    sim.pays(vec![paid("Priya", 30.0, "lunch")]);
    sim.say(
        "dinner",
        "priya",
        "I got lunch, 30, split it three ways between me, Sam and Jo",
    );
    let line = &sim.state.ledger[0];
    assert_eq!(line.currency, "AUD");
    assert_eq!(
        ours_core::tab::describe(&sim.state, line),
        "Priya 30 (lunch) split three ways"
    );
    sim.pays(vec![paid("Alex", 50.0, "cab")]);
    sim.say("dinner", "alex", "I paid the cab, 50. Kim owes 20 of it");
    assert_eq!(
        ours_core::tab::describe(&sim.state, &sim.state.ledger[1]),
        "Alex 50 (cab) Kim owes 20"
    );
    let net = balances(&sim.state);
    assert_eq!(net[&user("kim")], -2000);
    assert_eq!(net[&user("alex")], 2000);
    assert_eq!(net[&user("priya")], 2000);
}

#[test]
fn settle_up_dms_each_debtor_and_the_check_round_trip_zeroes_the_pair() {
    let mut sim = dinner();
    sim.pays(vec![paid("Priya", 38.0, "wine")]);
    sim.say("dinner", "priya", "I got the wine, 38");
    let line = tab_line(&sim);
    sim.reply("dinner", "sam", "not me", &line);

    let saved = sim.dm("priya", "my payment link is https://monzo.me/priya");
    assert_eq!(texts(&saved.effects), vec![LINK_SAVED]);
    assert_eq!(
        sim.state.payment_links[&user("priya")],
        "https://monzo.me/priya"
    );

    let settle_up = sim.say("dinner", "sam", "Q, settle up");
    assert_eq!(
        texts(&settle_up.effects),
        vec!["Sent three people what they owe, by DM."]
    );
    let to_kim = dms_to(&settle_up.effects, "kim");
    assert_eq!(
        to_kim,
        vec![
            "You owe Priya 9.50 USD for the tab. Priya's payment link: https://monzo.me/priya\nReact ✅ here once you've paid."
        ]
    );
    // Nobody but the debtors hears it, and never in the room.
    assert!(dms_to(&settle_up.effects, "priya").is_empty());
    assert!(dms_to(&settle_up.effects, "sam").is_empty());

    // Kim marks it paid with ✅; Priya is asked to confirm.
    let ask = sim.last_dm_to("kim").unwrap();
    let marked = sim.react("dm-kim", &ask, "kim", "✅");
    assert_eq!(
        dms_to(&marked, "priya"),
        vec![
            "Kim says they've paid you 9.50 USD for the tab. React ✅ to confirm, or ❌ if it hasn't arrived."
        ]
    );
    assert!(texts(&marked).is_empty());
    let confirm = sim.last_dm_to("priya").unwrap();
    let confirmed = sim.react("dm-priya", &confirm, "priya", "✅");
    assert_eq!(
        dms_to(&confirmed, "kim"),
        vec!["Priya confirmed your 9.50 USD; it's off the tab."]
    );
    let net = balances(&sim.state);
    assert!(!net.contains_key(&user("kim")), "the pair is zeroed");
    assert_eq!(net[&user("priya")], 1900);
    let last = sim.state.ledger.last().unwrap();
    assert!(matches!(last.kind, LineKind::Settlement { .. }));

    // Jo says "paid"; Priya disputes it, both hear, and it stops there.
    let jo = sim.dm("jo", "paid");
    assert_eq!(
        texts(&jo.effects),
        vec!["Thanks. I've asked Priya to confirm."]
    );
    let confirm = sim.last_dm_to("priya").unwrap();
    let disputed = sim.react("dm-priya", &confirm, "priya", "❌");
    let line = "The 9.50 USD from Jo to Priya is disputed, so I've stopped there. Sort it out between you, then say settle up again.";
    assert_eq!(dms_to(&disputed, "jo"), vec![line]);
    assert_eq!(dms_to(&disputed, "priya"), vec![line]);
    assert!(texts(&disputed).is_empty());
    assert!(
        sim.state
            .settlements
            .values()
            .any(|s| matches!(s.status, SettlementStatus::Disputed { .. }))
    );
    assert_eq!(balances(&sim.state)[&user("jo")], -950);

    // Without a link the debtor is told to ask.
    sim.state.payment_links.clear();
    let again = sim.say("dinner", "kim", "Q, settle up");
    assert!(
        dms_to(&again.effects, "alex")[0]
            .starts_with("You owe Priya 9.50 USD for the tab. Ask Priya for their details")
    );
}

#[test]
fn a_payment_link_said_in_a_room_is_not_kept() {
    let mut sim = dinner();
    let said = sim.say(
        "dinner",
        "sam",
        "Q, my payment link is https://paypal.me/sam",
    );
    assert_eq!(texts(&said.effects), vec![LINK_NOT_KEPT]);
    assert!(sim.state.payment_links.is_empty());
    let held = &sim.state.rooms[&PlatformRoomId::new("dinner")].messages;
    assert!(!held.iter().any(|m| m.id == said.id));

    // Unaddressed, it is dropped too, and its author told by DM.
    let quiet = sim.say("dinner", "jo", "my payment link is https://paypal.me/jo");
    assert!(texts(&quiet.effects).is_empty());
    assert!(dms_to(&quiet.effects, "jo")[0].starts_with("I didn't keep the payment link"));
    assert!(extract_requests(&quiet.effects).is_empty());
    let held = &sim.state.rooms[&PlatformRoomId::new("dinner")].messages;
    assert!(!held.iter().any(|m| m.id == quiet.id));
    assert!(sim.state.payment_links.is_empty());
}

#[test]
fn the_digest_fires_at_the_set_hour_and_not_when_nothing_happened() {
    let mut sim = launch();
    sim.room(
        "random",
        "random",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            regular("sam", "Sam"),
        ],
    );
    sim.advance_to(instant("2026-11-03T10:00:00Z"));
    let set = sim.say("launch", "priya", "Q, digest at 4pm");
    assert_eq!(
        texts(&set.effects),
        vec!["Digest at 16:00 each working day."]
    );

    sim.promises(vec![promise("Alex", "draft to legal", Some("2026-11-04"))]);
    sim.say(
        "launch",
        "alex",
        "I'll have the draft to legal by Wednesday",
    );
    sim.promises(vec![promise("Sam", "send the slides", Some("2026-11-03"))]);
    sim.say("launch", "sam", "I'll send the slides today");
    let done = sim.dm("sam", "done");
    assert_eq!(texts(&done.effects), vec!["Marked done: send the slides."]);
    sim.say("random", "sam", "anyone seen my charger");

    let early = sim.advance_to(instant("2026-11-03T15:59:00Z"));
    assert!(texts(&early).is_empty());
    let digest = sim.advance_to(instant("2026-11-03T16:00:00Z"));
    assert_eq!(
        messages(&digest),
        vec![(
            "Due: Alex, draft to legal, tomorrow.\nClosed: Sam, send the slides.",
            None
        )]
    );

    // Wednesday: Alex is nudged at nine and closes it by DM.
    let nudge = sim.advance_to(instant("2026-11-04T09:00:00Z"));
    assert_eq!(dms_to(&nudge, "alex").len(), 1);
    sim.dm("alex", "done");
    let wednesday = sim.advance_to(instant("2026-11-04T16:00:00Z"));
    assert_eq!(texts(&wednesday), vec!["Closed: Alex, draft to legal."]);

    // Thursday nothing happened: no digest anywhere.
    let thursday = sim.advance_to(instant("2026-11-06T18:00:00Z"));
    assert!(texts(&thursday).is_empty(), "{:?}", texts(&thursday));

    // "no digest" turns it off.
    let off = sim.say("launch", "priya", "Q, no digest");
    assert_eq!(texts(&off.effects), vec!["No digest."]);
    assert!(sim.state.jobs().digest.is_none());
}

/// Five friends with a dinner on Thursday the 12th at 7:30pm, for which
/// Alex is away and Marcus can't make it.
fn thursday_dinner() -> (Sim, PlatformMessageId) {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("priya", "Priya"),
            regular("sam", "Sam"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
            regular("marcus", "Marcus"),
        ],
    );
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    for who in ["priya", "sam", "jo", "marcus"] {
        sim.say("dinner", who, "hello");
    }
    sim.extracts(vec![away_draft("Alex", "2026-11-12", "2026-11-12")]);
    sim.say("dinner", "alex", "I'm away on the 12th");
    let poll = sim.say(
        "dinner",
        "sam",
        "Q, poll dinner: thursday 7:30pm or friday 7:30pm",
    );
    (sim, poll.id)
}

#[test]
fn an_away_member_is_marked_on_the_ballot() {
    let (sim, _) = thursday_dinner();
    let p = sim.state.proposals.values().next().unwrap();
    let ballot = sim
        .sent
        .iter()
        .find_map(|s| match &s.outbound.action {
            ours_types::OutboundAction::SendMessage { text, .. } if text.starts_with("🗳️") => {
                Some(text.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        ballot,
        "🗳️ Dinner: Thursday 7:30pm or Friday 7:30pm?\n1️⃣ Thursday the 12th at 7:30pm (Alex away)\n2️⃣ Friday the 13th at 7:30pm"
    );
    assert!(p.is_open());
    assert!(ours_core::away(
        &sim.state,
        &user("alex"),
        instant("2026-11-12T19:30:00Z")
    ));
    assert!(!ours_core::away(
        &sim.state,
        &user("alex"),
        instant("2026-11-13T00:00:00Z")
    ));
}

#[test]
fn the_thursday_day_of_line() {
    let (mut sim, _) = thursday_dinner();
    let ballot = sim
        .state
        .proposals
        .values()
        .next()
        .unwrap()
        .ballot
        .message()
        .cloned()
        .unwrap();
    for who in ["sam", "priya", "jo"] {
        sim.react("dinner", &ballot, who, "1️⃣");
    }
    assert_eq!(sim.state.calendar.len(), 1);
    sim.say("dinner", "marcus", "Q I can't do thursday");
    let mut jo = promise("Jo", "bring dessert", None);
    jo.event = Some("Thursday's dinner".into());
    sim.promises(vec![jo]);
    sim.say("dinner", "jo", "I'll bring dessert");
    let c = sim.state.commitments.values().next().unwrap();
    assert_eq!(c.event.as_ref(), Some(&sim.state.calendar[0].decision));

    let before = sim.advance_to(instant("2026-11-12T17:29:00Z"));
    assert!(texts(&before).is_empty(), "{:?}", texts(&before));
    let line = sim.advance_to(instant("2026-11-12T17:30:00Z"));
    assert_eq!(
        messages(&line),
        vec![(
            "Dinner at 7:30pm, Alex is away, Marcus out. Jo is bringing dessert.",
            None
        )]
    );
    let after = sim.advance_to(instant("2026-11-13T23:00:00Z"));
    assert!(texts(&after).is_empty());
}

fn can_edit_and_pin() -> Capabilities {
    Capabilities {
        reactions: true,
        edit: true,
        pin: true,
        dms: true,
        ..Capabilities::default()
    }
}

#[test]
fn the_pinned_state_message_is_created_pinned_and_edited_once_a_minute() {
    let mut sim = dinner();
    sim.capabilities(can_edit_and_pin());
    let poll = sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let sent = texts(&poll.effects);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].starts_with("🗳️"));
    assert_eq!(
        sent[1],
        "Open: Thursday or Friday, 0 votes, closes Wednesday the 4th at 10pm.\nEveryone in the group."
    );
    let pin = sim.state.pins[&PlatformRoomId::new("dinner")]
        .message
        .clone()
        .unwrap();
    assert_eq!(pinned(&poll.effects), vec![&pin]);

    // A burst of votes is one edit, a minute after the first.
    let ballot = sim
        .state
        .proposals
        .values()
        .next()
        .unwrap()
        .ballot
        .message()
        .cloned()
        .unwrap();
    let mut burst = Vec::new();
    burst.extend(sim.react("dinner", &ballot, "sam", "1️⃣"));
    sim.advance(10);
    burst.extend(sim.react("dinner", &ballot, "jo", "2️⃣"));
    burst.extend(sim.advance(40));
    assert!(edits(&burst).is_empty());
    let edited = sim.advance(10);
    assert_eq!(
        edits(&edited),
        vec![(
            &pin,
            "Open: Thursday or Friday, 2 votes, closes Wednesday the 4th at 10pm.\nEveryone in the group."
        )]
    );
    assert!(edits(&sim.advance(5 * MIN)).is_empty());

    // A tab line adds the balances; the message is edited, never reposted.
    sim.pays(vec![paid("Priya", 38.0, "wine")]);
    sim.say("dinner", "priya", "I got the wine, 38");
    let later = sim.advance(MIN);
    let (id, text) = edits(&later)[0];
    assert_eq!(id, &pin);
    assert!(
        text.contains("Tab (USD): Priya +30.40, Alex -7.60, Jo -7.60, Kim -7.60, Sam -7.60."),
        "{text}"
    );
    let posts = sim
        .sent
        .iter()
        .filter(|s| matches!(&s.outbound.action, ours_types::OutboundAction::SendMessage { text, .. } if text.starts_with("Open:")))
        .count();
    assert_eq!(posts, 1);
}

/// Parses ICS text into its components, checking every line is well formed.
fn components(ics: &str) -> Vec<Vec<(String, String)>> {
    assert!(ics.ends_with("\r\n"));
    let mut unfolded: Vec<String> = Vec::new();
    for raw in ics.split("\r\n").filter(|l| !l.is_empty()) {
        assert!(raw.len() <= 75, "{raw}");
        match raw.strip_prefix(' ') {
            Some(more) => unfolded.last_mut().unwrap().push_str(more),
            None => unfolded.push(raw.to_owned()),
        }
    }
    assert_eq!(
        unfolded.first().map(String::as_str),
        Some("BEGIN:VCALENDAR")
    );
    assert_eq!(unfolded.last().map(String::as_str), Some("END:VCALENDAR"));
    let mut events = Vec::new();
    let mut current: Option<Vec<(String, String)>> = None;
    for line in &unfolded {
        let (name, value) = line.split_once(':').expect("name:value");
        match (name, value) {
            ("BEGIN", "VEVENT") => current = Some(Vec::new()),
            ("END", "VEVENT") => events.push(current.take().expect("open event")),
            _ => {
                if let Some(event) = current.as_mut() {
                    event.push((name.to_owned(), value.to_owned()));
                }
            }
        }
    }
    assert!(current.is_none());
    events
}

fn field<'a>(event: &'a [(String, String)], name: &str) -> &'a str {
    event
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("no {name}"))
}

#[test]
fn the_ics_feed_parses_and_a_members_feed_filters() {
    let (mut sim, _) = thursday_dinner();
    let ballot = sim
        .state
        .proposals
        .values()
        .next()
        .unwrap()
        .ballot
        .message()
        .cloned()
        .unwrap();
    for who in ["sam", "priya", "jo"] {
        sim.react("dinner", &ballot, who, "1️⃣");
    }
    sim.promises(vec![promise("Jo", "book a taxi", Some("2026-11-11"))]);
    sim.say("dinner", "jo", "I'll book a taxi by wednesday");
    let now = sim.now;
    let group = components(&render_ics(&sim.state, None, now));
    assert_eq!(group.len(), 1);
    let dinner = &group[0];
    assert_eq!(field(dinner, "SUMMARY"), "Dinner");
    assert_eq!(field(dinner, "DTSTART"), "20261112T193000Z");
    assert_eq!(field(dinner, "DTEND"), "20261112T213000Z");
    assert!(field(dinner, "UID").ends_with("@group"));
    assert_eq!(
        field(dinner, "DESCRIPTION"),
        "Dinner: Thursday the 12th at 7:30pm\\, 3 to 0 on majority of active members."
    );

    let jo = components(&render_ics(&sim.state, Some(&user("jo")), now));
    assert_eq!(jo.len(), 2);
    assert_eq!(field(&jo[1], "SUMMARY"), "Due: book a taxi");
    assert_eq!(field(&jo[1], "DTSTART"), "20261111T000000Z");
    let alex = components(&render_ics(&sim.state, Some(&user("alex")), now));
    assert!(alex.is_empty(), "Alex is not going");

    // The links, once the binary says where it serves them.
    let none = sim.say("dinner", "jo", "Q, calendar link");
    assert_eq!(
        texts(&none.effects),
        vec!["There's no calendar feed yet; the host can turn it on."]
    );
    sim.feed(Input::Command(Command::SetFeedBase {
        url: "https://q.example/feeds/group/".into(),
    }));
    let room = sim.say("dinner", "jo", "Q, calendar link");
    assert_eq!(
        texts(&room.effects),
        vec![
            "The group's calendar: https://q.example/feeds/group.ics. Add it to any calendar app; it updates as plans close."
        ]
    );
    // A member's link carries a token keyed to them, not the group secret.
    let dm = sim.dm("jo", "calendar link");
    let token = ours_core::member_feed_token("group", "jo");
    assert_eq!(
        texts(&dm.effects),
        vec![format!(
            "Your calendar: https://q.example/feeds/{token}/jo.ics. It has what you're in and your own commitments."
        )]
    );
    assert!(!texts(&dm.effects)[0].contains("feeds/group/"));
    // The room's first line and its state message say the feed exists.
    let room = PlatformRoomId::new("dinner");
    assert!(
        ours_core::setup::room_line(&sim.state, &room)
            .ends_with("Ask me for the calendar link to follow the group's calendar.")
    );
    let (state_message, _) = ours_core::pin::render(&sim.state, &room, sim.now);
    assert!(
        state_message.ends_with("Calendar feed: ask me for the calendar link."),
        "{state_message}"
    );
}

#[test]
fn slipping_asks_when_and_a_week_moves_it() {
    let mut sim = launch();
    sim.promises(vec![promise("Alex", "draft to legal", Some("2026-11-05"))]);
    sim.say("launch", "alex", "I'll have the draft to legal by Thursday");
    sim.advance_to(instant("2026-11-04T09:00:00Z"));
    let slipping = sim.dm("alex", "slipping");
    assert_eq!(
        texts(&slipping.effects),
        vec!["To when? Say a day, like Friday."]
    );
    let next_week = sim.dm("alex", "next week");
    assert_eq!(
        texts(&next_week.effects),
        vec!["Moved to the week of the 9th."]
    );
    let c = only_commitment(&sim);
    assert_eq!(c.due, Some(instant("2026-11-16T00:00:00Z")));
    // A longer line after a nudge is not taken as a new date.
    sim.advance_to(instant("2026-11-13T09:00:00Z"));
    sim.model_says("Sure.");
    let chat = sim.dm("alex", "can we talk about the launch plan on friday");
    assert_eq!(model_requests(&chat.effects).len(), 1);
}

#[test]
fn a_monthly_dinner_opens_with_its_lead_time_and_pauses_after_two_silences() {
    let mut sim = dinner();
    sim.advance_to(instant("2026-11-09T12:00:00Z"));
    let set = sim.say("dinner", "sam", "Q, monthly dinner on the first Thursday");
    assert_eq!(
        texts(&set.effects),
        vec![
            "Set: dinner, every month on the first Thursday. Next is Thursday the 3rd; I'll ask who's in three days before."
        ]
    );
    let rec = &sim.state.recurrences[0];
    assert_eq!(rec.rrule, "FREQ=MONTHLY;BYDAY=1TH");
    assert_eq!(rec.next, instant("2026-12-03T00:00:00Z"));

    // It opens three days ahead, at ten.
    let quiet = sim.advance_to(instant("2026-11-30T09:59:00Z"));
    assert!(!texts(&quiet).iter().any(|t| t.starts_with("🗳️")));
    let opened = sim.advance_to(instant("2026-11-30T10:00:00Z"));
    assert_eq!(
        texts(&opened),
        vec!["🗳️ Who's in for dinner, Thursday the 3rd? React 👍 if you are."]
    );
    let p = sim.state.proposals.values().next().unwrap();
    assert_eq!(p.opened_at, instant("2026-11-30T10:00:00Z"));

    // Nobody answers; it lapses after two deadlines.
    let lapsed = sim.advance_to(instant("2026-12-02T12:00:00Z"));
    assert!(texts(&lapsed).contains(&"No votes on Dinner, so I've let it go."));
    assert!(!sim.state.recurrences[0].paused);
    assert_eq!(
        sim.state.recurrences[0].next,
        instant("2027-01-07T00:00:00Z")
    );

    // The second silence pauses it with one line.
    let second = sim.advance_to(instant("2027-01-06T12:00:00Z"));
    assert!(
        texts(&second)
            .iter()
            .any(|t| t.starts_with("🗳️ Who's in for dinner"))
    );
    assert!(texts(&second).contains(
        &"No votes on Dinner, so I've let it go. Nobody answered the last two, so I've paused the dinner; say resume the dinner to bring it back."
    ));
    assert!(sim.state.recurrences[0].paused);
    let february = sim.advance_to(instant("2027-02-06T12:00:00Z"));
    assert!(!texts(&february).iter().any(|t| t.starts_with("🗳️")));

    let resumed = sim.say("dinner", "sam", "Q, resume the dinner");
    assert_eq!(
        texts(&resumed.effects),
        vec!["Resumed the dinner. Next is Thursday the 4th."]
    );
    let paused = sim.say("dinner", "sam", "Q, pause the dinner");
    assert_eq!(texts(&paused.effects), vec!["Paused the dinner."]);
}

#[test]
fn a_weekly_reminder_posts_a_day_ahead() {
    let mut sim = dinner();
    let set = sim.say(
        "dinner",
        "jo",
        "Q, remind us every Saturday 9am to bring boots",
    );
    assert_eq!(
        texts(&set.effects),
        vec![
            "Set: bring boots, every Saturday at 9am. Next is Saturday the 7th at 9am; I'll remind the room a day before."
        ]
    );
    let reminder = sim.advance_to(instant("2026-11-06T09:00:00Z"));
    assert_eq!(
        texts(&reminder),
        vec!["Reminder for Saturday the 7th at 9am: bring boots."]
    );
    assert_eq!(
        sim.state.recurrences[0].next,
        instant("2026-11-14T09:00:00Z")
    );
}

#[test]
fn an_away_span_moves_a_nudge_to_the_day_they_are_back() {
    let mut sim = launch();
    sim.promises(vec![promise("Alex", "budget", None)]);
    sim.say("launch", "alex", "I'll send the budget by Friday 5pm");
    let c = only_commitment(&sim);
    assert_eq!(c.due, Some(instant("2026-11-06T17:00:00Z")));
    assert!(c.due_has_time);
    sim.extracts(vec![away_draft("Alex", "2026-11-05", "2026-11-05")]);
    sim.say("launch", "alex", "I'm away on Thursday");

    // Thursday is the day before, but Alex is away, so nobody is chased.
    let thursday = sim.advance_to(instant("2026-11-05T23:00:00Z"));
    assert!(dms_to(&thursday, "alex").is_empty());
    assert!(
        notices(&thursday)
            .iter()
            .any(|n| n.contains("Alex is away"))
    );
    // Back on Friday: one DM at nine, and none at five.
    let friday = sim.advance_to(instant("2026-11-06T09:00:00Z"));
    assert_eq!(
        dms_to(&friday, "alex"),
        vec!["Budget is due at 5pm today. Done, slipping, or drop it?"]
    );
    let five = sim.advance_to(instant("2026-11-06T18:00:00Z"));
    assert!(dms_to(&five, "alex").is_empty());
}

#[test]
fn promises_and_the_tab_round_trip_through_a_snapshot_and_replay() {
    let mut sim = dinner();
    sim.capabilities(can_edit_and_pin());
    sim.pays(vec![paid("Priya", 38.0, "wine")]);
    sim.say("dinner", "priya", "I got the wine, 38");
    sim.promises(vec![promise("Jo", "bring dessert", Some("2026-11-06"))]);
    sim.say("dinner", "jo", "I'll bring dessert friday");
    sim.say("dinner", "sam", "Q, weekly drinks every friday 6pm");
    sim.dm("priya", "my payment link is https://monzo.me/priya");
    sim.say("dinner", "sam", "Q, settle up");
    assert!(!sim.state.commitments.is_empty());
    assert!(!sim.state.ledger.is_empty());
    assert!(!sim.state.recurrences.is_empty());
    assert!(!sim.state.settlements.is_empty());
    assert!(!sim.state.pins.is_empty());

    // Replaying the log rebuilds the same state and the same effects.
    let mut state = GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(step(&mut state, input, at));
    }
    assert_eq!(state, sim.state);
    assert_eq!(effects, sim.effects);

    // A snapshot restores it, and both copies take the next timer alike.
    let json = sim.state.snapshot().unwrap();
    let mut restored = GroupState::restore(&json).unwrap();
    assert_eq!(restored, sim.state);
    let timer = sim.state.timers.keys().next().unwrap().clone();
    let later = sim.now.plus_seconds(DAY);
    let original = step(
        &mut sim.state,
        Input::TimerFired { id: timer.clone() },
        later,
    );
    let replayed = step(&mut restored, Input::TimerFired { id: timer }, later);
    assert_eq!(original, replayed);
    assert_eq!(restored, sim.state);
}

/// Every key milestone five added, at any depth.
const PROMISE_KEYS: [&str; 18] = [
    "commitments",
    "ledger",
    "currency",
    "payment_links",
    "settlements",
    "pins",
    "recurrences",
    "digests",
    "day_of_sent",
    "feed_base",
    "jobs",
    "starts",
    "out",
    "then",
    "nudges",
    "nudged_at",
    "moved_by",
    "changed_at",
];

fn strip_keys(value: &mut serde_json::Value, keys: &[&str]) {
    match value {
        serde_json::Value::Object(map) => {
            for key in keys {
                map.remove(*key);
            }
            map.values_mut().for_each(|v| strip_keys(v, keys));
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| strip_keys(v, keys)),
        _ => {}
    }
}

#[test]
fn a_milestone_four_snapshot_still_loads_and_keeps_folding() {
    let (mut sim, _) = thursday_dinner();
    let ballot = sim
        .state
        .proposals
        .values()
        .next()
        .unwrap()
        .ballot
        .message()
        .cloned()
        .unwrap();
    sim.react("dinner", &ballot, "sam", "1️⃣");
    let mut json = serde_json::to_value(&sim.state).unwrap();
    strip_keys(&mut json, &PROMISE_KEYS);
    for key in PROMISE_KEYS {
        assert!(!json.to_string().contains(&format!("\"{key}\"")), "{key}");
    }
    let restored = GroupState::restore(&json.to_string()).unwrap();
    assert!(restored.commitments.is_empty() && restored.ledger.is_empty());
    assert_eq!(restored.currency, "USD");
    assert!(restored.pins.is_empty() && restored.recurrences.is_empty());
    assert!(restored.feed_base.is_none() && restored.profile.jobs.is_none());
    assert!(restored.facts.values().all(|f| f.starts.is_none()));
    assert!(restored.jobs().day_of);

    // An old log line with fact drafts still reads.
    let old = serde_json::json!({
        "extracted": {
            "room": "dinner",
            "message": "m1",
            "drafts": [{
                "subject": "Sam", "subject_kind": "person", "predicate": "drives",
                "object": "", "about_self": false, "stated": true, "durable": true,
                "expires": null, "quote": null
            }]
        }
    });
    let input: Input = serde_json::from_value(old).unwrap();
    let Input::Extracted { extraction, .. } = &input else {
        panic!("{input:?}");
    };
    assert_eq!(extraction.facts.len(), 1);
    assert!(extraction.commitments.is_empty());

    // It keeps folding: the rest of the votes close the dinner.
    let mut sim = Sim::with_state(restored);
    sim.now = instant("2026-11-09T11:00:00Z");
    sim.react("dinner", &ballot, "priya", "1️⃣");
    sim.react("dinner", &ballot, "jo", "1️⃣");
    assert_eq!(sim.state.decisions.len(), 1);
}
