//! A reasoning model can spend the whole token budget before the answer.
//! The client asks once more with more room, then gives up.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_model::{Dialect, LargeModel, ModelConfig, ModelError, OpenAiCompatible, SpendMeter};
use ours_types::GroupId;
use serde_json::{Value, json};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cut_off() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"content": "", "reasoning_content": "thinking..."}, "finish_reason": "length"}],
        "usage": {"prompt_tokens": 50, "completion_tokens": 160}
    }))
}

fn answered() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"content": "{\"ok\": true}"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 50, "completion_tokens": 20}
    }))
}

fn request() -> ours_model::ChatRequest {
    ours_model::ChatRequest {
        system: "answer in json".into(),
        messages: vec![],
        output_schema: json!({"type": "object"}),
        schema_name: "answer".into(),
        max_tokens: 160,
        temperature: 0.0,
    }
}

async fn client(server: &MockServer) -> OpenAiCompatible {
    let mut config = ModelConfig::new(format!("{}/v1", server.uri()), "muse");
    config.dialect = Dialect::LlamaCpp;
    config.max_retries = 0;
    OpenAiCompatible::new(config, Arc::new(SpendMeter::default())).unwrap()
}

#[tokio::test]
async fn a_cut_off_answer_is_asked_for_again_with_more_room() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(cut_off())
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(answered())
        .mount(&server)
        .await;
    let response = client(&server)
        .await
        .complete(&GroupId::new("g"), request())
        .await
        .unwrap();
    assert_eq!(response.json, json!({"ok": true}));
    let sent = server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 2);
    let first: Value = sent[0].body_json().unwrap();
    let second: Value = sent[1].body_json().unwrap();
    assert_eq!(first["max_tokens"], 160);
    assert_eq!(second["max_tokens"], 480);
}

#[tokio::test]
async fn a_second_cut_off_is_an_error_not_a_loop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(cut_off())
        .mount(&server)
        .await;
    let error = client(&server)
        .await
        .complete(&GroupId::new("g"), request())
        .await
        .unwrap_err();
    assert!(
        matches!(error, ModelError::MalformedOutput { .. }),
        "{error}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
