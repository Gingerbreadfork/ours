//! The baseline: the "Discord AI bot" of DESIGN.md section 2. It asks the
//! model for a reply whenever its name appears or it gets a DM, and says
//! nothing otherwise. No memory past the room's recent lines, no budget,
//! no idea of who may tell it what.

use crate::bench::BenchError;
use crate::bench::scenario::{DM, Scenario};
use crate::bench::transcript::{Entry, Transcript};
use crate::runtime::model::ChatSettings;
use ours_model::{ChatRequest, ChatTurn, LargeModel, TurnRole};
use ours_types::GroupId;
use serde_json::json;
use std::collections::BTreeMap;
use tracing::Instrument;

/// Lines of the room the wrapper sends with each question.
pub const WRAPPER_CONTEXT: usize = 20;

/// Plays the scenario's turns to a mention-triggered wrapper around `model`.
pub async fn run_wrapper(
    scenario: &Scenario,
    model: &dyn LargeModel,
    chat: ChatSettings,
) -> Result<Transcript, BenchError> {
    scenario.validate()?;
    let group = GroupId::new(format!("wrapper-{}", scenario.name));
    let name_of = |id: &str| {
        scenario
            .member(id)
            .map_or_else(|| id.to_owned(), |m| m.name.clone())
    };
    // Each room's lines so far, history first, as a bot fetching the channel sees them.
    let mut rooms: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut history: Vec<_> = scenario.history.iter().collect();
    history.sort_by_key(|h| std::cmp::Reverse(h.ago));
    for line in history {
        rooms.entry(line.room.clone()).or_default().push(format!(
            "{}: {}",
            name_of(&line.from),
            line.text
        ));
    }
    let mut entries = Vec::new();
    for (i, turn) in scenario.turns.iter().enumerate() {
        let Some(text) = &turn.text else { continue };
        let key = if turn.room == DM {
            format!("dm:{}", turn.from)
        } else {
            turn.room.clone()
        };
        let lines = rooms.entry(key).or_default();
        lines.push(format!("{}: {text}", name_of(&turn.from)));
        if turn.room != DM && !mentions(text, &scenario.agent_name) {
            continue;
        }
        let start = lines.len().saturating_sub(WRAPPER_CONTEXT);
        let request = ChatRequest {
            system: format!(
                "You are {}, an AI assistant in a group chat. Reply to the last message in one or two short lines of plain text.",
                scenario.agent_name
            ),
            messages: vec![ChatTurn {
                role: TurnRole::User,
                content: lines[start..].join("\n"),
            }],
            output_schema: json!({
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"],
                "additionalProperties": false
            }),
            schema_name: "reply".to_owned(),
            max_tokens: chat.max_tokens,
            temperature: chat.temperature,
        };
        let span = tracing::info_span!("bench_wrapper_turn", scenario = %scenario.name, turn = i);
        let reply = match model.complete(&group, request).instrument(span).await {
            Ok(response) => response.json["message"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_owned),
            Err(error) => {
                tracing::warn!(%error, turn = i, "the wrapper's model failed; it says nothing");
                None
            }
        };
        let Some(reply) = reply else { continue };
        lines.push(format!("{}: {reply}", scenario.agent_name));
        let entry = if turn.room == DM {
            Entry::dm(i, turn.from.clone(), reply)
        } else {
            Entry {
                room: Some(turn.room.clone()),
                ..Entry::message(i, reply)
            }
        };
        entries.push(Entry {
            t: Some(turn.t),
            ..entry.unprompted(false)
        });
    }
    Ok(Transcript {
        scenario: scenario.name.clone(),
        agent: format!("Plain wrapper, {}", model.name()),
        entries,
    })
}

/// Whether `name` appears in `text` as a word of its own, the way most bots
/// listen for their name.
pub fn mentions(text: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_counts_wherever_it_stands_alone() {
        assert!(mentions("Q, thurs or fri?", "Q"));
        assert!(mentions("q-final tickets anyone?", "Q"));
        assert!(mentions("ask @q", "Q"));
        assert!(!mentions("Q2 was rough", "Q"));
        assert!(!mentions("Qatar again?", "Q"));
    }
}
