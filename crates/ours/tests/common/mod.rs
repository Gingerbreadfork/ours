//! One running group over the in-memory connector and a fake model, for
//! the end-to-end tests of the runtime.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use async_trait::async_trait;
use ours::runtime::admin::AdminHub;
use ours::runtime::group::{Desired, GroupError, GroupRunner, RunEnd, WipeRequest, Wiring};
use ours::runtime::journal::Journal;
use ours::runtime::router::Router;
use ours_connector::{Connector, SimConnector, SimHandle};
use ours_model::{
    ChatRequest, ChatResponse, LargeModel, ModelError, NoExtractor, NoTimeResolver,
    RulesOnlyClassifier, Usage,
};
use ours_store::EventLog;
use ours_types::{
    GroupId, InboundEvent, Instant, Kind, Member, Message, Platform, PlatformRole, Room, RoomKind,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// Answers every question with the same line.
pub struct FakeModel;

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

pub fn member(id: &str) -> Member {
    Member {
        id: id.into(),
        display_name: id.to_owned(),
        platform_role: PlatformRole::Regular,
        is_bot: false,
        timezone: None,
    }
}

pub fn message(id: &str, room: &str, author: &str, text: &str) -> InboundEvent {
    InboundEvent::Message(Message {
        id: id.into(),
        room: room.into(),
        author: author.into(),
        text: text.into(),
        at: Instant::from(jiff::Timestamp::now()),
        reply_to: None,
        mentions: vec![],
        mentions_me: false,
        attachments: vec![],
        edited: false,
    })
}

pub struct Running {
    pub group: GroupId,
    pub handle: SimHandle,
    pub hub: AdminHub,
    pub store: Arc<dyn EventLog>,
    pub shutdown: watch::Sender<bool>,
    pub wipes: mpsc::Sender<WipeRequest>,
    pub task: JoinHandle<Result<RunEnd, GroupError>>,
    pub charter_path: std::path::PathBuf,
}

/// Starts a friends group with one channel, "general", holding sam, alex
/// and the bot, and reports ready.
pub async fn start(store: Arc<dyn EventLog>) -> Running {
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
    let (wipe_tx, wipe_rx) = mpsc::channel(1);
    hub.set_controls(&group, queue_tx.clone(), wipe_tx.clone());
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        jiff::Timestamp::now().as_nanosecond()
    );
    let charter_path = std::env::temp_dir().join(format!("ours-test-{stamp}.charter.md"));
    let wiring = Wiring {
        journal: Journal::new(Arc::clone(&store)),
        inputs: queue_rx,
        wipes: wipe_rx,
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
        charter_path: charter_path.clone(),
        decisions_path: std::env::temp_dir().join(format!("ours-test-{stamp}.decisions.md")),
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
    let task = tokio::spawn(runner.run(shutdown_rx));
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
    Running {
        group,
        handle,
        hub,
        store,
        shutdown: shutdown_tx,
        wipes: wipe_tx,
        task,
        charter_path,
    }
}

impl Running {
    /// Waits until `check` passes or five seconds are up.
    pub async fn wait_for(&self, mut check: impl FnMut() -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if check() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        check()
    }

    pub async fn stop(self) -> RunEnd {
        let _ = self.shutdown.send(true);
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }
}
