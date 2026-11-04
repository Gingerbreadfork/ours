//! The whole runtime, end to end, with the in-memory connector and a fake
//! model: a member addresses the agent, the agent acknowledges, asks the
//! model, replies, and the journal holds every step.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use ours::runtime::admin::AdminHub;
use ours::runtime::group::{Desired, GroupRunner, Wiring};
use ours::runtime::journal::Journal;
use ours::runtime::router::Router;
use ours_connector::{Connector, SimConnector};
use ours_model::{
    ChatRequest, ChatResponse, LargeModel, ModelError, NoExtractor, NoTimeResolver,
    RulesOnlyClassifier, Usage,
};
use ours_store::{EventLog, SqliteStore};
use ours_types::{
    GroupId, InboundEvent, Instant, Kind, Member, Message, OutboundAction, Platform, PlatformRole,
    Room, RoomKind,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

struct FakeModel;

#[async_trait]
impl LargeModel for FakeModel {
    async fn complete(
        &self,
        _: &GroupId,
        request: ChatRequest,
    ) -> Result<ChatResponse, ModelError> {
        assert!(
            request.system.contains("Q"),
            "persona name reaches the model"
        );
        Ok(ChatResponse {
            json: json!({ "message": "Hobson's, 14 March.", "actions": [] }),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cached_input_tokens: 0,
            },
            model: "fake".to_owned(),
            latency_ms: 1,
        })
    }

    fn name(&self) -> &str {
        "fake"
    }
}

fn member(id: &str) -> Member {
    Member {
        id: id.into(),
        display_name: id.to_owned(),
        platform_role: PlatformRole::Regular,
        is_bot: false,
        timezone: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn addressed_message_gets_an_ack_and_a_reply() {
    let store: Arc<dyn EventLog> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let group = GroupId::new("the-six");
    let (sim, handle) = SimConnector::new();
    let (out_tx, out_rx) = mpsc::channel(64);
    let (in_tx, mut in_rx) = mpsc::channel(64);
    tokio::spawn(Box::new(sim).run(in_tx, out_rx));

    let (queue_tx, queue_rx) = mpsc::channel(64);
    let mut router = Router::new();
    router.register(
        group.clone(),
        &[(Platform::Sim, "general".into())],
        queue_tx.clone(),
    );
    let router = Arc::new(router);
    let fanin = Arc::clone(&router);
    tokio::spawn(async move {
        while let Some(event) = in_rx.recv().await {
            fanin.dispatch(Platform::Sim, event).await;
        }
    });

    let hub = AdminHub::new();
    let (_wipe_tx, wipe_rx) = tokio::sync::mpsc::channel(1);
    let wiring = Wiring {
        journal: Journal::new(Arc::clone(&store)),
        inputs: queue_rx,
        wipes: wipe_rx,
        mail: tokio::sync::mpsc::channel(1).1,
        self_tx: queue_tx,
        outbound: HashMap::from([(Platform::Sim, out_tx)]),
        model: Arc::new(FakeModel),
        classifier: Arc::new(RulesOnlyClassifier),
        extractor: Arc::new(NoExtractor),
        resolver: Arc::new(NoTimeResolver),
        mailer: None,
        chat: ours::runtime::model::ChatSettings::default(),
        strict_schema: false,
        hub: hub.clone(),
        router: Arc::clone(&router),
        charter_path: std::env::temp_dir()
            .join(format!("ours-smoke-{}.charter.md", std::process::id())),
        decisions_path: std::env::temp_dir()
            .join(format!("ours-smoke-{}.decisions.md", std::process::id())),
    };
    let desired = Desired {
        name: "The Six".into(),
        kind: Kind::Friends,
        agent_name: Some("Q".into()),
        timezone: "Europe/London".into(),
        leads: vec![],
        hosts: vec![],
        rooms: vec![(Platform::Sim, "general".into())],
        model_label: "a fake model".into(),
        dial: None,
        voice: None,
        currency: "USD".into(),
        traits: vec![],
        bits: vec![],
        never_jokes_about: vec![],
        persona: None,
        legal_hold: false,
        feed_base: None,
    };
    let runner = GroupRunner::recover(group.clone(), desired, wiring)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let running = tokio::spawn(runner.run(shutdown_rx));

    let now = Instant::from(jiff::Timestamp::now());
    handle
        .push(InboundEvent::Ready {
            platform: Platform::Sim,
            me: "bot".into(),
            me_name: "Q".into(),
            capabilities: Default::default(),
        })
        .await
        .unwrap();
    handle
        .push(InboundEvent::RoomSync {
            room: Room {
                platform: Platform::Sim,
                id: "general".into(),
                name: Some("general".into()),
                kind: RoomKind::Channel,
                parent: None,
                container: None,
            },
            members: vec![
                member("sam"),
                member("alex"),
                Member {
                    is_bot: true,
                    ..member("bot")
                },
            ],
        })
        .await
        .unwrap();
    handle
        .push(InboundEvent::Message(Message {
            id: "m1".into(),
            room: "general".into(),
            author: "alex".into(),
            text: "Q, what was that bar in March?".into(),
            at: now,
            reply_to: None,
            mentions: vec![],
            mentions_me: false,
            attachments: vec![],
            edited: false,
        }))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut sent = Vec::new();
    while tokio::time::Instant::now() < deadline {
        sent.extend(handle.take_sent());
        if sent
            .iter()
            .any(|o| matches!(o.action, OutboundAction::Unreact { .. }))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let kinds: Vec<&str> = sent
        .iter()
        .map(|o| match &o.action {
            OutboundAction::React { emoji, .. } => emoji.as_str(),
            OutboundAction::SendMessage { .. } => "message",
            OutboundAction::Unreact { .. } => "unreact",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["👀", "message", "unreact"], "{sent:?}");
    let reply = sent.iter().find_map(|o| match &o.action {
        OutboundAction::SendMessage { text, reply_to, .. } => {
            Some((text.clone(), reply_to.clone()))
        }
        _ => None,
    });
    assert_eq!(
        reply,
        Some(("Hobson's, 14 March.".to_owned(), Some("m1".into())))
    );

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // Every input was journaled, the chain verifies, and a snapshot was taken on shutdown.
    let verified = store.verify_chain().unwrap();
    assert!(
        verified >= 6,
        "ready, sync, message, action results and the model reply: {verified}"
    );
    let snapshot = store
        .load_latest_snapshot()
        .unwrap()
        .expect("snapshot on shutdown");
    assert_eq!(snapshot.seq, verified);
}
