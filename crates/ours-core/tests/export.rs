//! The host's export (DESIGN.md sections 9 and 15): everything a group
//! holds, as JSON that reads back and Markdown a person can read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::sim::{
    ClassifierScript, ExtractorScript, ModelScript, Posted, Sim, draft, instant, paid, promise,
    self_draft, texts,
};
use ours_core::step::CHECK;
use ours_core::{Command, Export, ExportData, FactScope, Input, render_charter, render_export};
use ours_types::{FactDraft, Kind, MessageKind, PlatformRole, Role, Score, ScoreSource};

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

/// A team with one of everything: a host, a named lead, a member and a
/// guest; facts in every scope and one forgotten; a decision and an open
/// poll; a commitment, a tab line, mail, a waiting request, a starter
/// voice and a legal hold.
fn everything() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.feed(Input::Command(Command::SetHosts {
        users: vec!["olu".into()],
    }));
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("olu", "Olu", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
            ("sam", "Sam", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    sim.feed(Input::Command(Command::AddLead { user: "jo".into() }));
    sim.feed(Input::Command(Command::SetMemberZone {
        user: "sam".into(),
        zone: Some("Europe/Lisbon".into()),
    }));
    sim.advance_to(instant("2026-11-03T10:00:00Z"));
    sim.say("launch", "priya", "Q, be the host");

    hear(
        &mut sim,
        "launch",
        "jo",
        "the printer is Inkwell",
        vec![draft("The printer", "is", "Inkwell")],
    );
    hear(
        &mut sim,
        "launch",
        "sam",
        "I'm in Lisbon all next week",
        vec![FactDraft {
            durable: false,
            expires: Some("2026-11-15".into()),
            ..self_draft("Sam", "is in", "Lisbon", "in Lisbon all next week")
        }],
    );
    let bad = hear(
        &mut sim,
        "launch",
        "jo",
        "dana's budget is 40k",
        vec![draft("Dana", "has a budget of", "40k")],
    );
    sim.reply("launch", "priya", "Q, forget that", &bad.id);
    sim.extracts(vec![self_draft(
        "Sam",
        "is",
        "vegetarian",
        "I'm vegetarian",
    )]);
    sim.dm("sam", "Q, remember that I'm vegetarian");

    sim.say("launch", "priya", "Q, poll: Acme or Beacon");
    sim.say(
        "launch",
        "priya",
        "Q, go with Acme because the date matters",
    );
    sim.say("launch", "jo", "Q, poll: blue or green for the cover");

    sim.promises(vec![promise(
        "Sam",
        "the draft to legal",
        Some("2026-11-06"),
    )]);
    sim.say("launch", "sam", "I'll send the draft to legal by Friday");
    sim.pays(vec![paid("Priya", 120.0, "the venue deposit")]);
    sim.say("launch", "priya", "I paid the venue deposit, 120");

    sim.script(ModelScript::Reply(ours_core::sim::mail_plan(
        "bookings@baitong.com",
        "Table for five, Thursday 7:30",
        "Hello, a table for five at 7:30 on Thursday? Q for the group",
    )));
    sim.say(
        "launch",
        "priya",
        "Q, email bookings@baitong.com for a table for five on Thursday at 7:30",
    );
    sim.say("launch", "sam", "Q, quiet after 9");
    sim.say("launch", "priya", "Q, legal hold on");
    sim
}

#[test]
fn one_of_everything_exports_as_json_that_reads_back() {
    let sim = everything();
    let state = &sim.state;
    assert_eq!(state.persona_name.as_deref(), Some("host"));
    assert!(state.legal_hold);
    assert_eq!(state.decisions.len(), 1);
    assert!(state.proposals.values().any(|p| p.is_open()));
    assert_eq!(state.commitments.len(), 1);
    assert_eq!(state.ledger.len(), 1);
    assert_eq!(state.deferrals.len(), 1, "{:#?}", state.deferrals);
    assert!(!state.holds.is_empty());

    let Export { json, markdown } = render_export(state, sim.now);
    let data: ExportData = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&data).unwrap(), json);
    assert_eq!(data, ours_core::export::export_data(state, sim.now));

    assert_eq!(data.format, 1);
    assert_eq!(data.group.as_str(), "group");
    assert_eq!(data.kind, Kind::Team);
    assert_eq!(data.persona_name.as_deref(), Some("host"));
    assert_eq!(data.persona, state.persona);
    assert_eq!(data.profile, state.profile);
    assert_eq!(data.retention_days, 365);
    assert!(data.legal_hold && data.legal_hold_since.is_some());
    assert_eq!(data.charter, render_charter(state));
    assert_eq!(data.hosts, vec!["olu".into()]);
    assert_eq!(data.leads, vec!["jo".into()]);

    let member = |id: &str| data.members.iter().find(|m| m.id.as_str() == id).unwrap();
    assert_eq!(member("priya").role, Role::Lead);
    assert_eq!(member("olu").role, Role::Host);
    assert!(member("olu").host);
    assert!(member("jo").named_lead);
    assert_eq!(member("sam").role, Role::Member);
    assert_eq!(member("sam").zone.as_deref(), Some("Europe/Lisbon"));
    assert_eq!(member("dana").role, Role::Guest);
    assert_eq!(member("sam").rooms, vec!["launch".into()]);
    assert!(data.members.iter().all(|m| m.id.as_str() != "ours-bot"));

    // Every scope, labelled, and the forgotten one without its words.
    let scopes: Vec<(&str, &str)> = data
        .facts
        .iter()
        .map(|f| (f.scope.as_str(), f.status.as_str()))
        .collect();
    assert!(
        scopes.contains(&("said in the group", "lasting")),
        "{scopes:?}"
    );
    assert!(scopes.contains(&("said by Sam about themselves", "until 2026-11-15")));
    assert!(scopes.contains(&("private to Sam", "lasting")));
    assert!(scopes.contains(&("said in the group", "forgotten 2026-11-03 (forget that)")));
    assert!(
        data.facts
            .iter()
            .any(|f| matches!(f.fact.scope, FactScope::Private(_)))
    );
    assert!(!json.to_string().contains("40k"));

    assert_eq!(data.decisions.len(), 1);
    assert_eq!(
        data.decisions[0].reason.as_deref(),
        Some("the date matters")
    );
    assert_eq!(data.commitments.len(), 1);
    assert_eq!(data.ledger.len(), 1);
    assert_eq!(data.balances.values().sum::<i64>(), 0);
    assert!(data.actions.len() >= 5);
    assert_eq!(data.deferrals.len(), 1);
    for key in [
        "members",
        "facts",
        "decisions",
        "commitments",
        "ledger",
        "balances",
        "actions",
        "deferrals",
        "charter",
        "legal_hold",
        "profile",
        "persona",
    ] {
        assert!(json.get(key).is_some(), "{key}");
    }

    // The Markdown says the same for a person.
    for wanted in [
        "# Export of group group",
        "## The group",
        "- Kind: team",
        "- Voice: host (dial 0, concierge)",
        "- Retention: action log kept 365 days; perishable facts dropped once they expire",
        "- Legal hold: on since 2026-11-03; nothing expires",
        "## Rooms",
        "- #launch (`launch`)",
        "## People",
        "- Olu (`olu`): host, in #launch; zone the group's",
        "- Sam (`sam`): member, in #launch; zone Europe/Lisbon",
        "- Dana at Acme (`dana`): guest, in #launch",
        "## Leads and hosts",
        "- Named leads: Jo (`jo`)",
        "### Said in the group",
        "- The printer is Inkwell. Said in the group; lasting. From Jo in #launch, 2026-11-03",
        "### Said by members about themselves",
        "- Sam: \"in Lisbon all next week\". Said by Sam about themselves; until 2026-11-15.",
        "### Private, said in a DM",
        "Private to Sam; lasting.",
        "### Removed",
        "forgotten 2026-11-03 (forget that), first seen 2026-11-03; its words are gone",
        "## Decisions",
        "### 2026-11-03: Acme or Beacon?",
        "## Commitments",
        "- Sam: the draft to legal, due 2026-11-06",
        "## The tab",
        "- Priya 120 (the venue deposit)",
        "- Balances in USD: Jo -40, Priya +80, Sam -40",
        "## Action log",
        "the email to bookings@baitong.com",
        "## Waiting for a lead",
        "- Sam asked in #launch: \"Q, quiet after 9\"",
        "## Charter",
        "### The group",
    ] {
        assert!(
            markdown.contains(wanted),
            "missing {wanted:?} in\n{markdown}"
        );
    }
    assert!(!markdown.contains("40k"));
    assert!(!markdown.contains('\u{2014}'));
    assert!(markdown.ends_with('\n') && !markdown.ends_with("\n\n"));
}

#[test]
fn an_empty_group_exports_with_every_section_saying_none() {
    let sim = Sim::new(Kind::Friends);
    let export = render_export(&sim.state, sim.now);
    let data: ExportData = serde_json::from_value(export.json.clone()).unwrap();
    assert!(data.facts.is_empty() && data.members.is_empty());
    assert_eq!(data.retention_days, 90);
    assert!(export.markdown.contains("## People\n\n- none\n"));
    assert!(export.markdown.contains("## Action log\n\n- none\n"));
    assert!(
        export
            .markdown
            .contains("- Voice: plain default (dial 1, dry and brief)")
    );
}

#[test]
fn a_lead_approving_a_forget_scrubs_the_export_too() {
    let mut sim = everything();
    let printer = sim
        .state
        .facts
        .values()
        .find(|f| f.line() == "The printer is Inkwell")
        .unwrap()
        .sources[0]
        .message
        .clone();
    sim.extract(ExtractorScript::Silent);
    let asked = sim.reply("launch", "sam", "Q, forget that", &printer);
    assert_eq!(
        texts(&asked.effects),
        vec![ours_core::leads::DEFERRAL_REPLY]
    );
    let ask = sim
        .state
        .deferrals
        .iter()
        .find(|(_, d)| d.request == asked.id)
        .map(|(id, _)| id.clone())
        .unwrap();
    sim.react("launch", &ask, "priya", CHECK);
    let export = render_export(&sim.state, sim.now);
    assert!(!export.markdown.contains("Inkwell"));
    assert!(!export.json.to_string().contains("Inkwell"));
}
