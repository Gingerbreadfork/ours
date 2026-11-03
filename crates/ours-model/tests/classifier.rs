#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use ours_model::{
    Classifier, Dialect, ModelConfig, ModelError, OpenAiCompatible, Pricing, RulesOnlyClassifier,
    SmallModelClassifier, SpendMeter,
};
use ours_types::{
    ClassifyRequest, ContextLine, GroupId, MessageKind, PlatformMessageId, PlatformRoomId, Score,
    ScoreSource,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn group() -> GroupId {
    GroupId::new("group-1")
}

fn request() -> ClassifyRequest {
    ClassifyRequest {
        room: PlatformRoomId::new("room-1"),
        message: PlatformMessageId::new("msg-9"),
        agent_name: "Q".into(),
        speaker: "Sam".into(),
        text: "thursday works".into(),
        context: vec![
            ContextLine {
                speaker: "Priya".into(),
                text: "dinner thursday or friday?".into(),
                seconds_before: 120,
            },
            ContextLine {
                speaker: "Lee".into(),
                text: "friday is bad for me".into(),
                seconds_before: 45,
            },
        ],
        open_items: vec!["Dinner: Thursday or Friday (poll, 2 votes)".into()],
        rules: Score::default(),
    }
}

/// A reply whose usage is the size of one classify call, estimated at four
/// characters a token.
fn reply(content: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "served-small-2026",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content.to_string() },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 450, "completion_tokens": 75 }
    }))
}

fn classification() -> Value {
    json!({
        "addressed": 0.0,
        "open_decision": 0.9,
        "plan_forming": 0.2,
        "unanswered_question": 0.0,
        "fact": 0.0,
        "commitment": 0.1,
        "kind": "statement",
        "answerable_from_context": false,
        "reason": "Sam picks Thursday in the open poll."
    })
}

async fn mount(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(response)
        .mount(server)
        .await;
}

fn classifier(server: &MockServer, dialect: Dialect) -> SmallModelClassifier {
    let mut config = ModelConfig::new(format!("{}/v1", server.uri()), "gemma-small");
    config.dialect = dialect;
    config.max_retries = 0;
    let model = OpenAiCompatible::new(config, Arc::new(SpendMeter::default())).unwrap();
    SmallModelClassifier::new(Arc::new(model))
}

async fn sent_body(server: &MockServer) -> Value {
    let sent = server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    sent[0].body_json::<Value>().unwrap()
}

#[tokio::test]
async fn openai_dialect_sends_the_schema_under_response_format() {
    let server = MockServer::start().await;
    mount(&server, reply(classification())).await;

    classifier(&server, Dialect::OpenAi)
        .classify(&group(), request())
        .await
        .unwrap();

    let body = sent_body(&server).await;
    let format = &body["response_format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "classification");
    assert_eq!(format["json_schema"]["strict"], true);
    let schema = &format["json_schema"]["schema"];
    assert_eq!(schema["additionalProperties"], false);
    let props = schema["properties"].as_object().unwrap();
    for signal in [
        "addressed",
        "open_decision",
        "plan_forming",
        "unanswered_question",
        "fact",
        "commitment",
    ] {
        assert_eq!(props[signal]["type"], "number", "{signal}");
        assert_eq!(props[signal]["minimum"], 0.0, "{signal}");
        assert_eq!(props[signal]["maximum"], 1.0, "{signal}");
    }
    assert_eq!(
        props["kind"]["enum"],
        json!(["question", "instruction", "statement", "greeting", "other"])
    );
    assert_eq!(props["answerable_from_context"]["type"], "boolean");
    assert_eq!(props["reason"]["type"], "string");
    assert!(body.get("json_schema").is_none());
    assert_eq!(body["model"], "gemma-small");
    assert_eq!(body["temperature"], json!(0.0));
    assert_eq!(body["max_tokens"], 400);
}

#[tokio::test]
async fn llama_cpp_dialect_sends_the_schema_under_json_schema() {
    let server = MockServer::start().await;
    mount(&server, reply(classification())).await;

    classifier(&server, Dialect::LlamaCpp)
        .classify(&group(), request())
        .await
        .unwrap();

    let body = sent_body(&server).await;
    assert!(body.get("response_format").is_none());
    assert_eq!(body["json_schema"]["type"], "object");
    assert_eq!(body["json_schema"]["properties"]["fact"]["type"], "number");
}

#[tokio::test]
async fn the_prompt_names_the_agent_and_carries_the_context_and_the_message() {
    let server = MockServer::start().await;
    mount(&server, reply(classification())).await;
    let mut req = request();
    req.agent_name = "Quill".into();
    req.rules.addressed = 0.4;
    req.rules.kind = MessageKind::Question;

    classifier(&server, Dialect::OpenAi)
        .classify(&group(), req)
        .await
        .unwrap();

    let body = sent_body(&server).await;
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.contains("attention filter for Quill, a member of a group chat"));
    assert!(system.contains("reads everything and speaks rarely"));
    assert!(system.contains("addressed (1.0)"));
    assert!(system.contains("open_decision (0.9)"));
    assert!(system.contains("plan_forming (0.7)"));
    assert!(system.contains("unanswered_question (0.6)"));
    assert!(system.contains("fact (0.5)"));
    assert!(system.contains("commitment (0.5)"));
    assert!(system.contains("the recent lines alone contain the answer"));
    assert!(system.contains("a floor, never a verdict"));

    assert_eq!(messages[1]["role"], "user");
    assert_eq!(
        messages[1]["content"],
        "120s before, Priya: dinner thursday or friday?\n\
         45s before, Lee: friday is bad for me\n\
         Newest, said by Sam: thursday works\n\n\
         Open items:\n\
         - Dinner: Thursday or Friday (poll, 2 votes)\n\n\
         The rules found: addressed 0.4, kind question"
    );
}

#[tokio::test]
async fn a_reply_maps_to_the_score_fields_with_the_model_as_source() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(json!({
            "addressed": 0.0,
            "open_decision": 0.9,
            "plan_forming": 0.2,
            "unanswered_question": 0.6,
            "fact": 0.3,
            "commitment": 0.1,
            "kind": "question",
            "answerable_from_context": true,
            "reason": "Asks the time, and Priya stated it."
        })),
    )
    .await;

    let score = classifier(&server, Dialect::OpenAi)
        .classify(&group(), request())
        .await
        .unwrap();

    assert_eq!(
        score,
        Score {
            addressed: 0.0,
            open_decision: 0.9,
            plan_forming: 0.2,
            unanswered_question: 0.6,
            fact: 0.3,
            commitment: 0.1,
            banter: 0.0,
            kind: MessageKind::Question,
            answerable_from_context: true,
            source: ScoreSource::SmallModel {
                model: "gemma-small".into()
            },
        }
    );
    assert_eq!(score.max(), 0.9);
}

#[tokio::test]
async fn out_of_range_numbers_are_clamped() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(json!({
            "addressed": 3.5,
            "open_decision": -0.4,
            "plan_forming": 1.0001,
            "unanswered_question": 0.55,
            "fact": -100,
            "commitment": 42,
            "kind": "other",
            "answerable_from_context": false,
            "reason": "Out of range."
        })),
    )
    .await;

    let score = classifier(&server, Dialect::OpenAi)
        .classify(&group(), request())
        .await
        .unwrap();

    assert_eq!(score.addressed, 1.0);
    assert_eq!(score.open_decision, 0.0);
    assert_eq!(score.plan_forming, 1.0);
    assert_eq!(score.unanswered_question, 0.55);
    assert_eq!(score.fact, 0.0);
    assert_eq!(score.commitment, 1.0);
}

#[tokio::test]
async fn addressed_is_never_lowered_below_the_rules_hint() {
    let server = MockServer::start().await;
    mount(&server, reply(classification())).await;
    let mut req = request();
    req.rules.addressed = 1.0;

    let score = classifier(&server, Dialect::OpenAi)
        .classify(&group(), req)
        .await
        .unwrap();

    // The model said 0.0 for addressed; the other signals are its own.
    assert_eq!(score.addressed, 1.0);
    assert_eq!(score.open_decision, 0.9);
}

#[tokio::test]
async fn addressed_can_be_raised_above_the_rules_hint() {
    let server = MockServer::start().await;
    let mut content = classification();
    content["addressed"] = json!(0.8);
    mount(&server, reply(content)).await;
    let mut req = request();
    req.rules.addressed = 0.3;

    let score = classifier(&server, Dialect::OpenAi)
        .classify(&group(), req)
        .await
        .unwrap();

    assert_eq!(score.addressed, 0.8);
}

#[tokio::test]
async fn a_server_error_surfaces_as_is() {
    let server = MockServer::start().await;
    mount(&server, ResponseTemplate::new(500).set_body_string("boom")).await;

    let error = classifier(&server, Dialect::OpenAi)
        .classify(&group(), request())
        .await
        .unwrap_err();

    match error {
        ModelError::Http { status, body } => {
            assert_eq!(status, 500);
            assert_eq!(body, "boom");
        }
        other => panic!("expected Http, got {other:?}"),
    }
}

#[tokio::test]
async fn a_reply_of_the_wrong_shape_is_malformed_output() {
    let server = MockServer::start().await;
    mount(&server, reply(json!({"addressed": 1.0}))).await;

    let error = classifier(&server, Dialect::OpenAi)
        .classify(&group(), request())
        .await
        .unwrap_err();

    assert!(
        matches!(error, ModelError::MalformedOutput { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn one_call_is_charged_to_the_group_by_the_mocked_usage() {
    let server = MockServer::start().await;
    mount(&server, reply(classification())).await;
    let mut config = ModelConfig::new(format!("{}/v1", server.uri()), "gemma-small");
    config.pricing = Some(Pricing {
        input_per_million: 1000.0,
        output_per_million: 2000.0,
        cached_input_per_million: 0.0,
    });
    let meter = Arc::new(SpendMeter::default());
    let model = OpenAiCompatible::new(config, meter.clone()).unwrap();

    SmallModelClassifier::new(Arc::new(model))
        .classify(&group(), request())
        .await
        .unwrap();

    // 450 input and 75 output tokens at the rates above.
    let expected = (450.0 * 1000.0 + 75.0 * 2000.0) / 1_000_000.0;
    assert!((meter.spent_today(&group()) - expected).abs() < 1e-9);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn rules_only_echoes_the_hint_with_the_rules_source() {
    let mut req = request();
    req.rules = Score {
        addressed: 1.0,
        unanswered_question: 0.6,
        kind: MessageKind::Question,
        answerable_from_context: true,
        source: ScoreSource::SmallModel {
            model: "stale".into(),
        },
        ..Score::default()
    };
    let hint = req.rules.clone();

    let score = RulesOnlyClassifier.classify(&group(), req).await.unwrap();

    assert_eq!(
        score,
        Score {
            source: ScoreSource::Rules,
            ..hint
        }
    );
    assert_eq!(RulesOnlyClassifier.name(), "rules");
}

#[tokio::test]
async fn names_come_from_the_model_and_the_rules() {
    let server = MockServer::start().await;
    assert_eq!(classifier(&server, Dialect::OpenAi).name(), "gemma-small");
}
