//! The pinned state message (DESIGN.md section 8, "State"): one per room,
//! posted when there is first something to show, then edited in place and
//! never reposted. Edits wait a minute so a burst of votes is one edit. It
//! needs a platform that can edit; one that cannot pin keeps it unpinned.

use crate::action::{self, ActionClass, ActionRecord, Recording, Undo};
use crate::commitment::Commitment;
use crate::decide::standing;
use crate::decision::upcoming_line;
use crate::effect::{Effect, edit, queue};
use crate::leads::listen_line;
use crate::memory::Place;
use crate::proposal::Proposal;
use crate::state::{FollowUp, GroupState, PendingAction};
use crate::tab::{balance_parts, balances_at};
use crate::time;
use crate::timers::{self, TimerPurpose};
use ours_types::{
    Instant, Outbound, OutboundAction, PlatformMessageId, PlatformRoomId, TimeMeaning,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most lines the message may have: about a screen.
pub const LINES: usize = 12;
/// How long changes settle before the message is edited.
pub const REPIN_SECONDS: i64 = 60;
/// Open proposals shown.
const OPEN_SHOWN: usize = 2;
/// Commitments shown before "and N more".
const OUTSTANDING_SHOWN: usize = 5;
pub const NOTHING_ON: &str = "Nothing open, nothing due.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Pinned {
    /// The message, once its id is known.
    pub message: Option<PlatformMessageId>,
    /// What it should say now.
    pub rendered: String,
    /// What the platform shows.
    #[serde(default)]
    pub shown: String,
}

/// What the room's state message says, and whether any of it is more than
/// whom the agent listens to.
pub fn render(state: &GroupState, room: &PlatformRoomId, now: Instant) -> (String, bool) {
    let place = Place::Room(room.clone());
    let tz = state.timezone.as_str();
    let mut lines: Vec<String> = Vec::new();
    let mut open: Vec<&Proposal> = state
        .proposals
        .values()
        .filter(|p| p.is_open() && &p.room == room)
        .collect();
    open.sort_by_key(|p| (p.opened_at, p.id.seq()));
    for p in open.iter().take(OPEN_SHOWN) {
        let closes = TimeMeaning::Instant {
            at: p.deadline,
            has_time: true,
        };
        let closes = time::echo(&closes, tz, now).unwrap_or_default();
        lines.push(format!(
            "Open: {}, {}, closes {closes}.",
            p.title(),
            standing(p)
        ));
    }
    if let Some(line) = upcoming_line(state, &place, now) {
        lines.push(line);
    }
    let mut due: Vec<&Commitment> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && c.visible(state, &place))
        .collect();
    due.sort_by_key(|c| (c.due.is_none(), c.due, c.id.seq()));
    for (i, c) in due.iter().take(OUTSTANDING_SHOWN).enumerate() {
        let line = crate::commitment::line(state, c, tz, now).replacen(": ", ", ", 1);
        lines.push(if i == 0 {
            format!("Outstanding: {line}")
        } else {
            line
        });
    }
    if due.len() > OUTSTANDING_SHOWN {
        lines.push(format!("and {} more", due.len() - OUTSTANDING_SHOWN));
    }
    let balances = balances_at(state, &place, &state.currency);
    if !balances.is_empty() {
        lines.push(format!(
            "Tab ({}): {}.",
            state.currency,
            balance_parts(state, &balances).join(", ")
        ));
    }
    let need = !lines.is_empty();
    if !need {
        lines.push(NOTHING_ON.to_owned());
    }
    let mut tail = vec![listen_line(state, room)];
    if !state.model_label.is_empty() {
        tail.push(format!("Running on {}.", state.model_label));
    }
    if state.feed_base.is_some() {
        tail.push("Calendar feed: ask me for the calendar link.".to_owned());
    }
    lines.truncate(LINES.saturating_sub(tail.len()));
    lines.extend(tail);
    (lines.join("\n"), need)
}

/// Brings every room's state message up to date after a step: posts it on
/// first need, and arms the edit when what it should say has changed.
pub(crate) fn reconcile(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    let rooms: Vec<PlatformRoomId> = state
        .rooms
        .iter()
        .filter(|(_, r)| r.info.is_some() && !r.is_dm())
        .map(|(id, _)| id.clone())
        .collect();
    for room in rooms {
        if !state.me_in(&room).is_some_and(|m| m.capabilities.edit) {
            continue;
        }
        let (text, need) = render(state, &room, now);
        let Some(pin) = state.pins.get_mut(&room) else {
            // Posting it is speaking in the room, which waits for the end of observing.
            if need && !state.is_observing() {
                post(state, &room, text, now, fx);
            }
            continue;
        };
        pin.rendered = text;
        let stale = pin.message.is_some() && pin.shown != pin.rendered;
        let armed = timers::any_where(
            state,
            |p| matches!(p, TimerPurpose::Repin { room: r } if *r == room),
        );
        if stale && !armed {
            let purpose = TimerPurpose::Repin { room: room.clone() };
            timers::arm(state, fx, now.plus_seconds(REPIN_SECONDS), purpose);
        }
    }
}

fn post(
    state: &mut GroupState,
    room: &PlatformRoomId,
    text: String,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    state.pins.insert(
        room.clone(),
        Pinned {
            message: None,
            rendered: text.clone(),
            shown: text.clone(),
        },
    );
    queue(
        state,
        fx,
        PendingAction {
            room: room.clone(),
            text,
            reply_to: None,
            at: now,
            then: Some(FollowUp::StateMessage { room: room.clone() }),
            trace: None,
            record: Recording::Unrecorded,
        },
    );
}

/// The state message's id came back: keep it and pin it where the
/// platform can.
pub(crate) fn posted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(pin) = state.pins.get_mut(room) else {
        return;
    };
    pin.message = Some(message.clone());
    // Furniture rather than an act, so a bare "undo" never means it.
    let mut record = ActionRecord::new(
        state.mint_action(),
        ActionClass::Pin,
        room.clone(),
        now,
        "the pinned state message",
        Undo::None {
            why: "it follows the group's state, so change what it shows instead".to_owned(),
        },
    )
    .hidden();
    record.lines.push(message.clone());
    action::push(state, record);
    if state.me_in(room).is_some_and(|m| m.capabilities.pin) {
        let action = OutboundAction::Pin {
            room: room.clone(),
            message: message.clone(),
        };
        fx.push(Effect::Send(Outbound {
            id: state.mint_action(),
            action,
        }));
    }
}

/// The minute is up: edit the message to what it should say now.
pub(crate) fn on_repin(state: &mut GroupState, room: &PlatformRoomId, fx: &mut Vec<Effect>) {
    let Some(pin) = state.pins.get_mut(room) else {
        return;
    };
    let Some(message) = pin.message.clone() else {
        return;
    };
    if pin.shown == pin.rendered {
        return;
    }
    pin.shown = pin.rendered.clone();
    let text = pin.shown.clone();
    edit(state, fx, room, &message, text);
}

/// The state message failed to send or was deleted: post afresh on next need.
pub(crate) fn forget(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: Option<&PlatformMessageId>,
) {
    let gone = state
        .pins
        .get(room)
        .is_some_and(|p| message.is_none_or(|m| p.message.as_ref() == Some(m)));
    if gone {
        state.pins.remove(room);
    }
}
