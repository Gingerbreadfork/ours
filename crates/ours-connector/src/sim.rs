//! An in-memory connector. Tests and the replay harness push inbound events
//! through `SimHandle` and read back what the engine asked to send.

use crate::Connector;
use async_trait::async_trait;
use ours_types::{
    Capabilities, InboundEvent, Outbound, OutboundAction, Platform, PlatformMessageId,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Default)]
struct Shared {
    sent: Mutex<Vec<Outbound>>,
    next_message: AtomicU64,
}

/// The test's side of a `SimConnector`.
#[derive(Debug, Clone)]
pub struct SimHandle {
    inbound: mpsc::Sender<InboundEvent>,
    shared: Arc<Shared>,
}

impl SimHandle {
    /// Deliver an event as if the platform had produced it.
    pub async fn push(&self, event: InboundEvent) -> anyhow::Result<()> {
        self.inbound
            .send(event)
            .await
            .map_err(|_| anyhow::anyhow!("sim connector is gone"))
    }

    /// Everything the engine has asked to send so far, in order, draining it.
    pub fn take_sent(&self) -> Vec<Outbound> {
        let mut sent = self.shared.sent.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *sent)
    }
}

#[derive(Debug)]
pub struct SimConnector {
    capabilities: Capabilities,
    inbound_src: mpsc::Receiver<InboundEvent>,
    shared: Arc<Shared>,
}

impl SimConnector {
    pub fn new() -> (SimConnector, SimHandle) {
        Self::with_capabilities(Capabilities {
            reaction_set: None,
            reactions: true,
            threads: false,
            polls: false,
            dms: true,
            edit: true,
            delete: true,
            pin: true,
            history: false,
            timezones: false,
            external_flag: false,
            typing: false,
        })
    }

    pub fn with_capabilities(capabilities: Capabilities) -> (SimConnector, SimHandle) {
        let (tx, rx) = mpsc::channel(256);
        let shared = Arc::new(Shared::default());
        let handle = SimHandle {
            inbound: tx,
            shared: Arc::clone(&shared),
        };
        (
            SimConnector {
                capabilities,
                inbound_src: rx,
                shared,
            },
            handle,
        )
    }
}

#[async_trait]
impl Connector for SimConnector {
    fn platform(&self) -> Platform {
        Platform::Sim
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    async fn run(
        mut self: Box<Self>,
        inbound: mpsc::Sender<InboundEvent>,
        mut outbound: mpsc::Receiver<Outbound>,
    ) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                event = self.inbound_src.recv() => match event {
                    Some(event) => {
                        if inbound.send(event).await.is_err() {
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                },
                action = outbound.recv() => match action {
                    Some(action) => {
                        let produces_message = matches!(
                            action.action,
                            OutboundAction::SendMessage { .. }
                                | OutboundAction::SendDm { .. }
                                | OutboundAction::CreatePoll { .. }
                        );
                        let message = produces_message.then(|| {
                            let n = self.shared.next_message.fetch_add(1, Ordering::Relaxed);
                            PlatformMessageId::new(format!("sim-{n}"))
                        });
                        let result = InboundEvent::ActionResult {
                            id: action.id.clone(),
                            ok: true,
                            message,
                            error: None,
                        };
                        self.shared.sent.lock().unwrap_or_else(|e| e.into_inner()).push(action);
                        if inbound.send(result).await.is_err() {
                            return Ok(());
                        }
                    }
                    None => return Ok(()),
                },
            }
        }
    }
}
