use serde::{Deserialize, Serialize};

/// Who spoke a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
}

impl TurnRole {
    pub(crate) fn as_wire(self) -> &'static str {
        match self {
            TurnRole::User => "user",
            TurnRole::Assistant => "assistant",
        }
    }
}

/// One message in the conversation sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: TurnRole,
    pub content: String,
}

/// A request for one JSON object matching `output_schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<ChatTurn>,
    pub output_schema: serde_json::Value,
    /// Names the schema for endpoints that require one: letters, digits,
    /// underscores and dashes.
    pub schema_name: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

/// Token counts for one call. `input_tokens` is the whole prompt and
/// includes the cached part.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
}

/// The parsed reply. The caller validates `json` against its own types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub json: serde_json::Value,
    pub usage: Usage,
    /// The model the endpoint says it ran, else the configured name.
    pub model: String,
    /// Wall time of the whole call, retries and waits included.
    pub latency_ms: u64,
}
