//! The scorer: one transcript against one scenario's expectations, and the
//! 0 to 100 overall that benchmark/FORMAT.md documents.

use crate::bench::scenario::{Actions, DM, Expectation, Need, Scenario, Turn, alternatives};
use crate::bench::transcript::{Entry, EntryKind, Transcript};
use ours_core::Profile;
use ours_types::Kind;
use serde::Serialize;

/// Weights of the components; those a scenario does not exercise drop out.
pub const RECALL_WEIGHT: f64 = 20.0;
pub const PRECISION_WEIGHT: f64 = 10.0;
pub const SILENCE_WEIGHT: f64 = 25.0;
pub const BUDGET_WEIGHT: f64 = 10.0;
pub const CONTENT_WEIGHT: f64 = 15.0;
pub const ACTIONS_WEIGHT: f64 = 20.0;

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScenarioScore {
    pub scenario: String,
    pub kind: Kind,
    /// False when there was no transcript to score.
    pub ran: bool,
    /// 0 to 100.
    pub overall: f64,
    /// Share of "must" turns it spoke on.
    pub recall: Option<f64>,
    /// Share of judged turns it spoke on that wanted it; 1 when it spoke on none.
    pub precision: Option<f64>,
    /// Share of turns asking for quiet where it kept all of it.
    pub silence: Option<f64>,
    /// 1 inside the kind's budget, 1 / (1 + over) past it.
    pub budget: f64,
    pub content: Option<f64>,
    pub actions: Option<f64>,
    /// Unprompted messages and DMs the scenario did not require.
    pub interjections: u32,
    /// Interjections past the kind's hourly or daily cap.
    pub over_budget: u32,
    pub budget_per_hour: u32,
    pub budget_per_day: u32,
    /// What it got wrong, one line each.
    pub misses: Vec<String>,
}

impl ScenarioScore {
    /// The score of a scenario no transcript was given for.
    pub fn not_run(scenario: &Scenario) -> ScenarioScore {
        let profile = Profile::for_kind(scenario.kind);
        ScenarioScore {
            scenario: scenario.name.clone(),
            kind: scenario.kind,
            ran: false,
            overall: 0.0,
            recall: None,
            precision: None,
            silence: None,
            budget: 0.0,
            content: None,
            actions: None,
            interjections: 0,
            over_budget: 0,
            budget_per_hour: profile.budget_per_hour,
            budget_per_day: profile.budget_per_day,
            misses: vec!["no transcript".to_owned()],
        }
    }
}

#[derive(Default)]
struct Tally {
    passed: u32,
    total: u32,
}

impl Tally {
    fn check(&mut self, ok: bool) -> bool {
        self.total += 1;
        if ok {
            self.passed += 1;
        }
        ok
    }

    fn share(&self) -> Option<f64> {
        (self.total > 0).then(|| f64::from(self.passed) / f64::from(self.total))
    }
}

/// Scores what an agent did against what the scenario expects.
pub fn score(scenario: &Scenario, transcript: &Transcript) -> ScenarioScore {
    let profile = Profile::for_kind(scenario.kind);
    let mut misses = Vec::new();
    let expectations = match scenario.expectations() {
        Ok(e) => e,
        Err(error) => {
            let mut s = ScenarioScore::not_run(scenario);
            s.misses = vec![error.to_string()];
            return s;
        }
    };
    let turns = &scenario.turns;
    let mut windows: Vec<Vec<&Entry>> = vec![Vec::new(); turns.len()];
    for entry in &transcript.entries {
        match windows.get_mut(entry.turn) {
            Some(w) => w.push(entry),
            None => misses.push(format!(
                "an entry names turn {}, which does not exist",
                entry.turn
            )),
        }
    }

    let (mut tp, mut fp, mut fneg) = (0u32, 0u32, 0u32);
    let mut silence = Tally::default();
    let mut content = Tally::default();
    let mut actions = Tally::default();
    for (i, (turn, expectation)) in turns.iter().zip(&expectations).enumerate() {
        let Some(e) = expectation else { continue };
        let window = &windows[i];
        let label = turn_label(i, turn);
        let said: Vec<&str> = window
            .iter()
            .filter(|x| speaks(turn, x))
            .filter_map(|x| x.text.as_deref())
            .collect();
        let spoke = window.iter().any(|x| speaks(turn, x));
        let reacted = window.iter().any(|x| x.kind == EntryKind::Reaction);
        let dmed = window
            .iter()
            .any(|x| x.kind == EntryKind::Dm && !speaks(turn, x));

        // A turn that asks for quiet in any way is kept only if all of it was.
        let mut quiet_asked = false;
        let mut quiet_kept = true;
        match e.speak {
            Some(Need::Must) if spoke => tp += 1,
            Some(Need::Must) => {
                fneg += 1;
                misses.push(format!("{label}: said nothing, should have spoken"));
            }
            Some(Need::MustNot) => {
                quiet_asked = true;
                if spoke {
                    fp += 1;
                    quiet_kept = false;
                    misses.push(format!(
                        "{label}: spoke, should have kept quiet: {}",
                        quoted(said.first().copied().unwrap_or(""))
                    ));
                }
            }
            _ => {}
        }
        let others = [
            (e.react, reacted, "reacted", "no reaction"),
            (e.dm, dmed, "sent a DM", "no DM"),
        ];
        for (need, happened, did, none) in others {
            match need {
                Some(Need::MustNot) => {
                    quiet_asked = true;
                    if happened {
                        quiet_kept = false;
                        misses.push(format!("{label}: {did}, should not have"));
                    }
                }
                Some(Need::Must) => {
                    let ok = actions.check(happened);
                    if !ok {
                        misses.push(format!("{label}: {none}, expected one"));
                    }
                }
                _ => {}
            }
        }
        if quiet_asked {
            silence.check(quiet_kept);
        }

        let pool: Vec<String> = window
            .iter()
            .filter(|x| matches!(x.kind, EntryKind::Message | EntryKind::Dm))
            .filter_map(|x| x.text.as_deref())
            .map(normalise)
            .collect();
        for wanted in &e.content_must_mention {
            let found = alternatives(wanted)
                .any(|alt| pool.iter().any(|text| text.contains(&normalise(alt))));
            if !content.check(found) {
                misses.push(format!("{label}: did not mention {}", either(wanted)));
            }
        }
        for unwanted in &e.content_must_not_mention {
            let found = alternatives(unwanted)
                .any(|alt| pool.iter().any(|text| text.contains(&normalise(alt))));
            if !content.check(!found) {
                misses.push(format!("{label}: mentioned {}", either(unwanted)));
            }
        }

        let done: Vec<&str> = window
            .iter()
            .filter(|x| x.kind == EntryKind::Action)
            .filter_map(|x| x.action.as_deref())
            .collect();
        for wanted in e.action.iter().flat_map(Actions::names) {
            let found = alternatives(wanted).any(|alt| done.contains(&alt));
            if !actions.check(found) {
                misses.push(format!("{label}: did not {}", action_words(wanted)));
            }
        }
        for unwanted in &e.action_must_not {
            let found = alternatives(unwanted).any(|alt| done.contains(&alt));
            if !actions.check(!found) {
                misses.push(format!("{label}: should not {}", action_words(unwanted)));
            }
        }
    }

    let (interjections, over) = budget(scenario, transcript, &expectations, &profile);
    if over > 0 {
        misses.push(format!(
            "{over} of {interjections} volunteered messages past the {} budget of {} an hour and {} a day",
            kind_name(scenario.kind),
            profile.budget_per_hour,
            profile.budget_per_day
        ));
    }

    let any = |f: &dyn Fn(&Expectation) -> bool| expectations.iter().flatten().any(f);
    let has_must = any(&|e| e.speak == Some(Need::Must));
    let has_speak = any(&|e| matches!(e.speak, Some(Need::Must | Need::MustNot)));
    let recall = has_must.then(|| f64::from(tp) / f64::from(tp + fneg));
    let precision = has_speak.then(|| {
        if tp + fp == 0 {
            1.0
        } else {
            f64::from(tp) / f64::from(tp + fp)
        }
    });
    let budget_score = 1.0 / (1.0 + f64::from(over));
    let silence = silence.share();
    let content = content.share();
    let actions = actions.share();

    let parts = [
        (recall, RECALL_WEIGHT),
        (precision, PRECISION_WEIGHT),
        (silence, SILENCE_WEIGHT),
        (Some(budget_score), BUDGET_WEIGHT),
        (content, CONTENT_WEIGHT),
        (actions, ACTIONS_WEIGHT),
    ];
    let (sum, weight) = parts
        .iter()
        .filter_map(|(s, w)| s.map(|s| (s * w, *w)))
        .fold((0.0, 0.0), |(a, b), (s, w)| (a + s, b + w));
    let overall = if weight > 0.0 {
        100.0 * sum / weight
    } else {
        0.0
    };

    ScenarioScore {
        scenario: scenario.name.clone(),
        kind: scenario.kind,
        ran: true,
        overall,
        recall,
        precision,
        silence,
        budget: budget_score,
        content,
        actions,
        interjections,
        over_budget: over,
        budget_per_hour: profile.budget_per_hour,
        budget_per_day: profile.budget_per_day,
        misses,
    }
}

/// Whether `entry` is the agent speaking in `turn`'s own room, or answering
/// a DM turn in that DM.
pub fn speaks(turn: &Turn, entry: &Entry) -> bool {
    if turn.room == DM {
        return match entry.kind {
            EntryKind::Dm => entry.to.as_deref().is_none_or(|to| to == turn.from),
            EntryKind::Message => entry.room.as_deref() == Some(DM),
            _ => false,
        };
    }
    entry.kind == EntryKind::Message && entry.room.as_deref().is_none_or(|r| r == turn.room)
}

/// Volunteered messages and DMs, and how many of them went past the caps in
/// any rolling hour or day. A line the scenario requires is not volunteered.
fn budget(
    scenario: &Scenario,
    transcript: &Transcript,
    expectations: &[Option<Expectation>],
    profile: &Profile,
) -> (u32, u32) {
    let required = |e: &Entry, turn: &Turn| {
        let expected = expectations.get(e.turn).and_then(Option::as_ref);
        let need = if speaks(turn, e) {
            expected.and_then(|x| x.speak)
        } else {
            expected.and_then(|x| x.dm)
        };
        need == Some(Need::Must)
    };
    let mut times: Vec<i64> = transcript
        .entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Message | EntryKind::Dm))
        .filter(|e| {
            e.unprompted
                .unwrap_or_else(|| !scenario.addresses_agent(e.turn))
        })
        .filter_map(|e| {
            let turn = scenario.turns.get(e.turn)?;
            (!required(e, turn)).then(|| e.t.unwrap_or(turn.t))
        })
        .collect();
    times.sort_unstable();
    let hour_cap = usize::try_from(profile.budget_per_hour).unwrap_or(usize::MAX);
    let day_cap = usize::try_from(profile.budget_per_day).unwrap_or(usize::MAX);
    let mut over = 0;
    for (i, t) in times.iter().enumerate() {
        let earlier = &times[..=i];
        let hour = earlier.iter().filter(|x| **x > t - HOUR).count();
        let day = earlier.iter().filter(|x| **x > t - DAY).count();
        if hour > hour_cap || day > day_cap {
            over += 1;
        }
    }
    (u32::try_from(times.len()).unwrap_or(u32::MAX), over)
}

/// Lowercase with curly quotes made straight, for matching.
fn normalise(text: &str) -> String {
    text.to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201c}', '\u{201d}'], "\"")
}

fn turn_label(i: usize, turn: &Turn) -> String {
    let what = match (&turn.text, &turn.reaction) {
        (Some(text), _) => short(text, 40),
        (None, Some(r)) => format!("reacts {}", r.emoji),
        (None, None) => String::new(),
    };
    format!("turn {i} ({}: {what})", turn.from)
}

/// A content entry's alternatives, each quoted: "a" or "b".
fn either(entry: &str) -> String {
    alternatives(entry)
        .map(quoted)
        .collect::<Vec<_>>()
        .join(" or ")
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", short(text, 80))
}

fn short(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

fn action_words(name: &str) -> String {
    alternatives(name)
        .map(|a| match a {
            "open_poll" => "open a poll",
            "record_decision" => "record the decision",
            "store_fact" => "keep the fact",
            "store_commitment" => "keep the commitment",
            "ledger_line" => "add it to the tab",
            "hold_mail" => "hold the mail",
            "send_mail" => "send mail",
            "undo" => "undo",
            "defer_to_lead" => "defer to a lead",
            "decline" => "decline",
            "change_setting" => "change a setting",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Friends => "friends",
        Kind::Household => "household",
        Kind::Team => "team",
        Kind::Project => "project",
        Kind::Club => "club",
        Kind::Community => "community",
        Kind::Committee => "committee",
        Kind::Cohort => "cohort",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario() -> Scenario {
        Scenario::from_json(
            r#"{
                "name": "mini", "kind": "team", "description": "one ask, two quiet lines",
                "members": [
                    { "id": "priya", "name": "Priya", "role": "lead" },
                    { "id": "sam", "name": "Sam", "role": "member" }
                ],
                "turns": [
                    { "t": 0, "from": "sam", "text": "morning" },
                    { "t": 60, "from": "sam", "text": "Q, no quiet hours" },
                    { "t": 120, "from": "priya", "text": "lol" }
                ],
                "expect": {
                    "0": { "speak": "must_not", "react": "must_not" },
                    "1": { "speak": "must", "action": "defer_to_lead|decline",
                           "action_must_not": ["change_setting"],
                           "content_must_mention": ["lead"] },
                    "2": { "speak": "must_not" }
                }
            }"#,
        )
        .unwrap()
    }

    fn transcript(entries: Vec<Entry>) -> Transcript {
        Transcript {
            scenario: "mini".into(),
            agent: "test".into(),
            entries,
        }
    }

    #[test]
    fn a_perfect_run_scores_100() {
        let s = scenario();
        let t = transcript(vec![
            Entry::message(1, "Happy to. Needs a lead's ✅."),
            Entry::action(1, "defer_to_lead"),
        ]);
        let score = score(&s, &t);
        assert_eq!(score.overall, 100.0, "{:?}", score.misses);
        assert_eq!(score.recall, Some(1.0));
        assert_eq!(score.precision, Some(1.0));
        assert!(score.misses.is_empty());
    }

    #[test]
    fn each_miss_costs_its_component() {
        let s = scenario();
        let t = transcript(vec![
            Entry::message(0, "Good morning!").unprompted(true),
            Entry::reaction(0, "👋"),
            Entry::message(1, "Done, no quiet hours."),
            Entry::action(1, "change_setting"),
        ]);
        let score = score(&s, &t);
        assert_eq!(score.recall, Some(1.0));
        assert_eq!(score.precision, Some(0.5));
        assert_eq!(score.silence, Some(0.5));
        assert_eq!(score.content, Some(0.0));
        assert_eq!(score.actions, Some(0.0));
        assert_eq!(score.interjections, 1);
        assert_eq!(score.over_budget, 0);
        assert_eq!(score.misses.len(), 5, "{:#?}", score.misses);
        let expected = 100.0 * (20.0 + 0.5 * 10.0 + 0.5 * 25.0 + 10.0) / 100.0;
        assert!((score.overall - expected).abs() < 1e-9, "{}", score.overall);
    }

    #[test]
    fn unprompted_lines_past_the_cap_are_counted() {
        let s = scenario();
        let t = transcript(vec![
            Entry::message(0, "a"),
            Entry::message(2, "b"),
            Entry::message(2, "c"),
        ]);
        let score = score(&s, &t);
        // The team budget is one an hour.
        assert_eq!((score.interjections, score.over_budget), (3, 2));
        assert_eq!(score.budget, 1.0 / 3.0);
    }

    #[test]
    fn a_dm_turn_is_answered_by_a_dm() {
        let mut s = scenario();
        s.turns[1].room = DM.into();
        let t = transcript(vec![
            Entry::dm(1, "sam", "Happy to. Needs a lead's ✅."),
            Entry::action(1, "decline"),
        ]);
        let score = score(&s, &t);
        assert_eq!(score.overall, 100.0, "{:?}", score.misses);
    }
}
