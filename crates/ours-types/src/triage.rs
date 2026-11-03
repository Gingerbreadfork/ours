//! The attention score every message gets (DESIGN.md section 7) and the
//! request a classifier answers. The engine's rules pass produces a first
//! score; a small model may refine it; the engine decides what to do with it.

use crate::ids::{PlatformMessageId, PlatformRoomId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What kind of thing the message is, as far as the agent is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Question,
    Instruction,
    Statement,
    Greeting,
    #[default]
    Other,
}

/// Where a score came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScoreSource {
    #[default]
    Rules,
    SmallModel {
        model: String,
    },
}

/// Each signal from the table in DESIGN.md section 7, 0 to 1. The score of
/// a message is the largest of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Score {
    pub addressed: f32,
    pub open_decision: f32,
    pub plan_forming: f32,
    pub unanswered_question: f32,
    pub fact: f32,
    pub commitment: f32,
    /// Something a mate would react to: a joke that landed, big news, a
    /// photo, a celebration. Not part of `max`; it never makes the agent
    /// act, only lets it chime in when the dial and the budget allow.
    #[serde(default)]
    pub banter: f32,
    pub kind: MessageKind,
    /// The agent could answer this from the recent messages alone.
    pub answerable_from_context: bool,
    pub source: ScoreSource,
}

impl Score {
    pub fn max(&self) -> f32 {
        [
            self.addressed,
            self.open_decision,
            self.plan_forming,
            self.unanswered_question,
            self.fact,
            self.commitment,
        ]
        .into_iter()
        .fold(0.0, f32::max)
    }
}

/// One earlier line of the room, as a classifier should read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextLine {
    pub speaker: String,
    pub text: String,
    /// Seconds before the message being classified.
    pub seconds_before: i64,
}

/// What a classifier needs to score one message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassifyRequest {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    pub agent_name: String,
    pub speaker: String,
    pub text: String,
    /// The last few messages before this one, oldest first.
    pub context: Vec<ContextLine>,
    /// Anything the group currently has open, one line each, for the
    /// open-decision signal. Empty until decisions exist.
    pub open_items: Vec<String>,
    /// The rules pass's own score, for the model to refine rather than redo.
    pub rules: Score,
}
