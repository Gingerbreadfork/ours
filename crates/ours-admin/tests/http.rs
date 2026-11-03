#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, LOCATION,
    SET_COOKIE, WWW_AUTHENTICATE,
};
use axum::http::{Request, Response, StatusCode};
use futures::{Stream, StreamExt};
use ours_admin::{
    AdminConfig, AdminEvent, AdminServer, AdminSource, ComplianceReport, Export, FactCounts,
    GroupFacts, GroupSummary, HostFacts, ModelFacts, TelemetryFacts, router,
};
use ours_types::{GroupId, Instant};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tower::ServiceExt;

const TOKEN: &str = "test-token_123";
const CHARTER: &str = "# Book club\n\n- Keep it short.\n- Ask before posting.\n";
const DECISION_LOG: &str = "# Decisions\n\n## Where to eat\n\nOutcome: Acme\n";
const ICS_SECRET: &str = "cal-secret_42";
const PAGE_LINES: usize = 580;

fn ledger_fixture() -> Value {
    json!({
        "commitments": [
            { "who": "u1", "who_name": "Sam", "what": "bring dessert", "due": "2026-11-12T17:00:00Z", "status": "open", "event": "Dinner" },
            { "who": "u2", "what": "book the pitch", "due": null, "status": { "slipped": { "to": "2026-11-20" } } },
            { "who": "u3", "who_name": "Ana", "what": "send the draft", "due": "2026-11-01", "status": "done" },
        ],
        "lines": [
            { "payer": "u1", "payer_name": "Sam", "amount_cents": 3800, "currency": "EUR", "what": "wine", "split": "equal", "created_at": "2026-11-02T20:00:00Z", "voided": false },
            { "payer": "u2", "amount_cents": 1000, "what": "ice", "split": { "shares": { "u1": 1, "u2": 2 } }, "created_at": "2026-11-03T09:00:00Z", "voided": true },
        ],
        "balances": [
            { "user": "u1", "name": "Sam", "cents": 1900 },
            { "user": "u2", "name": "Ana", "cents": -1900 },
        ],
        "currency": "EUR",
    })
}

fn decisions_fixture() -> Value {
    json!({
        "open": [{
            "question": "Thursday or Friday?",
            "options": [
                { "label": "Thursday", "votes": ["u1", "u2"] },
                { "label": "Friday", "votes": 1 },
            ],
            "deadline": "2026-11-05T18:00:00Z",
            "rule": "majority_of_active",
            "opened_at": "2026-11-03T09:00:00Z",
            "room": "r1",
        }],
        "closed": [{
            "question": "Where to eat?",
            "outcome": "Acme",
            "rule": "lead_decides",
            "decider": "u1",
            "reason": null,
            "opened_at": "2026-11-01T09:00:00Z",
            "closed_at": "2026-11-02T09:00:00Z",
            "room": "r1",
            "options": [{ "label": "Acme", "votes": ["u1"] }, { "label": "Bao", "votes": [] }],
        }],
        "upcoming": [{ "when": "2026-11-12T19:00:00Z", "label": "Dinner", "attendees": ["u1", "u2", "u3"] }],
    })
}

fn actions_fixture() -> Vec<Value> {
    vec![
        json!({
            "id": "a3", "class": "email_venue", "tier": 1, "room": "r1",
            "requested_by": "u1", "requested_by_name": "Sam",
            "approved_by": "u2", "approved_by_name": "Ana",
            "at": "2026-11-03T10:00:00Z",
            "status": { "held": { "until": "2026-11-03T10:10:00Z" } },
            "summary": "Email Acme to book Friday",
            "undo": { "cancel_email": { "to": "bookings@acme.example" } },
            "outcome": null,
        }),
        json!({
            "id": "a2", "class": "poll", "tier": 0, "room": "r1",
            "requested_by": "u1", "at": "2026-11-03T09:00:00Z",
            "status": "done", "summary": "Opened a poll", "undo": "close the poll",
        }),
        json!({
            "id": "a1", "class": "payment", "tier": 2,
            "at": "2026-11-02T09:00:00Z",
            "status": { "failed": { "error": "card declined" } },
            "summary": "Pay the deposit", "undo": null, "outcome": { "code": 402 },
        }),
    ]
}

fn catalogue_fixture() -> Vec<Value> {
    vec![
        json!({ "class": "poll", "tier": 0, "how_undone": "closed and retracted at once" }),
        json!({ "class": "email_venue", "tier": 1, "how_undone": "held, then a cancellation email" }),
    ]
}

struct Fake {
    groups: Vec<GroupSummary>,
    events: Vec<AdminEvent>,
    charter: Option<String>,
    facts: Vec<Value>,
    decisions: Value,
    decision_log: Option<String>,
    ledger: Value,
    actions: Vec<Value>,
    tx: broadcast::Sender<AdminEvent>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn knows(&self, group: &GroupId) -> bool {
        self.groups.iter().any(|g| &g.id == group)
    }
}

#[async_trait]
impl AdminSource for Fake {
    async fn groups(&self) -> Vec<GroupSummary> {
        self.groups.clone()
    }

    async fn state(&self, group: &GroupId) -> Option<Value> {
        self.groups
            .iter()
            .find(|g| &g.id == group)
            .map(|g| json!({ "name": g.name, "charter": "be brief" }))
    }

    async fn charter(&self, group: &GroupId) -> Option<String> {
        self.groups
            .iter()
            .any(|g| &g.id == group)
            .then(|| self.charter.clone())
            .flatten()
    }

    async fn recent(&self, group: &GroupId, limit: usize) -> Vec<AdminEvent> {
        let mine: Vec<AdminEvent> = self
            .events
            .iter()
            .filter(|e| &e.group == group)
            .cloned()
            .collect();
        let skip = mine.len().saturating_sub(limit);
        mine.into_iter().skip(skip).collect()
    }

    async fn facts(&self, group: &GroupId) -> Vec<Value> {
        if self.groups.iter().any(|g| &g.id == group) {
            self.facts.clone()
        } else {
            Vec::new()
        }
    }

    async fn decisions(&self, group: &GroupId) -> Value {
        if self.groups.iter().any(|g| &g.id == group) {
            self.decisions.clone()
        } else {
            json!({ "open": [], "closed": [], "upcoming": [] })
        }
    }

    async fn decision_log(&self, group: &GroupId) -> Option<String> {
        self.groups
            .iter()
            .any(|g| &g.id == group)
            .then(|| self.decision_log.clone())
            .flatten()
    }

    async fn ledger(&self, group: &GroupId) -> Value {
        if self.knows(group) {
            self.ledger.clone()
        } else {
            json!({ "commitments": [], "lines": [], "balances": [], "currency": "" })
        }
    }

    async fn ics(&self, group: &GroupId, secret: &str, member: Option<&str>) -> Option<String> {
        if !self.knows(group) || secret != ICS_SECRET {
            return None;
        }
        let calendar = member.map_or_else(
            || "X-WR-CALNAME:Book club".to_owned(),
            |user| format!("X-WR-CALNAME:Book club for {user}"),
        );
        Some(format!(
            "BEGIN:VCALENDAR\r\n{calendar}\r\nEND:VCALENDAR\r\n"
        ))
    }

    async fn actions(&self, group: &GroupId) -> Vec<Value> {
        if self.knows(group) {
            self.actions.clone()
        } else {
            Vec::new()
        }
    }

    async fn catalogue(&self) -> Vec<Value> {
        catalogue_fixture()
    }

    fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        self.tx.subscribe()
    }

    async fn compliance(&self) -> ComplianceReport {
        ComplianceReport {
            host: HostFacts {
                version: "0.9.0".into(),
                data_dir: "/var/lib/ours".into(),
                admin_bind: "127.0.0.1:8787".into(),
                model: ModelFacts {
                    label: "Gemma 4".into(),
                    endpoint: "http://localhost:8080/v1".into(),
                    local: true,
                },
                mail: Some("club@example.org".into()),
                telemetry: TelemetryFacts {
                    enabled: false,
                    endpoint: None,
                    sample: SAMPLE.into(),
                },
                connectors: vec!["discord".into()],
            },
            groups: vec![GroupFacts {
                id: GroupId::new("g1"),
                name: "Book <club>".into(),
                kind: "team".into(),
                file: "/var/lib/ours/g1.db".into(),
                file_bytes: 4096,
                retention_days: 365,
                legal_hold: true,
                members: 5,
                leads: 1,
                facts: FactCounts {
                    group: 3,
                    room: 2,
                    private: 1,
                    self_stated: 4,
                },
                decisions: 2,
                commitments: 3,
                ledger_lines: 6,
                actions: 7,
                journal_records: 800,
                redacted_records: 9,
                snapshots: 2,
            }],
        }
    }

    async fn export(&self, group: &GroupId) -> Option<Export> {
        self.knows(group).then(|| Export {
            json: "{\"group\":\"g1\"}".into(),
            markdown: "# Book club\n".into(),
        })
    }

    async fn set_legal_hold(&self, group: &GroupId, on: bool) -> bool {
        if !self.knows(group) {
            return false;
        }
        self.calls
            .lock()
            .unwrap()
            .push(format!("hold {} {on}", group.as_str()));
        true
    }

    async fn wipe(&self, group: &GroupId) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wipe {}", group.as_str()));
        Ok(())
    }

    async fn persona(&self, group: &GroupId) -> Option<Value> {
        self.knows(group).then(|| json!({ "name": "Q", "dial": 0 }))
    }

    async fn profile(&self, group: &GroupId) -> Option<Value> {
        self.knows(group).then(|| json!({ "retention_days": 365 }))
    }
}

const SAMPLE: &str = "{\"v\":1,\"groups\":2,\"decisions_closed\":5}";

fn fact(id: &str, retracted: Value) -> Value {
    json!({
        "id": id,
        "subject": "Sam",
        "subject_kind": "member",
        "predicate": "drives",
        "object": "the van",
        "scope": "group",
        "rooms": ["r1"],
        "sources": [{ "room": "r1", "message": "m1", "speaker": "u1", "at": "2026-11-01T10:00:00Z" }],
        "confidence": 0.9,
        "first_seen": "2026-11-01T10:00:00Z",
        "last_confirmed": "2026-11-01T10:00:00Z",
        "expires": null,
        "retracted": retracted,
        "quote": null,
    })
}

fn event(group: &str, seconds: i64, line: &str) -> AdminEvent {
    AdminEvent {
        group: GroupId::new(group),
        at: Instant::UNIX_EPOCH.plus_seconds(seconds),
        kind: "triage".into(),
        line: line.into(),
        detail: None,
    }
}

fn fake() -> (Arc<Fake>, broadcast::Sender<AdminEvent>) {
    fake_with_charter(Some(CHARTER.into()))
}

fn fake_with_charter(charter: Option<String>) -> (Arc<Fake>, broadcast::Sender<AdminEvent>) {
    fake_with(charter, Some(DECISION_LOG.into()))
}

fn fake_with(
    charter: Option<String>,
    decision_log: Option<String>,
) -> (Arc<Fake>, broadcast::Sender<AdminEvent>) {
    let (tx, _) = broadcast::channel(16);
    let groups = vec![GroupSummary {
        id: GroupId::new("g1"),
        name: "Book club".into(),
        kind: "friends".into(),
        platforms: vec!["discord".into()],
        rooms: 2,
        members: 5,
    }];
    let events = vec![
        event("g1", 1, "first line"),
        event("g1", 2, "replayed line"),
        event("g2", 3, "someone else's line"),
    ];
    let source = Arc::new(Fake {
        groups,
        events,
        charter,
        facts: vec![
            fact("f1", Value::Null),
            fact(
                "f2",
                json!({ "at": "2026-11-02T09:00:00Z", "reason": "that's wrong" }),
            ),
            json!({ "id": "f3", "subject": "Ana" }),
        ],
        decisions: decisions_fixture(),
        decision_log,
        ledger: ledger_fixture(),
        actions: actions_fixture(),
        tx: tx.clone(),
        calls: Mutex::new(Vec::new()),
    });
    (source, tx)
}

fn app() -> (Router, broadcast::Sender<AdminEvent>) {
    let (source, tx) = fake();
    (router(source, TOKEN), tx)
}

async fn get(app: &Router, uri: &str, headers: &[(&str, String)]) -> Result<Response<Body>> {
    let mut request = Request::builder().uri(uri);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    Ok(app.clone().oneshot(request.body(Body::empty())?).await?)
}

fn bearer() -> [(&'static str, String); 1] {
    [(AUTHORIZATION.as_str(), format!("Bearer {TOKEN}"))]
}

async fn body_json(response: Response<Body>) -> Result<Value> {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn body_text(response: Response<Body>) -> Result<String> {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok(String::from_utf8(bytes.to_vec())?)
}

#[tokio::test]
async fn healthz_needs_no_token() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/healthz", &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_text(response).await?, "ok");
    Ok(())
}

#[tokio::test]
async fn page_is_refused_without_a_token() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/", &[]).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(response).await?["error"], "unauthorized");
    Ok(())
}

#[tokio::test]
async fn page_is_served_with_the_bearer() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response.headers()[CONTENT_TYPE].to_str()?.to_owned();
    assert!(content_type.starts_with("text/html"));
    assert!(body_text(response).await?.contains("Ours"));
    Ok(())
}

#[tokio::test]
async fn page_stays_small_self_contained_and_uses_the_charter_endpoint() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/", &bearer()).await?;
    let csp = response.headers()[CONTENT_SECURITY_POLICY]
        .to_str()?
        .to_owned();
    assert!(csp.contains("default-src 'none'"), "got {csp:?}");
    assert!(csp.contains("connect-src 'self'"), "got {csp:?}");
    let page = body_text(response).await?;
    assert!(page.lines().count() < PAGE_LINES);
    assert!(!page.contains("http://") && !page.contains("https://"));
    assert!(!page.contains("<script src") && !page.contains("<link rel=\"stylesheet\""));
    assert!(
        page.contains("/api/groups/"),
        "page does not call the group API"
    );
    assert!(
        page.contains("/charter"),
        "page does not call the charter endpoint"
    );
    assert!(page.contains("Charter") && page.contains("localStorage"));
    Ok(())
}

#[tokio::test]
async fn page_has_a_memory_tab_that_reads_the_facts_endpoint() -> Result<()> {
    let (app, _tx) = app();
    let page = body_text(get(&app, "/", &bearer()).await?).await?;
    assert!(page.contains(">Memory</button>"), "no Memory tab");
    assert!(page.contains("/facts?include_retracted=true"));
    assert!(page.contains("show retracted") && page.contains("Filter facts"));
    assert!(page.contains("noopener"));
    assert!(
        page.lines().count() < PAGE_LINES,
        "{} lines",
        page.lines().count()
    );
    Ok(())
}

#[tokio::test]
async fn page_has_a_decisions_tab_that_reads_the_decisions_endpoints() -> Result<()> {
    let (app, _tx) = app();
    let page = body_text(get(&app, "/", &bearer()).await?).await?;
    assert!(page.contains(">Decisions</button>"), "no Decisions tab");
    assert!(
        page.contains("/decisions\""),
        "page does not fetch /decisions"
    );
    assert!(page.contains("/decisions.md"), "page does not link the log");
    for section in ["<h2>Open</h2>", "<h2>Upcoming</h2>", "<h2>Closed</h2>"] {
        assert!(page.contains(section), "no {section}");
    }
    assert!(page.contains("opened a poll") && page.contains("decided|closed"));
    assert!(
        page.lines().count() < PAGE_LINES,
        "{} lines",
        page.lines().count()
    );
    assert!(!page.contains("http://") && !page.contains("https://"));
    Ok(())
}

#[tokio::test]
async fn a_wrong_token_is_refused_in_every_form() -> Result<()> {
    let (app, _tx) = app();
    let wrong_bearer = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let wrong_cookie = [(COOKIE.as_str(), "ours_admin=nope".to_owned())];
    for response in [
        get(&app, "/api/groups", &wrong_bearer).await?,
        get(&app, "/api/groups", &wrong_cookie).await?,
        get(&app, "/?token=nope", &[]).await?,
        get(&app, "/?token=", &[]).await?,
    ] {
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

#[tokio::test]
async fn query_token_sets_the_cookie_and_redirects() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, &format!("/?token={TOKEN}"), &[]).await?;
    assert!(response.status().is_redirection());
    assert_eq!(response.headers()[LOCATION], "/");
    let cookie = response.headers()[SET_COOKIE].to_str()?.to_owned();
    assert!(cookie.starts_with(&format!("ours_admin={TOKEN};")));
    assert!(cookie.contains("HttpOnly"));

    let pair = cookie.split(';').next().context("empty cookie")?.to_owned();
    let response = get(&app, "/", &[(COOKIE.as_str(), pair)]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn redirect_drops_the_token_but_keeps_the_rest() -> Result<()> {
    let (app, _tx) = app();
    let uri = format!("/api/groups/g1/events?token={TOKEN}&limit=1");
    let response = get(&app, &uri, &[]).await?;
    assert!(response.status().is_redirection());
    assert_eq!(
        response.headers()[LOCATION],
        "/api/groups/g1/events?limit=1"
    );
    Ok(())
}

#[tokio::test]
async fn groups_come_back_as_json() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await?,
        json!([{
            "id": "g1",
            "name": "Book club",
            "kind": "friends",
            "platforms": ["discord"],
            "rooms": 2,
            "members": 5,
        }])
    );
    Ok(())
}

#[tokio::test]
async fn state_is_json_for_a_known_group_and_404_for_an_unknown_one() -> Result<()> {
    let (app, _tx) = app();
    let known = get(&app, "/api/groups/g1/state", &bearer()).await?;
    assert_eq!(known.status(), StatusCode::OK);
    assert_eq!(body_json(known).await?["charter"], "be brief");

    let unknown = get(&app, "/api/groups/nope/state", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");
    Ok(())
}

#[tokio::test]
async fn charter_is_markdown_for_a_known_group() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/charter", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(body_text(response).await?, CHARTER);
    Ok(())
}

#[tokio::test]
async fn charter_is_404_for_an_unknown_group_and_for_a_group_without_one() -> Result<()> {
    let (app, _tx) = app();
    let unknown = get(&app, "/api/groups/nope/charter", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");

    let (source, _tx) = fake_with_charter(None);
    let app = router(source, TOKEN);
    let empty = get(&app, "/api/groups/g1/charter", &bearer()).await?;
    assert_eq!(empty.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(empty).await?["error"], "no charter yet");
    Ok(())
}

#[tokio::test]
async fn charter_needs_the_token() -> Result<()> {
    let (app, _tx) = app();
    let denied = get(&app, "/api/groups/g1/charter", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let denied = get(&app, "/api/groups/g1/charter", &wrong).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

fn ids(facts: &Value) -> Result<Vec<&str>> {
    Ok(facts
        .as_array()
        .context("not an array")?
        .iter()
        .filter_map(|f| f["id"].as_str())
        .collect())
}

#[tokio::test]
async fn facts_hide_retracted_ones_by_default() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/facts", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let facts = body_json(response).await?;
    assert_eq!(ids(&facts)?, ["f1", "f3"]);
    assert_eq!(facts[0]["object"], "the van");
    assert_eq!(facts[0]["confidence"], 0.9);

    let explicit = get(
        &app,
        "/api/groups/g1/facts?include_retracted=false",
        &bearer(),
    )
    .await?;
    assert_eq!(ids(&body_json(explicit).await?)?, ["f1", "f3"]);
    Ok(())
}

#[tokio::test]
async fn facts_include_retracted_ones_when_asked() -> Result<()> {
    let (app, _tx) = app();
    let response = get(
        &app,
        "/api/groups/g1/facts?include_retracted=true",
        &bearer(),
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let facts = body_json(response).await?;
    assert_eq!(ids(&facts)?, ["f1", "f2", "f3"]);
    assert_eq!(facts[1]["retracted"]["reason"], "that's wrong");
    Ok(())
}

#[tokio::test]
async fn facts_are_404_for_an_unknown_group_and_need_the_token() -> Result<()> {
    let (app, _tx) = app();
    let unknown = get(&app, "/api/groups/nope/facts", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");

    let denied = get(&app, "/api/groups/g1/facts", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let denied = get(&app, "/api/groups/g1/facts?include_retracted=true", &wrong).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn decisions_are_the_sources_object_for_a_known_group() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/decisions", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let decisions = body_json(response).await?;
    assert_eq!(decisions, decisions_fixture());
    assert_eq!(decisions["open"][0]["question"], "Thursday or Friday?");
    assert_eq!(decisions["closed"][0]["outcome"], "Acme");
    assert_eq!(decisions["upcoming"][0]["label"], "Dinner");
    Ok(())
}

#[tokio::test]
async fn decisions_are_404_for_an_unknown_group_and_need_the_token() -> Result<()> {
    let (app, _tx) = app();
    let unknown = get(&app, "/api/groups/nope/decisions", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");

    let denied = get(&app, "/api/groups/g1/decisions", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let denied = get(&app, "/api/groups/g1/decisions", &wrong).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn the_decision_log_is_markdown_for_a_known_group() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/decisions.md", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(body_text(response).await?, DECISION_LOG);
    Ok(())
}

#[tokio::test]
async fn the_decision_log_is_404_without_a_log_or_a_group_and_needs_the_token() -> Result<()> {
    let (app, _tx) = app();
    let unknown = get(&app, "/api/groups/nope/decisions.md", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");

    let denied = get(&app, "/api/groups/g1/decisions.md", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let (source, _tx) = fake_with(Some(CHARTER.into()), None);
    let app = router(source, TOKEN);
    let empty = get(&app, "/api/groups/g1/decisions.md", &bearer()).await?;
    assert_eq!(empty.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(empty).await?["error"], "no decision log yet");
    Ok(())
}

#[tokio::test]
async fn events_are_recent_oldest_first_and_limited() -> Result<()> {
    let (app, _tx) = app();
    let all = body_json(get(&app, "/api/groups/g1/events", &bearer()).await?).await?;
    let lines: Vec<&str> = all
        .as_array()
        .context("not an array")?
        .iter()
        .filter_map(|e| e["line"].as_str())
        .collect();
    assert_eq!(lines, ["first line", "replayed line"]);

    let one = body_json(get(&app, "/api/groups/g1/events?limit=1", &bearer()).await?).await?;
    assert_eq!(one[0]["line"], "replayed line");
    assert_eq!(one.as_array().map(Vec::len), Some(1));

    let unknown = get(&app, "/api/groups/nope/events", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn page_has_a_tab_tab_that_reads_the_ledger_endpoint() -> Result<()> {
    let (app, _tx) = app();
    let page = body_text(get(&app, "/", &bearer()).await?).await?;
    assert!(page.contains(">Tab</button>"), "no Tab tab");
    assert!(page.contains("/ledger\""), "page does not fetch /ledger");
    for section in ["<h2>Promises</h2>", "<h2>Lines</h2>", "<h2>Balances</h2>"] {
        assert!(page.contains(section), "no {section}");
    }
    assert!(page.contains("Earlier ("), "no collapsed earlier promises");
    assert!(page.contains("remembered|Added|settled|due|moved"));
    for key in [
        "amount_cents",
        "who_name",
        "payer_name",
        "voided",
        "created_at",
    ] {
        assert!(page.contains(key), "page does not read {key}");
    }
    Ok(())
}

#[tokio::test]
async fn the_charter_tab_shows_the_calendar_feed_pattern() -> Result<()> {
    let (app, _tx) = app();
    let page = body_text(get(&app, "/", &bearer()).await?).await?;
    assert!(page.contains("\"/ics/\" + encodeURIComponent(group.id)"));
    assert!(page.contains("secret.ics") && page.contains("secret/member.ics"));
    assert!(page.contains("The agent gives members their links."));
    assert!(
        !page.contains(ICS_SECRET),
        "the page must not know a secret"
    );
    Ok(())
}

#[tokio::test]
async fn the_page_stays_inside_its_line_budget_and_self_contained() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/", &bearer()).await?;
    let csp = response.headers()[CONTENT_SECURITY_POLICY]
        .to_str()?
        .to_owned();
    assert_eq!(
        csp,
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
         connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'none'"
    );
    let page = body_text(response).await?;
    assert!(
        page.lines().count() < PAGE_LINES,
        "{} lines",
        page.lines().count()
    );
    assert!(!page.contains("http://") && !page.contains("https://"));
    assert!(!page.contains("<script src") && !page.contains("<link rel=\"stylesheet\""));
    Ok(())
}

#[tokio::test]
async fn the_ledger_is_the_sources_object_for_a_known_group() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/ledger", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[CONTENT_TYPE]
            .to_str()?
            .starts_with("application/json")
    );
    let ledger = body_json(response).await?;
    assert_eq!(ledger, ledger_fixture());
    assert_eq!(ledger["commitments"][0]["what"], "bring dessert");
    assert_eq!(ledger["lines"][0]["amount_cents"], 3800);
    assert_eq!(ledger["balances"][1]["cents"], -1900);
    Ok(())
}

#[tokio::test]
async fn the_ledger_is_404_for_an_unknown_group_and_needs_the_token() -> Result<()> {
    let (app, _tx) = app();
    let unknown = get(&app, "/api/groups/nope/ledger", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");

    let denied = get(&app, "/api/groups/g1/ledger", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let denied = get(&app, "/api/groups/g1/ledger", &wrong).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn page_has_an_actions_tab_that_reads_the_actions_and_catalogue_endpoints() -> Result<()> {
    let (app, _tx) = app();
    let page = body_text(get(&app, "/", &bearer()).await?).await?;
    assert!(page.contains(">Actions</button>"), "no Actions tab");
    assert!(page.contains("/actions\""), "page does not fetch /actions");
    assert!(
        page.contains("/api/catalogue"),
        "page does not fetch the catalogue"
    );
    assert!(page.contains("hide tier 0") && page.contains("checked"));
    assert!(page.contains("What can be undone") && page.contains("<table>"));
    assert!(page.contains("new Set([\"send\", \"notice\", \"outside\"])"));
    for key in [
        "requested_by_name",
        "approved_by_name",
        "undo",
        "outcome",
        "until",
    ] {
        assert!(page.contains(key), "page does not read {key}");
    }
    for word in ["held until", "sent", "undone", "failed"] {
        assert!(page.contains(word), "page does not show {word}");
    }
    assert!(
        page.lines().count() < PAGE_LINES,
        "{} lines",
        page.lines().count()
    );
    assert!(!page.contains("http://") && !page.contains("https://"));
    Ok(())
}

#[tokio::test]
async fn actions_are_the_sources_records_for_a_known_group() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/actions", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[CONTENT_TYPE]
            .to_str()?
            .starts_with("application/json")
    );
    let actions = body_json(response).await?;
    assert_eq!(actions, Value::Array(actions_fixture()));
    assert_eq!(
        actions[0]["status"]["held"]["until"],
        "2026-11-03T10:10:00Z"
    );
    assert_eq!(actions[2]["outcome"]["code"], 402);
    Ok(())
}

#[tokio::test]
async fn actions_are_404_for_an_unknown_group_and_need_the_token() -> Result<()> {
    let (app, _tx) = app();
    let unknown = get(&app, "/api/groups/nope/actions", &bearer()).await?;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await?["error"], "unknown group");

    let denied = get(&app, "/api/groups/g1/actions", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let denied = get(&app, "/api/groups/g1/actions", &wrong).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn the_catalogue_is_the_sources_list_and_needs_the_token() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/catalogue", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[CONTENT_TYPE]
            .to_str()?
            .starts_with("application/json")
    );
    let catalogue = body_json(response).await?;
    assert_eq!(catalogue, Value::Array(catalogue_fixture()));
    assert_eq!(catalogue[1]["tier"], 1);

    let denied = get(&app, "/api/catalogue", &[]).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let wrong = [(AUTHORIZATION.as_str(), "Bearer nope".to_owned())];
    let denied = get(&app, "/api/catalogue", &wrong).await?;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn the_group_calendar_is_served_to_the_secret_without_a_token() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, &format!("/ics/g1/{ICS_SECRET}.ics"), &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let body = body_text(response).await?;
    assert!(body.starts_with("BEGIN:VCALENDAR\r\n"), "got {body:?}");
    assert!(body.contains("X-WR-CALNAME:Book club\r\n"), "got {body:?}");
    Ok(())
}

#[tokio::test]
async fn a_wrong_secret_or_group_is_404_and_looks_the_same() -> Result<()> {
    let (app, _tx) = app();
    let mut bodies = Vec::new();
    for uri in [
        "/ics/g1/wrong.ics".to_owned(),
        "/ics/g1/wrong/u1.ics".to_owned(),
        format!("/ics/nope/{ICS_SECRET}.ics"),
        format!("/ics/nope/{ICS_SECRET}/u1.ics"),
        format!("/ics/g1/{ICS_SECRET}"),
        format!("/ics/g1/{ICS_SECRET}/u1"),
        "/ics/g1/.ics".to_owned(),
        format!("/ics/g1/{ICS_SECRET}/.ics"),
        "/ics/g1".to_owned(),
    ] {
        for headers in [Vec::new(), bearer().to_vec()] {
            let response = get(&app, &uri, &headers).await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(response.headers().get(WWW_AUTHENTICATE).is_none(), "{uri}");
            bodies.push(body_text(response).await?);
        }
    }
    assert!(bodies.iter().all(|body| body == &bodies[0]), "{bodies:?}");
    Ok(())
}

#[tokio::test]
async fn the_member_calendar_filters_through_to_the_source() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, &format!("/ics/g1/{ICS_SECRET}/u2.ics"), &[]).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/calendar; charset=utf-8"
    );
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let body = body_text(response).await?;
    assert!(
        body.contains("X-WR-CALNAME:Book club for u2\r\n"),
        "got {body:?}"
    );

    let encoded = get(&app, &format!("/ics/g1/{ICS_SECRET}/u%201.ics"), &[]).await?;
    assert!(body_text(encoded).await?.contains("Book club for u 1"));
    Ok(())
}

#[tokio::test]
async fn a_real_server_serves_the_calendar_without_a_token() -> Result<()> {
    let (source, _tx) = fake();
    let server = AdminServer::new(AdminConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        token: Some(TOKEN.into()),
    })?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(server.serve_listener(listener, source));

    let feed = raw_get(addr, &format!("/ics/g1/{ICS_SECRET}.ics"), "").await?;
    assert!(feed.starts_with("HTTP/1.1 200"), "got {feed:?}");
    assert!(feed.contains("BEGIN:VCALENDAR"));
    let probe = raw_get(addr, "/ics/g1/wrong.ics", "").await?;
    assert!(probe.starts_with("HTTP/1.1 404"), "got {probe:?}");

    task.abort();
    Ok(())
}

#[tokio::test]
async fn unknown_paths_are_404_json() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/nothing/here", &[]).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(response).await?["error"], "not found");
    Ok(())
}

type Chunks = Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>;

async fn next_chunk(chunks: &mut Chunks) -> Result<String> {
    let chunk = tokio::time::timeout(Duration::from_secs(2), chunks.next())
        .await
        .context("no chunk within 2s")?
        .ok_or_else(|| anyhow!("stream ended"))??;
    Ok(String::from_utf8(chunk.to_vec())?)
}

#[tokio::test]
async fn stream_refuses_unknown_groups_and_missing_tokens() -> Result<()> {
    let (app, _tx) = app();
    assert_eq!(
        get(&app, "/api/groups/nope/stream", &bearer())
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&app, "/api/groups/g1/stream", &[]).await?.status(),
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

#[tokio::test]
async fn stream_replays_then_forwards_live_events_for_its_group_only() -> Result<()> {
    let (app, tx) = app();
    let response = get(&app, "/api/groups/g1/stream", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "text/event-stream");
    let mut chunks: Chunks = Box::pin(response.into_body().into_data_stream());

    let first = next_chunk(&mut chunks).await?;
    assert!(first.contains("first line"), "got {first:?}");
    let second = next_chunk(&mut chunks).await?;
    assert!(second.contains("replayed line"), "got {second:?}");
    assert!(second.starts_with("data:"));

    tx.send(event("g2", 10, "other group"))?;
    tx.send(event("g1", 11, "live line"))?;
    let live = next_chunk(&mut chunks).await?;
    assert!(live.contains("live line"), "got {live:?}");
    assert!(!live.contains("other group"));
    Ok(())
}

#[tokio::test]
async fn stream_does_not_repeat_an_event_that_is_also_in_the_backlog() -> Result<()> {
    let (app, tx) = app();
    let response = get(&app, "/api/groups/g1/stream?limit=1", &bearer()).await?;
    let mut chunks: Chunks = Box::pin(response.into_body().into_data_stream());
    assert!(next_chunk(&mut chunks).await?.contains("replayed line"));

    tx.send(event("g1", 2, "replayed line"))?;
    tx.send(event("g1", 12, "fresh line"))?;
    let next = next_chunk(&mut chunks).await?;
    assert!(next.contains("fresh line"), "got {next:?}");
    Ok(())
}

#[tokio::test]
async fn stream_reports_how_many_events_a_slow_reader_missed() -> Result<()> {
    let (tx, _) = broadcast::channel(2);
    let source = Arc::new(Fake {
        groups: fake().0.groups.clone(),
        events: Vec::new(),
        charter: None,
        facts: Vec::new(),
        decisions: json!({}),
        decision_log: None,
        ledger: json!({}),
        actions: Vec::new(),
        tx: tx.clone(),
        calls: Mutex::new(Vec::new()),
    });
    let app = router(source, TOKEN);
    let response = get(&app, "/api/groups/g1/stream", &bearer()).await?;
    let mut chunks: Chunks = Box::pin(response.into_body().into_data_stream());

    for n in 0..5 {
        tx.send(event("g1", n, &format!("line {n}")))?;
    }
    let first = next_chunk(&mut chunks).await?;
    assert!(first.starts_with("event: lagged"), "got {first:?}");
    assert!(first.contains("data: 3"), "got {first:?}");
    Ok(())
}

#[tokio::test]
async fn a_real_server_answers_healthz_and_guards_the_rest() -> Result<()> {
    let (source, _tx) = fake();
    let server = AdminServer::new(AdminConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
        token: Some(TOKEN.into()),
    })?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(server.serve_listener(listener, source));

    let health = raw_get(addr, "/healthz", "").await?;
    assert!(health.starts_with("HTTP/1.1 200"), "got {health:?}");
    let denied = raw_get(addr, "/api/groups", "").await?;
    assert!(denied.starts_with("HTTP/1.1 401"), "got {denied:?}");
    let allowed = raw_get(
        addr,
        "/api/groups",
        &format!("Authorization: Bearer {TOKEN}\r\n"),
    )
    .await?;
    assert!(allowed.starts_with("HTTP/1.1 200"), "got {allowed:?}");
    assert!(allowed.contains("Book club"));

    task.abort();
    Ok(())
}

async fn raw_get(addr: SocketAddr, path: &str, extra_headers: &str) -> Result<String> {
    let mut socket = TcpStream::connect(addr).await?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: test\r\n{extra_headers}Connection: close\r\n\r\n");
    socket.write_all(request.as_bytes()).await?;
    let mut text = String::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_string(&mut text)).await??;
    Ok(text)
}

async fn post(app: &Router, uri: &str, body: Value) -> Result<Response<Body>> {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))?;
    Ok(app.clone().oneshot(request).await?)
}

fn app_with_source() -> (Router, Arc<Fake>) {
    let (source, _tx) = fake();
    (router(source.clone(), TOKEN), source)
}

#[tokio::test]
async fn compliance_page_shows_sample_retention_and_hold() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/compliance", &[]).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = get(&app, "/compliance", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await?;
    assert!(html.contains(&SAMPLE.replace('"', "&quot;")));
    assert!(html.contains("365 days"));
    assert!(html.contains("<td>on</td>"));
    assert!(html.contains("Book &lt;club&gt;"));
    assert!(html.contains("turn off expiry"));
    assert!(html.contains("/api/groups/g1/export.json"));
    assert!(html.contains("forget me"));
    let order = [
        "What is kept",
        "Where it lives",
        "Who sees what",
        "Member rights",
        "Retention",
        "What leaves this machine",
        "Audit",
        "Out of scope",
    ];
    let mut at = 0;
    for title in order {
        let found = html[at..].find(&format!("<h2>{title}</h2>"));
        assert!(found.is_some(), "missing or out of order: {title}");
        at += found.unwrap_or(0);
    }
    Ok(())
}

#[tokio::test]
async fn exports_download_with_filenames() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/export.json", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"ours-g1-export.json\""
    );
    assert_eq!(body_text(response).await?, "{\"group\":\"g1\"}");
    let response = get(&app, "/api/groups/g1/export.md", &bearer()).await?;
    assert_eq!(
        response.headers()[CONTENT_TYPE],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"ours-g1-export.md\""
    );
    let response = get(&app, "/api/groups/nope/export.md", &bearer()).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn wipe_needs_the_group_id() -> Result<()> {
    let (app, source) = app_with_source();
    let response = post(&app, "/api/groups/g1/wipe", json!({ "confirm": "wrong" })).await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(source.calls.lock().unwrap().is_empty());
    let response = post(&app, "/api/groups/nope/wipe", json!({ "confirm": "nope" })).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = post(&app, "/api/groups/g1/wipe", json!({ "confirm": "g1" })).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(*source.calls.lock().unwrap(), vec!["wipe g1".to_owned()]);
    Ok(())
}

#[tokio::test]
async fn legal_hold_posts_call_through() -> Result<()> {
    let (app, source) = app_with_source();
    let response = post(&app, "/api/groups/g1/legal-hold", json!({ "on": true })).await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await?, json!({ "legal_hold": true }));
    post(&app, "/api/groups/g1/legal-hold", json!({ "on": false })).await?;
    assert_eq!(
        *source.calls.lock().unwrap(),
        vec!["hold g1 true".to_owned(), "hold g1 false".to_owned()]
    );
    let response = post(&app, "/api/groups/nope/legal-hold", json!({ "on": true })).await?;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn new_routes_need_the_token() -> Result<()> {
    let (app, _tx) = app();
    let request = Request::builder()
        .method("POST")
        .uri("/api/groups/g1/wipe")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "confirm": "g1" }).to_string()))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn persona_and_profile_read_and_404() -> Result<()> {
    let (app, _tx) = app();
    let response = get(&app, "/api/groups/g1/persona", &bearer()).await?;
    assert_eq!(
        body_json(response).await?,
        json!({ "name": "Q", "dial": 0 })
    );
    let response = get(&app, "/api/groups/g1/profile", &bearer()).await?;
    assert_eq!(body_json(response).await?, json!({ "retention_days": 365 }));
    for path in ["persona", "profile"] {
        let response = get(&app, &format!("/api/groups/nope/{path}"), &bearer()).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    Ok(())
}
