#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use ours_model::{
    ChatRequest, ChatTurn, Dialect, LargeModel, ModelConfig, ModelError, OpenAiCompatible, Pricing,
    SpendMeter, TurnRole, Usage,
};
use ours_types::GroupId;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn group() -> GroupId {
    GroupId::new("group-1")
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "reply": { "type": "string" } },
        "required": ["reply"],
        "additionalProperties": false
    })
}

fn request() -> ChatRequest {
    ChatRequest {
        system: "You are Q.".into(),
        messages: vec![
            ChatTurn {
                role: TurnRole::User,
                content: "hello".into(),
            },
            ChatTurn {
                role: TurnRole::Assistant,
                content: "hi".into(),
            },
            ChatTurn {
                role: TurnRole::User,
                content: "what is for dinner".into(),
            },
        ],
        output_schema: schema(),
        schema_name: "plan".into(),
        max_tokens: 256,
        temperature: 0.2,
    }
}

fn chat_reply(content: &str) -> Value {
    json!({
        "id": "chatcmpl-1",
        "model": "served-model-2026",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 120,
            "completion_tokens": 12,
            "prompt_tokens_details": { "cached_tokens": 100 }
        }
    })
}

fn ok(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(chat_reply(content))
}

fn config(server: &MockServer, dialect: Dialect) -> ModelConfig {
    let mut config = ModelConfig::new(format!("{}/v1", server.uri()), "my-model");
    config.dialect = dialect;
    config.max_retries = 2;
    config
}

fn client_with(config: ModelConfig, meter: Arc<SpendMeter>) -> OpenAiCompatible {
    OpenAiCompatible::new(config, meter)
        .unwrap()
        .with_backoff(Duration::from_millis(1), Duration::from_millis(5))
}

fn client(server: &MockServer, dialect: Dialect) -> OpenAiCompatible {
    client_with(config(server, dialect), Arc::new(SpendMeter::default()))
}

async fn bodies(server: &MockServer) -> Vec<Value> {
    requests(server)
        .await
        .iter()
        .map(|r| r.body_json::<Value>().unwrap())
        .collect()
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

#[tokio::test]
async fn openai_dialect_returns_json_and_usage_and_sends_response_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok(r#"{"reply": "pasta"}"#))
        .mount(&server)
        .await;
    let mut cfg = config(&server, Dialect::OpenAi);
    cfg.api_key = Some("sk-test".into());
    let model = client_with(cfg, Arc::new(SpendMeter::default()));

    let response = model.complete(&group(), request()).await.unwrap();

    assert_eq!(response.json, json!({"reply": "pasta"}));
    assert_eq!(
        response.usage,
        Usage {
            input_tokens: 120,
            output_tokens: 12,
            cached_input_tokens: 100
        }
    );
    assert_eq!(response.model, "served-model-2026");

    let sent = requests(&server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer sk-test"
    );
    let body = sent[0].body_json::<Value>().unwrap();
    assert_eq!(
        body["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": { "name": "plan", "schema": schema(), "strict": true }
        })
    );
    assert!(body.get("json_schema").is_none());
    assert_eq!(body["model"], "my-model");
    assert_eq!(body["max_tokens"], 256);
    assert_eq!(body["temperature"], json!(0.2));
    assert_eq!(body["stream"], false);
    let roles: Vec<&str> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(body["messages"][0]["content"], "You are Q.");
}

#[tokio::test]
async fn llama_cpp_dialect_puts_the_schema_under_json_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok(r#"{"reply": "soup"}"#))
        .mount(&server)
        .await;

    let response = client(&server, Dialect::LlamaCpp)
        .complete(&group(), request())
        .await
        .unwrap();

    assert_eq!(response.json, json!({"reply": "soup"}));
    let sent = requests(&server).await;
    assert!(sent[0].headers.get("authorization").is_none());
    let body = sent[0].body_json::<Value>().unwrap();
    assert_eq!(body["json_schema"], schema());
    assert!(body.get("response_format").is_none());
}

#[tokio::test]
async fn ollama_dialect_uses_the_native_chat_api_with_format() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "{\"reply\": \"rice\"}" },
            "done": true,
            "prompt_eval_count": 40,
            "eval_count": 9
        })))
        .mount(&server)
        .await;

    let mut cfg = config(&server, Dialect::Ollama);
    cfg.base_url = server.uri();
    let response = client_with(cfg, Arc::new(SpendMeter::default()))
        .complete(&group(), request())
        .await
        .unwrap();

    assert_eq!(response.json, json!({"reply": "rice"}));
    assert_eq!(response.usage.input_tokens, 40);
    assert_eq!(response.usage.output_tokens, 9);
    assert_eq!(response.model, "llama3.2");
    let body = &bodies(&server).await[0];
    assert_eq!(body["format"], schema());
    assert_eq!(body["stream"], false);
    assert_eq!(body["options"]["num_predict"], 256);
    assert_eq!(body["options"]["temperature"], json!(0.2));
    assert!(body.get("response_format").is_none());
}

#[tokio::test]
async fn a_429_then_a_200_succeeds_with_two_requests() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok(r#"{"reply": "ok"}"#))
        .mount(&server)
        .await;

    let response = client(&server, Dialect::OpenAi)
        .complete(&group(), request())
        .await
        .unwrap();

    assert_eq!(response.json, json!({"reply": "ok"}));
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn server_errors_are_retried_until_the_attempts_run_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
        .mount(&server)
        .await;

    let error = client(&server, Dialect::OpenAi)
        .complete(&group(), request())
        .await
        .unwrap_err();

    match error {
        ModelError::Http { status, body } => {
            assert_eq!(status, 503);
            assert_eq!(body, "overloaded");
        }
        other => panic!("expected Http, got {other:?}"),
    }
    // One attempt plus max_retries of 2.
    assert_eq!(requests(&server).await.len(), 3);
}

#[tokio::test]
async fn a_400_fails_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad schema"))
        .mount(&server)
        .await;

    let error = client(&server, Dialect::OpenAi)
        .complete(&group(), request())
        .await
        .unwrap_err();

    match error {
        ModelError::Http { status, body } => {
            assert_eq!(status, 400);
            assert_eq!(body, "bad schema");
        }
        other => panic!("expected Http, got {other:?}"),
    }
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn fenced_json_is_unwrapped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("```json\n{\"reply\": \"fenced\"}\n```"))
        .mount(&server)
        .await;

    let response = client(&server, Dialect::OpenAi)
        .complete(&group(), request())
        .await
        .unwrap();

    assert_eq!(response.json, json!({"reply": "fenced"}));
}

#[tokio::test]
async fn malformed_content_returns_the_raw_text_and_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("Sure! The answer is pasta."))
        .mount(&server)
        .await;

    let error = client(&server, Dialect::OpenAi)
        .complete(&group(), request())
        .await
        .unwrap_err();

    match error {
        ModelError::MalformedOutput { raw } => assert_eq!(raw, "Sure! The answer is pasta."),
        other => panic!("expected MalformedOutput, got {other:?}"),
    }
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn a_reply_that_is_not_a_chat_completion_is_malformed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy page</html>"))
        .mount(&server)
        .await;

    let error = client(&server, Dialect::OpenAi)
        .complete(&group(), request())
        .await
        .unwrap_err();

    match error {
        ModelError::MalformedOutput { raw } => assert_eq!(raw, "<html>proxy page</html>"),
        other => panic!("expected MalformedOutput, got {other:?}"),
    }
}

#[tokio::test]
async fn an_attempt_that_runs_long_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("{}").set_delay(Duration::from_millis(2500)))
        .mount(&server)
        .await;
    let mut cfg = config(&server, Dialect::OpenAi);
    cfg.timeout_secs = 1;
    cfg.max_retries = 0;

    let error = client_with(cfg, Arc::new(SpendMeter::default()))
        .complete(&group(), request())
        .await
        .unwrap_err();

    assert!(matches!(error, ModelError::Timeout), "{error:?}");
}

#[tokio::test]
async fn an_unreachable_endpoint_is_a_transport_error() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut cfg = ModelConfig::new(format!("http://127.0.0.1:{port}/v1"), "m");
    cfg.max_retries = 1;

    let error = client_with(cfg, Arc::new(SpendMeter::default()))
        .complete(&group(), request())
        .await
        .unwrap_err();

    assert!(matches!(error, ModelError::Transport(_)), "{error:?}");
}

#[tokio::test]
async fn a_group_at_its_cap_is_blocked_without_an_http_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("{}"))
        .mount(&server)
        .await;
    let meter = Arc::new(SpendMeter::new(1.00));
    meter.record(&group(), 1.00);
    let model = client_with(config(&server, Dialect::OpenAi), Arc::clone(&meter));

    let error = model.complete(&group(), request()).await.unwrap_err();

    match error {
        ModelError::CapExceeded {
            group: g,
            spent,
            cap,
        } => {
            assert_eq!(g, group());
            assert!((spent - 1.00).abs() < 1e-9);
            assert_eq!(cap, 1.00);
        }
        other => panic!("expected CapExceeded, got {other:?}"),
    }
    assert!(requests(&server).await.is_empty());

    // Another group is unaffected.
    let other = GroupId::new("group-2");
    assert!(model.complete(&other, request()).await.is_ok());
}

#[tokio::test]
async fn a_call_is_charged_and_the_next_one_is_blocked_at_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(r#"{"reply": "ok"}"#))
        .mount(&server)
        .await;
    let mut cfg = config(&server, Dialect::OpenAi);
    // 20 fresh and 100 cached prompt tokens and 12 output tokens at these
    // rates come to 0.90 dollars a call.
    cfg.pricing = Some(Pricing {
        input_per_million: 10_000.0,
        output_per_million: 50_000.0,
        cached_input_per_million: 1_000.0,
    });
    let meter = Arc::new(SpendMeter::new(1.00));
    let model = client_with(cfg, Arc::clone(&meter));

    model.complete(&group(), request()).await.unwrap();
    assert!((meter.spent_today(&group()) - 0.9).abs() < 1e-9);

    // 0.90 is under the cap, so a second call goes out and tips it over.
    model.complete(&group(), request()).await.unwrap();
    assert!((meter.spent_today(&group()) - 1.8).abs() < 1e-9);

    let error = model.complete(&group(), request()).await.unwrap_err();
    assert!(matches!(error, ModelError::CapExceeded { .. }), "{error:?}");
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn without_pricing_nothing_is_charged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("{}"))
        .mount(&server)
        .await;
    let meter = Arc::new(SpendMeter::new(1.00));
    let model = client_with(config(&server, Dialect::OpenAi), Arc::clone(&meter));

    model.complete(&group(), request()).await.unwrap();

    assert_eq!(meter.spent_today(&group()), 0.0);
}

#[tokio::test]
async fn malformed_output_still_counts_against_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("not json"))
        .mount(&server)
        .await;
    let mut cfg = config(&server, Dialect::OpenAi);
    cfg.pricing = Some(Pricing {
        input_per_million: 10_000.0,
        output_per_million: 0.0,
        cached_input_per_million: 0.0,
    });
    let meter = Arc::new(SpendMeter::new(1.00));
    let model = client_with(cfg, Arc::clone(&meter));

    assert!(model.complete(&group(), request()).await.is_err());

    // 20 fresh prompt tokens at 10,000 dollars a million.
    assert!((meter.spent_today(&group()) - 0.2).abs() < 1e-9);
}

#[test]
fn bad_configuration_is_refused() {
    let meter = || Arc::new(SpendMeter::default());
    let cases = [
        ModelConfig::new("not a url", "m"),
        ModelConfig::new("ftp://example.com", "m"),
        ModelConfig::new("http://localhost:8080/v1", "  "),
        {
            let mut c = ModelConfig::new("http://localhost:8080/v1", "m");
            c.timeout_secs = 0;
            c
        },
        {
            let mut c = ModelConfig::new("http://localhost:8080/v1", "m");
            c.pricing = Some(Pricing {
                input_per_million: -1.0,
                output_per_million: 0.0,
                cached_input_per_million: 0.0,
            });
            c
        },
    ];
    for config in cases {
        let shown = format!("{config:?}");
        let result = OpenAiCompatible::new(config, meter());
        assert!(matches!(result, Err(ModelError::Config(_))), "{shown}");
    }
}

#[tokio::test]
async fn usable_as_a_trait_object() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(r#"{"reply": "ok"}"#))
        .mount(&server)
        .await;
    let model: Arc<dyn LargeModel> = Arc::new(client(&server, Dialect::OpenAi));

    assert_eq!(model.name(), "my-model");
    assert!(model.complete(&group(), request()).await.is_ok());
}
