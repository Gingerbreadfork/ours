//! What the admin server reads. The binary implements `AdminSource` over
//! the engine, so this crate never sees the engine's types.

use async_trait::async_trait;
use ours_types::{GroupId, Instant};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// One row in the group list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSummary {
    pub id: GroupId,
    pub name: String,
    pub kind: String,
    pub platforms: Vec<String>,
    pub rooms: usize,
    pub members: usize,
}

/// One line of the live feed: something the agent saw or did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminEvent {
    pub group: GroupId,
    pub at: Instant,
    pub kind: String,
    pub line: String,
    pub detail: Option<serde_json::Value>,
}

/// A group's export: the same content as JSON and as Markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    pub json: String,
    pub markdown: String,
}

/// Where the large model runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFacts {
    pub label: String,
    pub endpoint: String,
    pub local: bool,
}

/// What telemetry is set to. `sample` is the JSON one record has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryFacts {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub sample: String,
}

/// Facts about the install as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    pub version: String,
    pub data_dir: String,
    pub admin_bind: String,
    pub model: ModelFacts,
    /// The group address mail is sent from, when mail is configured.
    pub mail: Option<String>,
    pub telemetry: TelemetryFacts,
    pub connectors: Vec<String>,
}

/// Fact counts by scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactCounts {
    pub group: u32,
    pub room: u32,
    pub private: u32,
    pub self_stated: u32,
}

/// What one group keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupFacts {
    pub id: GroupId,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub file_bytes: u64,
    pub retention_days: u32,
    pub legal_hold: bool,
    pub members: u32,
    pub leads: u32,
    pub facts: FactCounts,
    pub decisions: u32,
    pub commitments: u32,
    pub ledger_lines: u32,
    pub actions: u32,
    pub journal_records: u64,
    pub redacted_records: u64,
    pub snapshots: u32,
}

/// Everything the compliance page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComplianceReport {
    pub host: HostFacts,
    pub groups: Vec<GroupFacts>,
}

#[async_trait]
pub trait AdminSource: Send + Sync + 'static {
    /// Every group the host runs.
    async fn groups(&self) -> Vec<GroupSummary>;

    /// The group's current state as JSON, or `None` for an unknown group.
    async fn state(&self, group: &GroupId) -> Option<serde_json::Value>;

    /// The group's charter as Markdown, or `None` if the group is unknown
    /// or has no charter yet.
    async fn charter(&self, group: &GroupId) -> Option<String>;

    /// The last `limit` events of the group, oldest first.
    async fn recent(&self, group: &GroupId, limit: usize) -> Vec<AdminEvent>;

    /// The group's facts, each as the engine serialises it. Retracted facts
    /// are included and the route filters them. Empty for an unknown group.
    async fn facts(&self, group: &GroupId) -> Vec<serde_json::Value>;

    /// The group's decisions as `{ "open": [...], "closed": [...],
    /// "upcoming": [...] }`: proposals, decision records and calendar
    /// entries as the engine serialises them. Empty lists for an unknown
    /// group.
    async fn decisions(&self, group: &GroupId) -> serde_json::Value;

    /// The group's decision log as Markdown, or `None` if the group is
    /// unknown or has no log.
    async fn decision_log(&self, group: &GroupId) -> Option<String>;

    /// The group's promises and tab as `{ "commitments": [...], "lines":
    /// [...], "balances": [...], "currency": "..." }`, as the engine
    /// serialises them. Empty lists for an unknown group.
    async fn ledger(&self, group: &GroupId) -> serde_json::Value;

    /// The group's calendar as ICS text, or the one member's view of it when
    /// `member` is given. `None` when the group is unknown or `secret` is
    /// wrong; the routes answer both with 404. The implementation checks
    /// the secret, this crate only passes it through.
    async fn ics(&self, group: &GroupId, secret: &str, member: Option<&str>) -> Option<String>;

    /// The group's action records, newest first, each as the engine
    /// serialises it: `id`, `class`, `tier`, `room`, `requested_by`,
    /// `approved_by`, `at`, `status`, `summary`, `undo` and `outcome`, with
    /// the `*_name` keys when known. Empty for an unknown group.
    async fn actions(&self, group: &GroupId) -> Vec<serde_json::Value>;

    /// One entry per action class: `class`, `tier` and `how_undone`. The
    /// same for every group.
    async fn catalogue(&self) -> Vec<serde_json::Value>;

    /// What the compliance page shows. Empty until the binary fills it.
    async fn compliance(&self) -> ComplianceReport {
        ComplianceReport::default()
    }

    /// The group's export, or `None` for an unknown group.
    async fn export(&self, _group: &GroupId) -> Option<Export> {
        None
    }

    /// Turns the group's legal hold (expiry off) on or off. False when the
    /// group is unknown or the change was not made.
    async fn set_legal_hold(&self, _group: &GroupId, _on: bool) -> bool {
        false
    }

    /// Wipes the group's file and state for good. The route has already
    /// checked the host typed the group id.
    async fn wipe(&self, _group: &GroupId) -> Result<(), String> {
        Err("wipe is not supported".to_owned())
    }

    /// The group's persona as JSON, or `None` for an unknown group.
    async fn persona(&self, _group: &GroupId) -> Option<serde_json::Value> {
        None
    }

    /// The group's profile as JSON, or `None` for an unknown group.
    async fn profile(&self, _group: &GroupId) -> Option<serde_json::Value> {
        None
    }

    /// A receiver of every new event, for all groups, as it happens.
    fn subscribe(&self) -> broadcast::Receiver<AdminEvent>;
}
