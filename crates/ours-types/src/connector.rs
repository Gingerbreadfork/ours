//! The normalised model every connector speaks. A connector turns its
//! platform's events into `InboundEvent`s and carries out `Outbound`
//! actions, reporting each result back as an `InboundEvent::ActionResult`.
//! The engine never sees a platform type.

use crate::ids::{ActionId, PlatformMessageId, PlatformRoomId, PlatformUserId};
use crate::instant::Instant;
use crate::{Platform, PlatformRole};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a connector can do. The engine falls back where a platform lacks
/// something (reaction ballots where there are no polls).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Capabilities {
    pub reactions: bool,
    pub threads: bool,
    pub polls: bool,
    pub dms: bool,
    pub edit: bool,
    pub delete: bool,
    pub pin: bool,
    pub history: bool,
    /// The platform tells us members' timezones.
    pub timezones: bool,
    /// The platform marks external or guest accounts.
    pub external_flag: bool,
    /// The platform can show that the agent is writing.
    #[serde(default)]
    pub typing: bool,
    /// The only reactions the platform allows, when it restricts them
    /// (Telegram); None means any emoji. The engine substitutes from this
    /// set for the reactions it relies on.
    #[serde(default)]
    pub reaction_set: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    Channel,
    Dm,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Room {
    pub platform: Platform,
    pub id: PlatformRoomId,
    pub name: Option<String>,
    pub kind: RoomKind,
    /// For threads, the room they hang off.
    pub parent: Option<PlatformRoomId>,
    /// The server, workspace or group the room belongs to on the platform.
    pub container: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Member {
    pub id: PlatformUserId,
    pub display_name: String,
    pub platform_role: PlatformRole,
    pub is_bot: bool,
    /// IANA zone name when the platform knows it.
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Attachment {
    pub kind: String,
    pub url: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub id: PlatformMessageId,
    pub room: PlatformRoomId,
    pub author: PlatformUserId,
    pub text: String,
    pub at: Instant,
    pub reply_to: Option<PlatformMessageId>,
    pub mentions: Vec<PlatformUserId>,
    /// The agent was mentioned, by handle or by platform mention.
    pub mentions_me: bool,
    pub attachments: Vec<Attachment>,
    pub edited: bool,
}

/// Everything a platform can tell the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InboundEvent {
    /// The connector is up and knows who the agent is on this platform.
    Ready {
        platform: Platform,
        me: PlatformUserId,
        me_name: String,
        #[serde(default)]
        capabilities: Capabilities,
    },
    /// A room and its full membership, sent on join and whenever it changes.
    RoomSync {
        room: Room,
        members: Vec<Member>,
    },
    Message(Message),
    MessageEdited(Message),
    MessageDeleted {
        room: PlatformRoomId,
        id: PlatformMessageId,
    },
    ReactionAdded {
        room: PlatformRoomId,
        message: PlatformMessageId,
        user: PlatformUserId,
        emoji: String,
        at: Instant,
    },
    ReactionRemoved {
        room: PlatformRoomId,
        message: PlatformMessageId,
        user: PlatformUserId,
        emoji: String,
        at: Instant,
    },
    MemberJoined {
        room: PlatformRoomId,
        member: Member,
        at: Instant,
    },
    MemberLeft {
        room: PlatformRoomId,
        user: PlatformUserId,
        at: Instant,
    },
    MemberUpdated {
        room: PlatformRoomId,
        member: Member,
    },
    /// The outcome of an `Outbound` action, so the engine learns the ids of
    /// what it sent and whether a send failed.
    ActionResult {
        id: ActionId,
        ok: bool,
        /// The message the action produced, when it produced one.
        message: Option<PlatformMessageId>,
        error: Option<String>,
    },
    /// A vote on a native poll the agent created, or its removal.
    PollVote {
        room: PlatformRoomId,
        /// The message that carries the poll.
        message: PlatformMessageId,
        user: PlatformUserId,
        /// Zero-based, in the order the options were given.
        option: u32,
        added: bool,
        at: Instant,
    },
    /// A native poll finished on the platform's side.
    PollEnded {
        room: PlatformRoomId,
        message: PlatformMessageId,
        /// Vote counts per option, in order.
        counts: Vec<u32>,
    },
    /// History fetched on request, oldest first.
    History {
        room: PlatformRoomId,
        messages: Vec<Message>,
    },
    Disconnected {
        platform: Platform,
        reason: String,
    },
}

/// Everything the engine can ask a platform to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundAction {
    SendMessage {
        room: PlatformRoomId,
        text: String,
        reply_to: Option<PlatformMessageId>,
    },
    EditMessage {
        room: PlatformRoomId,
        id: PlatformMessageId,
        text: String,
    },
    DeleteMessage {
        room: PlatformRoomId,
        id: PlatformMessageId,
    },
    React {
        room: PlatformRoomId,
        message: PlatformMessageId,
        emoji: String,
    },
    Unreact {
        room: PlatformRoomId,
        message: PlatformMessageId,
        emoji: String,
    },
    SendDm {
        user: PlatformUserId,
        text: String,
    },
    Pin {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    /// A native poll where the platform has them; the result's message id is
    /// the poll's, and votes come back as `PollVote`.
    CreatePoll {
        room: PlatformRoomId,
        question: String,
        /// Two to ten options, in order.
        options: Vec<String>,
        /// Whether a person may pick several.
        multiple: bool,
        /// How long the platform keeps it open, in hours.
        hours: u32,
    },
    /// End a native poll early.
    EndPoll {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    /// Show the room the agent is writing, where the platform has an
    /// indicator. Fire and forget; the result carries no message.
    Typing {
        room: PlatformRoomId,
    },
    /// Ask for up to `limit` messages before `before` (or the newest when
    /// unset); answered with `InboundEvent::History`, oldest first.
    FetchHistory {
        room: PlatformRoomId,
        limit: u32,
        #[serde(default)]
        before: Option<PlatformMessageId>,
    },
}

/// An action with the id the engine will match its result to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outbound {
    pub id: ActionId,
    pub action: OutboundAction,
}

impl Outbound {
    pub fn new(action: OutboundAction) -> Self {
        Outbound {
            id: ActionId::generate(),
            action,
        }
    }
}
