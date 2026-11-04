//! It decides: proposals from a forming plan or a direct ask, reaction
//! ballots and native polls, votes in plain words, nudges, close rules,
//! the record and the calendar (DESIGN.md sections 6, 10 and 12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::proposal::{NUMBERS, Named, named};
use ours_core::sim::{
    ClassifierScript, Sim, TimeScript, any_send, classify_requests, dms, ended_polls, instant,
    messages, model_requests, notices, plan_score, polls, reactions, texts, time_requests,
};
use ours_core::step::EYES;
use ours_core::{
    Ballot, CloseRule, Command, Decision, GroupState, Input, Outcome, Proposal, ProposalKind,
    ProposalStatus, TimerPurpose, render_decisions, step, upcoming,
};
use ours_types::{
    Kind, PlatformMessageId, PlatformRole, PlatformUserId, Score, ScoreSource, TimeMeaning,
};

const MIN: i64 = 60;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;

fn regular(id: &'static str, name: &'static str) -> (&'static str, &'static str, PlatformRole) {
    (id, name, PlatformRole::Regular)
}

/// Everyone in `room` posts once, so they count as active members.
fn all_say_hello(sim: &mut Sim, room: &str, users: &[&str]) {
    for user in users {
        sim.say(room, user, "hello");
        sim.advance(MIN);
    }
}

fn only_proposal(sim: &Sim) -> &Proposal {
    assert_eq!(sim.state.proposals.len(), 1, "{:?}", sim.state.proposals);
    sim.state.proposals.values().next().unwrap()
}

fn latest_proposal(sim: &Sim) -> &Proposal {
    sim.state
        .proposals
        .values()
        .max_by_key(|p| (p.opened_at, p.id.seq()))
        .unwrap()
}

fn ballot_of(p: &Proposal) -> PlatformMessageId {
    p.ballot
        .message()
        .cloned()
        .expect("the ballot's id came back")
}

fn only_decision(sim: &Sim) -> &Decision {
    assert_eq!(sim.state.decisions.len(), 1);
    sim.state.decisions.values().next().unwrap()
}

/// DMs sent, as (user, text), sorted by user.
fn dm_list(effects: &[ours_core::Effect]) -> Vec<(&str, &str)> {
    let mut all: Vec<(&str, &str)> = dms(effects)
        .into_iter()
        .map(|(u, t)| (u.as_str(), t))
        .collect();
    all.sort();
    all
}

fn proposal_timers(sim: &Sim) -> usize {
    sim.state
        .timers
        .values()
        .filter(|t| {
            matches!(
                t.purpose,
                TimerPurpose::Nudge { .. } | TimerPurpose::Close { .. }
            )
        })
        .count()
}

const SIX: [&str; 6] = ["sam", "priya", "jo", "alex", "marcus", "kim"];

/// Six friends in #dinner in Melbourne, all about this month, and the clock
/// at Monday 19:00 there.
fn dinner_friends() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.zone("Australia/Melbourne");
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
            regular("marcus", "Marcus"),
            regular("kim", "Kim"),
        ],
    );
    sim.advance_to(instant("2026-11-05T12:00:00+11:00"));
    all_say_hello(&mut sim, "dinner", &SIX);
    sim.advance_to(instant("2026-11-09T19:00:00+11:00"));
    sim
}

#[test]
fn the_monday_dinner_closes_on_a_majority_of_active_members() {
    let mut sim = dinner_friends();
    let plan = sim.say("dinner", "sam", "dinner this week?");
    sim.advance(MIN);
    sim.say("dinner", "priya", "yes pls");
    sim.advance(MIN);
    sim.classify(ClassifierScript::Score(plan_score()));
    let offered = sim.say("dinner", "jo", "thurs or fri");

    // A forming plan with two days on offer opens a reaction ballot.
    let ballot_text = "🗳️ Dinner: Thursday or Friday?\n1️⃣ Thursday the 12th\n2️⃣ Friday the 13th";
    assert_eq!(
        messages(&offered.effects),
        vec![(ballot_text, Some(&plan.id))]
    );
    let p = only_proposal(&sim).clone();
    let ballot = ballot_of(&p);
    assert_eq!(
        reactions(&offered.effects),
        vec![(&ballot, "1️⃣"), (&ballot, "2️⃣")]
    );
    assert_eq!(p.rule, CloseRule::MajorityOfActive);
    assert_eq!(p.topic.as_deref(), Some("Dinner"));
    assert_eq!(
        p.organiser.as_ref().map(PlatformUserId::as_str),
        Some("sam")
    );
    assert_eq!(p.deadline, instant("2026-11-10T19:02:00+11:00"));
    assert_eq!(sim.state.budget.spent.len(), 1);
    // Jo's line is kept as what was said.
    assert_eq!(p.remarks, vec![("jo".into(), offered.id.clone())]);

    // Four of six react within the hour; one prefers Friday.
    let mut last = Vec::new();
    for (user, emoji) in [
        ("sam", "1️⃣"),
        ("priya", "1️⃣"),
        ("alex", "2️⃣"),
        ("jo", "1️⃣"),
        ("kim", "1️⃣"),
    ] {
        sim.advance(8 * MIN);
        last = sim.react("dinner", &ballot, user, emoji);
        if user != "kim" {
            assert!(!any_send(&last), "{user} should not close it");
        }
    }
    assert_eq!(
        messages(&last),
        vec![(
            "Thursday it is, 4 to 1. Holding Thursday the 12th.",
            Some(&ballot)
        )]
    );
    let d = only_decision(&sim);
    assert_eq!(d.winner, 0);
    assert_eq!(d.outcome, Outcome::Count { counts: vec![4, 1] });
    assert_eq!(d.rule, CloseRule::MajorityOfActive);
    assert_eq!(d.open_seconds, 40 * MIN);
    assert_eq!(d.said.len(), 1);
    assert_eq!(d.said[0].text, "thurs or fri");
    assert!(matches!(
        only_proposal(&sim).status,
        ProposalStatus::Closed(_)
    ));
    assert_eq!(proposal_timers(&sim), 0);

    // Thursday is on the calendar, on the right day in Melbourne, for four.
    let entry = &sim.state.calendar[0];
    assert_eq!(entry.label, "Dinner");
    assert_eq!(entry.attendees.len(), 4);
    let TimeMeaning::Instant { at, has_time } = entry.when else {
        panic!("a day");
    };
    assert!(!has_time);
    assert_eq!(at, instant("2026-11-12T00:00:00+11:00"));
    assert_eq!(upcoming(&sim.state, sim.now, 7).len(), 1);
    assert!(upcoming(&sim.state, sim.now.plus_seconds(4 * DAY), 7).is_empty());

    // The model hears what is coming up and nothing is open.
    sim.model_says("Sure.");
    let asked = sim.say("dinner", "alex", "Q, anything I should know?");
    let system = &model_requests(&asked.effects)[0].system;
    assert!(
        system.contains("Upcoming: Thursday the 12th, Dinner, 4 coming."),
        "{system}"
    );
    assert!(!system.contains("Open:"), "{system}");

    // Tuesday: a late can't-do does not reopen; it offers a re-vote.
    sim.advance_to(instant("2026-11-10T12:00:00+11:00"));
    let cant = sim.say("dinner", "marcus", "Q I can't do thursday");
    assert_eq!(
        messages(&cant.effects),
        vec![(
            "Noted. Four still can. Keep Thursday the 12th, or re-vote?",
            Some(&cant.id)
        )]
    );
    assert!(model_requests(&cant.effects).is_empty());
    assert_eq!(sim.state.proposals.len(), 1);
    let gone = sim.say("dinner", "kim", "ah wait, I can't do thursday either");
    assert!(texts(&gone.effects)[0].starts_with("Noted. Three still can."));
    assert_eq!(sim.state.calendar[0].attendees.len(), 3);
    let keep = sim.say("dinner", "sam", "keep it");
    assert!(!any_send(&keep.effects));

    // "Why did we pick Thursday" is answered from the record.
    let why = sim.say("dinner", "alex", "Q, why did we pick Thursday?");
    assert!(model_requests(&why.effects).is_empty());
    assert_eq!(
        texts(&why.effects),
        vec![
            "Dinner: Thursday the 12th, 4 to 1 on majority of active members, decided Monday the 9th after 40 minutes.\nThe other option was Friday."
        ]
    );

    // Words that match no record go to the model.
    sim.model_says("Nothing on a venue yet.");
    let venue = sim.say("dinner", "alex", "Q, what did we decide about the venue?");
    assert_eq!(model_requests(&venue.effects).len(), 1);

    // "re-vote" reopens a copy with a fresh ballot.
    let again = sim.say("dinner", "kim", "Q, re-vote");
    assert_eq!(sim.state.proposals.len(), 2);
    let copy = latest_proposal(&sim).clone();
    assert!(copy.is_open());
    assert!(copy.options.iter().all(|o| o.votes.is_empty()));
    assert_eq!(copy.reopens.as_ref(), Some(&only_decision(&sim).id));
    assert_eq!(
        texts(&again.effects),
        vec!["🗳️ Dinner: Thursday or Friday?\n1️⃣ Thursday the 12th\n2️⃣ Friday the 13th"]
    );
}

#[test]
fn the_monday_print_run_is_decided_by_a_lead() {
    let mut sim = Sim::new(Kind::Team);
    sim.zone("Europe/London");
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            regular("sam", "Sam"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
        ],
    );
    sim.feed(Input::Command(Command::AddLead { user: "sam".into() }));
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    let asked = sim.say(
        "launch",
        "jo",
        "are we going with Acme or Beacon for the print run?",
    );
    sim.advance(2 * MIN);
    sim.classify(ClassifierScript::Score(Score {
        plan_forming: 0.9,
        source: ScoreSource::SmallModel {
            model: "sim".into(),
        },
        ..Score::default()
    }));
    let opened = sim.say(
        "launch",
        "alex",
        "Beacon were cheaper but Acme hit the date last time",
    );
    // Lead decides: no ballot, one marker line in the thread.
    assert_eq!(
        messages(&opened.effects),
        vec![(
            "📌 Open: Acme or Beacon. Lead decides; I'll keep the thread.",
            Some(&asked.id)
        )]
    );
    assert!(reactions(&opened.effects).is_empty());
    let p = only_proposal(&sim);
    assert_eq!(p.rule, CloseRule::LeadDecides);
    assert_eq!(p.ballot, Ballot::None);
    assert_eq!(p.topic.as_deref(), Some("Print run"));

    // A member's view is kept, not counted.
    sim.advance(5 * MIN);
    let view = sim.say("launch", "jo", "Beacon for me");
    assert!(!any_send(&view.effects));
    assert_eq!(only_proposal(&sim).options[1].views.len(), 1);

    // "Where are we" reads the views back.
    let tally = sim.say("launch", "priya", "Q, where are we?");
    assert_eq!(
        texts(&tally.effects),
        vec!["Print run: Acme: nobody yet, Beacon (Jo).\nOpen 5 minutes; a lead decides."]
    );

    // A lead's statement in the thread closes it, with the reason kept.
    sim.advance(3 * MIN);
    let decided = sim.say("launch", "sam", "Acme, the date matters more than 8%");
    assert_eq!(
        messages(&decided.effects),
        vec![("Decided: Acme, by Sam, for the date.", Some(&asked.id))]
    );
    let d = only_decision(&sim);
    assert_eq!(d.outcome, Outcome::Decider { user: "sam".into() });
    assert_eq!(d.reason.as_deref(), Some("the date matters more than 8%"));
    assert_eq!(d.closed_by.as_ref(), Some(&decided.id));
    let said: Vec<(&str, &str)> = d
        .said
        .iter()
        .map(|s| (s.name.as_str(), s.text.as_str()))
        .collect();
    assert_eq!(
        said,
        vec![
            ("Jo", "are we going with Acme or Beacon for the print run?"),
            (
                "Alex",
                "Beacon were cheaper but Acme hit the date last time"
            ),
            ("Jo", "Beacon for me"),
            ("Sam", "Acme, the date matters more than 8%"),
        ]
    );
    assert!(sim.state.calendar.is_empty());

    // In February, "why did we go with Acme" gets the answer.
    sim.advance(90 * DAY);
    let why = sim.say("launch", "alex", "Q, why did we go with Acme?");
    assert_eq!(
        texts(&why.effects),
        vec![
            "Print run: Acme, decided by Sam on Monday 9 November 2026: \"the date matters more than 8%\".\nThe other option was Beacon."
        ]
    );

    // Anything else about it reaches the model with the record attached.
    sim.model_says("Yes, Acme.");
    let asked = sim.say("launch", "alex", "Q, is the print run still with Acme?");
    let system = &model_requests(&asked.effects)[0].system;
    assert!(
        system.contains(
            "Decisions that may matter:\n- Print run: Acme or Beacon?: Acme, by Sam (lead decides), because \"the date matters more than 8%\""
        ),
        "{system}"
    );
    // Under lead decides only a lead may reopen it.
    let again = sim.say("launch", "jo", "Q, re-vote");
    assert_eq!(texts(&again.effects), vec!["That's one for Priya or Sam."]);
    assert_eq!(sim.state.proposals.len(), 1);

    // The decision log has it all.
    let log = render_decisions(&sim.state);
    assert!(
        log.contains("## 2026-11-09: Print run: Acme or Beacon?"),
        "{log}"
    );
    assert!(log.contains("- Decided: Acme, by Sam"), "{log}");
    assert!(
        log.contains("- Reason: \"the date matters more than 8%\""),
        "{log}"
    );
    assert!(log.contains("  - Alex: \"Beacon were cheaper but Acme hit the date last time\""));
}

#[test]
fn a_member_cannot_decide_under_lead_decides_but_a_lead_can_go_with_one() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[("priya", "Priya", PlatformRole::Admin), regular("jo", "Jo")],
    );
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    let ask = sim.say("launch", "jo", "Q, poll: Acme or Beacon");
    // A direct ask gets a ballot even when a lead decides.
    assert_eq!(
        texts(&ask.effects)[0],
        "🗳️ Acme or Beacon?\n1️⃣ Acme\n2️⃣ Beacon"
    );
    let pid = only_proposal(&sim).id.clone();
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("launch", &ballot, "jo", "2️⃣");
    assert!(only_proposal(&sim).is_open());
    let member = sim.say("launch", "jo", "Q, go with Beacon");
    assert_eq!(texts(&member.effects), vec!["That's one for Priya."]);
    let lead = sim.say(
        "launch",
        "priya",
        "Q, go with Acme because the date matters",
    );
    assert_eq!(
        texts(&lead.effects),
        vec!["Decided: Acme, by Priya, for the date."]
    );
    let d = only_decision(&sim);
    assert_eq!(d.proposal, pid);
    assert_eq!(d.reason.as_deref(), Some("the date matters"));
}

/// Three friends active in #dinner, group zone UTC, Monday 10:00.
fn three_friends() -> Sim {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
        ],
    );
    sim.advance_to(instant("2026-11-09T09:50:00Z"));
    all_say_hello(&mut sim, "dinner", &["sam", "priya", "jo"]);
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    sim
}

#[test]
fn a_native_poll_platform_uses_create_poll_and_poll_votes() {
    let mut sim = three_friends();
    sim.native_polls();
    let ask = sim.say("dinner", "sam", "Q, poll thursday vs friday");
    assert_eq!(
        polls(&ask.effects),
        vec![(
            "Thursday or Friday?",
            &["Thursday the 12th".to_owned(), "Friday the 13th".to_owned()][..],
            24
        )]
    );
    assert!(texts(&ask.effects).is_empty());
    let poll = sim.last_poll().unwrap();
    let p = only_proposal(&sim);
    assert_eq!(
        p.ballot,
        Ballot::Native {
            message: Some(poll.clone())
        }
    );
    assert!(sim.state.rooms[&"dinner".into()].is_own(&poll));

    sim.poll_vote("dinner", &poll, "sam", 0, true);
    sim.poll_vote("dinner", &poll, "priya", 1, true);
    assert_eq!(only_proposal(&sim).counts(), vec![1, 1]);
    sim.poll_vote("dinner", &poll, "priya", 1, false);
    assert_eq!(only_proposal(&sim).counts(), vec![1, 0]);
    let closed = sim.poll_vote("dinner", &poll, "jo", 0, true);
    assert_eq!(
        texts(&closed),
        vec!["Thursday it is, 2 to 0. Holding Thursday the 12th."]
    );
    assert_eq!(ended_polls(&closed), vec![&poll]);
    // The platform's own end after that changes nothing.
    assert!(sim.poll_ended("dinner", &poll, &[2, 0]).is_empty());

    // A poll the platform ends while still open closes on its counts.
    sim.say("dinner", "jo", "Q, poll: pizza, thai or indian");
    let second = sim.last_poll().unwrap();
    let ended = sim.poll_ended("dinner", &second, &[0, 1, 0]);
    assert_eq!(texts(&ended), vec!["Thai it is, 1 to 0."]);
    assert!(ended_polls(&ended).is_empty());
    assert_eq!(sim.state.decisions.len(), 2);
}

#[test]
fn reaction_votes_change_and_unchange() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
            regular("kim", "Kim"),
            ("dana", "Dana", PlatformRole::External),
        ],
    );
    sim.advance_to(instant("2026-11-09T09:50:00Z"));
    all_say_hello(
        &mut sim,
        "dinner",
        &["sam", "priya", "jo", "alex", "kim", "dana"],
    );
    sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let ballot = ballot_of(only_proposal(&sim));
    let vote = |sim: &Sim| only_proposal(sim).vote_of(&"sam".into());

    sim.react("dinner", &ballot, "sam", "1️⃣");
    assert_eq!(vote(&sim), Some(0));
    sim.react("dinner", &ballot, "sam", "2️⃣");
    assert_eq!(vote(&sim), Some(1));
    assert_eq!(only_proposal(&sim).counts(), vec![0, 1]);
    // Taking the newer one off falls back to the one still on.
    sim.unreact("dinner", &ballot, "sam", "2️⃣");
    assert_eq!(vote(&sim), Some(0));
    sim.unreact("dinner", &ballot, "sam", "1️⃣");
    assert_eq!(vote(&sim), None);
    // Platforms that send the keycap without its variation selector count too.
    sim.react("dinner", &ballot, "sam", "2\u{20e3}");
    assert_eq!(vote(&sim), Some(1));
    // A guest's reaction and the agent's own are not votes.
    sim.react("dinner", &ballot, "dana", "1️⃣");
    sim.react("dinner", &ballot, "ours-bot", "1️⃣");
    assert_eq!(only_proposal(&sim).counts(), vec![0, 1]);
    // Other emoji on the ballot change nothing.
    sim.react("dinner", &ballot, "priya", "🎉");
    assert_eq!(only_proposal(&sim).counts(), vec![0, 1]);
}

#[test]
fn a_line_that_names_an_option_is_a_vote() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
            regular("kim", "Kim"),
            ("dana", "Dana", PlatformRole::External),
        ],
    );
    sim.advance_to(instant("2026-11-09T09:50:00Z"));
    all_say_hello(
        &mut sim,
        "dinner",
        &["sam", "priya", "jo", "alex", "kim", "dana"],
    );
    sim.say("dinner", "sam", "Q, poll thursday vs friday");

    let works = sim.say("dinner", "priya", "thursday works");
    assert_eq!(reactions(&works.effects), vec![(&works.id, NUMBERS[0])]);
    assert!(texts(&works.effects).is_empty());
    let p = only_proposal(&sim);
    assert_eq!(p.vote_of(&"priya".into()), Some(0));
    assert_eq!(p.options[0].views, vec![("priya".into(), works.id.clone())]);

    sim.say("dinner", "jo", "friday for me");
    sim.say("dinner", "alex", "2");
    let p = only_proposal(&sim);
    assert_eq!(p.counts(), vec![1, 2]);
    // The classifier hears what is open.
    let request = classify_requests(&sim.effects).into_iter().last().unwrap();
    assert_eq!(request.open_items, vec!["Thursday or Friday (3 votes)"]);
    assert_eq!(request.rules.open_decision, 0.9);

    // "Whatever works" is an abstention, not a vote for the leader.
    sim.say("dinner", "kim", "whatever works for everyone");
    let p = only_proposal(&sim);
    assert_eq!(p.counts(), vec![1, 2]);
    assert!(p.abstained.contains_key(&"kim".into()));
    // A guest's preference is a view, never a vote.
    let guest = sim.say("dinner", "dana", "thursday works");
    let p = only_proposal(&sim);
    assert_eq!(p.counts(), vec![1, 2]);
    assert!(p.options[0].views.iter().any(|(u, _)| u.as_str() == "dana"));
    assert!(reactions(&guest.effects).is_empty());
    // A question is not a vote.
    sim.say("dinner", "priya", "wait, is friday the 13th?");
    assert_eq!(only_proposal(&sim).vote_of(&"priya".into()), Some(0));
    // A line the classifier is sure about counts even when it is long.
    sim.classify(ClassifierScript::Score(Score {
        open_decision: 0.95,
        source: ScoreSource::SmallModel {
            model: "sim".into(),
        },
        ..Score::default()
    }));
    sim.say(
        "dinner",
        "priya",
        "honestly after the week I've had I'm leaning friday now if that's ok",
    );
    // Three of five on Friday is a majority of the active members.
    assert!(!only_proposal(&sim).is_open());
    assert_eq!(
        only_decision(&sim).outcome,
        Outcome::Count { counts: vec![0, 3] }
    );
    assert_eq!(
        named(
            "honestly after the week I've had I'm leaning friday now",
            only_proposal(&sim)
        ),
        Named::One {
            index: 1,
            clear: false
        }
    );
}

#[test]
fn an_addressed_vote_gets_a_word_back() {
    let mut sim = three_friends();
    sim.say("dinner", "sam", "Q, poll: pizza, thai or indian");
    let said = sim.say("dinner", "priya", "Q, thai please");
    assert_eq!(
        messages(&said.effects),
        vec![("Counted: Thai.", Some(&said.id))]
    );
    assert_eq!(
        reactions(&said.effects),
        vec![(&said.id, EYES), (&said.id, NUMBERS[1])]
    );
    assert!(model_requests(&said.effects).is_empty());
}

#[test]
fn nudges_go_at_halfway_and_near_the_deadline_to_non_voters_only() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
            regular("alex", "Alex"),
        ],
    );
    sim.advance_to(instant("2026-11-09T11:50:00Z"));
    all_say_hello(&mut sim, "dinner", &["sam", "priya", "jo", "alex"]);
    sim.advance_to(instant("2026-11-09T12:00:00Z"));
    sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let p = only_proposal(&sim).clone();
    assert_eq!(p.deadline, instant("2026-11-10T12:00:00Z"));
    let ballot = ballot_of(&p);
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.react("dinner", &ballot, "priya", "1️⃣");

    // Halfway is midnight, inside quiet hours, so the nudges wait for 08:00.
    let night = sim.advance_to(instant("2026-11-10T07:59:00Z"));
    assert!(dms(&night).is_empty());
    let morning = sim.advance_to(instant("2026-11-10T08:00:00Z"));
    let text = "Thursday or Friday in #dinner is open: 1 Thursday the 12th, 2 Friday the 13th. Reply with a number or react on the poll.";
    assert_eq!(dm_list(&morning), vec![("alex", text), ("jo", text)]);

    // Jo answers by DM with a number.
    let answer = sim.dm("jo", "2");
    assert_eq!(
        texts(&answer.effects),
        vec!["Counted: Friday the 13th, for Thursday or Friday in #dinner."]
    );
    assert_eq!(only_proposal(&sim).counts(), vec![2, 1]);

    // Near the deadline only Alex hears again.
    let late = sim.advance_to(instant("2026-11-10T09:00:00Z"));
    assert_eq!(
        dm_list(&late),
        vec![(
            "alex",
            "Thursday or Friday in #dinner closes in 3 hours: 1 Thursday the 12th, 2 Friday the 13th. Reply with a number or react on the poll."
        )]
    );
    assert_eq!(only_proposal(&sim).nudged[&PlatformUserId::new("alex")], 2);

    // A later deadline brings new nudge times, but never a third nudge.
    let moved = sim.say("dinner", "priya", "Q, deadline wednesday 6pm");
    assert_eq!(
        texts(&moved.effects),
        vec!["Thursday or Friday now closes Wednesday the 11th at 6pm, tomorrow."]
    );
    let rest = sim.advance_to(instant("2026-11-11T17:59:00Z"));
    assert!(dms(&rest).is_empty(), "{:?}", dms(&rest));
    let closed = sim.advance_to(instant("2026-11-11T18:00:00Z"));
    assert_eq!(
        texts(&closed),
        vec!["Thursday it is, 2 to 1. Holding Thursday the 12th."]
    );
    assert_eq!(proposal_timers(&sim), 0);
}

#[test]
fn nudges_move_to_the_hour_a_member_usually_answers() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
        ],
    );
    // Jo usually posts at nine in the evening.
    for day in 4..=6 {
        sim.advance_to(instant(&format!("2026-11-0{day}T21:00:00Z")));
        sim.say("dinner", "jo", "evening all");
    }
    sim.advance_to(instant("2026-11-09T07:50:00Z"));
    all_say_hello(&mut sim, "dinner", &["sam", "priya"]);
    sim.advance_to(instant("2026-11-09T08:00:00Z"));
    sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "1️⃣");
    // Halfway is 20:00; Jo's nudge moves to their hour, still in the window.
    let half = sim.advance_to(instant("2026-11-09T20:00:00Z"));
    let to: Vec<&str> = dm_list(&half).into_iter().map(|(u, _)| u).collect();
    assert_eq!(to, vec!["priya"]);
    let before = sim.advance_to(instant("2026-11-09T20:59:00Z"));
    assert!(dms(&before).is_empty());
    let theirs = sim.advance_to(instant("2026-11-09T21:00:00Z"));
    let to: Vec<&str> = dm_list(&theirs).into_iter().map(|(u, _)| u).collect();
    assert_eq!(to, vec!["jo"]);
}

#[test]
fn two_proposals_in_one_thread_close_in_one_line() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
        ],
    );
    sim.thread_room(
        "plans",
        "friday plans",
        "dinner",
        &[
            regular("sam", "Sam"),
            regular("priya", "Priya"),
            regular("jo", "Jo"),
        ],
    );
    sim.advance_to(instant("2026-11-09T09:50:00Z"));
    all_say_hello(&mut sim, "plans", &["sam", "priya", "jo"]);
    sim.say("plans", "sam", "Q, poll when: thursday or friday");
    sim.say("plans", "sam", "Q, poll where: thai or pizza");
    assert_eq!(sim.state.proposals.len(), 2);
    let mut ps: Vec<Proposal> = sim.state.proposals.values().cloned().collect();
    ps.sort_by_key(|p| (p.opened_at, p.id.seq()));
    let when = ballot_of(&ps[0]);
    let place = ballot_of(&ps[1]);
    assert_eq!(ps[0].question, "When: Thursday or Friday?");

    sim.react("plans", &when, "sam", "1️⃣");
    let first = sim.react("plans", &when, "priya", "1️⃣");
    // Closed, but the line waits for the other one.
    assert!(texts(&first).is_empty());
    assert_eq!(sim.state.decisions.len(), 1);
    sim.react("plans", &place, "sam", "1️⃣");
    let both = sim.react("plans", &place, "jo", "1️⃣");
    assert_eq!(
        texts(&both),
        vec!["Decided: Thursday, 2 to 0; Thai, 2 to 0. Holding Thursday the 12th."]
    );
    assert_eq!(sim.state.decisions.len(), 2);
    assert!(sim.state.proposals.values().all(|p| p.held.is_none()));
}

#[test]
fn an_unanswered_proposal_lapses_after_two_deadlines_with_one_line() {
    let mut sim = three_friends();
    let ask = sim.say("dinner", "sam", "Q, poll: pizza or thai");
    let ballot = ballot_of(only_proposal(&sim));
    // The first deadline passes quietly.
    let first = sim.advance(DAY);
    assert!(texts(&first).is_empty());
    assert!(only_proposal(&sim).is_open());
    assert_eq!(only_proposal(&sim).lapsed, 1);
    let nudges_so_far = dms(&sim.effects).len();
    // The second does too, but for one line.
    let second = sim.advance(DAY);
    assert_eq!(
        messages(&second),
        vec![(
            "No votes on Pizza or Thai, so I've let it go.",
            Some(&ballot)
        )]
    );
    assert_eq!(only_proposal(&sim).status, ProposalStatus::Expired);
    assert!(sim.state.decisions.is_empty());
    // Dead plans are not nagged: nothing in the second window.
    assert_eq!(dms(&sim.effects).len(), nudges_so_far);
    assert_eq!(proposal_timers(&sim), 0);
    let _ = ask;
}

#[test]
fn whats_open_and_where_are_we() {
    let mut sim = three_friends();
    let none = sim.say("dinner", "jo", "Q, what's open?");
    assert_eq!(texts(&none.effects), vec!["Nothing's open."]);
    sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "2️⃣");
    sim.advance(3 * HOUR);
    let open = sim.say("dinner", "jo", "Q, what's open?");
    assert_eq!(
        texts(&open.effects),
        vec!["Thursday or Friday in #dinner: 1 vote, open 3 hours."]
    );
    let tally = sim.say("dinner", "jo", "Q, where are we");
    assert_eq!(
        texts(&tally.effects),
        vec![
            "Thursday or Friday: Thursday 0, Friday 1.\nOpen 3 hours; closes in 21 hours, or once one has 2 of 3."
        ]
    );
    // From a DM, what's open covers the member's rooms.
    let dm = sim.dm("priya", "what's open");
    assert_eq!(
        texts(&dm.effects),
        vec!["Thursday or Friday in #dinner: 1 vote, open 3 hours."]
    );
    // The model is told how long it has been open.
    sim.model_says("Sure.");
    let asked = sim.say("dinner", "jo", "Q, hi");
    let system = &model_requests(&asked.effects)[0].system;
    assert!(
        system.contains("Open: Thursday or Friday in #dinner, 1 vote, open 3 hours."),
        "{system}"
    );
}

#[test]
fn who_is_in_is_an_attendance_poll() {
    let mut sim = three_friends();
    let ask = sim.say("dinner", "sam", "Q, who's in for Saturday?");
    assert_eq!(
        texts(&ask.effects),
        vec!["🗳️ Who's in for Saturday the 14th? React 👍 if you are."]
    );
    let p = only_proposal(&sim).clone();
    assert_eq!(p.kind, ProposalKind::Attendance);
    assert_eq!(p.rule, CloseRule::WhoeverIsComing);
    let ballot = ballot_of(&p);
    assert_eq!(reactions(&ask.effects).last(), Some(&(&ballot, "👍")));
    sim.react("dinner", &ballot, "sam", "👍");
    sim.say("dinner", "jo", "I'm in");
    assert_eq!(only_proposal(&sim).counts(), vec![2]);
    let closed = sim.advance(DAY);
    assert_eq!(
        texts(&closed),
        vec!["Saturday the 14th: Jo and Sam are in."]
    );
    assert_eq!(sim.state.calendar[0].attendees.len(), 2);
}

#[test]
fn a_member_in_another_zone_gets_their_own_reading() {
    let mut sim = Sim::new(Kind::Friends);
    sim.zone("Australia/Melbourne");
    sim.room(
        "dinner",
        "dinner",
        &[regular("sam", "Sam"), regular("alex", "Alex")],
    );
    let said = sim.say("dinner", "alex", "Q, I'm in New York this month");
    assert_eq!(
        texts(&said.effects),
        vec!["Noted: America/New_York for you, 1pm there now."]
    );
    assert_eq!(
        ours_core::zone_of(&sim.state, &"alex".into()),
        "America/New_York"
    );
    // Thursday 07:00 in Melbourne, Wednesday 15:00 in New York.
    sim.advance_to(instant("2026-11-11T20:00:00Z"));
    let theirs = sim.say("dinner", "alex", "Q, who's in for tomorrow?");
    assert_eq!(
        texts(&theirs.effects),
        vec!["🗳️ Who's in for Thursday the 12th? React 👍 if you are."]
    );
    let ours = sim.say("dinner", "sam", "Q, who's in for tomorrow?");
    assert_eq!(
        texts(&ours.effects),
        vec!["🗳️ Who's in for Friday the 13th? React 👍 if you are."]
    );
    // The platform's zone never overrides a stated one.
    for (id, name) in [("alex", "Alex"), ("sam", "Sam")] {
        let mut member = ours_core::sim::member(id, name, PlatformRole::Regular);
        member.timezone = Some("europe/paris".into());
        sim.feed(Input::Inbound(ours_types::InboundEvent::MemberUpdated {
            room: "dinner".into(),
            member,
        }));
    }
    assert_eq!(
        ours_core::zone_of(&sim.state, &"alex".into()),
        "America/New_York"
    );
    assert_eq!(
        ours_core::zone_of(&sim.state, &"sam".into()),
        "Europe/Paris"
    );
}

#[test]
fn times_the_rules_cannot_read_go_to_the_resolver() {
    let mut sim = three_friends();
    sim.resolves(TimeScript::Resolve {
        meaning: TimeMeaning::Span {
            start: instant("2027-01-04T00:00:00Z"),
            end: instant("2027-01-11T00:00:00Z"),
        },
        echo: Some("the week of 4 January".into()),
    });
    let ask = sim.say(
        "dinner",
        "sam",
        "Q, poll: after the holidays or this weekend",
    );
    let asked = time_requests(&ask.effects);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].text, "After the holidays");
    assert_eq!(asked[0].zone, "UTC");
    assert_eq!(asked[0].local_now, "2026-11-09 10:00 Monday");
    let p = only_proposal(&sim);
    assert_eq!(p.options[0].echo.as_deref(), Some("the week of 4 January"));
    assert!(matches!(p.options[0].when, Some(TimeMeaning::Span { .. })));

    // A lead's deadline the rules cannot read waits for the resolver.
    sim.resolves(TimeScript::Silent);
    let moved = sim.say("dinner", "sam", "Q, deadline before the holidays");
    assert_eq!(time_requests(&moved.effects).len(), 1);
    assert!(texts(&moved.effects).is_empty());
    let answer = sim.feed(Input::TimeResolved(ours_types::TimeResolution {
        room: "dinner".into(),
        message: moved.id.clone(),
        phrase: Some("before the holidays".into()),
        meaning: TimeMeaning::Instant {
            at: instant("2026-11-10T17:00:00Z"),
            has_time: true,
        },
        echo: None,
    }));
    assert_eq!(
        texts(&answer),
        vec!["After the holidays or This weekend now closes Tuesday the 10th at 5pm, tomorrow."]
    );
    // Nobody answering the next one is owned up to.
    sim.resolves(TimeScript::Silent);
    sim.say("dinner", "sam", "Q, deadline when the exams finish");
    let guard = sim.advance(MIN);
    assert_eq!(
        texts(&guard),
        vec!["I couldn't tell when that is; try something like friday 6pm."]
    );
}

#[test]
fn observing_turns_a_poll_into_a_would_have_dm() {
    let mut sim = Sim::observing(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("olu", "Olu", PlatformRole::Owner),
            regular("sam", "Sam"),
            regular("jo", "Jo"),
        ],
    );
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    sim.say("dinner", "sam", "dinner this week?");
    sim.say("dinner", "olu", "yes");
    sim.classify(ClassifierScript::Score(plan_score()));
    let offered = sim.say("dinner", "jo", "thurs or fri");
    assert!(texts(&offered.effects).is_empty());
    assert!(sim.state.proposals.is_empty());
    let line = "In #dinner I would have opened a poll: Dinner: Thursday or Friday?\n👍 if I should have, 👎 if not.";
    assert_eq!(
        dm_list(&offered.effects),
        vec![("jo", line), ("olu", line), ("sam", line)]
    );
    assert!(
        !notices(&offered.effects)
            .iter()
            .any(|n| n.contains("polls arrive"))
    );
}

#[test]
fn a_lead_changes_the_rule_by_sentence() {
    let mut sim = three_friends();
    sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let changed = sim.say("dinner", "sam", "Q, unanimous on this one");
    assert_eq!(
        texts(&changed.effects),
        vec!["Thursday or Friday closes on unanimous now."]
    );
    assert!(notices(&changed.effects).iter().any(|n| n.starts_with(
        "Rule for Thursday or Friday changed from majority of active members to unanimous by Sam."
    )));
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.react("dinner", &ballot, "priya", "1️⃣");
    assert!(only_proposal(&sim).is_open());
    let closed = sim.react("dinner", &ballot, "jo", "1️⃣");
    assert_eq!(
        texts(&closed),
        vec!["Thursday it is, 3 to 0. Holding Thursday the 12th."]
    );
    // With nothing open, the rule becomes the group's.
    let group = sim.say("dinner", "sam", "Q, majority of voters");
    assert_eq!(
        texts(&group.effects),
        vec!["New proposals close on majority of those who voted."]
    );
    assert_eq!(sim.state.decision_rule, Some(CloseRule::MajorityOfVoters));
}

#[test]
fn decisions_round_trip_through_a_snapshot_and_keep_folding() {
    let mut sim = three_friends();
    sim.say("dinner", "sam", "Q, poll thursday vs friday");
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.react("dinner", &ballot, "priya", "1️⃣");
    sim.resolves(TimeScript::Silent);
    sim.resolves(TimeScript::Silent);
    sim.say(
        "dinner",
        "jo",
        "Q, poll: after the holidays or before christmas",
    );
    assert_eq!(sim.state.decisions.len(), 1);
    assert_eq!(sim.state.calendar.len(), 1);
    assert_eq!(sim.state.pending_times.len(), 2);
    let json = sim.state.snapshot().unwrap();
    let mut restored = GroupState::restore(&json).unwrap();
    assert_eq!(restored, sim.state);
    assert_eq!(serde_json::to_string(&restored).unwrap(), json);

    // Both copies take the same votes and the same deadline alike.
    let second = ballot_of(latest_proposal(&sim));
    let inputs = [
        Input::Inbound(ours_types::InboundEvent::ReactionAdded {
            room: "dinner".into(),
            message: second.clone(),
            user: "sam".into(),
            emoji: "2️⃣".into(),
            at: sim.now,
        }),
        Input::Inbound(ours_types::InboundEvent::ReactionAdded {
            room: "dinner".into(),
            message: second,
            user: "jo".into(),
            emoji: "2️⃣".into(),
            at: sim.now,
        }),
    ];
    for input in inputs {
        let original = step(&mut sim.state, input.clone(), sim.now);
        let replayed = step(&mut restored, input, sim.now);
        assert_eq!(original, replayed);
    }
    assert_eq!(restored, sim.state);
    assert_eq!(restored.decisions.len(), 2);

    // The whole log replays to the same state.
    let mut state = GroupState::new("group".into(), Kind::Friends);
    let mut effects = Vec::new();
    for (at, input) in sim.log.clone() {
        effects.extend(step(&mut state, input, at));
    }
    assert_eq!(effects, sim.effects);
}

#[test]
fn ties_go_to_the_organiser_then_the_earlier_date() {
    let mut sim = three_friends();
    sim.say("dinner", "priya", "Q, poll friday vs thursday");
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.react("dinner", &ballot, "jo", "2️⃣");
    // The organiser has not voted, so the earlier date wins the tie.
    let closed = sim.advance(DAY);
    assert_eq!(
        texts(&closed),
        vec!["Thursday it is, 1 to 1. Holding Thursday the 12th."]
    );

    sim.say("dinner", "sam", "Q, poll friday vs thursday");
    let ballot = ballot_of(latest_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.react("dinner", &ballot, "priya", "2️⃣");
    let closed = sim.advance(DAY);
    assert_eq!(
        texts(&closed),
        vec!["Friday it is, 1 to 1. Holding Friday the 13th."]
    );
}

#[test]
fn under_lead_decides_the_deadline_brings_a_tally_then_it_lapses() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[("priya", "Priya", PlatformRole::Admin), regular("jo", "Jo")],
    );
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    sim.say("launch", "jo", "are we going with Acme or Beacon?");
    sim.classify(ClassifierScript::Score(Score {
        plan_forming: 0.9,
        ..Score::default()
    }));
    sim.say("launch", "jo", "need to know by tomorrow");
    assert_eq!(only_proposal(&sim).rule, CloseRule::LeadDecides);
    sim.say("launch", "jo", "I'd say Beacon");
    let first = sim.advance(DAY);
    assert_eq!(
        texts(&first),
        vec!["Acme or Beacon: Acme: nobody yet, Beacon (Jo).\nOpen 24 hours; a lead decides."]
    );
    assert!(only_proposal(&sim).is_open());
    let second = sim.advance(DAY);
    assert_eq!(
        texts(&second),
        vec!["No lead has decided Acme or Beacon, so I've let it go. Say re-vote to reopen it."]
    );
    assert_eq!(only_proposal(&sim).status, ProposalStatus::Expired);
    // A lead can bring it back.
    let again = sim.say("launch", "priya", "Q, re-vote");
    assert_eq!(
        texts(&again.effects),
        vec!["🗳️ Acme or Beacon?\n1️⃣ Acme\n2️⃣ Beacon"]
    );
}

#[test]
fn a_revote_won_by_an_undated_option_takes_the_old_date_off_the_calendar() {
    let mut sim = three_friends();
    sim.say("dinner", "sam", "Q, poll: thursday 7pm or takeaway at home");
    let ballot = ballot_of(only_proposal(&sim));
    sim.react("dinner", &ballot, "sam", "1️⃣");
    sim.react("dinner", &ballot, "priya", "1️⃣");
    assert_eq!(sim.state.calendar.len(), 1);

    sim.say("dinner", "jo", "Q, re-vote");
    let again = ballot_of(latest_proposal(&sim));
    sim.react("dinner", &again, "sam", "2️⃣");
    let closed = sim.react("dinner", &again, "priya", "2️⃣");
    assert_eq!(texts(&closed), vec!["Takeaway at home it is, 2 to 0."]);
    assert_eq!(sim.state.decisions.len(), 2);
    assert!(sim.state.calendar.is_empty(), "{:?}", sim.state.calendar);
}

#[test]
fn an_option_the_resolver_reads_as_soon_brings_the_deadline_in() {
    let mut sim = three_friends();
    sim.resolves(TimeScript::Resolve {
        meaning: TimeMeaning::Instant {
            at: instant("2026-11-09T17:30:00Z"),
            has_time: true,
        },
        echo: Some("today at 5:30pm".into()),
    });
    let ask = sim.say("dinner", "sam", "Q, poll: after work or this weekend");
    assert_eq!(time_requests(&ask.effects).len(), 1);
    // Half the time to the soonest option, as if it had been read at once.
    let p = only_proposal(&sim);
    assert_eq!(p.deadline, instant("2026-11-09T13:45:00Z"));
    let closes: Vec<_> = sim
        .state
        .timers
        .values()
        .filter(|t| matches!(t.purpose, TimerPurpose::Close { .. }))
        .map(|t| t.at)
        .collect();
    assert_eq!(closes, vec![instant("2026-11-09T13:45:00Z")]);
    assert!(
        notices(&ask.effects)
            .iter()
            .any(|n| n.contains("now closes") && n.contains("sooner")),
        "{:?}",
        notices(&ask.effects)
    );
}

#[test]
fn an_addressed_statement_that_mentions_an_option_is_not_a_vote() {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[("priya", "Priya", PlatformRole::Admin), regular("jo", "Jo")],
    );
    sim.advance_to(instant("2026-11-09T10:00:00Z"));
    sim.say("launch", "jo", "Q, poll: Acme or Beacon");
    assert_eq!(only_proposal(&sim).rule, CloseRule::LeadDecides);
    sim.model_says("They did; it was a paper shortage.");
    let long = sim.say(
        "launch",
        "priya",
        "Q, I was reading that Acme had some trouble with a print run last spring",
    );
    assert!(only_proposal(&sim).is_open());
    assert!(sim.state.decisions.is_empty());
    assert_eq!(model_requests(&long.effects).len(), 1);
    // Nor is one by DM.
    sim.model_says("Noted.");
    let dm = sim.dm(
        "jo",
        "I heard Beacon had a delivery problem with their last big order",
    );
    assert_eq!(model_requests(&dm.effects).len(), 1);
    assert_eq!(only_proposal(&sim).counts(), vec![0, 0]);
    // A short line from a lead still decides.
    let short = sim.say("launch", "priya", "Q, Acme please");
    assert!(texts(&short.effects)[0].starts_with("Decided: Acme, by Priya"));
}
