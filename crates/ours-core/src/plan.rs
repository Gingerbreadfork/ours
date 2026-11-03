//! What the large model returns: a structured plan, never prose to parse.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The model's answer to one addressed message.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    /// The reply to send, threaded under the message that asked. Null to say nothing.
    pub message: Option<String>,
    /// Anything to do besides the reply.
    #[serde(default)]
    pub actions: Vec<PlannedAction>,
    /// The msg ids, from what you remember, of the facts the reply relies on.
    #[serde(default)]
    pub cites: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlannedAction {
    /// A separate message in the room, not threaded under the question.
    Say {
        text: String,
    },
    /// A reaction on the message that asked.
    React {
        emoji: String,
    },
    /// An email from the group's address, held before it goes so it can be
    /// undone. Only to an address someone gave in the room.
    Mail {
        to: String,
        subject: String,
        /// Plain text, signed with your name for the group. The reply
        /// address is added for you.
        body: String,
    },
    Nothing,
}

impl PlannedAction {
    pub fn is_mail(&self) -> bool {
        matches!(self, PlannedAction::Mail { .. })
    }
}

/// The JSON Schema the model's output must match.
pub fn plan_schema() -> serde_json::Value {
    schemars::schema_for!(Plan).to_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_names_the_fields() {
        let schema = plan_schema();
        assert!(schema["properties"]["message"].is_object());
        assert!(schema["properties"]["actions"].is_object());
        assert!(schema["properties"]["cites"].is_object());
        let text = schema.to_string();
        assert!(text.contains("\"say\""));
        assert!(text.contains("\"react\""));
        assert!(text.contains("\"mail\""));
    }

    #[test]
    fn a_plan_without_actions_parses() {
        let plan: Plan = serde_json::from_str(r#"{"message":"Thursday."}"#).unwrap();
        assert_eq!(plan.message.as_deref(), Some("Thursday."));
        assert!(plan.actions.is_empty());
        assert!(plan.cites.is_empty());
    }
}
