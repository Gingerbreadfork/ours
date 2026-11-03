//! A connector is an adapter between one chat platform and the engine's
//! event model. It owns its own connection, emits `InboundEvent`s, carries
//! out `Outbound` actions and answers each with an `ActionResult`.

pub mod sim;

use async_trait::async_trait;
use ours_types::{Capabilities, InboundEvent, Outbound, Platform};
use tokio::sync::mpsc;

pub use sim::{SimConnector, SimHandle};

#[async_trait]
pub trait Connector: Send + 'static {
    fn platform(&self) -> Platform;

    fn capabilities(&self) -> Capabilities;

    /// Run until the outbound channel closes or the connection is lost.
    /// Inbound events go to `inbound`; actions arrive on `outbound`.
    async fn run(
        self: Box<Self>,
        inbound: mpsc::Sender<InboundEvent>,
        outbound: mpsc::Receiver<Outbound>,
    ) -> anyhow::Result<()>;
}
