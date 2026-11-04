//! It remembers: facts with scope, rooms, provenance and expiry, recall
//! that never crosses a room, sources on cited answers, and the rights
//! everyone has over what it holds (DESIGN.md sections 9 and 15).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::extract::NOTHING_TO_KEEP;
use ours_core::leads::DEFERRAL_REPLY;
use ours_core::memory::end_of_day;
use ours_core::rights::{
    FORGOT_THAT, ILL_DM_YOU, NOTHING_CITED, NOTHING_FROM_YOU, NOTHING_HELD, WHICH_ONE,
};
use ours_core::sim::{
    ClassifierScript, ExtractorScript, ModelScript, Posted, Sim, any_send, citing, dms, draft,
    extract_requests, messages, model_requests, notices, reactions, redactions, self_draft, texts,
    unreactions,
};
use ours_core::step::{CHECK, EYES};
use ours_core::{
    Command, DeferredChange, Fact, FactId, FactScope, GroupState, Input, MemoryChange, Place,
    Source, dump, recall, step,
};
use ours_types::{
    FactDraft, Kind, MessageKind, PlatformRole, PlatformUserId, Score, ScoreSource, SubjectKind,
};

const DAY: i64 = 24 * 60 * 60;

/// Four friends in #dinner; Sam and Jo also climb.
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
    sim.room(
        "climbing",
        "climbing",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim
}

/// Priya leads #launch as an admin; Sam and Marcus are members; Dana is a guest.
fn work() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
            ("marcus", "Marcus", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    sim
}

/// A message the small model scores as a fact, and what the extractor finds in it.
fn hear(sim: &mut Sim, room: &str, user: &str, text: &str, drafts: Vec<FactDraft>) -> Posted {
    sim.classify(ClassifierScript::Score(Score {
        fact: 0.8,
        kind: MessageKind::Statement,
        source: ScoreSource::SmallModel {
            model: "sim".into(),
        },
        ..Score::default()
    }));
    sim.extracts(drafts);
    sim.say(room, user, text)
}

fn fact<'a>(sim: &'a Sim, line: &str) -> &'a Fact {
    sim.state
        .facts
        .values()
        .find(|f| f.line() == line)
        .unwrap_or_else(|| panic!("no fact {line:?}"))
}

/// The id of the fact that reads `line`, taken before it can be forgotten.
fn id(sim: &Sim, line: &str) -> FactId {
    fact(sim, line).id.clone()
}

fn by_id<'a>(sim: &'a Sim, id: &FactId) -> &'a Fact {
    &sim.state.facts[id]
}

/// A removed fact keeps its row and dates but none of its words or sources.
fn assert_scrubbed(fact: &Fact) {
    assert!(fact.retracted.is_some());
    assert_eq!(fact.line(), "");
    assert!(fact.quote.is_none() && fact.subject_user.is_none() && fact.sources.is_empty());
    assert!(!fact.rooms.is_empty());
}

fn lines(sim: &Sim, place: &Place, query: &str) -> Vec<String> {
    recall(&sim.state, place, query, sim.now, 8)
        .into_iter()
        .map(Fact::line)
        .collect()
}

fn room(id: &str) -> Place {
    Place::Room(id.into())
}

fn dm(user: &str) -> Place {
    Place::Dm(user.into())
}

fn perishable(base: FactDraft, until: Option<&str>) -> FactDraft {
    FactDraft {
        durable: false,
        expires: until.map(str::to_owned),
        ..base
    }
}

#[test]
fn facts_from_a_room_and_a_dm_get_their_scopes() {
    let mut sim = friends();
    sim.say("dinner", "sam", "booking Hobson's for thursday");
    sim.advance(60);
    let heard = hear(
        &mut sim,
        "dinner",
        "jo",
        "Priya's vegetarian now, fyi",
        vec![draft("Priya", "is", "vegetarian")],
    );

    let requests = extract_requests(&heard.effects);
    assert_eq!(requests.len(), 1);
    let request = requests[0];
    assert_eq!(request.message, heard.id);
    assert_eq!(request.speaker, "Jo");
    assert_eq!(request.agent_name, "Q");
    assert!(!request.private);
    assert_eq!(request.date, "2026-11-02");
    assert_eq!(request.timezone, "UTC");
    let context: Vec<(&str, &str, i64)> = request
        .context
        .iter()
        .map(|l| (l.speaker.as_str(), l.text.as_str(), l.seconds_before))
        .collect();
    assert_eq!(context, vec![("Sam", "booking Hobson's for thursday", 60)]);
    assert!(
        notices(&heard.effects).contains(&"remembered: Priya is vegetarian, from #dinner"),
        "{:?}",
        notices(&heard.effects)
    );
    assert!(!any_send(&heard.effects));

    let priya = fact(&sim, "Priya is vegetarian");
    assert_eq!(priya.scope, FactScope::Group);
    assert_eq!(priya.subject_user, Some("priya".into()));
    assert_eq!(
        priya.rooms.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        vec!["dinner"]
    );
    assert_eq!(
        priya.sources,
        vec![Source {
            room: "dinner".into(),
            message: heard.id.clone(),
            speaker: "jo".into(),
            at: sim.now,
        }]
    );
    assert_eq!(priya.confidence, 0.9);
    assert_eq!(priya.expires, None);
    assert_eq!((priya.first_seen, priya.learned_at), (sim.now, sim.now));
    assert!(priya.retracted.is_none());

    // About themselves in a room: self-stated, kept in their own words.
    sim.extracts(vec![self_draft(
        "I",
        "is allergic to",
        "peanuts",
        "allergic to peanuts",
    )]);
    let own = sim.say("dinner", "sam", "I'm allergic to peanuts btw");
    assert_eq!(extract_requests(&own.effects).len(), 1);
    let sam = fact(&sim, "Sam is allergic to peanuts");
    assert_eq!(sam.scope, FactScope::SelfStated("sam".into()));
    assert_eq!(sam.subject_user, Some("sam".into()));
    assert_eq!(sam.quote.as_deref(), Some("allergic to peanuts"));

    // Said in a DM: private to the one who said it. Implied, so 0.6.
    sim.model_says("Congrats on the move.");
    sim.extracts(vec![FactDraft {
        stated: false,
        ..perishable(
            self_draft("Sam", "is moving to", "Leith", "moving to Leith"),
            Some("2027-03-31"),
        )
    }]);
    let said = sim.dm("sam", "I'm moving to Leith in March");
    assert_eq!(texts(&said.effects), vec!["Congrats on the move."]);
    let request = extract_requests(&said.effects)[0];
    assert!(request.private);
    let leith = fact(&sim, "Sam is moving to Leith");
    assert_eq!(leith.scope, FactScope::Private("sam".into()));
    assert_eq!(
        leith.rooms.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        vec!["dm-sam"]
    );
    assert_eq!(leith.confidence, 0.6);
    assert_eq!(leith.expires, end_of_day("2027-03-31", "UTC"));
    assert_eq!(leith.expires.unwrap().to_string(), "2027-04-01T00:00:00Z");
}

#[test]
fn a_guests_words_are_kept_by_the_same_rules() {
    let mut sim = work();
    sim.extracts(vec![self_draft("Dana", "is", "vegan", "I'm vegan")]);
    sim.say("launch", "dana", "I'm vegan, if lunch is on us");
    let dana = fact(&sim, "Dana at Acme is vegan");
    assert_eq!(dana.scope, FactScope::SelfStated("dana".into()));
    assert_eq!(dana.subject_user, Some("dana".into()));

    // A name that begins a display name still resolves.
    hear(
        &mut sim,
        "launch",
        "sam",
        "dana signs for acme",
        vec![draft("Dana", "signs for", "Acme")],
    );
    assert_eq!(
        fact(&sim, "Dana signs for Acme").subject_user,
        Some("dana".into())
    );
}

#[test]
fn a_repeat_reconfirms_and_a_change_supersedes() {
    let mut sim = friends();
    let first = hear(
        &mut sim,
        "dinner",
        "jo",
        "Priya's vegetarian",
        vec![draft("Priya", "is", "vegetarian")],
    );
    sim.advance(3600);
    let again = hear(
        &mut sim,
        "dinner",
        "alex",
        "priya doesn't eat meat",
        vec![draft("priya", "is", "Vegetarian")],
    );
    assert!(notices(&again.effects).contains(&"reconfirmed: Priya is vegetarian, from #dinner"));
    assert_eq!(sim.state.facts.len(), 1);
    let kept = fact(&sim, "Priya is vegetarian");
    let speakers: Vec<&str> = kept.sources.iter().map(|s| s.speaker.as_str()).collect();
    assert_eq!(speakers, vec!["jo", "alex"]);
    assert_eq!(kept.sources[0].message, first.id);
    assert_eq!(kept.first_seen, ours_core::sim::START);
    assert_eq!(kept.last_confirmed, sim.now);
    let old_id = kept.id.clone();

    sim.advance(3600);
    hear(
        &mut sim,
        "dinner",
        "jo",
        "priya's gone vegan",
        vec![draft("Priya", "is", "vegan")],
    );
    let old = &sim.state.facts[&old_id];
    let retraction = old.retracted.as_ref().unwrap();
    assert_eq!(retraction.reason, "superseded");
    assert_eq!(retraction.by, Some("jo".into()));
    assert_eq!(retraction.at, sim.now);
    assert!(fact(&sim, "Priya is vegan").retracted.is_none());
    assert_eq!(
        lines(&sim, &room("dinner"), "what does priya eat"),
        vec!["Priya is vegan"]
    );
    // The row stays: what was thought before is still on record.
    assert_eq!(sim.state.facts.len(), 2);
}

#[test]
fn perishable_facts_expire_at_the_end_of_their_day_in_the_groups_zone() {
    let mut sim = friends();
    sim.feed(Input::Command(Command::SetTimezone {
        zone: "America/New_York".into(),
    }));
    hear(
        &mut sim,
        "dinner",
        "jo",
        "alex is away till wednesday",
        vec![perishable(draft("Alex", "is", "away"), Some("2026-11-04"))],
    );
    let away = fact(&sim, "Alex is away");
    assert_eq!(
        away.expires.unwrap().to_string(),
        "2026-11-05T05:00:00Z",
        "the end of Wednesday in New York"
    );
    hear(
        &mut sim,
        "dinner",
        "priya",
        "sam's off sick",
        vec![perishable(draft("Sam", "is", "off sick"), None)],
    );
    assert_eq!(
        fact(&sim, "Sam is off sick").expires,
        Some(sim.now.plus_seconds(30 * DAY))
    );
    let place = room("dinner");
    assert_eq!(lines(&sim, &place, "is alex away"), vec!["Alex is away"]);

    let away_id = id(&sim, "Alex is away");
    // Expired at midnight in New York, before the nightly sweep at 04:00.
    sim.advance_to(ours_core::sim::instant("2026-11-05T06:00:00Z"));
    assert!(lines(&sim, &place, "is alex away").is_empty());
    assert!(
        by_id(&sim, &away_id).retracted.is_none(),
        "expired, not yet swept"
    );

    // The sweep scrubs it and keeps the empty row.
    let swept = sim.advance_to(ours_core::sim::instant("2026-11-05T09:00:00Z"));
    assert_scrubbed(by_id(&sim, &away_id));
    assert_eq!(
        by_id(&sim, &away_id).retracted.as_ref().unwrap().reason,
        "expired"
    );
    assert_eq!(notices(&swept), vec!["Retention: dropped 1 expired fact."]);
    assert_eq!(lines(&sim, &place, "is sam sick"), vec!["Sam is off sick"]);
    sim.advance(30 * DAY);
    assert!(lines(&sim, &place, "is sam sick").is_empty());
    assert_eq!(sim.state.facts.len(), 2);
}

#[test]
fn recall_finds_facts_by_their_words_and_by_name() {
    let mut sim = friends();
    hear(
        &mut sim,
        "dinner",
        "alex",
        "Hobson's was the one with the weird cocktails",
        vec![FactDraft {
            subject_kind: SubjectKind::Place,
            ..draft("Hobson's", "is", "the bar with the weird cocktails")
        }],
    );
    hear(
        &mut sim,
        "dinner",
        "jo",
        "Priya's vegetarian",
        vec![draft("Priya", "is", "vegetarian")],
    );
    hear(
        &mut sim,
        "dinner",
        "priya",
        "jo drove last time",
        vec![FactDraft {
            stated: false,
            ..draft("Jo", "drives", "")
        }],
    );
    hear(
        &mut sim,
        "dinner",
        "sam",
        "jo's got a van",
        vec![draft("Jo", "has", "a van")],
    );
    let place = room("dinner");
    assert_eq!(
        lines(&sim, &place, "Q, what was that bar with the cocktails?"),
        vec!["Hobson's is the bar with the weird cocktails"]
    );
    assert_eq!(
        lines(&sim, &place, "is Priya coming?"),
        vec!["Priya is vegetarian"]
    );
    // A two-letter name still finds its person; the stated fact ranks first.
    assert_eq!(
        lines(&sim, &place, "can jo give us a lift"),
        vec!["Jo has a van", "Jo drives"]
    );
    assert!(lines(&sim, &place, "the football tonight").is_empty());
    assert_eq!(
        recall(&sim.state, &place, "jo", sim.now, 1).len(),
        1,
        "the limit holds"
    );
}

#[test]
fn nothing_crosses_rooms_but_a_member_hears_it_in_a_dm() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "climbing",
        "sam",
        "the wall closes at nine on sundays",
        vec![FactDraft {
            subject_kind: SubjectKind::Place,
            ..draft("The wall", "closes at", "nine on Sundays")
        }],
    );
    let query = "when does the wall close";
    let wall = vec!["The wall closes at nine on Sundays".to_owned()];
    assert_eq!(lines(&sim, &room("climbing"), query), wall);
    assert!(lines(&sim, &room("dinner"), query).is_empty());
    assert_eq!(lines(&sim, &dm("jo"), query), wall);
    assert!(lines(&sim, &dm("priya"), query).is_empty());

    // The model is offered it only where it may be said.
    sim.model_says("No idea, sorry.");
    let asked = sim.say("dinner", "jo", "Q, when does the wall close on sundays?");
    let system = &model_requests(&asked.effects)[0].system;
    assert!(!system.contains("What you remember"), "{system}");
    sim.model_says("Nine on Sundays.");
    let in_dm = sim.dm("jo", "when does the wall close on sundays?");
    let system = &model_requests(&in_dm.effects)[0].system;
    assert!(
        system.contains(&format!(
            "What you remember that may matter:\n- The wall closes at nine on Sundays (Sam, 2026-11-02, msg {})",
            said.id
        )),
        "{system}"
    );
    assert!(sim.state.pending_requests.is_empty());

    // Private facts stay with the person who said them.
    sim.model_says("Good luck with the move.");
    sim.extracts(vec![self_draft(
        "Sam",
        "is moving to",
        "Leith",
        "moving to Leith",
    )]);
    sim.dm("sam", "I'm moving to Leith");
    let leith = vec!["Sam is moving to Leith".to_owned()];
    assert_eq!(lines(&sim, &dm("sam"), "leith"), leith);
    assert_eq!(
        lines(&sim, &Place::of(&sim.state, &"dm-sam".into()), "leith"),
        leith
    );
    assert!(lines(&sim, &dm("jo"), "leith").is_empty());
    assert!(lines(&sim, &room("climbing"), "leith").is_empty());
    assert!(lines(&sim, &room("dm-sam"), "leith").is_empty());
}

#[test]
fn a_self_stated_fact_reaches_the_model_only_in_its_own_words() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "dinner",
        "sam",
        "fwiw I don't drink any more",
        vec![self_draft(
            "Sam",
            "does not",
            "drink",
            "I don't drink any more",
        )],
    );
    sim.script(ModelScript::Reply(citing(
        "Sam said \"I don't drink any more\", so maybe not the wine bar.",
        &[said.id.as_str()],
    )));
    let asked = sim.say(
        "dinner",
        "jo",
        "Q, should we do the wine bar for Sam's birthday?",
    );
    let system = &model_requests(&asked.effects)[0].system;
    assert!(
        system.contains(&format!(
            "- Sam: \"I don't drink any more\" (Sam, 2026-11-02, msg {}); say this only in these words",
            said.id
        )),
        "{system}"
    );
    assert!(system.contains("never reworded"));
    // The sim platform has no links, so a quote is sourced in words.
    assert_eq!(
        texts(&asked.effects),
        vec![
            "Sam said \"I don't drink any more\", so maybe not the wine bar.\nSource: Sam in #dinner, 2026-11-02"
        ]
    );
}

#[test]
fn a_cited_answer_links_its_source_and_why_names_the_facts() {
    let mut sim = Sim::new(Kind::Friends);
    sim.discord_room(
        "200",
        "general",
        "100",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("priya", "Priya", PlatformRole::Regular),
        ],
    );
    let said = hear(
        &mut sim,
        "200",
        "sam",
        "priya's vegetarian btw",
        vec![draft("Priya", "is", "vegetarian")],
    );
    sim.advance(3600);
    sim.script(ModelScript::Reply(citing(
        "Hobson's has a veggie menu, so it works for Priya.",
        &[&format!("msg {}", said.id)],
    )));
    let asked = sim.say("200", "sam", "Q, where should we take Priya?");
    let system = &model_requests(&asked.effects)[0].system;
    assert!(
        system.contains(&format!(
            "- Priya is vegetarian (Sam, 2026-11-02, msg {})",
            said.id
        )),
        "{system}"
    );
    let reply = format!(
        "Hobson's has a veggie menu, so it works for Priya.\nSource: https://discord.com/channels/100/200/{}",
        said.id
    );
    assert_eq!(
        messages(&asked.effects),
        vec![(reply.as_str(), Some(&asked.id))]
    );
    let priya = fact(&sim, "Priya is vegetarian");
    assert_eq!(priya.last_confirmed, sim.now, "used without challenge");
    let id = priya.id.clone();

    let answer = sim.last_sent_message().unwrap();
    let trace = sim.state.rooms[&"200".into()].trace_of(&answer).unwrap();
    assert_eq!(trace.facts, vec![id]);
    let why = sim.reply("200", "priya", "why", &answer);
    assert!(model_requests(&why.effects).is_empty());
    let text = texts(&why.effects)[0];
    assert_eq!(text.lines().count(), 4, "{text}");
    assert_eq!(text.lines().last(), Some("From: Priya is vegetarian"));

    // A cite the model was never offered is ignored.
    sim.script(ModelScript::Reply(citing("Sure.", &["m999"])));
    let other = sim.say("200", "sam", "Q, sound good?");
    assert_eq!(texts(&other.effects), vec!["Sure."]);
}

#[test]
fn what_do_you_know_about_me_is_a_full_dump_by_dm() {
    let mut sim = friends();
    let drives = hear(
        &mut sim,
        "dinner",
        "jo",
        "sam drives, he can take us",
        vec![draft("Sam", "drives", "")],
    );
    sim.advance(60);
    let allergy = hear(
        &mut sim,
        "dinner",
        "sam",
        "I'm allergic to peanuts",
        vec![self_draft(
            "Sam",
            "is allergic to",
            "peanuts",
            "allergic to peanuts",
        )],
    );
    sim.advance(60);
    let sick = hear(
        &mut sim,
        "climbing",
        "sam",
        "off sick this week, no climbing",
        vec![perishable(
            self_draft("Sam", "is", "off sick", "off sick this week"),
            Some("2026-11-08"),
        )],
    );
    sim.advance(60);
    sim.model_says("Got it.");
    sim.extracts(vec![self_draft(
        "Sam",
        "has the number",
        "0412 555 019",
        "my new number is 0412 555 019",
    )]);
    let number = sim.dm("sam", "my new number is 0412 555 019");
    // Jo's private note about Sam stays Jo's.
    sim.extracts(vec![draft("Sam", "is getting", "a surprise party")]);
    let note = sim.dm("jo", "remember that Sam's party is a surprise");
    assert_eq!(
        texts(&note.effects),
        vec!["Noted: Sam is getting a surprise party."]
    );
    let surprise = fact(&sim, "Sam is getting a surprise party");
    assert_eq!(surprise.scope, FactScope::Private("jo".into()));
    assert_eq!(surprise.subject_user, Some("sam".into()));

    let asked = sim.say("dinner", "sam", "Q, what do you know about me?");
    assert!(model_requests(&asked.effects).is_empty());
    assert_eq!(
        messages(&asked.effects),
        vec![(ILL_DM_YOU, Some(&asked.id))]
    );
    let expected = format!(
        "Everything I hold about you:\n\
         Lasting:\n\
         Sam drives (Jo in #dinner, 2026-11-02, msg {})\n\
         Sam is allergic to peanuts (Sam in #dinner, 2026-11-02, msg {})\n\
         Sam has the number 0412 555 019 (Sam in a DM, 2026-11-02, msg {})\n\
         For now:\n\
         Sam is off sick, until 2026-11-08 (Sam in #climbing, 2026-11-02, msg {})\n\
         I also keep the hour you usually reply, for timing only.\n\
         Say forget me to clear all of it.",
        drives.id, allergy.id, number.id, sick.id
    );
    let sent = dms(&asked.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, &PlatformUserId::new("sam"));
    assert_eq!(sent[0].1, expected);
    assert!(!expected.contains("surprise"));

    // Asked in a DM, it comes straight back.
    let again = sim.dm("sam", "what do you know about me");
    assert_eq!(texts(&again.effects), vec![expected.as_str()]);

    // Someone who has said nothing gets one line, and a guest gets the same.
    let mut work = work();
    let guest = work.say("launch", "dana", "Q, what do you know about me");
    assert_eq!(texts(&guest.effects), vec![ILL_DM_YOU]);
    let sent = dms(&guest.effects);
    assert!(sent[0].1.starts_with(NOTHING_HELD), "{}", sent[0].1);
}

#[test]
fn forget_me_retracts_everything_about_them_in_one_line() {
    let mut sim = friends();
    let drives = hear(
        &mut sim,
        "dinner",
        "jo",
        "sam drives",
        vec![draft("Sam", "drives", "")],
    );
    let veggie = hear(
        &mut sim,
        "dinner",
        "sam",
        "priya's veggie",
        vec![draft("Priya", "is", "vegetarian")],
    );
    let dog = hear(
        &mut sim,
        "dinner",
        "jo",
        "alex has a dog",
        vec![draft("Alex", "has", "a dog")],
    );
    assert!(ours_core::usual_hour(&sim.state, &"sam".into()).is_none());
    let lol = sim.say("dinner", "sam", "lol");
    assert!(
        sim.state
            .patterns
            .members
            .contains_key(&PlatformUserId::new("sam"))
    );

    let ids = [id(&sim, "Sam drives"), id(&sim, "Priya is vegetarian")];
    let forgot = sim.say("dinner", "sam", "Q, forget me");
    assert!(model_requests(&forgot.effects).is_empty());
    assert_eq!(
        messages(&forgot.effects),
        vec![("Forgot what I knew about Sam, as asked.", Some(&forgot.id))]
    );
    for id in &ids {
        let gone = by_id(&sim, id);
        assert_scrubbed(gone);
        let retraction = gone.retracted.as_ref().unwrap();
        assert_eq!(retraction.reason, "forget me");
        assert_eq!(retraction.by, Some("sam".into()));
        assert_eq!(gone.first_seen, ours_core::sim::START);
    }
    assert!(fact(&sim, "Alex has a dog").retracted.is_none());
    assert!(
        !sim.state
            .patterns
            .members
            .contains_key(&PlatformUserId::new("sam"))
    );
    assert_eq!(dump(&sim.state, &"sam".into(), sim.now), NOTHING_HELD);
    assert!(lines(&sim, &room("dinner"), "does sam drive").is_empty());

    // The journal is scrubbed of everything Sam said and of what was said
    // about Sam; the ring keeps only the request and the rest.
    let sam = PlatformUserId::new("sam");
    assert_eq!(
        redactions(&forgot.effects),
        vec![(
            &[
                veggie.id.clone(),
                lol.id.clone(),
                forgot.id.clone(),
                drives.id.clone()
            ][..],
            Some(&sam)
        )]
    );
    assert!(
        sim.state
            .actions
            .iter()
            .all(|r| !r.summary.contains("drives") && !r.summary.contains("vegetarian")),
        "the record keeps no forgotten words"
    );
    let held: Vec<_> = sim.state.rooms[&"dinner".into()]
        .messages
        .iter()
        .map(|m| m.id.clone())
        .collect();
    assert!(held.contains(&forgot.id) && held.contains(&dog.id));
    for gone in [&veggie.id, &lol.id, &drives.id] {
        assert!(!held.contains(gone), "{gone:?}");
    }
}

#[test]
fn forget_that_is_for_your_own_facts_and_a_leads_for_anyone() {
    let mut sim = work();
    let lisbon = hear(
        &mut sim,
        "launch",
        "sam",
        "I'm in Lisbon all next week",
        vec![perishable(
            self_draft("Sam", "is in", "Lisbon", "in Lisbon all next week"),
            Some("2026-11-15"),
        )],
    );
    let signs = hear(
        &mut sim,
        "launch",
        "marcus",
        "priya signs off the print run",
        vec![draft("Priya", "signs off", "the print run")],
    );

    let lisbon_id = id(&sim, "Sam is in Lisbon");
    let signs_id = id(&sim, "Priya signs off the print run");

    // A member forgets their own words, and the journal loses the message.
    let own = sim.reply("launch", "sam", "Q, forget that", &lisbon.id);
    assert_eq!(texts(&own.effects), vec![FORGOT_THAT]);
    assert_eq!(
        redactions(&own.effects),
        vec![(std::slice::from_ref(&lisbon.id), None)]
    );
    assert_scrubbed(by_id(&sim, &lisbon_id));
    let gone = by_id(&sim, &lisbon_id).retracted.as_ref().unwrap();
    assert_eq!(
        (gone.reason.as_str(), gone.by.clone()),
        ("forget that", Some("sam".into()))
    );

    // Someone else's fact waits for a lead.
    let asked = sim.reply("launch", "sam", "Q, forget that", &signs.id);
    assert_eq!(texts(&asked.effects), vec![DEFERRAL_REPLY]);
    assert!(redactions(&asked.effects).is_empty());
    assert!(
        fact(&sim, "Priya signs off the print run")
            .retracted
            .is_none()
    );
    let (ask, deferral) = sim.state.deferrals.iter().next().unwrap();
    assert!(matches!(
        &deferral.change,
        DeferredChange::Memory {
            change: MemoryChange::Forget { .. }
        }
    ));
    let ask = ask.clone();
    let approved = sim.react("launch", &ask, "priya", CHECK);
    assert_eq!(
        texts(&approved),
        vec!["Forgot that. Asked by Sam, approved by Priya."]
    );
    assert!(by_id(&sim, &signs_id).retracted.is_some());
    assert_eq!(
        redactions(&approved),
        vec![(std::slice::from_ref(&signs.id), None)]
    );

    // A guest is declined; a lead forgets anyone's.
    let run = hear(
        &mut sim,
        "launch",
        "marcus",
        "the print run is 500 copies",
        vec![draft("The print run", "is", "500 copies")],
    );
    let run_id = id(&sim, "The print run is 500 copies");
    let guest = sim.reply("launch", "dana", "Q, forget that", &run.id);
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert!(redactions(&guest.effects).is_empty());
    assert!(
        fact(&sim, "The print run is 500 copies")
            .retracted
            .is_none()
    );
    let lead = sim.reply("launch", "priya", "Q, forget that", &run.id);
    assert_eq!(texts(&lead.effects), vec![FORGOT_THAT]);
    assert!(by_id(&sim, &run_id).retracted.is_some());

    // Alone, it means the last thing kept from the speaker.
    hear(
        &mut sim,
        "launch",
        "sam",
        "my birthday is 3 May",
        vec![self_draft(
            "Sam",
            "has a birthday on",
            "3 May",
            "my birthday is 3 May",
        )],
    );
    let birthday = id(&sim, "Sam has a birthday on 3 May");
    let alone = sim.say("launch", "sam", "Q, forget that");
    assert_eq!(texts(&alone.effects), vec![FORGOT_THAT]);
    assert_scrubbed(by_id(&sim, &birthday));
    let nothing = sim.say("launch", "dana", "Q, forget that");
    assert_eq!(texts(&nothing.effects), vec![NOTHING_FROM_YOU]);

    // On the agent's own message, it means the facts that message cited.
    let printer = hear(
        &mut sim,
        "launch",
        "marcus",
        "the printer is Inkwell",
        vec![draft("The printer", "is", "Inkwell")],
    );
    sim.script(ModelScript::Reply(citing(
        "Inkwell.",
        &[printer.id.as_str()],
    )));
    sim.say("launch", "priya", "Q, who's the printer?");
    let answer = sim.last_sent_message().unwrap();
    let printer_id = id(&sim, "The printer is Inkwell");
    let cited = sim.reply("launch", "priya", "forget that", &answer);
    assert_eq!(texts(&cited.effects), vec![FORGOT_THAT]);
    assert_scrubbed(by_id(&sim, &printer_id));
    assert_eq!(
        redactions(&cited.effects),
        vec![(std::slice::from_ref(&printer.id), None)]
    );
}

#[test]
fn thats_wrong_drops_what_was_cited_and_keeps_the_correction() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "dinner",
        "jo",
        "priya's vegetarian",
        vec![draft("Priya", "is", "vegetarian")],
    );
    sim.script(ModelScript::Reply(citing(
        "Priya's vegetarian, so the veggie place.",
        &[said.id.as_str()],
    )));
    sim.say("dinner", "sam", "Q, where can we take Priya?");
    let answer = sim.last_sent_message().unwrap();

    let vegetarian = id(&sim, "Priya is vegetarian");
    sim.extract(ExtractorScript::Silent);
    let wrong = sim.reply("dinner", "priya", "that's wrong, I'm vegan now", &answer);
    assert!(model_requests(&wrong.effects).is_empty());
    assert_eq!(reactions(&wrong.effects), vec![(&wrong.id, EYES)]);
    assert!(unreactions(&wrong.effects).is_empty(), "the 👀 waits");
    assert!(texts(&wrong.effects).is_empty());
    assert_scrubbed(by_id(&sim, &vegetarian));
    assert_eq!(
        redactions(&wrong.effects),
        vec![(std::slice::from_ref(&said.id), None)]
    );
    let old = by_id(&sim, &vegetarian).retracted.as_ref().unwrap();
    assert_eq!(
        (old.reason.as_str(), old.by.clone()),
        ("corrected", Some("priya".into()))
    );
    let request = extract_requests(&wrong.effects)[0];
    assert_eq!(request.text, "I'm vegan now");
    assert_eq!(request.speaker, "Priya");
    let last = request.context.last().unwrap();
    assert_eq!(
        (last.speaker.as_str(), last.text.as_str()),
        (
            "Q",
            "Priya's vegetarian, so the veggie place.\nSource: Jo in #dinner, 2026-11-02"
        )
    );

    let landed = sim.feed(Input::Extracted {
        room: "dinner".into(),
        message: wrong.id.clone(),
        extraction: ours_core::sim::facts(vec![self_draft(
            "Priya",
            "is",
            "vegan",
            "I'm vegan now",
        )]),
    });
    assert_eq!(
        messages(&landed),
        vec![("Corrected: Priya is vegan.", Some(&wrong.id))]
    );
    assert_eq!(unreactions(&landed), vec![(&wrong.id, EYES)]);
    let vegan = fact(&sim, "Priya is vegan");
    assert_eq!(vegan.scope, FactScope::SelfStated("priya".into()));
    assert_eq!(vegan.sources[0].speaker, PlatformUserId::new("priya"));
    assert_eq!(vegan.sources[0].message, wrong.id);

    // "why" still explains the old answer, without the words it removed.
    let why = sim.reply("dinner", "jo", "why", &answer);
    let text = texts(&why.effects)[0];
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(
        !text.contains("From:") && !text.contains("vegetarian"),
        "{text}"
    );

    // With nothing after it, it only drops; with nothing cited, it says so.
    let again = hear(
        &mut sim,
        "dinner",
        "jo",
        "sam drives",
        vec![draft("Sam", "drives", "")],
    );
    sim.script(ModelScript::Reply(citing(
        "Sam drives.",
        &[again.id.as_str()],
    )));
    sim.say("dinner", "alex", "Q, does Sam drive?");
    let answer = sim.last_sent_message().unwrap();
    let dropped = sim.reply("dinner", "alex", "that's not right", &answer);
    assert_eq!(texts(&dropped.effects), vec!["Dropped that."]);
    assert_eq!(
        redactions(&dropped.effects),
        vec![(std::slice::from_ref(&again.id), None)]
    );
    assert!(extract_requests(&dropped.effects).is_empty());
    sim.model_says("Thursday.");
    sim.say("dinner", "jo", "Q, when's dinner?");
    let plain = sim.last_sent_message().unwrap();
    let nothing = sim.reply("dinner", "jo", "wrong", &plain);
    assert_eq!(texts(&nothing.effects), vec![NOTHING_CITED]);
    assert!(redactions(&nothing.effects).is_empty());
}

#[test]
fn remember_that_keeps_a_fact_for_good() {
    let mut sim = friends();
    let said = sim.say("dinner", "sam", "got the car back today");
    assert!(extract_requests(&said.effects).is_empty());
    sim.extracts(vec![FactDraft {
        stated: false,
        ..perishable(self_draft("Sam", "has", "a car", "got the car back"), None)
    }]);
    let kept = sim.reply("dinner", "jo", "Q, remember that", &said.id);
    assert_eq!(
        messages(&kept.effects),
        vec![("Noted: Sam has a car.", Some(&kept.id))]
    );
    assert_eq!(reactions(&kept.effects), vec![(&kept.id, EYES)]);
    assert_eq!(unreactions(&kept.effects), vec![(&kept.id, EYES)]);
    let request = extract_requests(&kept.effects)[0];
    assert_eq!(
        (request.speaker.as_str(), request.text.as_str()),
        ("Sam", "got the car back today")
    );
    let car = fact(&sim, "Sam has a car");
    assert_eq!(car.expires, None, "durable");
    assert_eq!(car.confidence, 0.9, "stated");
    assert_eq!(car.scope, FactScope::SelfStated("sam".into()));
    assert_eq!(car.sources[0].message, said.id);

    // With the fact after it, those words are read.
    sim.extracts(vec![draft("The door code", "is", "4471")]);
    let code = sim.say("dinner", "priya", "Q, remember that the door code is 4471");
    assert_eq!(
        extract_requests(&code.effects)[0].text,
        "the door code is 4471"
    );
    assert_eq!(texts(&code.effects), vec!["Noted: The door code is 4471."]);
    let door = fact(&sim, "The door code is 4471");
    assert_eq!(door.scope, FactScope::Group);
    assert_eq!(door.sources[0].speaker, PlatformUserId::new("priya"));

    // In a DM it stays private.
    sim.extracts(vec![self_draft(
        "Alex",
        "is training for",
        "a marathon",
        "training for a marathon",
    )]);
    let private = sim.dm("alex", "remember that I'm training for a marathon");
    assert_eq!(
        texts(&private.effects),
        vec!["Noted: Alex is training for a marathon."]
    );
    assert_eq!(
        fact(&sim, "Alex is training for a marathon").scope,
        FactScope::Private("alex".into())
    );

    // Nothing to find: one line saying so.
    let nothing = sim.say("dinner", "jo", "Q, remember that lol");
    assert_eq!(texts(&nothing.effects), vec![NOTHING_TO_KEEP]);

    // A guest may only ask for their own words to be kept.
    let mut work = work();
    let marcus = work.say("launch", "marcus", "the deadline moved to friday");
    let guest = work.reply("launch", "dana", "Q, remember that", &marcus.id);
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert!(extract_requests(&guest.effects).is_empty());
}

#[test]
fn just_for_this_week_makes_a_fact_expire_in_seven_days() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "dinner",
        "alex",
        "I'm off booze",
        vec![self_draft("Alex", "is", "off booze", "I'm off booze")],
    );
    assert_eq!(fact(&sim, "Alex is off booze").expires, None);
    let week = sim.reply("dinner", "alex", "Q, just for this week", &said.id);
    assert_eq!(
        texts(&week.effects),
        vec!["Keeping that until Monday 2026-11-09."]
    );
    assert_eq!(
        fact(&sim, "Alex is off booze").expires,
        Some(sim.now.plus_seconds(7 * DAY))
    );
    assert_eq!(
        lines(&sim, &room("dinner"), "booze"),
        vec!["Alex is off booze"]
    );
    sim.advance(7 * DAY);
    assert!(lines(&sim, &room("dinner"), "booze").is_empty());

    // Someone else's fact in a team waits for a lead.
    let mut work = work();
    let run = hear(
        &mut work,
        "launch",
        "marcus",
        "the print run is 500",
        vec![draft("The print run", "is", "500 copies")],
    );
    let asked = work.reply("launch", "sam", "Q, just for this week", &run.id);
    assert_eq!(texts(&asked.effects), vec![DEFERRAL_REPLY]);
    let deferral = work.state.deferrals.values().next().unwrap();
    assert!(matches!(
        deferral.change,
        DeferredChange::Memory {
            change: MemoryChange::Expire { .. }
        }
    ));
}

#[test]
fn a_deleted_message_takes_the_facts_that_came_only_from_it() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "dinner",
        "jo",
        "oops wrong chat",
        vec![draft("Jo", "is", "pregnant")],
    );
    let pregnant = id(&sim, "Jo is pregnant");
    let deleted = sim.feed(Input::Inbound(ours_types::InboundEvent::MessageDeleted {
        room: "dinner".into(),
        id: said.id.clone(),
    }));
    assert_eq!(
        notices(&deleted),
        vec!["Dropped 1 fact with a deleted message in #dinner."]
    );
    assert_scrubbed(by_id(&sim, &pregnant));
    let gone = by_id(&sim, &pregnant).retracted.as_ref().unwrap();
    assert_eq!(gone.reason, "message deleted");
    assert!(lines(&sim, &room("dinner"), "jo").is_empty());
    assert!(!any_send(&deleted));
}

#[test]
fn an_answer_landing_for_nothing_pending_changes_nothing() {
    let mut sim = friends();
    let before = sim.state.clone();
    let fx = sim.feed(Input::Extracted {
        room: "dinner".into(),
        message: "m404".into(),
        extraction: ours_core::sim::facts(vec![draft("Sam", "drives", "")]),
    });
    assert!(fx.is_empty());
    assert_eq!(sim.state, before);
}

#[test]
fn a_forgotten_fact_keeps_no_trace_of_its_words_and_replays_the_same() {
    let mut sim = friends();
    hear(
        &mut sim,
        "dinner",
        "jo",
        "priya's seeing a cardiologist",
        vec![FactDraft {
            quote: Some("seeing a cardiologist".into()),
            ..draft("Priya", "is seeing", "a cardiologist")
        }],
    );
    let heart = id(&sim, "Priya is seeing a cardiologist");
    hear(
        &mut sim,
        "dinner",
        "jo",
        "the bar is Hobson's",
        vec![draft("The bar", "is", "Hobson's")],
    );
    let bar = id(&sim, "The bar is Hobson's");
    sim.advance(60);
    hear(
        &mut sim,
        "dinner",
        "jo",
        "no, the bar is the Crown",
        vec![draft("The bar", "is", "the Crown")],
    );
    sim.say("dinner", "priya", "Q, forget me");

    let json = serde_json::to_string(&sim.state.facts[&heart]).unwrap();
    // The words and who said them are gone; the retraction still names
    // who asked.
    for word in ["cardiologist", "seeing", "Priya", "\"jo\""] {
        assert!(!json.contains(word), "{word} in {json}");
    }
    let retraction = sim.state.facts[&heart].retracted.as_ref().unwrap();
    assert_eq!(retraction.reason, "forget me");
    assert_eq!(retraction.by, Some("priya".into()));
    // A superseded fact keeps its words: it is history, not a removal.
    let old = &sim.state.facts[&bar];
    assert_eq!(old.retracted.as_ref().unwrap().reason, "superseded");
    assert_eq!(old.line(), "The bar is Hobson's");

    let mut state = GroupState::new("group".into(), Kind::Friends);
    for (at, input) in sim.log.clone() {
        step(&mut state, input, at);
    }
    assert_eq!(state, sim.state);
    assert_eq!(state.facts[&heart], sim.state.facts[&heart]);
}

#[test]
fn forget_me_scrubs_the_words_from_why_traces_and_records() {
    let mut sim = friends();
    sim.model_says("Hobson's.");
    let asked = sim.say(
        "dinner",
        "sam",
        "Q, what was that bar with the weird cocktails?",
    );
    let reply = sim.last_sent_message().unwrap();
    let before = sim.state.rooms[&"dinner".into()]
        .traces
        .iter()
        .find(|(id, _)| *id == reply)
        .map(|(_, t)| t.trigger.clone())
        .unwrap();
    assert!(before.contains("weird cocktails"), "{before}");
    sim.say("dinner", "sam", "Q, forget me");
    let json = serde_json::to_string(&sim.state).unwrap();
    assert!(
        !json.contains("weird cocktails"),
        "the words survived a forget"
    );
    assert!(json.contains(ours_core::FORGOTTEN_TRIGGER));
    let _ = asked;
}

#[test]
fn a_members_correction_to_someone_elses_fact_waits_for_a_lead() {
    let mut sim = work();
    let signs = hear(
        &mut sim,
        "launch",
        "marcus",
        "priya signs off the print run",
        vec![draft("Priya", "signs off", "the print run")],
    );
    let signs_id = id(&sim, "Priya signs off the print run");
    sim.script(ModelScript::Reply(citing(
        "Priya signs it off.",
        &[signs.id.as_str()],
    )));
    sim.say("launch", "sam", "Q, who signs off the print run?");
    let answer = sim.last_sent_message().unwrap();

    // A guest is declined and nothing changes.
    let guest = sim.reply("launch", "dana", "that's wrong", &answer);
    assert_eq!(texts(&guest.effects), vec!["That's one for Priya."]);
    assert!(redactions(&guest.effects).is_empty());
    assert!(by_id(&sim, &signs_id).retracted.is_none());

    // A member's correction waits, with the words that came after it.
    let wrong = sim.reply("launch", "sam", "that's wrong, Jo signs off now", &answer);
    assert_eq!(texts(&wrong.effects), vec![DEFERRAL_REPLY]);
    assert!(redactions(&wrong.effects).is_empty());
    assert!(extract_requests(&wrong.effects).is_empty());
    assert!(by_id(&sim, &signs_id).retracted.is_none());
    let held = &sim.state.rooms[&"launch".into()].messages;
    assert!(held.iter().any(|m| m.id == signs.id), "the source stays");
    let (ask, deferral) = sim.state.deferrals.iter().next().unwrap();
    assert!(matches!(
        &deferral.change,
        DeferredChange::Memory {
            change: MemoryChange::Correct { rest, .. }
        } if rest == "Jo signs off now"
    ));

    // A lead's ✅ drops it as Sam's correction and reads what is right.
    let ask = ask.clone();
    sim.extracts(vec![draft("Jo", "signs off", "the print run")]);
    let approved = sim.react("launch", &ask, "priya", CHECK);
    assert_scrubbed(by_id(&sim, &signs_id));
    let old = by_id(&sim, &signs_id).retracted.as_ref().unwrap();
    assert_eq!(
        (old.reason.as_str(), old.by.clone()),
        ("corrected", Some("sam".into()))
    );
    assert_eq!(
        redactions(&approved),
        vec![(std::slice::from_ref(&signs.id), None)]
    );
    let request = extract_requests(&approved)[0];
    assert_eq!(
        (request.speaker.as_str(), request.text.as_str()),
        ("Sam", "Jo signs off now")
    );
    assert_eq!(
        messages(&approved),
        vec![("Corrected: Jo signs off the print run.", Some(&wrong.id))]
    );
    assert!(
        notices(&approved).contains(&"Memory: Asked by Sam, approved by Priya."),
        "{:?}",
        notices(&approved)
    );
    let jo = fact(&sim, "Jo signs off the print run");
    assert_eq!(jo.sources[0].speaker, PlatformUserId::new("sam"));
}

#[test]
fn a_member_remembering_someone_elses_words_waits_for_a_lead() {
    let mut sim = work();
    let marcus = sim.say("launch", "marcus", "the deadline moved to friday");
    let asked = sim.reply("launch", "sam", "Q, remember that", &marcus.id);
    assert_eq!(texts(&asked.effects), vec![DEFERRAL_REPLY]);
    assert!(extract_requests(&asked.effects).is_empty());
    let ask = sim.state.deferrals.keys().next().unwrap().clone();

    sim.extracts(vec![draft("The deadline", "moved to", "friday")]);
    let approved = sim.react("launch", &ask, "priya", CHECK);
    let request = extract_requests(&approved)[0];
    assert_eq!(
        (request.speaker.as_str(), request.text.as_str()),
        ("Marcus", "the deadline moved to friday")
    );
    assert_eq!(
        messages(&approved),
        vec![("Noted: The deadline moved to friday.", Some(&asked.id))]
    );
    let deadline = fact(&sim, "The deadline moved to friday");
    assert_eq!((deadline.expires, deadline.confidence), (None, 0.9));
    assert_eq!(deadline.sources[0].speaker, PlatformUserId::new("marcus"));

    // Their own words need nobody.
    let own = sim.say("launch", "sam", "the venue holds sixty");
    sim.extracts(vec![draft("The venue", "holds", "sixty")]);
    let kept = sim.reply("launch", "sam", "Q, remember that", &own.id);
    assert_eq!(texts(&kept.effects), vec!["Noted: The venue holds sixty."]);

    // On the agent's answer, keeping someone else's fact waits too.
    let run = hear(
        &mut sim,
        "launch",
        "marcus",
        "the print run is 500 for now",
        vec![perishable(draft("The print run", "is", "500"), None)],
    );
    sim.script(ModelScript::Reply(citing("500.", &[run.id.as_str()])));
    sim.say("launch", "sam", "Q, how big is the print run?");
    let answer = sim.last_sent_message().unwrap();
    let keep = sim.reply("launch", "sam", "remember that", &answer);
    assert_eq!(texts(&keep.effects), vec![DEFERRAL_REPLY]);
    assert!(fact(&sim, "The print run is 500").expires.is_some());
    let ask = sim
        .state
        .deferrals
        .iter()
        .find(|(_, d)| d.request == keep.id)
        .map(|(k, _)| k.clone())
        .unwrap();
    let approved = sim.react("launch", &ask, "priya", CHECK);
    assert_eq!(
        texts(&approved),
        vec!["Keeping that for good. Asked by Sam, approved by Priya."]
    );
    assert_eq!(fact(&sim, "The print run is 500").expires, None);

    // Undo of it says why it stays, not that something was forgotten.
    let undo = sim.say("launch", "priya", "Q, undo");
    assert_eq!(
        texts(&undo.effects),
        vec!["That one can't be undone: say forget that or just for this week to change it."]
    );
}

#[test]
fn a_lone_thats_wrong_means_only_a_recent_or_the_latest_answer() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "dinner",
        "jo",
        "sam drives",
        vec![draft("Sam", "drives", "")],
    );
    let drives = id(&sim, "Sam drives");
    sim.script(ModelScript::Reply(citing(
        "Sam drives.",
        &[said.id.as_str()],
    )));
    sim.say("dinner", "alex", "Q, does Sam drive?");

    // Old, and the room has moved on: it asks which.
    sim.advance(30 * 60);
    sim.say("dinner", "priya", "anyway, pizza?");
    let old = sim.say("dinner", "alex", "Q, that's wrong");
    assert_eq!(texts(&old.effects), vec![WHICH_ONE]);
    assert!(redactions(&old.effects).is_empty());
    assert!(by_id(&sim, &drives).retracted.is_none());

    // Old but still the latest in the room, it is what was meant.
    sim.script(ModelScript::Reply(citing(
        "Sam drives.",
        &[said.id.as_str()],
    )));
    sim.say("dinner", "alex", "Q, so who drives?");
    sim.advance(30 * 60);
    let latest = sim.say("dinner", "alex", "Q, that's wrong");
    assert_eq!(texts(&latest.effects), vec!["Dropped that."]);
    assert!(by_id(&sim, &drives).retracted.is_some());
}

#[test]
fn undo_of_just_for_this_week_says_nothing_was_forgotten() {
    let mut sim = friends();
    let said = hear(
        &mut sim,
        "dinner",
        "alex",
        "I'm off booze",
        vec![self_draft("Alex", "is", "off booze", "I'm off booze")],
    );
    sim.reply("dinner", "alex", "Q, just for this week", &said.id);
    let undo = sim.say("dinner", "alex", "Q, undo");
    assert_eq!(
        texts(&undo.effects),
        vec!["That one can't be undone: say remember that to keep it for good."]
    );
}
