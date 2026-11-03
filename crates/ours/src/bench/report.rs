//! The leaderboard: scores per agent and scenario, as Markdown.

use crate::bench::score::{ScenarioScore, kind_name};
use std::fmt::Write as _;

/// What a row with no scores says.
pub const NOT_YET_RUN: &str = "not yet run";

/// The leaderboard and the component breakdown, one row per agent and one
/// column per scenario. An agent with no scores is shown as not yet run.
pub fn render_results(results: &[(String, Vec<ScenarioScore>)]) -> String {
    let mut columns: Vec<(&str, &str)> = Vec::new();
    for (_, scores) in results {
        for s in scores {
            if !columns.iter().any(|(name, _)| *name == s.scenario) {
                columns.push((&s.scenario, kind_name(s.kind)));
            }
        }
    }
    let mut out = String::new();
    out.push_str("| Agent | Overall | Ran |");
    for (name, _) in &columns {
        let _ = write!(out, " {name} |");
    }
    out.push_str("\n|---|---:|---:|");
    out.push_str(&"---:|".repeat(columns.len()));
    out.push('\n');
    for (agent, scores) in results {
        let ran: Vec<&ScenarioScore> = scores.iter().filter(|s| s.ran).collect();
        if ran.is_empty() {
            let _ = write!(out, "| {agent} | {NOT_YET_RUN} | 0/{} |", columns.len());
            out.push_str(&" - |".repeat(columns.len()));
            out.push('\n');
            continue;
        }
        let overall = mean(ran.iter().map(|s| s.overall)).unwrap_or(0.0);
        let _ = write!(
            out,
            "| {agent} | {overall:.0} | {}/{} |",
            ran.len(),
            columns.len()
        );
        for (name, _) in &columns {
            match scores.iter().find(|s| s.scenario == *name) {
                Some(s) if s.ran => {
                    let _ = write!(out, " {:.0} |", s.overall);
                }
                Some(_) => out.push_str(" not run |"),
                None => out.push_str(" - |"),
            }
        }
        out.push('\n');
    }

    out.push_str(
        "\n| Agent | Recall | Precision | Silence | Over budget | Content | Actions |\n|---|---:|---:|---:|---:|---:|---:|\n",
    );
    for (agent, scores) in results {
        let ran: Vec<&ScenarioScore> = scores.iter().filter(|s| s.ran).collect();
        if ran.is_empty() {
            let _ = writeln!(out, "| {agent} | - | - | - | - | - | - |");
            continue;
        }
        let pct = |pick: fn(&ScenarioScore) -> Option<f64>| {
            mean(ran.iter().filter_map(|s| pick(s)))
                .map_or_else(|| "-".to_owned(), |m| format!("{:.0}%", 100.0 * m))
        };
        let over: u32 = ran.iter().map(|s| s.over_budget).sum();
        let _ = writeln!(
            out,
            "| {agent} | {} | {} | {} | {over} | {} | {} |",
            pct(|s| s.recall),
            pct(|s| s.precision),
            pct(|s| s.silence),
            pct(|s| s.content),
            pct(|s| s.actions),
        );
    }
    out
}

/// Every miss, per agent and scenario, for the page under the tables.
pub fn render_misses(results: &[(String, Vec<ScenarioScore>)]) -> String {
    let mut out = String::new();
    for (agent, scores) in results {
        let missed: Vec<&ScenarioScore> = scores
            .iter()
            .filter(|s| s.ran && !s.misses.is_empty())
            .collect();
        if missed.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {agent}\n");
        for s in missed {
            let _ = writeln!(out, "**{}** ({:.0})\n", s.scenario, s.overall);
            for miss in &s.misses {
                let _ = writeln!(out, "- {miss}");
            }
            out.push('\n');
        }
    }
    out
}

/// A whole results page: heading, the tables, and what each agent missed.
pub fn results_page(results: &[(String, Vec<ScenarioScore>)], note: &str) -> String {
    let mut out = String::from("# Benchmark results\n\n");
    if !note.trim().is_empty() {
        out.push_str(note.trim());
        out.push_str("\n\n");
    }
    out.push_str(
        "Scores are 0 to 100, from the formula in [FORMAT.md](FORMAT.md#the-score). \
         Each scenario column is that scenario's overall; Overall is their mean.\n\n",
    );
    out.push_str(&render_results(results));
    let misses = render_misses(results);
    if !misses.is_empty() {
        out.push_str("\n## What each agent missed\n\n");
        out.push_str(&misses);
    }
    out.trim_end().to_owned() + "\n"
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0u32), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / f64::from(n))
}
