#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use ours_model::{
    Dialect, Extractor, ModelConfig, ModelError, NoExtractor, OpenAiCompatible,
    SmallModelExtractor, SpendMeter,
};
use ours_types::{
    CommitmentDraft, ContextLine, ExtractRequest, Extraction, FactDraft, GroupId, LedgerDraft,
    PlatformMessageId, PlatformRoomId, SubjectKind,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn group() -> GroupId {
    GroupId::new("group-1")
}

fn request() -> ExtractRequest {
    ExtractRequest {
        room: PlatformRoomId::new("room-1"),
        message: PlatformMessageId::new("msg-9"),
        agent_name: "Q".into(),
        speaker: "Sam".into(),
        text: "I'm vegetarian, and I'm off sick until Friday".into(),
        private: false,
        context: vec![
            ContextLine {
                speaker: "Priya".into(),
                text: "what should I cook on Saturday?".into(),
                seconds_before: 60,
            },
            ContextLine {
                speaker: "Lee".into(),
                text: "anything".into(),
                seconds_before: 20,
            },
        ],
        date: "2026-11-03".into(),
        timezone: "Europe/London".into(),
    }
}

fn reply(content: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": "served-small-2026",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content.to_string() },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 500, "completion_tokens": 90 }
    }))
}

fn fact(subject: &str, predicate: &str, object: &str) -> Value {
    json!({
        "subject": subject,
        "subject_kind": "person",
        "predicate": predicate,
        "object": object,
        "about_self": true,
        "stated": true,
        "durable": true,
        "expires": null,
        "starts": null,
        "quote": null
    })
}

fn commitment(who: &str, what: &str) -> Value {
    json!({
        "who": who,
        "what": what,
        "for_whom": null,
        "due": null,
        "event": null,
        "stated": true,
        "quote": null
    })
}

fn ledger_line(payer: &str, amount: Value, what: &str) -> Value {
    json!({
        "payer": payer,
        "amount": amount,
        "currency": null,
        "what": what,
        "participants": [],
        "event": null,
        "quote": null
    })
}

fn facts(items: Vec<Value>) -> Value {
    json!({ "facts": items, "commitments": [], "ledger": [] })
}

fn commitments(items: Vec<Value>) -> Value {
    json!({ "facts": [], "commitments": items, "ledger": [] })
}

fn ledger(items: Vec<Value>) -> Value {
    json!({ "facts": [], "commitments": [], "ledger": items })
}

fn nothing() -> Value {
    json!({ "facts": [], "commitments": [], "ledger": [] })
}

async fn mount(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(response)
        .mount(server)
        .await;
}

fn extractor(server: &MockServer, dialect: Dialect) -> SmallModelExtractor {
    let mut config = ModelConfig::new(format!("{}/v1", server.uri()), "gemma-small");
    config.dialect = dialect;
    config.max_retries = 0;
    let model = OpenAiCompatible::new(config, Arc::new(SpendMeter::default())).unwrap();
    SmallModelExtractor::new(Arc::new(model))
}

async fn sent_body(server: &MockServer) -> Value {
    let sent = server.received_requests().await.unwrap();
    assert_eq!(sent.len(), 1);
    sent[0].body_json::<Value>().unwrap()
}

fn names(value: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn openai_dialect_sends_the_schema_under_response_format() {
    let server = MockServer::start().await;
    mount(&server, reply(nothing())).await;

    extractor(&server, Dialect::OpenAi)
        .extract(&group(), request())
        .await
        .unwrap();

    let body = sent_body(&server).await;
    let format = &body["response_format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "extraction");
    assert_eq!(format["json_schema"]["strict"], true);
    let schema = &format["json_schema"]["schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        names(&schema["required"]),
        ["commitments", "facts", "ledger"]
    );

    let fact = &schema["properties"]["facts"]["items"];
    assert_eq!(fact["additionalProperties"], false);
    assert_eq!(fact["required"].as_array().unwrap().len(), 10);
    assert_eq!(
        fact["properties"]["subject_kind"]["enum"],
        json!(["person", "place", "event", "thing", "group"])
    );
    for name in ["expires", "starts"] {
        assert_eq!(fact["properties"][name]["type"], json!(["string", "null"]));
    }

    let commitment = &schema["properties"]["commitments"]["items"];
    assert_eq!(commitment["additionalProperties"], false);
    assert_eq!(
        names(&commitment["required"]),
        ["due", "event", "for_whom", "quote", "stated", "what", "who"]
    );
    assert_eq!(
        commitment["properties"]["due"]["type"],
        json!(["string", "null"])
    );

    let line = &schema["properties"]["ledger"]["items"];
    assert_eq!(line["additionalProperties"], false);
    assert_eq!(
        names(&line["required"]),
        [
            "amount",
            "currency",
            "event",
            "participants",
            "payer",
            "quote",
            "what"
        ]
    );
    assert_eq!(line["properties"]["amount"]["type"], "number");
    assert_eq!(
        line["properties"]["currency"]["type"],
        json!(["string", "null"])
    );
    assert_eq!(line["properties"]["participants"]["type"], "array");

    for name in ["facts", "commitments", "ledger"] {
        assert!(
            schema["properties"][name].get("maxItems").is_none(),
            "{name}"
        );
    }
    assert!(body.get("json_schema").is_none());
    assert_eq!(body["model"], "gemma-small");
    assert_eq!(body["temperature"], json!(0.0));
    assert_eq!(body["max_tokens"], 1000);
}

#[tokio::test]
async fn llama_cpp_dialect_sends_the_schema_under_json_schema() {
    let server = MockServer::start().await;
    mount(&server, reply(nothing())).await;

    extractor(&server, Dialect::LlamaCpp)
        .extract(&group(), request())
        .await
        .unwrap();

    let body = sent_body(&server).await;
    assert!(body.get("response_format").is_none());
    let schema = &body["json_schema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        names(&schema["required"]),
        ["commitments", "facts", "ledger"]
    );
    let properties = &schema["properties"];
    assert_eq!(properties["facts"]["type"], "array");
    assert_eq!(properties["commitments"]["type"], "array");
    assert_eq!(properties["ledger"]["type"], "array");
    assert_eq!(
        properties["facts"]["items"]["properties"]["durable"]["type"],
        "boolean"
    );
    assert_eq!(
        properties["commitments"]["items"]["properties"]["who"]["type"],
        "string"
    );
    assert_eq!(
        properties["ledger"]["items"]["properties"]["amount"]["type"],
        "number"
    );
}

#[tokio::test]
async fn the_prompt_names_the_agent_and_carries_the_context_the_date_and_privacy() {
    let server = MockServer::start().await;
    mount(&server, reply(nothing())).await;
    let mut req = request();
    req.agent_name = "Quill".into();
    req.private = true;

    extractor(&server, Dialect::OpenAi)
        .extract(&group(), req)
        .await
        .unwrap();

    let body = sent_body(&server).await;
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.contains("memory of Quill, a member of a group chat"));
    assert!(system.contains("anything about Quill itself"));
    assert!(system.contains("birthdays"));
    assert!(system.contains("YYYY-MM-DD"));
    assert!(system.contains("at most five facts"));
    for kind in ["facts", "commitments", "ledger lines"] {
        assert!(system.contains(kind), "{kind}");
    }
    assert!(system.contains("I'll bring dessert"));
    assert!(system.contains("I got the wine, 38"));
    assert!(system.contains("starts"));
    assert!(system.contains("YYYY-MM-DDTHH:MM"));

    assert_eq!(messages[1]["role"], "user");
    assert_eq!(
        messages[1]["content"],
        "60s before, Priya: what should I cook on Saturday?\n\
         20s before, Lee: anything\n\
         Now, Sam: I'm vegetarian, and I'm off sick until Friday\n\n\
         Date: 2026-11-03 (Europe/London)\n\
         Private message: yes"
    );
}

async fn extract(server: &MockServer) -> Extraction {
    extractor(server, Dialect::OpenAi)
        .extract(&group(), request())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_reply_maps_to_facts() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(facts(vec![
            json!({
                "subject": "Sam",
                "subject_kind": "person",
                "predicate": "is vegetarian",
                "object": "yes",
                "about_self": true,
                "stated": true,
                "durable": true,
                "expires": null,
                "starts": null,
                "quote": "I'm vegetarian"
            }),
            json!({
                "subject": "Sam",
                "subject_kind": "person",
                "predicate": "is off sick",
                "object": "until Friday",
                "about_self": true,
                "stated": true,
                "durable": false,
                "expires": "2026-11-07",
                "starts": null,
                "quote": "off sick until Friday"
            }),
        ])),
    )
    .await;

    let found = extract(&server).await;

    assert_eq!(
        found.facts,
        vec![
            FactDraft {
                subject: "Sam".into(),
                subject_kind: SubjectKind::Person,
                predicate: "is vegetarian".into(),
                object: "yes".into(),
                about_self: true,
                stated: true,
                durable: true,
                expires: None,
                starts: None,
                quote: Some("I'm vegetarian".into()),
            },
            FactDraft {
                subject: "Sam".into(),
                subject_kind: SubjectKind::Person,
                predicate: "is off sick".into(),
                object: "until Friday".into(),
                about_self: true,
                stated: true,
                durable: false,
                expires: Some("2026-11-07".into()),
                starts: None,
                quote: Some("off sick until Friday".into()),
            },
        ]
    );
    assert!(found.commitments.is_empty());
    assert!(found.ledger.is_empty());
}

#[tokio::test]
async fn a_span_keeps_its_start_and_end() {
    let server = MockServer::start().await;
    let mut away = fact("Sam", "is away", "on holiday");
    away["durable"] = json!(false);
    away["starts"] = json!("2026-11-12");
    away["expires"] = json!("2026-11-16");
    mount(&server, reply(facts(vec![away]))).await;

    let found = extract(&server).await;

    assert_eq!(found.facts[0].starts.as_deref(), Some("2026-11-12"));
    assert_eq!(found.facts[0].expires.as_deref(), Some("2026-11-16"));
}

#[tokio::test]
async fn a_reply_with_all_three_kinds_maps_to_one_extraction() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(json!({
            "facts": [fact("Sam", "drives", "a van")],
            "commitments": [{
                "who": "Sam",
                "what": "have the draft ready",
                "for_whom": "Priya",
                "due": "2026-11-05",
                "event": "the launch",
                "stated": true,
                "quote": "I'll have the draft by Thursday"
            }, {
                "who": "Lee",
                "what": "bring dessert",
                "for_whom": null,
                "due": "2026-11-03T19:30",
                "event": null,
                "stated": false,
                "quote": null
            }],
            "ledger": [{
                "payer": "Sam",
                "amount": 38,
                "currency": "gbp",
                "what": "wine",
                "participants": ["Lee", "Priya"],
                "event": "the cabin trip",
                "quote": "I got the wine, 38"
            }]
        })),
    )
    .await;

    let found = extract(&server).await;

    assert_eq!(found.facts.len(), 1);
    assert_eq!(found.facts[0].object, "a van");
    assert_eq!(
        found.commitments,
        vec![
            CommitmentDraft {
                who: "Sam".into(),
                what: "have the draft ready".into(),
                for_whom: Some("Priya".into()),
                due: Some("2026-11-05".into()),
                event: Some("the launch".into()),
                stated: true,
                quote: Some("I'll have the draft by Thursday".into()),
            },
            CommitmentDraft {
                who: "Lee".into(),
                what: "bring dessert".into(),
                for_whom: None,
                due: Some("2026-11-03T19:30".into()),
                event: None,
                stated: false,
                quote: None,
            },
        ]
    );
    assert_eq!(
        found.ledger,
        vec![LedgerDraft {
            payer: "Sam".into(),
            amount: 38.0,
            currency: Some("GBP".into()),
            what: "wine".into(),
            participants: vec!["Lee".into(), "Priya".into()],
            event: Some("the cabin trip".into()),
            quote: Some("I got the wine, 38".into()),
        }]
    );
}

#[tokio::test]
async fn nothing_worth_keeping_is_an_empty_extraction() {
    let server = MockServer::start().await;
    mount(&server, reply(nothing())).await;

    let found = extract(&server).await;

    assert_eq!(found, Extraction::default());
}

#[tokio::test]
async fn arrays_the_model_leaves_out_are_empty() {
    let server = MockServer::start().await;
    mount(&server, reply(json!({ "ledger": [] }))).await;

    let found = extract(&server).await;

    assert_eq!(found, Extraction::default());
}

#[tokio::test]
async fn more_than_five_of_a_kind_are_cut_to_five() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(json!({
            "facts": (0..8).map(|i| fact("Sam", "owns", &format!("thing {i}"))).collect::<Vec<_>>(),
            "commitments": (0..8).map(|i| commitment("Sam", &format!("task {i}"))).collect::<Vec<_>>(),
            "ledger": (0..8).map(|i| ledger_line("Sam", json!(10 + i), &format!("item {i}"))).collect::<Vec<_>>(),
        })),
    )
    .await;

    let found = extract(&server).await;

    let objects: Vec<&str> = found.facts.iter().map(|d| d.object.as_str()).collect();
    assert_eq!(
        objects,
        ["thing 0", "thing 1", "thing 2", "thing 3", "thing 4"]
    );
    let whats: Vec<&str> = found.commitments.iter().map(|d| d.what.as_str()).collect();
    assert_eq!(whats, ["task 0", "task 1", "task 2", "task 3", "task 4"]);
    let items: Vec<&str> = found.ledger.iter().map(|d| d.what.as_str()).collect();
    assert_eq!(items, ["item 0", "item 1", "item 2", "item 3", "item 4"]);
}

#[tokio::test]
async fn drafts_with_an_empty_field_are_dropped_and_the_rest_trimmed() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(json!({
            "facts": [
                fact("", "drives", "a van"),
                fact("Sam", "  ", "a van"),
                fact("Sam", "drives", "   "),
                fact("  Lee ", " owns ", " a boat\n"),
            ],
            "commitments": [
                commitment("", "bring dessert"),
                commitment("Sam", "  "),
                commitment("  Priya ", " book the pitch\n"),
            ],
            "ledger": [
                ledger_line("", json!(38), "wine"),
                ledger_line("Sam", json!(38), "\t"),
                ledger_line(" Lee ", json!(60), " the pitch "),
            ],
        })),
    )
    .await;

    let found = extract(&server).await;

    assert_eq!(found.facts.len(), 1);
    assert_eq!(found.facts[0].subject, "Lee");
    assert_eq!(found.facts[0].predicate, "owns");
    assert_eq!(found.facts[0].object, "a boat");
    assert_eq!(found.commitments.len(), 1);
    assert_eq!(found.commitments[0].who, "Priya");
    assert_eq!(found.commitments[0].what, "book the pitch");
    assert_eq!(found.ledger.len(), 1);
    assert_eq!(found.ledger[0].payer, "Lee");
    assert_eq!(found.ledger[0].what, "the pitch");
}

#[tokio::test]
async fn long_quotes_are_cut_at_120_characters() {
    let server = MockServer::start().await;
    let long = json!("q".repeat(300));
    let mut f = fact("Sam", "owns", "a boat");
    f["quote"] = long.clone();
    let mut c = commitment("Sam", "bring dessert");
    c["quote"] = long.clone();
    let mut l = ledger_line("Sam", json!(38), "wine");
    l["quote"] = long;
    mount(
        &server,
        reply(json!({ "facts": [f], "commitments": [c], "ledger": [l] })),
    )
    .await;

    let found = extract(&server).await;

    let cut = "q".repeat(120);
    assert_eq!(found.facts[0].quote.as_deref(), Some(cut.as_str()));
    assert_eq!(found.commitments[0].quote.as_deref(), Some(cut.as_str()));
    assert_eq!(found.ledger[0].quote.as_deref(), Some(cut.as_str()));
}

#[tokio::test]
async fn a_bad_expires_becomes_none() {
    let server = MockServer::start().await;
    let mut vague = fact("Sam", "is off sick", "this week");
    vague["durable"] = json!(false);
    vague["expires"] = json!("this Friday");
    let mut impossible = fact("Sam", "is away", "on holiday");
    impossible["durable"] = json!(false);
    impossible["expires"] = json!("2026-02-30");
    impossible["starts"] = json!("next Monday");
    let mut good = fact("Sam", "is busy", "until the move");
    good["durable"] = json!(false);
    good["expires"] = json!("2026-11-20");
    mount(&server, reply(facts(vec![vague, impossible, good]))).await;

    let found = extract(&server).await;

    let drafts = found.facts;
    assert_eq!(drafts.len(), 3);
    assert_eq!(drafts[0].expires, None);
    assert_eq!(drafts[1].expires, None);
    assert_eq!(drafts[1].starts, None);
    assert_eq!(drafts[2].expires.as_deref(), Some("2026-11-20"));
    assert!(drafts.iter().all(|d| !d.durable));
}

#[tokio::test]
async fn a_bad_due_becomes_none() {
    let server = MockServer::start().await;
    let due = |value: &str| {
        let mut c = commitment("Sam", "send the notes");
        c["due"] = json!(value);
        c
    };
    mount(
        &server,
        reply(commitments(vec![
            due("Thursday"),
            due("2026-02-30"),
            due("2026-11-03T25:00"),
            due("2026-11-03 20:00"),
            due("2026-11-05"),
        ])),
    )
    .await;

    let found = extract(&server).await;

    let dues: Vec<Option<&str>> = found.commitments.iter().map(|c| c.due.as_deref()).collect();
    assert_eq!(dues, [None, None, None, None, Some("2026-11-05")]);
    assert!(found.commitments.iter().all(|c| c.what == "send the notes"));

    let server = MockServer::start().await;
    mount(
        &server,
        reply(commitments(vec![due("2026-11-03T20:00"), due("")])),
    )
    .await;
    let found = extract(&server).await;
    assert_eq!(
        found.commitments[0].due.as_deref(),
        Some("2026-11-03T20:00")
    );
    assert_eq!(found.commitments[1].due, None);
}

#[tokio::test]
async fn a_bad_amount_drops_the_line() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(ledger(vec![
            ledger_line("Sam", json!(0), "nothing"),
            ledger_line("Sam", json!(-12.5), "a refund"),
            ledger_line("Sam", json!(0.0), "zero"),
            ledger_line("Sam", json!(38.5), "wine"),
        ])),
    )
    .await;

    let found = extract(&server).await;

    assert_eq!(found.ledger.len(), 1);
    assert_eq!(found.ledger[0].what, "wine");
    assert_eq!(found.ledger[0].amount, 38.5);
}

#[tokio::test]
async fn an_amount_that_is_not_a_number_is_malformed_output() {
    let server = MockServer::start().await;
    mount(
        &server,
        reply(ledger(vec![ledger_line(
            "Sam",
            json!("thirty-eight"),
            "wine",
        )])),
    )
    .await;

    let error = extractor(&server, Dialect::OpenAi)
        .extract(&group(), request())
        .await
        .unwrap_err();

    assert!(
        matches!(error, ModelError::MalformedOutput { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn currency_is_upper_cased_and_kept_only_as_three_letters() {
    let server = MockServer::start().await;
    let with = |code: Value| {
        let mut l = ledger_line("Sam", json!(38), "wine");
        l["currency"] = code;
        l
    };
    mount(
        &server,
        reply(ledger(vec![
            with(json!("eur")),
            with(json!(" Sgd ")),
            with(json!("€")),
            with(json!("dollars")),
            with(Value::Null),
        ])),
    )
    .await;

    let found = extract(&server).await;

    let codes: Vec<Option<&str>> = found.ledger.iter().map(|l| l.currency.as_deref()).collect();
    assert_eq!(codes, [Some("EUR"), Some("SGD"), None, None, None]);
    assert_eq!(found.ledger.len(), 5);
}

#[tokio::test]
async fn participants_are_trimmed_and_blank_names_dropped() {
    let server = MockServer::start().await;
    let mut line = ledger_line("Sam", json!(120), "tickets");
    line["participants"] = json!([" Lee ", "", "  ", "Priya"]);
    mount(&server, reply(ledger(vec![line]))).await;

    let found = extract(&server).await;

    assert_eq!(found.ledger[0].participants, ["Lee", "Priya"]);
}

#[tokio::test]
async fn a_server_error_surfaces_as_is() {
    let server = MockServer::start().await;
    mount(&server, ResponseTemplate::new(500).set_body_string("boom")).await;

    let error = extractor(&server, Dialect::OpenAi)
        .extract(&group(), request())
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
    mount(&server, reply(json!({"facts": "none"}))).await;

    let error = extractor(&server, Dialect::OpenAi)
        .extract(&group(), request())
        .await
        .unwrap_err();

    assert!(
        matches!(error, ModelError::MalformedOutput { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn no_extractor_finds_nothing_without_a_call() {
    let found = NoExtractor.extract(&group(), request()).await.unwrap();
    assert_eq!(found, Extraction::default());
    assert!(found.facts.is_empty());
    assert!(found.commitments.is_empty());
    assert!(found.ledger.is_empty());
    assert_eq!(NoExtractor.name(), "none");
}

#[tokio::test]
async fn the_name_comes_from_the_model() {
    let server = MockServer::start().await;
    assert_eq!(extractor(&server, Dialect::OpenAi).name(), "gemma-small");
}
