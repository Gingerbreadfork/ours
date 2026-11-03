//! The engine's model request on the wire: the engine speaks `ModelRequest`
//! and `Plan`; the client speaks JSON and a schema.

use ours_core::{ModelRequest, Plan, TurnRole};
use ours_model::{ChatRequest, ChatTurn, LargeModel};
use ours_types::GroupId;
use serde_json::Value;

/// How the chat model is asked for replies; extraction and scoring stay at 0.
#[derive(Debug, Clone, Copy)]
pub struct ChatSettings {
    pub temperature: f32,
    pub max_tokens: u32,
}

impl Default for ChatSettings {
    fn default() -> Self {
        ChatSettings {
            temperature: 0.8,
            max_tokens: 700,
        }
    }
}

/// Calls the model and turns its answer into what the engine expects. Every
/// failure becomes a string, because the engine only needs to know it failed.
pub async fn ask(
    model: &dyn LargeModel,
    group: &GroupId,
    request: ModelRequest,
    strict: bool,
    chat: ChatSettings,
) -> Result<Plan, String> {
    let output_schema = if strict {
        strict_schema(request.output_schema)
    } else {
        request.output_schema
    };
    let chat = ChatRequest {
        system: request.system,
        messages: request
            .messages
            .into_iter()
            .map(|turn| ChatTurn {
                role: match turn.role {
                    TurnRole::User => ours_model::TurnRole::User,
                    TurnRole::Assistant => ours_model::TurnRole::Assistant,
                },
                content: turn.content,
            })
            .collect(),
        output_schema,
        schema_name: "plan".to_owned(),
        max_tokens: chat.max_tokens,
        temperature: chat.temperature,
    };
    let response = model
        .complete(group, chat)
        .await
        .map_err(|e| e.to_string())?;
    tracing::debug!(plan = %response.json, "model plan");
    serde_json::from_value(response.json).map_err(|e| format!("plan did not match the schema: {e}"))
}

/// Rewrites a schema into the shape OpenAI's strict mode accepts: every
/// object closes `additionalProperties` and requires all of its properties.
/// Optional fields keep their nullable type, so the meaning is unchanged.
pub fn strict_schema(mut schema: Value) -> Value {
    tighten(&mut schema);
    schema
}

fn tighten(value: &mut Value) {
    let Value::Object(map) = value else {
        if let Value::Array(items) = value {
            items.iter_mut().for_each(tighten);
        }
        return;
    };
    if let Some(Value::Object(properties)) = map.get("properties") {
        let keys: Vec<Value> = properties
            .keys()
            .map(|k| Value::String(k.clone()))
            .collect();
        map.insert("required".to_owned(), Value::Array(keys));
        map.insert("additionalProperties".to_owned(), Value::Bool(false));
    }
    for child in map.values_mut() {
        tighten(child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strict_schema_closes_every_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "message": { "type": ["string", "null"] },
                "actions": { "type": "array", "items": { "$ref": "#/$defs/A" } }
            },
            "required": ["message"],
            "$defs": {
                "A": { "oneOf": [
                    { "type": "object", "properties": { "type": { "const": "say" }, "text": { "type": "string" } } },
                    { "type": "object", "properties": { "type": { "const": "nothing" } } }
                ] }
            }
        });
        let strict = strict_schema(schema);
        assert_eq!(strict["additionalProperties"], json!(false));
        assert_eq!(strict["required"], json!(["message", "actions"]));
        let first = &strict["$defs"]["A"]["oneOf"][0];
        assert_eq!(first["additionalProperties"], json!(false));
        assert_eq!(first["required"], json!(["type", "text"]));
    }

    #[test]
    fn the_engine_plan_schema_survives_tightening() {
        let strict = strict_schema(ours_core::plan_schema());
        let plan: Plan =
            serde_json::from_value(json!({ "message": "hi", "actions": [] })).expect("plan");
        assert_eq!(plan.message.as_deref(), Some("hi"));
        assert!(strict["properties"]["actions"].is_object());
    }
}
