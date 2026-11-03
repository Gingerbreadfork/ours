//! The JSON that goes to and comes back from each dialect.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ChatRequest, Dialect, ModelError, Usage};

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct OpenAiBody<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    stream: bool,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<&'a Value>,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: NamedSchema<'a>,
}

#[derive(Serialize)]
struct NamedSchema<'a> {
    name: &'a str,
    schema: &'a Value,
    strict: bool,
}

#[derive(Serialize)]
struct OllamaBody<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    stream: bool,
    format: &'a Value,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_predict: u32,
}

/// The path under the configured base URL that the dialect posts to.
pub(crate) fn path(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::OpenAi | Dialect::LlamaCpp => "chat/completions",
        Dialect::Ollama => "api/chat",
    }
}

/// The request body as bytes. The typed body is serialised directly so a
/// temperature of 0.2 stays 0.2 on the wire; extra fields are spliced in
/// as text for the same reason.
pub(crate) fn request_body(
    dialect: Dialect,
    model: &str,
    request: &ChatRequest,
    extra: &serde_json::Map<String, Value>,
) -> Result<Vec<u8>, ModelError> {
    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    if !request.system.is_empty() {
        messages.push(WireMessage {
            role: "system",
            content: &request.system,
        });
    }
    messages.extend(request.messages.iter().map(|turn| WireMessage {
        role: turn.role.as_wire(),
        content: &turn.content,
    }));

    let bytes = match dialect {
        Dialect::OpenAi | Dialect::LlamaCpp => {
            let is_openai = dialect == Dialect::OpenAi;
            serde_json::to_vec(&OpenAiBody {
                model,
                messages,
                stream: false,
                max_tokens: request.max_tokens,
                temperature: request.temperature,
                response_format: is_openai.then(|| ResponseFormat {
                    kind: "json_schema",
                    json_schema: NamedSchema {
                        name: &request.schema_name,
                        schema: &request.output_schema,
                        strict: true,
                    },
                }),
                json_schema: (!is_openai).then_some(&request.output_schema),
            })
        }
        Dialect::Ollama => serde_json::to_vec(&OllamaBody {
            model,
            messages,
            stream: false,
            format: &request.output_schema,
            options: OllamaOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
            },
        }),
    };
    let mut bytes =
        bytes.map_err(|e| ModelError::Config(format!("request could not be encoded: {e}")))?;
    if extra.is_empty() {
        return Ok(bytes);
    }
    let extra_json = serde_json::to_vec(extra)
        .map_err(|e| ModelError::Config(format!("extra fields could not be encoded: {e}")))?;
    bytes.pop();
    bytes.push(b',');
    bytes.extend_from_slice(&extra_json[1..]);
    Ok(bytes)
}

/// What the envelope of a reply holds.
#[derive(Debug)]
pub(crate) struct Reply {
    pub content: Option<String>,
    pub usage: Usage,
    pub model: Option<String>,
}

pub(crate) fn parse_reply(dialect: Dialect, body: &str) -> Result<Reply, serde_json::Error> {
    match dialect {
        Dialect::OpenAi | Dialect::LlamaCpp => parse_openai(body),
        Dialect::Ollama => parse_ollama(body),
    }
}

#[derive(Deserialize)]
struct OpenAiReply {
    model: Option<String>,
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: Option<OpenAiMessage>,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Deserialize)]
struct PromptDetails {
    cached_tokens: Option<u64>,
}

fn parse_openai(body: &str) -> Result<Reply, serde_json::Error> {
    let reply: OpenAiReply = serde_json::from_str(body)?;
    let usage = reply.usage.map_or_else(Usage::default, |u| {
        let input = u.prompt_tokens.unwrap_or(0);
        let cached = u
            .prompt_tokens_details
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);
        Usage {
            input_tokens: input,
            output_tokens: u.completion_tokens.unwrap_or(0),
            cached_input_tokens: cached.min(input),
        }
    });
    let content = reply
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message)
        .and_then(|message| message.content);
    Ok(Reply {
        content,
        usage,
        model: reply.model,
    })
}

#[derive(Deserialize)]
struct OllamaReply {
    model: Option<String>,
    message: Option<OpenAiMessage>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

fn parse_ollama(body: &str) -> Result<Reply, serde_json::Error> {
    let reply: OllamaReply = serde_json::from_str(body)?;
    Ok(Reply {
        content: reply.message.and_then(|message| message.content),
        usage: Usage {
            input_tokens: reply.prompt_eval_count.unwrap_or(0),
            output_tokens: reply.eval_count.unwrap_or(0),
            cached_input_tokens: 0,
        },
        model: reply.model,
    })
}

/// True when the server says the answer hit the token limit, which with a
/// reasoning model usually means the thinking ate the budget.
pub(crate) fn was_truncated(dialect: Dialect, body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    let reason = match dialect {
        Dialect::Ollama => value.get("done_reason"),
        _ => value
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason")),
    };
    reason.and_then(Value::as_str) == Some("length")
}

/// Parses the assistant text as JSON, unwrapping a Markdown code fence
/// when the text does not parse as it stands.
pub(crate) fn parse_json(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    serde_json::from_str(unwrap_fence(trimmed)?).ok()
}

/// The inside of a ``` fence, without the language tag, if the text is one.
fn unwrap_fence(text: &str) -> Option<&str> {
    let inner = text.strip_prefix("```")?;
    let inner = inner.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    Some(inner.trim_end().strip_suffix("```").unwrap_or(inner).trim())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn plain_json_parses() {
        assert_eq!(parse_json(" {\"a\": 1}\n"), Some(json!({"a": 1})));
    }

    #[test]
    fn json_fence_is_unwrapped() {
        let text = "```json\n{\"a\": 1}\n```";
        assert_eq!(parse_json(text), Some(json!({"a": 1})));
    }

    #[test]
    fn bare_fence_and_surrounding_space_are_unwrapped() {
        let text = "\n```\n[1, 2]\n```\n";
        assert_eq!(parse_json(text), Some(json!([1, 2])));
    }

    #[test]
    fn single_line_fence_is_unwrapped() {
        assert_eq!(parse_json("```json {\"a\": 1}```"), Some(json!({"a": 1})));
    }

    #[test]
    fn unterminated_fence_still_parses() {
        assert_eq!(parse_json("```json\n{\"a\": 1}"), Some(json!({"a": 1})));
    }

    #[test]
    fn prose_is_not_json() {
        assert_eq!(parse_json("Sure, here you go: {\"a\": 1}"), None);
        assert_eq!(parse_json("```json\nnot json\n```"), None);
        assert_eq!(parse_json(""), None);
    }

    #[test]
    fn openai_reply_reads_cached_tokens() {
        let body = json!({
            "model": "m-1",
            "choices": [{"message": {"content": "{}"}}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 7,
                "prompt_tokens_details": {"cached_tokens": 64}
            }
        })
        .to_string();
        let reply = parse_reply(Dialect::OpenAi, &body).unwrap();
        assert_eq!(reply.content.as_deref(), Some("{}"));
        assert_eq!(reply.model.as_deref(), Some("m-1"));
        assert_eq!(
            reply.usage,
            Usage {
                input_tokens: 100,
                output_tokens: 7,
                cached_input_tokens: 64
            }
        );
    }

    #[test]
    fn openai_reply_without_usage_is_zero() {
        let body = json!({"choices": [{"message": {"content": "{}"}}]}).to_string();
        let reply = parse_reply(Dialect::LlamaCpp, &body).unwrap();
        assert_eq!(reply.usage, Usage::default());
    }

    #[test]
    fn ollama_reply_reads_eval_counts() {
        let body = json!({
            "model": "llama",
            "message": {"role": "assistant", "content": "{}"},
            "prompt_eval_count": 12,
            "eval_count": 3
        })
        .to_string();
        let reply = parse_reply(Dialect::Ollama, &body).unwrap();
        assert_eq!(reply.usage.input_tokens, 12);
        assert_eq!(reply.usage.output_tokens, 3);
        assert_eq!(reply.content.as_deref(), Some("{}"));
    }

    #[test]
    fn temperature_is_not_widened_on_the_wire() {
        let request = ChatRequest {
            system: String::new(),
            messages: Vec::new(),
            output_schema: json!({}),
            schema_name: "s".into(),
            max_tokens: 10,
            temperature: 0.2,
        };
        let bytes = request_body(Dialect::OpenAi, "m", &request, &serde_json::Map::new()).unwrap();
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("\"temperature\":0.2,")
        );
    }
}

#[cfg(test)]
mod extra_tests {
    use super::*;
    use crate::{ChatRequest, ChatTurn, TurnRole};
    use serde_json::json;

    #[test]
    fn extra_fields_land_at_the_top_level() {
        let request = ChatRequest {
            system: "s".into(),
            messages: vec![ChatTurn {
                role: TurnRole::User,
                content: "hi".into(),
            }],
            output_schema: json!({ "type": "object" }),
            schema_name: "plan".into(),
            max_tokens: 10,
            temperature: 0.1,
        };
        let mut extra = serde_json::Map::new();
        extra.insert(
            "chat_template_kwargs".into(),
            json!({ "enable_thinking": false }),
        );
        let body: Value = serde_json::from_slice(
            &request_body(Dialect::LlamaCpp, "m", &request, &extra).unwrap(),
        )
        .unwrap();
        assert_eq!(
            body["chat_template_kwargs"]["enable_thinking"],
            json!(false)
        );
        assert!(body["json_schema"].is_object());
    }
}
