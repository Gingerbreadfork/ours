//! Why the agent sent a message (DESIGN.md section 8, "why"). Every message
//! it sends carries a trace, and an addressed "why" reads it back without a
//! model call.

use crate::memory::FactId;
use ours_types::{Instant, Score, ScoreSource};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Traces kept per room, oldest dropped first.
pub const TRACES_KEPT: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Trace {
    /// What made the agent send the message, as a short phrase.
    pub trigger: String,
    /// The rule that applied, as a short phrase.
    pub rule: String,
    pub score: Option<Score>,
    pub at: Instant,
    /// Facts the message relied on.
    #[serde(default)]
    pub facts: Vec<FactId>,
}

impl Trace {
    pub fn new(trigger: impl Into<String>, rule: impl Into<String>, at: Instant) -> Trace {
        Trace {
            trigger: trigger.into(),
            rule: rule.into(),
            score: None,
            at,
            facts: Vec::new(),
        }
    }

    pub fn scored(mut self, score: Option<Score>) -> Trace {
        self.score = score;
        self
    }

    pub fn citing(mut self, facts: Vec<FactId>) -> Trace {
        self.facts = facts;
        self
    }

    /// The answer to "why": the trigger, the rule and the score, three lines
    /// at most.
    pub fn explain(&self) -> String {
        let mut lines = vec![
            format!("Trigger: {}", self.trigger),
            format!("Rule: {}", self.rule),
        ];
        if let Some(score) = &self.score {
            lines.push(score_line(score));
        }
        lines.join("\n")
    }
}

/// "Score: 0.60 on unanswered question, from the rules pass."
pub fn score_line(score: &Score) -> String {
    let source = match &score.source {
        ScoreSource::Rules => "the rules pass".to_owned(),
        ScoreSource::SmallModel { model } => format!("the small model {model}"),
    };
    match top_signal(score) {
        Some((name, value)) => format!("Score: {value:.2} on {name}, from {source}."),
        None => format!("Score: 0.00, nothing stood out, from {source}."),
    }
}

/// The strongest signal in a score and its name, if any is above zero.
pub fn top_signal(score: &Score) -> Option<(&'static str, f32)> {
    let signals = [
        ("addressed", score.addressed),
        ("open decision", score.open_decision),
        ("plan forming", score.plan_forming),
        ("unanswered question", score.unanswered_question),
        ("fact", score.fact),
        ("commitment", score.commitment),
    ];
    signals.into_iter().filter(|(_, v)| *v > 0.0).fold(
        None,
        |best: Option<(&str, f32)>, (name, v)| match best {
            Some((_, b)) if b >= v => best,
            _ => Some((name, v)),
        },
    )
}

/// `text` on one line, cut to about `max` characters.
pub fn shorten(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

/// `text` shortened and in double quotes, for a line that quotes someone.
pub fn quote(text: &str, max: usize) -> String {
    format!("\"{}\"", shorten(text, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strongest_signal_names_the_score() {
        let score = Score {
            unanswered_question: 0.6,
            fact: 0.5,
            ..Score::default()
        };
        assert_eq!(top_signal(&score), Some(("unanswered question", 0.6)));
        assert_eq!(
            score_line(&score),
            "Score: 0.60 on unanswered question, from the rules pass."
        );
        assert_eq!(top_signal(&Score::default()), None);
        let model = Score {
            addressed: 1.0,
            source: ScoreSource::SmallModel {
                model: "gemma".into(),
            },
            ..Score::default()
        };
        assert!(score_line(&model).ends_with("from the small model gemma."));
    }

    #[test]
    fn long_quotes_are_cut() {
        assert_eq!(quote("short  one", 20), "\"short one\"");
        assert_eq!(quote("a much longer line of text", 12), "\"a much lo...\"");
    }

    #[test]
    fn an_explanation_is_three_lines_at_most() {
        let at = Instant::UNIX_EPOCH;
        let plain = Trace::new("Sam asked", "addressed messages are always answered", at);
        assert_eq!(plain.explain().lines().count(), 2);
        let scored = plain.scored(Some(Score {
            addressed: 1.0,
            ..Score::default()
        }));
        assert_eq!(scored.explain().lines().count(), 3);
    }
}
