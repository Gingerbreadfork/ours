//! Durable timers on the virtual clock. The engine arms and cancels them as
//! effects and only ever sees them again as `Input::TimerFired`.

use crate::commitment::CommitmentId;
use crate::effect::Effect;
use crate::proposal::{DecisionId, ProposalId};
use crate::recur::RecurrenceId;
use crate::state::GroupState;
use ours_types::{
    ActionId, Instant, PlatformMessageId, PlatformRoomId, PlatformUserId, RequestId, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How long the agent waits for the model before saying it failed.
pub const MODEL_GUARD_SECONDS: i64 = 30;

/// Which of a proposal's two nudges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NudgeAt {
    Half,
    Deadline,
}

/// Which of a commitment's two nudges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DueAt {
    /// The day before it is due.
    Before,
    At,
}

/// Why a timer exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimerPurpose {
    /// Fires if the model has not answered `request` in time.
    ModelGuard { request: RequestId },
    /// The answer window for a question nobody has answered: react 🙋 if
    /// it is still open.
    RaiseHand {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    /// The window again after the 🙋: answer if the question is still open.
    AnswerAnyway {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    /// The end of the observe period: tell leads and hosts what it saw.
    ObserveEnd,
    /// Fires if the extractor has not answered a "remember that" in time.
    ExtractGuard {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    /// A backfill waited too long for a page or for its last facts.
    BackfillGuard { room: PlatformRoomId },
    /// Nudge members who have not voted. Without a user it picks each
    /// member's moment; with one it is that member's moment.
    Nudge {
        proposal: ProposalId,
        which: NudgeAt,
        #[serde(default)]
        user: Option<PlatformUserId>,
    },
    /// A proposal's deadline.
    Close { proposal: ProposalId },
    /// Fires if the resolver has not answered a deadline in time.
    TimeGuard {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    /// A commitment's nudge by DM.
    Due {
        commitment: CommitmentId,
        which: DueAt,
    },
    /// The room's digest at the profile's time.
    Digest { room: PlatformRoomId },
    /// The one line on a day with a calendar entry; `entry` is the earliest.
    DayOf {
        room: PlatformRoomId,
        entry: DecisionId,
    },
    /// Edit the room's pinned state message once the changes settle.
    Repin { room: PlatformRoomId },
    /// Open a recurrence's next instance.
    Recur { id: RecurrenceId },
    /// A held action's window is over: send it, or let an unanswered
    /// request for a ✅ lapse.
    Release { action: ActionId },
    /// The daily sweep that drops old action records and expired facts.
    Retention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Timer {
    pub at: Instant,
    pub purpose: TimerPurpose,
}

pub(crate) fn arm(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    at: Instant,
    purpose: TimerPurpose,
) -> TimerId {
    let id = state.mint_timer();
    state.timers.insert(
        id.clone(),
        Timer {
            at,
            purpose: purpose.clone(),
        },
    );
    fx.push(Effect::SetTimer {
        id: id.clone(),
        at,
        purpose,
    });
    id
}

pub(crate) fn cancel(state: &mut GroupState, fx: &mut Vec<Effect>, id: &TimerId) {
    if state.timers.remove(id).is_some() {
        fx.push(Effect::CancelTimer { id: id.clone() });
    }
}

/// Cancels every armed timer whose purpose matches.
pub(crate) fn cancel_where(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    matches: impl Fn(&TimerPurpose) -> bool,
) {
    let ids: Vec<TimerId> = state
        .timers
        .iter()
        .filter(|(_, t)| matches(&t.purpose))
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        cancel(state, fx, &id);
    }
}

pub(crate) fn any_where(state: &GroupState, matches: impl Fn(&TimerPurpose) -> bool) -> bool {
    state.timers.values().any(|t| matches(&t.purpose))
}
