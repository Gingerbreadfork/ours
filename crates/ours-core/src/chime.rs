//! Chiming in (DESIGN.md sections 3 and 16): at dial 2 and 3, banter nobody
//! addressed to the agent may get a reaction or one short line, the way a
//! member joins in. Each chime spends the interjection budget and the day's
//! chime count, and a shush ends them until midnight.

use crate::action::{self, ActionClass, ActionRecord, Origin, Recording, Undo};
use crate::addressing::is_addressed;
use crate::attention::{ACT_THRESHOLD, OPEN_DECISION, raising};
use crate::authority::resolve_role;
use crate::budget::{can_speak_unprompted, record_chime};
use crate::commitment::{Commitment, CommitmentStatus};
use crate::effect::{Effect, react, send_as};
use crate::plan::{Plan, PlannedAction};
use crate::proposal::words;
use crate::settings::addressed_trigger;
use crate::state::{Answering, GroupState, PendingRequest};
use crate::step::{Ask, ask_model};
use crate::timers;
use crate::trace::Trace;
use crate::triage::rules_score;
use ours_types::{Instant, Message, Role, Score};
use tracing::{debug, info, warn};

/// The line added to the prompt when the agent may chime in.
pub const CHIME_PROMPT: &str = "Nobody addressed you. Chime in: react, or one short line in your voice; never a question; nothing if it would not land.";
/// The lowest dial that chimes in.
pub const CHIME_DIAL: u8 = 2;
/// The longest line a chime may be, in characters.
pub const CHIME_CHARS: usize = 200;

/// Words that make a message about money, which is never joked about.
const MONEY: [&str; 24] = [
    "owe",
    "owes",
    "owed",
    "owing",
    "debt",
    "paid",
    "pay",
    "paying",
    "payment",
    "settle",
    "settled",
    "settling",
    "refund",
    "venmo",
    "paypal",
    "revolut",
    "splitwise",
    "iou",
    "tab",
    "invoice",
    "money",
    "cash",
    "quid",
    "bucks",
];

/// Words that say a promise is late.
const SLIP: [&str; 14] = [
    "late",
    "slipping",
    "slipped",
    "behind",
    "forgot",
    "sorry",
    "won't make it",
    "can't make it",
    "didn't get",
    "haven't done",
    "not done",
    "push it",
    "pushing it",
    "next week",
];

/// "chimed in: banter 0.82", the rule a chime's trace carries.
pub fn rule(banter: f32) -> String {
    format!("chimed in: banter {banter:.2}")
}

/// Asks the model for a chime on `held` when its banter is high enough and
/// nothing holds the agent back.
pub(crate) fn consider(
    state: &mut GroupState,
    held: &Message,
    score: &Score,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if score.banter < state.profile.chime_threshold {
        return;
    }
    if let Some(why) = held_back(state, held, score, now) {
        debug!(message = %held.id, why, "no chime");
        return;
    }
    info!(message = %held.id, banter = score.banter, "chiming in");
    let ask = Ask {
        room: held.room.clone(),
        message: held.id.clone(),
        speaker: held.author.clone(),
        role: resolve_role(state, &held.room, &held.author),
        answering: Answering::Chime,
        score: Some(score.clone()),
    };
    ask_model(state, ask, now, fx);
}

/// Why the agent stays out of it, if it does.
fn held_back(
    state: &GroupState,
    held: &Message,
    score: &Score,
    now: Instant,
) -> Option<&'static str> {
    let room = &held.room;
    if state.profile.dial < CHIME_DIAL {
        return Some("the dial is below 2");
    }
    if state.is_observing() {
        return Some("observing");
    }
    if state.rooms.get(room).is_none_or(|r| r.is_dm()) {
        return Some("a DM");
    }
    if !state.is_human(room, &held.author) || is_addressed(state, held) {
        return Some("not banter from a person");
    }
    if resolve_role(state, room, &held.author) == Role::Guest {
        return Some("a guest's message");
    }
    if !welcome(state, now) {
        return Some("no budget, the wrong hour or shushed today");
    }
    let in_flight = state
        .pending_requests
        .values()
        .filter(|p| p.answering == Answering::Chime)
        .count();
    let in_flight = u32::try_from(in_flight).unwrap_or(u32::MAX);
    let today = state.budget.chimes_today(now, &state.timezone);
    if today.saturating_add(in_flight) >= state.profile.chimes_a_day() {
        return Some("the day's chimes are spent");
    }
    if score.open_decision >= OPEN_DECISION
        || f64::from(score.plan_forming) >= state.profile.speech_threshold
        || raising(state, &held.id)
    {
        return Some("a vote, a plan or a question it may answer");
    }
    if touchy(state, held, score, now) {
        return Some("money or a promise");
    }
    None
}

/// Whether an unprompted reaction or line is welcome now.
fn welcome(state: &GroupState, now: Instant) -> bool {
    can_speak_unprompted(state, now) && !state.budget.shushed_today(now, &state.timezone)
}

/// Money, the tab, or a promise that is late or being moved. The rules
/// pass is asked too, so "I got the wine, 38" counts whatever the model says.
fn touchy(state: &GroupState, held: &Message, score: &Score, now: Instant) -> bool {
    let promise = score
        .commitment
        .max(rules_score(state, &held.room, held).commitment);
    if promise >= ACT_THRESHOLD || held.text.contains(['$', '£', '€', '¥', '₹']) {
        return true;
    }
    let said = words(&held.text);
    let padded = format!(" {} ", said.join(" "));
    let has = |phrase: &&str| padded.contains(&format!(" {phrase} "));
    if MONEY.iter().any(has) {
        return true;
    }
    let replied = held.reply_to.as_ref();
    let tab = state.ledger.iter().any(|line| {
        line.source.message == held.id || replied.is_some_and(|r| line.posted.as_ref() == Some(r))
    });
    if tab {
        return true;
    }
    let slip = SLIP.iter().any(has);
    state
        .commitments
        .values()
        .filter(|c| c.source.room == held.room)
        .any(|c| {
            c.moved_by.contains(&held.id)
                || replied.is_some_and(|r| *r == c.source.message || c.nudges.contains(r))
                || (c.is_open()
                    && ((slip && c.who == held.author) || (late(c, now) && names(c, &said))))
        })
}

fn late(c: &Commitment, now: Instant) -> bool {
    c.overdue(now) || matches!(c.status, CommitmentStatus::Moved { .. })
}

/// Whether the message names what the commitment is about.
fn names(c: &Commitment, said: &[String]) -> bool {
    words(&c.what)
        .iter()
        .any(|w| w.len() > 3 && said.contains(w))
}

/// The model's chime: a reaction, a line threaded to the message, or both,
/// counted once. Anything else in the plan is dropped, since nobody asked.
pub(crate) fn reply(
    state: &mut GroupState,
    pending: &PendingRequest,
    reply: Result<Plan, String>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    timers::cancel(state, fx, &pending.guard);
    let plan = match reply {
        Ok(plan) => plan,
        Err(error) => {
            warn!(%error, "model request failed");
            fx.push(Effect::Notice(format!(
                "The model failed to chime in: {error}"
            )));
            return;
        }
    };
    if !welcome(state, now) || state.is_observing() {
        debug!(message = %pending.message, "the room went quiet before the chime landed");
        return;
    }
    let room = &pending.room;
    let Some(held) = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| m.id == pending.message))
        .cloned()
    else {
        return;
    };
    let (emoji, line) = chime_of(plan);
    let emoji = emoji.and_then(|e| crate::reactions::fit(state, room, &e));
    if emoji.is_none() && line.is_none() {
        debug!(message = %held.id, "nothing that would land");
        return;
    }
    let banter = pending.score.as_ref().map_or(0.0, |s| s.banter);
    let whose = state.display_name(room, &held.author);
    if let Some(emoji) = emoji {
        let id = react(state, fx, room, &held.id, &emoji);
        let undo = Undo::Unreact {
            room: room.clone(),
            message: held.id.clone(),
            emoji: emoji.clone(),
        };
        let summary = format!("my {emoji} on {whose}'s message");
        let record = ActionRecord::new(id, ActionClass::Reaction, room.clone(), now, summary, undo);
        action::push(state, record);
    }
    if let Some(text) = line {
        let trace = Trace::new(addressed_trigger(state, &held), rule(banter), now);
        let nobody = Recording::For {
            origin: Origin::default(),
        };
        let reply_to = Some(held.id.clone());
        send_as(state, fx, room, text, reply_to, now, trace, nobody);
    }
    record_chime(state, now);
    let count = state.budget.chimes_today(now, &state.timezone);
    fx.push(Effect::Notice(format!(
        "Chimed in on {whose}'s message in {}: banter {banter:.2}, {count} of {} today.",
        state.room_label(room),
        state.profile.chimes_a_day()
    )));
}

/// The first reaction in a plan, and its line when it is one short line
/// with no question in it.
fn chime_of(plan: Plan) -> (Option<String>, Option<String>) {
    let emoji = plan.actions.iter().find_map(|a| match a {
        PlannedAction::React { emoji } if !emoji.trim().is_empty() => Some(emoji.trim().to_owned()),
        _ => None,
    });
    let said = plan.actions.into_iter().find_map(|a| match a {
        PlannedAction::Say { text } => Some(text),
        _ => None,
    });
    let line = plan
        .message
        .into_iter()
        .chain(said)
        .map(|t| t.trim().to_owned())
        .find(|t| fits(t));
    (emoji, line)
}

fn fits(line: &str) -> bool {
    !line.is_empty() && !line.contains(['?', '\n']) && line.chars().count() <= CHIME_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chime_is_one_short_line_without_a_question() {
        let plan = Plan {
            message: Some("Who brought the cake?".into()),
            actions: vec![
                PlannedAction::Say {
                    text: "  Called it.  ".into(),
                },
                PlannedAction::React {
                    emoji: "🎉".into()
                },
                PlannedAction::React {
                    emoji: "😂".into()
                },
            ],
            cites: Vec::new(),
        };
        assert_eq!(
            chime_of(plan),
            (Some("🎉".to_owned()), Some("Called it.".to_owned()))
        );
        assert!(!fits(&"a".repeat(CHIME_CHARS + 1)));
        assert!(!fits("one\ntwo"));
        assert!(fits("Called it."));
        assert_eq!(rule(0.824), "chimed in: banter 0.82");
    }
}
