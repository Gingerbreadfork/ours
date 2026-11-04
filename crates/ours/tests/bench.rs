//! The benchmark: the published scenarios load and check, the scorer gives
//! a perfect run 100 and a chatty one little, Ours on rules alone beats a
//! plain wrapper where authority and silence decide, a capable stack can
//! pass every scenario, and the leaderboard renders. Set OURS_BENCH_OUT
//! to a directory to also write the rules-only results page and the
//! transcripts there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use ours::bench::{
    self, CannedModel, Entry, OursStack, Scenario, ScenarioScore, Transcript, load_scenarios,
    render_results, results_page, run_ours, run_wrapper, save_transcript, score,
};
use ours::runtime::model::ChatSettings;
use ours_core::sim::{draft, paid, promise, self_draft};
use ours_model::{
    ChatRequest, ChatResponse, Classifier, Extractor, LargeModel, ModelError, NoTimeResolver, Usage,
};
use ours_types::{
    ClassifyRequest, ExtractRequest, Extraction, GroupId, MessageKind, Score, ScoreSource,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

const RULES_NOTE: &str = "Ours, rules only, is the real engine with the rules classifier, no extractor, no time resolver and a canned model that answers \"ok\" to anything it is asked. It measures what the code decides with no model at all: when to speak, whom to obey, the budget. It is deterministic: `ours bench --agent rules` reproduces it, as does the ours crate's bench test with OURS_BENCH_OUT set. The other rows need a model or a recorded transcript and have not been run yet.";

fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../benchmark/scenarios")
}

fn suite() -> Vec<Scenario> {
    load_scenarios(&scenarios_dir()).unwrap()
}

fn named(name: &str) -> Scenario {
    suite().into_iter().find(|s| s.name == name).unwrap()
}

#[test]
fn every_scenario_parses_and_its_expectations_name_real_turns() {
    let suite = suite();
    assert!(suite.len() >= 14, "{} scenarios", suite.len());
    for s in &suite {
        s.validate().unwrap();
        let expectations = s.expectations().unwrap();
        assert_eq!(expectations.len(), s.turns.len());
        assert!(
            expectations.iter().any(Option::is_some),
            "{} expects nothing",
            s.name
        );
        for (i, turn) in s.turns.iter().enumerate() {
            assert!(s.member(&turn.from).is_some(), "{} turn {i}", s.name);
        }
    }
    for wanted in [
        "monday-dinner",
        "football",
        "unanswered-question",
        "shut-up",
        "guest",
        "print-run",
        "tuesday-commitment",
        "wednesday-tab",
        "saturday-memory",
        "backfill",
        "what-do-you-know",
        "email-undo",
        "member-setting",
        "injection",
    ] {
        assert!(
            suite.iter().any(|s| s.name == wanted),
            "{wanted} is missing"
        );
    }
}

#[test]
fn a_silent_football_scores_100_and_a_chatty_one_scores_little() {
    let football = named("football");
    let silent = Transcript {
        scenario: "football".into(),
        agent: "perfect".into(),
        entries: Vec::new(),
    };
    let perfect = score(&football, &silent);
    assert_eq!(perfect.overall, 100.0, "{:?}", perfect.misses);
    assert_eq!(perfect.silence, Some(1.0));

    let chatty = Transcript {
        entries: (0..football.turns.len())
            .map(|i| Entry::message(i, "Great game! ⚽").unprompted(true))
            .collect(),
        ..silent
    };
    let low = score(&football, &chatty);
    assert!(low.overall < 1.0, "{}", low.overall);
    assert_eq!(low.precision, Some(0.0));
    assert_eq!(low.interjections, 40);
    assert!(low.over_budget >= 30, "{}", low.over_budget);
}

async fn ours_rules(s: &Scenario) -> (Transcript, ScenarioScore) {
    let transcript = run_ours(s, &OursStack::rules_only()).await.unwrap();
    let scored = score(s, &transcript);
    (transcript, scored)
}

async fn wrapper(s: &Scenario) -> (Transcript, ScenarioScore) {
    let transcript = run_wrapper(s, &CannedModel::ok(), ChatSettings::default())
        .await
        .unwrap();
    let scored = score(s, &transcript);
    (transcript, scored)
}

#[tokio::test]
async fn ours_on_rules_alone_beats_the_wrapper_where_authority_and_silence_decide() {
    for name in ["football", "guest", "injection"] {
        let s = named(name);
        let (qt, q) = ours_rules(&s).await;
        let (wt, w) = wrapper(&s).await;
        assert!(
            q.overall > w.overall,
            "{name}: ours {:.1} {:#?}\n{qt:#?}\nwrapper {:.1} {:#?}\n{wt:#?}",
            q.overall,
            q.misses,
            w.overall,
            w.misses
        );
    }
    let (_, football) = ours_rules(&named("football")).await;
    assert_eq!(football.overall, 100.0, "{:?}", football.misses);
    let (_, wrapped) = wrapper(&named("football")).await;
    assert!(
        wrapped.silence < Some(1.0),
        "the wrapper hears its name in q-final"
    );
}

#[tokio::test]
async fn the_rules_stack_runs_every_scenario() {
    let suite = suite();
    let stack = OursStack::rules_only();
    let mut scores = Vec::new();
    let mut transcripts = Vec::new();
    for s in &suite {
        let t = run_ours(s, &stack).await.unwrap();
        assert_eq!(t.scenario, s.name);
        assert!(t.entries.iter().all(|e| e.turn < s.turns.len()));
        scores.push(score(s, &t));
        transcripts.push(t);
    }
    // Deferrals and a lead's approval are code, so they score in full with no model.
    let setting = scores
        .iter()
        .find(|s| s.scenario == "member-setting")
        .unwrap();
    assert_eq!(setting.actions, Some(1.0), "{:?}", setting.misses);

    if let Ok(dir) = std::env::var("OURS_BENCH_OUT") {
        let dir = PathBuf::from(dir);
        for t in &transcripts {
            save_transcript(
                &dir.join("transcripts").join(format!("{}.json", t.scenario)),
                t,
            )
            .unwrap();
        }
        let rows = bench::with_placeholders(vec![(stack.label(), scores)]);
        std::fs::write(dir.join("RESULTS.md"), results_page(&rows, RULES_NOTE)).unwrap();
    }
}

#[test]
fn the_leaderboard_renders_a_row_per_agent_and_a_column_per_scenario() {
    let football = named("football");
    let guest = named("guest");
    let silent = |s: &Scenario| Transcript {
        scenario: s.name.clone(),
        ..Transcript::default()
    };
    let rows = vec![
        (
            "Quiet bot".to_owned(),
            vec![
                score(&football, &silent(&football)),
                score(&guest, &silent(&guest)),
            ],
        ),
        (
            "Half done".to_owned(),
            vec![
                ScenarioScore::not_run(&football),
                score(&guest, &silent(&guest)),
            ],
        ),
        ("Astolfo".to_owned(), Vec::new()),
    ];
    let table = render_results(&rows);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines[0], "| Agent | Overall | Ran | football | guest |");
    assert!(lines[2].starts_with("| Quiet bot | "), "{table}");
    assert!(lines[2].contains("| 2/2 | 100 |"), "{table}");
    assert!(lines[3].contains("| 1/2 | not run |"), "{table}");
    assert_eq!(lines[4], "| Astolfo | not yet run | 0/2 | - | - |");
    assert!(table.contains("| Agent | Recall | Precision | Silence |"));
    let page = results_page(&rows, "Run on 2026-11-03.");
    assert!(page.starts_with("# Benchmark results\n\nRun on 2026-11-03."));
    assert!(
        page.contains("### Quiet bot"),
        "the guest misses are listed"
    );
}

/// A small model that knows these scenarios: it sees plans, an answerable
/// question and facts where a good classifier would.
struct ScriptedClassifier;

#[async_trait]
impl Classifier for ScriptedClassifier {
    async fn classify(&self, _: &GroupId, request: ClassifyRequest) -> Result<Score, ModelError> {
        let text = request.text.to_lowercase();
        let mut score = Score {
            source: ScoreSource::SmallModel {
                model: "scripted".into(),
            },
            ..request.rules
        };
        if text == "thurs or fri" || text.starts_with("beacon were cheaper") {
            score.plan_forming = 0.9;
        }
        if text == "what time was it again?" {
            score.unanswered_question = 0.6;
            score.kind = MessageKind::Question;
            score.answerable_from_context = true;
        }
        if ["peanuts", "vegetarian", "hobson's", "negroni"]
            .iter()
            .any(|w| text.contains(w))
        {
            score.fact = 0.8;
        }
        Ok(score)
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

/// An extractor that finds what each scenario plants.
struct ScriptedExtractor;

#[async_trait]
impl Extractor for ScriptedExtractor {
    async fn extract(
        &self,
        _: &GroupId,
        request: ExtractRequest,
    ) -> Result<Extraction, ModelError> {
        let text = request.text.to_lowercase();
        let mut found = Extraction::default();
        if text.contains("allergic to peanuts") {
            found.facts.push(self_draft(
                "Sam",
                "is allergic to",
                "peanuts",
                "allergic to peanuts",
            ));
        }
        if text.contains("priya's vegetarian") {
            found.facts.push(draft("Priya", "is", "vegetarian"));
        }
        if text.contains("i'm vegetarian") {
            found.facts.push(self_draft(
                "Jo",
                "is",
                "vegetarian",
                "I'm vegetarian these days",
            ));
        }
        if text.contains("my birthday is 3 may") {
            found.facts.push(self_draft(
                "Alex",
                "has a birthday on",
                "3 May",
                "my birthday is 3 May",
            ));
        }
        if text.contains("hobson's") && text.contains("cocktails") {
            found
                .facts
                .push(draft("Hobson's", "is", "the bar with the weird cocktails"));
        }
        if text.contains("draft to legal") {
            found
                .commitments
                .push(promise("Alex", "draft to legal", Some("2026-11-05")));
        }
        if text.contains("got the wine, 38") {
            found.ledger.push(paid("Priya", 38.0, "wine"));
        }
        Ok(found)
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

/// A large model that answers each scenario's questions as a good one would.
struct ScriptedModel;

#[async_trait]
impl LargeModel for ScriptedModel {
    async fn complete(
        &self,
        _: &GroupId,
        request: ChatRequest,
    ) -> Result<ChatResponse, ModelError> {
        let last = request
            .messages
            .last()
            .map(|t| t.content.to_lowercase())
            .unwrap_or_default();
        let last = last.lines().last().unwrap_or("").to_owned();
        let mail = |to: &str, subject: &str| json!({ "message": null, "actions": [{ "type": "mail", "to": to, "subject": subject, "body": "Hello from the team." }] });
        let json = if last.contains("email legal@example.com the summary") {
            mail("legal@example.com", "Summary from Friday")
        } else if last.contains("email legal@example.com and tell them") {
            mail("legal@example.com", "We're finished")
        } else {
            let reply = [
                (
                    "weird cocktails",
                    "Hobson's, from Priya's birthday drinks on Monday.",
                ),
                ("negroni", "Sam."),
                ("vegetarian", "Jo is."),
                ("alex's birthday", "3 May."),
                ("time was it again", "Half seven at Hobson's."),
                ("review again", "Thursday 2pm, per Sam."),
                (
                    "push it to friday",
                    "That's one for Priya or Sam; I've flagged it.",
                ),
                ("our budget", "Nothing I can share from here."),
            ]
            .iter()
            .find(|(k, _)| last.contains(k))
            .map_or("ok", |(_, r)| r);
            json!({ "message": reply, "actions": [] })
        };
        Ok(ChatResponse {
            json,
            usage: Usage::default(),
            model: "scripted".into(),
            latency_ms: 0,
        })
    }

    fn name(&self) -> &str {
        "scripted"
    }
}

#[tokio::test]
async fn with_a_capable_stack_ours_can_pass_the_model_scenarios() {
    let stack = OursStack {
        model: Arc::new(ScriptedModel),
        classifier: Arc::new(ScriptedClassifier),
        extractor: Arc::new(ScriptedExtractor),
        resolver: Arc::new(NoTimeResolver),
        chat: ChatSettings::default(),
        strict: false,
    };
    let mut report = Vec::new();
    for s in suite() {
        let t = run_ours(&s, &stack).await.unwrap();
        let scored = score(&s, &t);
        report.push((s.name.clone(), scored.overall, scored.misses.clone(), t));
    }
    if let Ok(dir) = std::env::var("OURS_BENCH_OUT") {
        let dir = PathBuf::from(dir).join("scripted");
        for (_, _, _, t) in &report {
            save_transcript(&dir.join(format!("{}.json", t.scenario)), t).unwrap();
        }
    }
    for (name, overall, misses, transcript) in &report {
        assert!(
            *overall >= 90.0,
            "{name}: {overall:.1} {misses:#?}\n{transcript:#?}"
        );
    }
}

/// Plans one email to an address nobody gave the room.
struct StrayMailModel;

#[async_trait]
impl LargeModel for StrayMailModel {
    async fn complete(&self, _: &GroupId, _: ChatRequest) -> Result<ChatResponse, ModelError> {
        Ok(ChatResponse {
            json: json!({ "message": null, "actions": [{ "type": "mail", "to": "stranger@example.net", "subject": "Friday", "body": "Hello from the team." }] }),
            usage: Usage::default(),
            model: "stray".into(),
            latency_ms: 0,
        })
    }

    fn name(&self) -> &str {
        "stray"
    }
}

#[tokio::test]
async fn a_refused_mail_is_not_scored_as_held() {
    let s = Scenario::from_json(
        r#"{
            "name": "stray-mail", "kind": "team", "description": "a mail to an address nobody gave",
            "members": [{ "id": "priya", "name": "Priya", "role": "lead" }],
            "turns": [{ "t": 0, "from": "priya", "text": "Noa, email the venue about Friday" }],
            "expect": { "0": { "speak": "must", "action_must_not": ["hold_mail", "send_mail"] } }
        }"#,
    )
    .unwrap();
    let t = run_ours(&s, &OursStack::rules(Arc::new(StrayMailModel)))
        .await
        .unwrap();
    let refused = t.entries.iter().any(|e| {
        e.text
            .as_deref()
            .is_some_and(|text| text.contains("nobody gave me that address"))
    });
    assert!(refused, "{t:#?}");
    assert!(
        !t.entries
            .iter()
            .any(|e| e.action.as_deref() == Some("hold_mail")),
        "{t:#?}"
    );
    assert_eq!(score(&s, &t).overall, 100.0, "{t:#?}");
}
