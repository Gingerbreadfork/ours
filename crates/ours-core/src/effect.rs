//! Everything the engine asks the outside world to do. The binary executes
//! effects in order and feeds what comes back in as new inputs.

use crate::action::Recording;
use crate::state::{DmFollowUp, GroupState, PendingAction, PendingDm};
use crate::timers::TimerPurpose;
use crate::trace::Trace;
use ours_types::{
    ActionId, ClassifyRequest, ExtractRequest, Instant, Outbound, OutboundAction, OutsideAction,
    PlatformMessageId, PlatformRoomId, PlatformUserId, RequestId, TimeRequest, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Hand to the room's connector; its `ActionResult` comes back as an input.
    Send(Outbound),
    /// Arm a durable timer; when `at` passes, feed `Input::TimerFired`.
    SetTimer {
        id: TimerId,
        at: Instant,
        purpose: TimerPurpose,
    },
    CancelTimer {
        id: TimerId,
    },
    /// Call the large model; its answer comes back as `Input::ModelReply`.
    AskModel(ModelRequest),
    /// Score one message with the small model. Always answered with
    /// `Input::Scored`, carrying `rules` when the model fails or is off.
    Classify(ClassifyRequest),
    /// Pull the facts out of one message. Always answered with
    /// `Input::Extracted`, with no drafts when the extractor fails or is off.
    Extract(ExtractRequest),
    /// Read a time expression the rules could not. Always answered with
    /// `Input::TimeResolved`, meaning `None` when the resolver fails or is off.
    ResolveTime(TimeRequest),
    /// Carry out an action outside the group through its integration, now:
    /// any hold window has already passed. Always answered with
    /// `Input::OutsideResult` for the same `action`.
    Outside {
        action: ActionId,
        outside: OutsideAction,
    },
    /// Scrub these messages from the journal, and with `user` everything
    /// that person said, after a forget (DESIGN.md section 9, "Forgetting").
    Redact {
        messages: Vec<PlatformMessageId>,
        user: Option<PlatformUserId>,
    },
    /// One line for the admin page's audit trail.
    Notice(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModelRequest {
    pub id: RequestId,
    pub system: String,
    /// The room's recent messages, oldest first, ending with the one to answer.
    pub messages: Vec<ChatTurn>,
    /// JSON Schema for `Plan`; the answer must match it.
    pub output_schema: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChatTurn {
    pub role: TurnRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    /// People in the room, each line prefixed with the speaker's name.
    User,
    /// The agent's own earlier messages.
    Assistant,
}

/// Queues a message to `room` with why it was sent, and remembers it until
/// its id comes back. It becomes an action record of its own.
pub(crate) fn send(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    text: impl Into<String>,
    reply_to: Option<PlatformMessageId>,
    now: Instant,
    trace: Trace,
) {
    send_as(state, fx, room, text, reply_to, now, trace, Recording::Own);
}

/// Queues a message that enters the action record as `record` says.
#[allow(clippy::too_many_arguments)]
pub(crate) fn send_as(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    text: impl Into<String>,
    reply_to: Option<PlatformMessageId>,
    now: Instant,
    trace: Trace,
    record: Recording,
) {
    queue(
        state,
        fx,
        PendingAction {
            room: room.clone(),
            text: text.into(),
            reply_to,
            at: now,
            then: None,
            trace: Some(trace),
            record,
        },
    );
}

/// Queues a message described by `pending`, follow-up included.
pub(crate) fn queue(state: &mut GroupState, fx: &mut Vec<Effect>, pending: PendingAction) {
    let id = state.mint_action();
    let action = OutboundAction::SendMessage {
        room: pending.room.clone(),
        text: pending.text.clone(),
        reply_to: pending.reply_to.clone(),
    };
    state.pending_actions.insert(id.clone(), pending);
    fx.push(Effect::Send(Outbound { id, action }));
}

/// Queues a DM to `user` and returns the send's id. With `feedback`, a 👍
/// or 👎 from them on it counts toward observe mode's tally.
pub(crate) fn send_dm(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    user: &PlatformUserId,
    text: impl Into<String>,
    now: Instant,
    trace: Trace,
    feedback: bool,
) -> ActionId {
    let pending = PendingDm {
        user: user.clone(),
        text: text.into(),
        at: now,
        trace: Some(trace),
        feedback,
        then: None,
        record: Recording::Own,
    };
    queue_dm(state, fx, pending)
}

/// Queues a DM that enters the action record as `record` says.
pub(crate) fn send_dm_as(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    user: &PlatformUserId,
    text: impl Into<String>,
    now: Instant,
    trace: Trace,
    record: Recording,
) {
    let pending = PendingDm {
        user: user.clone(),
        text: text.into(),
        at: now,
        trace: Some(trace),
        feedback: false,
        then: None,
        record,
    };
    queue_dm(state, fx, pending);
}

/// Queues a DM that something waits on, such as a nudge whose reply matters.
pub(crate) fn send_dm_then(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    user: &PlatformUserId,
    text: impl Into<String>,
    now: Instant,
    trace: Trace,
    then: DmFollowUp,
) {
    let pending = PendingDm {
        user: user.clone(),
        text: text.into(),
        at: now,
        trace: Some(trace),
        feedback: false,
        then: Some(then),
        record: Recording::Own,
    };
    queue_dm(state, fx, pending);
}

fn queue_dm(state: &mut GroupState, fx: &mut Vec<Effect>, pending: PendingDm) -> ActionId {
    let id = state.mint_action();
    let action = OutboundAction::SendDm {
        user: pending.user.clone(),
        text: pending.text.clone(),
    };
    state.pending_dms.insert(id.clone(), pending);
    fx.push(Effect::Send(Outbound {
        id: id.clone(),
        action,
    }));
    id
}

pub(crate) fn react(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    emoji: &str,
) -> ActionId {
    let action = OutboundAction::React {
        room: room.clone(),
        message: message.clone(),
        emoji: emoji.to_owned(),
    };
    let id = state.mint_action();
    fx.push(Effect::Send(Outbound {
        id: id.clone(),
        action,
    }));
    id
}

/// Deletes one of the agent's messages.
pub(crate) fn delete(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) {
    let action = OutboundAction::DeleteMessage {
        room: room.clone(),
        id: message.clone(),
    };
    fx.push(Effect::Send(Outbound {
        id: state.mint_action(),
        action,
    }));
}

/// Replaces the text of one of the agent's messages.
pub(crate) fn edit(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    text: impl Into<String>,
) {
    let action = OutboundAction::EditMessage {
        room: room.clone(),
        id: message.clone(),
        text: text.into(),
    };
    fx.push(Effect::Send(Outbound {
        id: state.mint_action(),
        action,
    }));
}

/// Shows `room` that the agent is writing, where its platform can. Nothing
/// waits on the result.
pub(crate) fn typing(state: &mut GroupState, fx: &mut Vec<Effect>, room: &PlatformRoomId) {
    if !state.me_in(room).is_some_and(|m| m.capabilities.typing) {
        return;
    }
    let action = OutboundAction::Typing { room: room.clone() };
    fx.push(Effect::Send(Outbound {
        id: state.mint_action(),
        action,
    }));
}

pub(crate) fn unreact(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    emoji: &str,
) {
    let action = OutboundAction::Unreact {
        room: room.clone(),
        message: message.clone(),
        emoji: emoji.to_owned(),
    };
    fx.push(Effect::Send(Outbound {
        id: state.mint_action(),
        action,
    }));
}
