//! Undo (DESIGN.md section 13, "Undo mechanics"). "undo" takes back the
//! agent's latest action in the room or thread, "undo the email" the latest
//! of that kind, and "undo" in reply to one of its messages that one. A
//! lead or host may undo anything group-scoped, a member what they asked
//! for or what is about them; anything else waits for a lead's ✅. Every
//! undo is a record of its own and its line names who undid.

use crate::action::{self, ActionClass, ActionRecord, ActionStatus, Recording, Undo};
use crate::commitment;
use crate::decide;
use crate::effect::{Effect, delete, send_as, unreact};
use crate::hold;
use crate::leads::{self, decline_line};
use crate::mail::{self, NOT_SENT};
use crate::memory::{self, UNDONE};
use crate::proposal::ProposalStatus;
use crate::reactions::{self, Meaning};
use crate::sentence::UndoWhat;
use crate::settings::addressed_trigger;
use crate::state::{DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, GroupState};
use crate::tab;
use crate::trace::Trace;
use ours_types::{
    ActionId, Instant, Message, OutsideAction, PlatformMessageId, PlatformRoomId, PlatformUserId,
    Role, Tier,
};

pub const NOTHING_TO_UNDO: &str = "Nothing of mine to undo here.";
pub const WHICH_ONE: &str = "Reply undo to the message of mine you mean.";
pub const CANT_DELETE: &str = "I can't delete messages on this platform.";
pub const SENDING_NOW: &str =
    "That's going out now; say undo again in a moment and I'll send a cancellation.";
pub const POLL_CLOSED: &str =
    "That poll has closed and the decision stands; say re-vote to open it again.";
pub const POLL_LAPSED: &str = "That poll already lapsed; say re-vote to open it again.";
const UNDO_RULE: &str = "undo takes back the agent's latest action here, or the one named";

/// Who is undoing: `by` carries it out; `asked` is the member whose
/// request a lead approved.
pub(crate) struct Undoer {
    pub by: PlatformUserId,
    pub asked: Option<PlatformUserId>,
    pub request: PlatformMessageId,
}

/// "undo", "undo the email", "cancel that", or "undo" in reply to a message.
pub(crate) fn handle(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    what: UndoWhat,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    let trigger = addressed_trigger(state, message);
    let say = |state: &mut GroupState, fx: &mut Vec<Effect>, text: &str, rule: &str| {
        let trace = Trace::new(trigger.clone(), rule, now);
        send_as(
            state,
            fx,
            room,
            text,
            Some(message.id.clone()),
            now,
            trace,
            Recording::Unrecorded,
        );
    };
    if what == UndoWhat::Unclear {
        say(state, fx, WHICH_ONE, "undo needs to know which action");
        return;
    }
    let Some(record) =
        target(state, message, what).and_then(|id| action::find(state, &id).cloned())
    else {
        say(
            state,
            fx,
            NOTHING_TO_UNDO,
            "undo found nothing of the agent's here",
        );
        return;
    };
    if let Some(line) = blocked(state, &record) {
        say(
            state,
            fx,
            &line,
            "undo says plainly when it cannot take something back",
        );
        return;
    }
    if role == Role::Guest {
        let line = decline_line(state, room);
        say(state, fx, &line, "guests may change only their own data");
        return;
    }
    if role < Role::Lead && !may_undo(state, &record, &message.author) {
        let deferral = Deferral {
            room: room.clone(),
            asked_by: message.author.clone(),
            request: message.id.clone(),
            request_text: message.text.clone(),
            change: DeferredChange::Undo {
                action: record.id.clone(),
            },
            expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
        };
        let trace = Trace::new(
            trigger.clone(),
            "a member's undo of someone else's action waits for a lead's ✅",
            now,
        );
        leads::open_deferral(state, fx, deferral, now, trace);
        return;
    }
    let undoer = Undoer {
        by: message.author.clone(),
        asked: None,
        request: message.id.clone(),
    };
    let trace = Trace::new(trigger, UNDO_RULE, now);
    perform(state, fx, &record, &undoer, now, trace);
}

/// The record an undo means.
fn target(state: &GroupState, message: &Message, what: UndoWhat) -> Option<ActionId> {
    let room = &message.room;
    let fits = |r: &ActionRecord| match what {
        UndoWhat::Class(c) => {
            r.class == c || (c == ActionClass::RoomMessage && r.class == ActionClass::Dm)
        }
        _ => true,
    };
    if let Some(parent) = &message.reply_to {
        let own = state.rooms.get(room).is_some_and(|r| r.is_own(parent));
        let hit = action::latest_where(state, |r| {
            &r.room == room
                && r.class != ActionClass::Undo
                && fits(r)
                && if own {
                    r.concerns_message(parent)
                } else {
                    r.instruction.as_ref() == Some(parent)
                }
        });
        if hit.is_some() || own || what == UndoWhat::Latest {
            return hit;
        }
    }
    match what {
        UndoWhat::Latest => action::latest_where(state, |r| {
            &r.room == room && r.visible && r.class != ActionClass::Undo && !r.is_undone()
        }),
        UndoWhat::Class(_) => {
            action::latest_where(state, |r| &r.room == room && fits(r) && !r.is_undone())
                .or_else(|| action::latest_where(state, |r| &r.room == room && fits(r)))
        }
        UndoWhat::Unclear => None,
    }
}

/// Why this record cannot be taken back now, if it cannot.
fn blocked(state: &GroupState, record: &ActionRecord) -> Option<String> {
    match &record.status {
        ActionStatus::Undone { by, .. } => {
            return Some(match by {
                Some(by) => format!(
                    "Already undone by {}.",
                    state.display_name(&record.room, by)
                ),
                None => "Already undone.".to_owned(),
            });
        }
        ActionStatus::Failed { .. } => {
            return Some("That one never went out, so there's nothing to undo.".to_owned());
        }
        ActionStatus::Declined => {
            return Some("That one was never done, so there's nothing to undo.".to_owned());
        }
        ActionStatus::Sending { .. } => return Some(SENDING_NOW.to_owned()),
        _ => {}
    }
    match &record.undo {
        Undo::None { why } => Some(format!("That one can't be undone: {why}.")),
        Undo::Delete { room, .. } if !can_delete(state, room) => Some(CANT_DELETE.to_owned()),
        Undo::EndPoll { proposal } => match state.proposals.get(proposal).map(|p| &p.status) {
            Some(ProposalStatus::Closed(_)) => Some(POLL_CLOSED.to_owned()),
            Some(ProposalStatus::Expired) => Some(POLL_LAPSED.to_owned()),
            _ => None,
        },
        _ => None,
    }
}

fn can_delete(state: &GroupState, room: &PlatformRoomId) -> bool {
    state.me_in(room).is_some_and(|m| m.capabilities.delete)
}

/// A member may undo what they asked for or approved, what is about them,
/// and anything in their own DM.
fn may_undo(state: &GroupState, record: &ActionRecord, user: &PlatformUserId) -> bool {
    if state.rooms.get(&record.room).is_some_and(|r| r.is_dm()) {
        return true;
    }
    if record.requested_by.as_ref() == Some(user) || record.approved_by.as_ref() == Some(user) {
        return true;
    }
    match &record.undo {
        Undo::RetractFacts { ids } => {
            !ids.is_empty()
                && ids
                    .iter()
                    .all(|id| state.facts.get(id).is_some_and(|f| f.concerns(user)))
        }
        Undo::DropCommitment { id } => state.commitments.get(id).is_some_and(|c| &c.who == user),
        Undo::VoidLine { id } => state.ledger.iter().any(|l| &l.id == id && &l.payer == user),
        _ => false,
    }
}

/// Takes `record` back, records the undo and says so in one line.
pub(crate) fn perform(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    record: &ActionRecord,
    undoer: &Undoer,
    now: Instant,
    trace: Trace,
) {
    let room = record.room.clone();
    let uid = state.mint_action();
    let mut undo = ActionRecord::new(
        uid.clone(),
        ActionClass::Undo,
        room.clone(),
        now,
        format!("undid {}", record.summary),
        Undo::None {
            why: "an undo isn't undone; ask for the thing again".to_owned(),
        },
    );
    undo.requested_by = Some(undoer.asked.clone().unwrap_or_else(|| undoer.by.clone()));
    undo.approved_by = undoer.asked.as_ref().map(|_| undoer.by.clone());
    undo.instruction = Some(undoer.request.clone());
    undo.undoes = Some(record.id.clone());
    let name = state.display_name(&room, &undoer.by);
    let undone_by = match &undoer.asked {
        Some(asker) => format!("Undone by {name} for {}", state.display_name(&room, asker)),
        None => format!("Undone by {name}"),
    };
    let mut line = Some(format!("{undone_by}: {}.", record.summary));
    match &record.undo {
        Undo::Delete { room: r, message } => {
            delete(state, fx, r, message);
            forget_message(state, r, message);
        }
        Undo::Unreact {
            room: r,
            message,
            emoji,
        } => unreact(state, fx, r, message, emoji),
        Undo::RetractFacts { ids } => {
            memory::retract(state, ids, UNDONE, Some(&undoer.by), now);
        }
        Undo::DropCommitment { id } => {
            let source = state.commitments.get(id).map(|c| c.source.clone());
            commitment::abandon(state, id, Some(&undoer.by), now, fx);
            if let Some(source) = source.filter(|_| record.visible) {
                reactions::remove(state, fx, &source.room, &source.message, Meaning::Pin);
            }
        }
        Undo::VoidLine { id } => {
            tab::void(state, id, &undoer.by, now);
        }
        Undo::EndPoll { proposal } => decide::withdraw(state, proposal, now, fx),
        Undo::RemoveEntry { decision } => state.calendar.retain(|e| &e.decision != decision),
        Undo::CancelHold => {
            let hold = hold::cancel(state, fx, &record.id);
            mail::cancelled_notice(
                state,
                fx,
                record,
                hold.as_ref().and_then(|h| h.notice.as_ref()),
            );
            let by = undo.approved_by.clone().or(undo.requested_by.clone());
            line = Some(format!(
                "{NOT_SENT}{}",
                mail::undoer_suffix(state, &room, record.requested_by.as_ref(), by.as_ref())
            ));
        }
        Undo::CancelMail { .. } => {
            let Some(OutsideAction::SendMail(sent)) = &record.outside else {
                return;
            };
            let cancel = mail::cancellation(state, record, sent);
            let outside = OutsideAction::CancelMail {
                original: record.id.clone(),
                mail: cancel,
            };
            undo.tier = Tier::Windowed;
            undo.status = ActionStatus::Sending { since: now };
            undo.outside = Some(outside.clone());
            fx.push(Effect::Outside {
                action: uid.clone(),
                outside,
            });
            // The line waits for the result, so it never claims what has not happened.
            line = None;
        }
        Undo::None { .. } => return,
    }
    if !matches!(record.undo, Undo::CancelMail { .. })
        && let Some(r) = action::find_mut(state, &record.id)
    {
        r.status = ActionStatus::Undone {
            at: now,
            by: Some(undoer.by.clone()),
        };
    }
    action::push(state, undo);
    fx.push(Effect::Notice(format!("{undone_by}: {}.", record.summary)));
    if let Some(text) = line {
        send_as(
            state,
            fx,
            &room,
            text,
            Some(undoer.request.clone()),
            now,
            trace,
            Recording::Line { action: uid },
        );
    }
}

/// Takes a deleted message out of what the room holds of the agent's.
fn forget_message(state: &mut GroupState, room: &PlatformRoomId, message: &PlatformMessageId) {
    if let Some(r) = state.rooms.get_mut(room) {
        r.remove(message);
        r.own_messages.retain(|m| m != message);
        r.traces.retain(|(m, _)| m != message);
    }
    state.deferrals.remove(message);
}

/// A lead's ✅ on a member's undo: carried out as if the lead had asked.
pub(crate) fn approved(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    action_id: &ActionId,
    deferral: &Deferral,
    approver: &PlatformUserId,
    now: Instant,
    trace: Trace,
) {
    let Some(record) = action::find(state, action_id).cloned() else {
        return;
    };
    if let Some(line) = blocked(state, &record) {
        send_as(
            state,
            fx,
            &deferral.room,
            line,
            Some(deferral.request.clone()),
            now,
            trace,
            Recording::Unrecorded,
        );
        return;
    }
    let undoer = Undoer {
        by: approver.clone(),
        asked: Some(deferral.asked_by.clone()),
        request: deferral.request.clone(),
    };
    perform(state, fx, &record, &undoer, now, trace);
}
