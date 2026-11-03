//! A new install has a past: backfill reads the last 90 days once, page by
//! page, and keeps facts with provenance (DESIGN.md section 18). And the
//! two patterns every profile keeps, for timing only (DESIGN.md section 12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::backfill::{BUSY_LINE, NOTHING_LINE, PAGE_GUARD_SECONDS, START_LINE};
use ours_core::leads::DEFERRAL_REPLY;
use ours_core::rights::PATTERN_LINE;
use ours_core::sim::{
    ExtractorScript, Sim, classify_requests, dms, extract_requests, fetches, messages, self_draft,
    texts,
};
use ours_core::step::CHECK;
use ours_core::{Input, dump, room_active, usual_hour};
use ours_types::{Kind, PlatformRole, PlatformRoomId};

const DAY: i64 = 24 * 60 * 60;

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

fn held(sim: &Sim) -> Vec<String> {
    sim.state.rooms[&PlatformRoomId::new("dinner")]
        .messages
        .iter()
        .map(|m| m.id.to_string())
        .collect()
}

#[test]
fn backfill_pages_back_with_the_cursor_and_reports_what_it_kept() {
    let mut sim = friends();
    sim.say("dinner", "sam", "evening all");
    let newer = vec![
        sim.old_message("dinner", "jo", "I'm vegetarian these days", 20 * DAY),
        sim.old_message("dinner", "priya", "lol same", 10 * DAY),
        sim.old_message("dinner", "alex", "my birthday is 3 May", 5 * DAY),
    ];
    let older = vec![
        sim.old_message("dinner", "sam", "I'm allergic to cats", 95 * DAY),
        sim.old_message("dinner", "jo", "I live in Leith now", 60 * DAY),
    ];
    sim.history_page(newer);
    sim.history_page(older);
    sim.extracts(vec![self_draft(
        "Jo",
        "is",
        "vegetarian",
        "vegetarian these days",
    )]);
    sim.extracts(vec![self_draft(
        "Alex",
        "has a birthday on",
        "3 May",
        "my birthday is 3 May",
    )]);
    sim.extracts(vec![self_draft(
        "Jo",
        "lives in",
        "Leith",
        "I live in Leith now",
    )]);
    let classified = sim.classified.len();

    let asked = sim.say("dinner", "sam", "Q, backfill");
    let said = messages(&asked.effects);
    assert_eq!(said[0], (START_LINE, Some(&asked.id)));
    let pages: Vec<_> = fetches(&asked.effects)
        .into_iter()
        .map(|(room, limit, before)| (room.as_str(), limit, before.map(|b| b.as_str())))
        .collect();
    assert_eq!(
        pages,
        vec![("dinner", 100, None), ("dinner", 100, Some("h1"))],
        "the second page starts before the oldest message of the first"
    );

    // History is read for facts, never classified and never put in the ring.
    assert_eq!(sim.classified.len(), classified);
    assert!(held(&sim).iter().all(|id| !id.starts_with('h')));
    let read: Vec<&str> = extract_requests(&asked.effects)
        .iter()
        .map(|r| r.message.as_str())
        .collect();
    assert_eq!(read, vec!["h1", "h3", "h5"], "nothing past 90 days");
    // The oldest line of a page gets its context from the page before it.
    let first = extract_requests(&asked.effects)[0];
    assert_eq!(first.context.len(), 1);
    assert_eq!(first.context[0].text, "I live in Leith now");
    assert_eq!(first.date, "2026-10-13");

    let jo = sim
        .state
        .facts
        .values()
        .find(|f| f.line() == "Jo is vegetarian")
        .unwrap();
    assert_eq!(jo.first_seen, sim.now.plus_seconds(-20 * DAY));
    assert_eq!(jo.learned_at, sim.now);
    assert_eq!(jo.sources[0].message.as_str(), "h1");

    assert_eq!(
        said.last().unwrap(),
        &(
            "Read 4 messages back to 2026-09-03: kept 3 facts. Tell me what's wrong.",
            Some(&asked.id)
        )
    );
    assert!(sim.state.backfills.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);
}

#[test]
fn one_backfill_at_a_time_and_the_summary_waits_for_the_last_facts() {
    let mut sim = friends();
    let page = vec![sim.old_message("dinner", "jo", "I'm vegan now", 3 * DAY)];
    sim.history_page(page);
    sim.history_page(Vec::new());
    sim.extract(ExtractorScript::Silent);
    let asked = sim.say("dinner", "priya", "Q, read the last 90 days");
    assert_eq!(fetches(&asked.effects).len(), 2);
    assert_eq!(texts(&asked.effects), vec![START_LINE]);
    assert_eq!(sim.state.backfills.len(), 1);

    let again = sim.say("dinner", "jo", "Q, catch up");
    assert_eq!(texts(&again.effects), vec![BUSY_LINE]);
    assert!(fetches(&again.effects).is_empty());

    let landed = sim.feed(Input::Extracted {
        room: "dinner".into(),
        message: "h1".into(),
        extraction: ours_core::sim::facts(vec![self_draft("Jo", "is", "vegan", "I'm vegan now")]),
    });
    assert_eq!(
        messages(&landed),
        vec![(
            "Read 1 message back to 2026-10-30: kept 1 fact. Tell me what's wrong.",
            Some(&asked.id)
        )]
    );
    assert!(sim.state.backfills.is_empty());
    assert_eq!(sim.timers_besides_retention(), 0);
}

#[test]
fn a_page_that_never_comes_ends_the_backfill() {
    let mut sim = friends();
    let asked = sim.say("dinner", "sam", "Q, backfill");
    assert_eq!(fetches(&asked.effects).len(), 1);
    assert!(sim.advance(PAGE_GUARD_SECONDS - 1).is_empty());
    let ended = sim.advance(1);
    assert_eq!(messages(&ended), vec![(NOTHING_LINE, Some(&asked.id))]);
    assert!(sim.state.backfills.is_empty());

    // A history page nobody asked for still goes to the ring.
    let stray = vec![sim.old_message("dinner", "jo", "old news", DAY)];
    sim.feed(Input::Inbound(ours_types::InboundEvent::History {
        room: "dinner".into(),
        messages: stray,
    }));
    assert_eq!(held(&sim)[0], "h1");
}

#[test]
fn in_a_team_a_members_backfill_waits_for_a_lead() {
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
    let guest = sim.say("launch", "dana", "Q, backfill");
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert!(fetches(&guest.effects).is_empty());

    let member = sim.say("launch", "sam", "Q, backfill");
    assert_eq!(texts(&member.effects), vec![DEFERRAL_REPLY]);
    assert!(fetches(&member.effects).is_empty());
    let ask = sim.last_sent_message().unwrap();
    sim.history_page(Vec::new());
    let approved = sim.react("launch", &ask, "priya", CHECK);
    assert_eq!(
        texts(&approved)[0],
        format!("{START_LINE} Asked by Sam, approved by Priya.")
    );
    assert_eq!(fetches(&approved).len(), 1);
    assert_eq!(texts(&approved).last().unwrap(), &NOTHING_LINE);

    // Asked in a DM, it says where to ask.
    let in_dm = sim.dm("priya", "backfill");
    assert_eq!(
        texts(&in_dm.effects),
        vec!["Ask me in the room you want me to read."]
    );
}

#[test]
fn patterns_count_hours_and_halve_each_week() {
    let mut sim = friends();
    // Monday 18:00 in the group's zone.
    for _ in 0..3 {
        sim.say("dinner", "sam", "lol");
        sim.advance(60);
    }
    sim.say("dinner", "priya", "ha");
    let sam = "sam".into();
    assert_eq!(usual_hour(&sim.state, &sam), Some(18));
    assert_eq!(
        usual_hour(&sim.state, &"priya".into()),
        None,
        "too few to say"
    );
    assert_eq!(sim.state.patterns.members[&sam][18], 3);
    let dinner = PlatformRoomId::new("dinner");
    assert!(room_active(&sim.state, &dinner, 1, 18));
    assert!(!room_active(&sim.state, &dinner, 1, 9));
    assert!(!room_active(&sim.state, &dinner, 2, 18));
    assert!(!room_active(&sim.state, &dinner, 8, 18));

    // A DM counts toward the person's hour, not toward any room.
    sim.model_says("Hi.");
    sim.dm("jo", "hi");
    assert!(sim.state.patterns.members.contains_key(&"jo".into()));
    assert!(!sim.state.patterns.rooms.contains_key(&"dm-jo".into()));

    // A week on, every count is halved.
    sim.advance(7 * DAY);
    sim.say("dinner", "alex", "back from holiday");
    assert_eq!(sim.state.patterns.members[&sam][18], 1);
    assert_eq!(usual_hour(&sim.state, &sam), None);
    assert!(!sim.state.patterns.members.contains_key(&"priya".into()));

    // They are not facts and are never reported, only mentioned.
    assert!(sim.state.facts.is_empty());
    let text = dump(&sim.state, &sam, sim.now);
    assert!(text.contains(PATTERN_LINE));
    assert!(!text.contains("18"));
    let asked = sim.say("dinner", "sam", "Q, what do you know about me");
    assert!(!dms(&asked.effects)[0].1.contains("18"));
    assert!(classify_requests(&asked.effects).is_empty());
}
