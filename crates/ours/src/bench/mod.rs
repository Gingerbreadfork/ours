//! The benchmark (DESIGN.md section 18, "Standards"): the scenario suite in
//! a language-neutral form, a scorer any group agent can be measured by,
//! and the agents under test. benchmark/FORMAT.md is the format and the
//! formula; benchmark/README.md says what is measured and why.

pub mod canned;
pub mod engine;
pub mod report;
pub mod scenario;
pub mod score;
pub mod transcript;
pub mod wrapper;

pub use canned::CannedModel;
pub use engine::{OursStack, run_ours};
pub use report::{render_misses, render_results, results_page};
pub use scenario::{
    Expectation, Need, RoleHint, Scenario, ScenarioMember, Turn, load_scenario, load_scenarios,
};
pub use score::{ScenarioScore, score};
pub use transcript::{
    Entry, EntryKind, Transcript, load_transcript, load_transcripts, save_transcript,
};
pub use wrapper::run_wrapper;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BenchError {
    #[error("could not read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{} is not a valid {what}: {source}", path.display())]
    Parse {
        path: PathBuf,
        what: &'static str,
        source: serde_json::Error,
    },
    #[error("scenario {scenario}: {problem}")]
    Invalid { scenario: String, problem: String },
    #[error("unknown agent {0:?}: use ours, wrapper, rules or transcript:<path>")]
    Agent(String),
    #[error("the {0} agent needs a model; give it a configuration with a [model] section")]
    NoModel(&'static str),
    #[error("could not build the model stack: {0}")]
    Setup(String),
}

/// Which agent `ours bench --agent` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSpec {
    /// Ours with the configured model, classifier and extractor.
    Ours,
    /// The plain wrapper around the configured model.
    Wrapper,
    /// Ours with the rules classifier, no extractor and a canned model.
    Rules,
    /// Transcripts recorded from any other agent: a file or a directory.
    Transcripts(PathBuf),
}

impl FromStr for AgentSpec {
    type Err = BenchError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "ours" => Ok(AgentSpec::Ours),
            "wrapper" => Ok(AgentSpec::Wrapper),
            "rules" => Ok(AgentSpec::Rules),
            other => match other.strip_prefix("transcript:") {
                Some(path) if !path.is_empty() => Ok(AgentSpec::Transcripts(path.into())),
                _ => Err(BenchError::Agent(other.to_owned())),
            },
        }
    }
}

impl AgentSpec {
    /// Whether this agent needs the configured model.
    pub fn needs_model(&self) -> bool {
        matches!(self, AgentSpec::Ours | AgentSpec::Wrapper)
    }
}

/// What `ours bench` was asked to do.
#[derive(Debug, Clone)]
pub struct BenchOptions {
    pub agents: Vec<AgentSpec>,
    /// The directory of scenario files.
    pub scenarios: PathBuf,
    /// Where the results page goes; nowhere when unset.
    pub out: Option<PathBuf>,
    /// Where each agent's transcripts are saved, one directory per agent.
    pub transcripts: Option<PathBuf>,
}

impl BenchOptions {
    pub fn needs_model(&self) -> bool {
        self.agents.iter().any(AgentSpec::needs_model)
    }
}

/// One agent's run over the suite.
#[derive(Debug, Clone)]
pub struct AgentRun {
    pub name: String,
    pub scores: Vec<ScenarioScore>,
    pub transcripts: Vec<Transcript>,
}

/// Runs or loads one agent over every scenario and scores it. `live` is the
/// configured stack, needed by the ours and wrapper agents.
pub async fn run_agent(
    spec: &AgentSpec,
    scenarios: &[Scenario],
    live: Option<&OursStack>,
) -> Result<AgentRun, BenchError> {
    let mut transcripts = Vec::new();
    let mut scores = Vec::new();
    let name = match spec {
        AgentSpec::Ours | AgentSpec::Rules => {
            let rules = OursStack::rules_only();
            let stack = match spec {
                AgentSpec::Ours => live.ok_or(BenchError::NoModel("ours"))?,
                _ => &rules,
            };
            for s in scenarios {
                let t = run_ours(s, stack).await?;
                scores.push(score(s, &t));
                transcripts.push(t);
            }
            stack.label()
        }
        AgentSpec::Wrapper => {
            let stack = live.ok_or(BenchError::NoModel("wrapper"))?;
            for s in scenarios {
                let t = run_wrapper(s, stack.model.as_ref(), stack.chat).await?;
                scores.push(score(s, &t));
                transcripts.push(t);
            }
            format!("Plain wrapper, {}", stack.model.name())
        }
        AgentSpec::Transcripts(path) => {
            let mut loaded = load_transcripts(path)?;
            for s in scenarios {
                match loaded.remove(&s.name) {
                    Some(t) => {
                        scores.push(score(s, &t));
                        transcripts.push(t);
                    }
                    None => scores.push(ScenarioScore::not_run(s)),
                }
            }
            transcripts
                .iter()
                .map(|t| t.agent.trim())
                .find(|a| !a.is_empty())
                .map(str::to_owned)
                .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
                .unwrap_or_else(|| "transcripts".to_owned())
        }
    };
    Ok(AgentRun {
        name,
        scores,
        transcripts,
    })
}

/// The rows a published page always shows, with those not run marked so.
pub fn with_placeholders(
    mut rows: Vec<(String, Vec<ScenarioScore>)>,
) -> Vec<(String, Vec<ScenarioScore>)> {
    let has = |rows: &[(String, Vec<ScenarioScore>)], word: &str| {
        rows.iter()
            .any(|(name, _)| name.to_lowercase().contains(word))
    };
    if !has(&rows, "rules only") {
        rows.push(("Ours, rules only".to_owned(), Vec::new()));
    }
    if !rows
        .iter()
        .any(|(name, _)| name.starts_with("Ours, ") && !name.contains("rules"))
    {
        rows.push(("Ours with a model".to_owned(), Vec::new()));
    }
    if !has(&rows, "wrapper") {
        rows.push(("Plain wrapper".to_owned(), Vec::new()));
    }
    if !has(&rows, "astolfo") {
        rows.push(("Astolfo".to_owned(), Vec::new()));
    }
    rows
}

/// Everything `ours bench` does: load the scenarios, run each agent,
/// save transcripts when asked, and write the results page. Returns the page.
pub async fn bench(options: &BenchOptions, live: Option<&OursStack>) -> Result<String, BenchError> {
    let scenarios = load_scenarios(&options.scenarios)?;
    let mut rows = Vec::new();
    for spec in &options.agents {
        let run = run_agent(spec, &scenarios, live).await?;
        tracing::info!(
            agent = %run.name,
            scenarios = run.scores.len(),
            overall = run.scores.iter().map(|s| s.overall).sum::<f64>() / run.scores.len().max(1) as f64,
            "benchmark run"
        );
        if let Some(dir) = &options.transcripts {
            let dir = dir.join(slug(&run.name));
            for t in &run.transcripts {
                save_transcript(&dir.join(format!("{}.json", t.scenario)), t)?;
            }
        }
        rows.push((run.name, run.scores));
    }
    let note = format!(
        "Run on {} over {} scenarios.",
        jiff::Timestamp::now().strftime("%Y-%m-%d"),
        scenarios.len()
    );
    let page = results_page(&with_placeholders(rows), &note);
    if let Some(out) = &options.out {
        write_page(out, &page)?;
    }
    Ok(page)
}

fn write_page(path: &Path, page: &str) -> Result<(), BenchError> {
    std::fs::write(path, page).map_err(|source| BenchError::Write {
        path: path.to_owned(),
        source,
    })
}

/// A file-system-safe form of an agent's name.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_parse_from_the_command_line() {
        assert_eq!("ours".parse::<AgentSpec>().unwrap(), AgentSpec::Ours);
        assert_eq!("rules".parse::<AgentSpec>().unwrap(), AgentSpec::Rules);
        assert_eq!(
            "transcript:runs/astolfo".parse::<AgentSpec>().unwrap(),
            AgentSpec::Transcripts("runs/astolfo".into())
        );
        assert!("transcript:".parse::<AgentSpec>().is_err());
        assert!("gpt".parse::<AgentSpec>().is_err());
        assert_eq!(slug("Ours, rules only"), "ours-rules-only");
    }

    #[test]
    fn placeholders_fill_the_standard_rows() {
        let rows = with_placeholders(vec![("Ours, rules only".into(), Vec::new())]);
        let names: Vec<&str> = rows.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Ours, rules only",
                "Ours with a model",
                "Plain wrapper",
                "Astolfo"
            ]
        );
    }
}
