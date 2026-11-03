//! Types shared by every Ours crate: identifiers, the enums the design
//! names (platform, role, kind of group, tier), an instant in time, and the
//! normalised event model that connectors speak. Nothing here does I/O.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod connector;
pub mod ids;
pub mod instant;
pub mod memory;
pub mod outside;
pub mod time;
pub mod triage;

pub use connector::{
    Attachment, Capabilities, InboundEvent, Member, Message, Outbound, OutboundAction, Room,
    RoomKind,
};
pub use ids::{
    ActionId, GroupId, PlatformMessageId, PlatformRoomId, PlatformUserId, RequestId, TimerId,
};
pub use instant::Instant;
pub use memory::{
    CommitmentDraft, ExtractRequest, Extraction, FactDraft, LedgerDraft, SubjectKind, message_link,
};
pub use outside::{InboundMail, OutgoingMail, OutsideAction, OutsideResult};
pub use time::{TimeMeaning, TimeRequest, TimeResolution};
pub use triage::{ClassifyRequest, ContextLine, MessageKind, Score, ScoreSource};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A chat platform a connector speaks. `Sim` is the in-memory connector
/// used by tests and the replay harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Discord,
    Slack,
    Telegram,
    Matrix,
    Mattermost,
    Signal,
    Teams,
    WhatsApp,
    Sim,
}

/// Who a speaker is to the agent, resolved per room (DESIGN.md section 5).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Guest,
    Member,
    Lead,
    Host,
}

/// What the platform itself says about a user in a room. Connectors report
/// this; the engine maps it to a `Role` through the group's profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlatformRole {
    Owner,
    Admin,
    Moderator,
    #[default]
    Regular,
    /// Marked external or guest by the platform (Slack Connect, guest accounts).
    External,
}

/// The kind of group, which sets defaults and nothing else (DESIGN.md section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Friends,
    Household,
    Team,
    Project,
    Club,
    Community,
    Committee,
    Cohort,
}

/// Consequence tier of an action (DESIGN.md section 13).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Done immediately, undone completely.
    Free,
    /// Done after a hold window; undone by a compensating action.
    Windowed,
    /// Asks exactly one person first.
    Irreversible,
}
