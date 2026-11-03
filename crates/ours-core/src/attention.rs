//! What the agent does with attention scores (DESIGN.md sections 7 and 12):
//! the classify round trip for messages nobody addressed to it, reaction
//! first for unanswered questions, the 🤫 and "shut up" that end unprompted
//! speech for the day, extraction for messages that carry a fact, a commitment or
//! money paid, proposals for plans that form, votes for lines about open
//! ones, and chime-ins on banter.

use crate::authority::resolve_role;
use crate::budget::{can_speak_unprompted, record_unprompted, stop_today};
use crate::effect::Effect;
use crate::extract;
use crate::observe;
use crate::reactions::{self, Meaning};
use crate::state::{Answering, GroupState, Hand};
use crate::step::{Ask, ask_model, render_mentions};
use crate::timers::{self, TimerPurpose};
use crate::trace::{Trace, quote, shorten};
use crate::triage::{PLAN_FORMING, PLAN_FORMING_SHUSHED, rules_score};
use crate::zone::stamp;
use ours_types::{
    ClassifyRequest, ContextLine, Instant, Message, PlatformMessageId, PlatformRoomId,
    PlatformUserId, Score,
};
use tracing::debug;

/// The reaction that offers an answer (DESIGN.md section 7, "Addressing").
pub const RAISED_HAND: &str = "🙋";
/// The reaction that tells the agent to be quiet.
pub const SHUSH: &str = "🤫";
/// The only answer a shush gets.
pub const THUMBS_UP: &str = "👍";
/// Earlier messages a classifier sees.
pub const CONTEXT_LINES: usize = 8;
/// How long a shushed member's plans weigh less.
pub const SHUSH_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Scores below this lead to nothing at all.
pub const ACT_THRESHOLD: f32 = 0.5;
/// An open-decision signal this strong makes a line that names an option a vote.
pub const OPEN_DECISION: f32 = 0.9;

/// Scores a message nobody addressed to the agent, stores the rules score
/// and asks the classifier to refine it.
pub(crate) fn classify(state: &mut GroupState, message: &Message, fx: &mut Vec<Effect>) {
    let room = &message.room;
    let score = rules_score(state, room, message);
    state.room_mut(room).set_score(&message.id, score.clone());
    fx.push(Effect::Classify(ClassifyRequest {
        room: room.clone(),
        message: message.id.clone(),
        agent_name: state.agent_name.clone(),
        speaker: state.display_name(room, &message.author),
        text: render_mentions(state, room, &message.text),
        context: context_lines(state, message, CONTEXT_LINES),
        open_items: crate::decide::open_items(state, room),
        rules: score,
    }));
}

/// The last `n` messages before `message`, oldest first.
pub(crate) fn context_lines(state: &GroupState, message: &Message, n: usize) -> Vec<ContextLine> {
    let room_id = &message.room;
    let Some(room) = state.rooms.get(room_id) else {
        return Vec::new();
    };
    let end = room
        .messages
        .iter()
        .position(|m| m.id == message.id)
        .unwrap_or(room.messages.len());
    let start = end.saturating_sub(n);
    room.messages
        .range(start..end)
        .map(|m| ContextLine {
            speaker: if state.is_me(&m.author) || room.is_own(&m.id) {
                state.agent_name.clone()
            } else {
                state.display_name(room_id, &m.author)
            },
            text: render_mentions(state, room_id, &m.text),
            seconds_before: message.at.seconds_since(m.at).max(0),
        })
        .collect()
}

/// The classifier's answer, or the rules score when it failed.
pub(crate) fn on_scored(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    mut score: Score,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let held = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message))
        .cloned();
    let Some(held) = held else {
        debug!(%message, "score for a message no longer held");
        return;
    };
    // The rules only fire on strong patterns, so the model may raise those
    // signals but not lower them. A shush lowers that member's plans whatever
    // the model says, and nothing counts as an open decision unless one is.
    let rules = rules_score(state, room, &held);
    score.fact = score.fact.max(rules.fact);
    score.commitment = score.commitment.max(rules.commitment);
    score.plan_forming = score.plan_forming.max(rules.plan_forming);
    if rules.plan_forming == PLAN_FORMING_SHUSHED {
        score.plan_forming = score.plan_forming.min(PLAN_FORMING_SHUSHED);
    }
    let open_here = state
        .proposals
        .values()
        .any(|p| &p.room == room && p.is_open());
    if !open_here {
        score.open_decision = rules.open_decision;
    }
    state.room_mut(room).set_score(message, score.clone());
    if score.max() >= ACT_THRESHOLD {
        act_on(state, &held, &score, now, fx);
    }
    crate::chime::consider(state, &held, &score, now, fx);
}

/// A score that crossed the act threshold: a hand for a question, a vote,
/// a poll or a read for facts.
fn act_on(
    state: &mut GroupState,
    held: &Message,
    score: &Score,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &held.room;
    let message = &held.id;
    let label = state.room_label(room);
    if score.unanswered_question >= ACT_THRESHOLD
        && score.answerable_from_context
        && !raising(state, message)
    {
        let at = now.plus_seconds(i64::from(state.profile.answer_window_seconds));
        let purpose = TimerPurpose::RaiseHand {
            room: room.clone(),
            message: message.clone(),
        };
        timers::arm(state, fx, at, purpose);
    }
    if score.open_decision >= OPEN_DECISION {
        crate::decide::open_decision(state, room, held, now, fx);
    }
    // A plan at the signal's full weight opens a poll; the interjection
    // budget and the hours decide whether it actually goes out.
    if score.plan_forming >= PLAN_FORMING
        && !crate::decide::plan_forming(state, room, held, score, now, fx)
    {
        fx.push(Effect::Notice(format!(
            "Plan forming in {label}; polls arrive with decisions."
        )));
    }
    if score.fact >= ACT_THRESHOLD || score.commitment >= ACT_THRESHOLD {
        extract::heard(state, held, now, fx);
    }
}

/// Whether a hand is already raised, or about to be, on `message`.
pub(crate) fn raising(state: &GroupState, message: &PlatformMessageId) -> bool {
    state.hands.contains_key(message)
        || timers::any_where(
            state,
            |p| matches!(p, TimerPurpose::RaiseHand { message: m, .. } if m == message),
        )
}

/// The answer window passed. React 🙋 if nobody else has spoken and the
/// budget allows, or tell the leads it would have while observing.
pub(crate) fn raise_hand(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if answered(state, room, message) {
        debug!(%message, "someone answered; no hand raised");
        return;
    }
    if !can_speak_unprompted(state, now) {
        debug!(%message, "no budget or the wrong hour; no hand raised");
        return;
    }
    let observed = state.is_observing();
    if observed {
        if let Some(text) = would_have(state, room, message, None) {
            let trace = unanswered_trace(state, room, message, now, OBSERVE_RULE);
            observe::report(state, fx, &text, trace);
        }
    } else {
        reactions::add(state, fx, room, message, Meaning::RaisedHand);
        let asker = asker_name(state, room, message);
        fx.push(Effect::Notice(format!(
            "Raised a hand on {asker}'s question in {}.",
            state.room_label(room)
        )));
    }
    record_unprompted(state, now);
    let at = now.plus_seconds(i64::from(state.profile.answer_window_seconds));
    let purpose = TimerPurpose::AnswerAnyway {
        room: room.clone(),
        message: message.clone(),
    };
    let timer = timers::arm(state, fx, at, purpose);
    state.hands.insert(
        message.clone(),
        Hand {
            room: room.clone(),
            timer,
            observed,
        },
    );
}

/// The window passed again after the 🙋: answer if the question is still open.
pub(crate) fn answer_anyway(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(hand) = state.hands.remove(message) else {
        return;
    };
    if answered(state, room, message) || !can_speak_unprompted(state, now) {
        if !hand.observed {
            reactions::remove(state, fx, room, message, Meaning::RaisedHand);
        }
        return;
    }
    let answering = if hand.observed {
        Answering::Observed
    } else {
        Answering::RaisedHand
    };
    answer(state, room, message, answering, now, fx);
}

/// A human reacted to a question the agent raised a hand on: answer now,
/// unless someone already has or the reaction was a 🤫. Returns whether the
/// reaction was about a raised hand.
pub(crate) fn on_question_reaction(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let raised = state
        .hands
        .get(message)
        .is_some_and(|h| &h.room == room && !h.observed);
    if !raised || !state.is_human(room, user) {
        return false;
    }
    if reactions::means(state, room, emoji, Meaning::Shush) {
        shush(state, room, user, message, now, fx);
        return true;
    }
    let Some(hand) = state.hands.remove(message) else {
        return false;
    };
    timers::cancel(state, fx, &hand.timer);
    if answered(state, room, message) {
        reactions::remove(state, fx, room, message, Meaning::RaisedHand);
        return true;
    }
    answer(state, room, message, Answering::RaisedHand, now, fx);
    true
}

/// Asks the model to answer `message` as the last turn, threaded to it.
fn answer(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    answering: Answering,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(asker) = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message))
        .map(|m| m.author.clone())
    else {
        return;
    };
    let role = resolve_role(state, room, &asker);
    let score = state
        .rooms
        .get(room)
        .and_then(|r| r.score_of(message))
        .cloned();
    let ask = Ask {
        room: room.clone(),
        message: message.clone(),
        speaker: asker,
        role,
        answering,
        score,
    };
    ask_model(state, ask, now, fx);
}

/// A 🤫 or "shut up" from `by`: nothing unprompted for the rest of the day,
/// their plans lowered for a week, every hand in the room down, and a 👍 on
/// `ack` and nothing more.
pub(crate) fn shush(
    state: &mut GroupState,
    room: &PlatformRoomId,
    by: &PlatformUserId,
    ack: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    stop_today(state, now);
    let until = now.plus_seconds(SHUSH_SECONDS);
    state.shushed.insert(by.clone(), until);
    stand_down(state, room, fx);
    reactions::add(state, fx, room, ack, Meaning::ThumbsUp);
    let name = state.display_name(room, by);
    fx.push(Effect::Notice(format!(
        "Shushed by {name} in {}: nothing unprompted for the rest of today, and {name}'s plans weigh less until {}.",
        state.room_label(room),
        stamp(until, &state.timezone)
    )));
}

/// Drops every raised or pending hand in `room`.
fn stand_down(state: &mut GroupState, room: &PlatformRoomId, fx: &mut Vec<Effect>) {
    timers::cancel_where(
        state,
        fx,
        |p| matches!(p, TimerPurpose::RaiseHand { room: r, .. } if r == room),
    );
    let raised: Vec<PlatformMessageId> = state
        .hands
        .iter()
        .filter(|(_, h)| &h.room == room)
        .map(|(id, _)| id.clone())
        .collect();
    for id in raised {
        if let Some(hand) = state.hands.remove(&id) {
            timers::cancel(state, fx, &hand.timer);
            if !hand.observed {
                reactions::remove(state, fx, room, &id, Meaning::RaisedHand);
            }
        }
    }
}

pub(crate) const OBSERVE_RULE: &str =
    "observe mode: what I would have done goes to leads and hosts by DM";
pub(crate) const RAISED_HAND_RULE: &str =
    "reaction first: a 🙋 after the answer window, an answer after a reaction or the window again";

/// The trace of anything done about an unanswered question.
pub(crate) fn unanswered_trace(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    rule: &str,
) -> Trace {
    let held = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message));
    let trigger = match held {
        Some(m) => format!(
            "{} asked in {} and nobody answered: {}",
            state.display_name(room, &m.author),
            state.room_label(room),
            quote(&m.text, 80)
        ),
        None => format!("a question in {} nobody answered", state.room_label(room)),
    };
    let score = state
        .rooms
        .get(room)
        .and_then(|r| r.score_of(message))
        .cloned();
    Trace::new(trigger, rule, now).scored(score)
}

/// "In #dinner I would have reacted 🙋 to Sam: what time was it again",
/// with what it would have said when there is a reply.
pub(crate) fn would_have(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    reply: Option<&str>,
) -> Option<String> {
    let held = state
        .rooms
        .get(room)?
        .messages
        .iter()
        .find(|m| &m.id == message)?;
    let speaker = state.display_name(room, &held.author);
    let said = shorten(&render_mentions(state, room, &held.text), 200);
    let label = state.room_label(room);
    let mut lines = vec![match reply {
        None => format!(
            "In {label} I would have reacted {} to {speaker}: {said}",
            reactions::emoji_for(state, room, Meaning::RaisedHand)
        ),
        Some(_) => format!("In {label} I would have replied to {speaker}: {said}"),
    }];
    if let Some(reply) = reply {
        lines.push(format!("I'd have said: {}", shorten(reply, 400)));
    }
    lines.push(reactions::feedback_line(state, room));
    Some(lines.join("\n"))
}

fn answered(state: &GroupState, room: &PlatformRoomId, message: &PlatformMessageId) -> bool {
    state
        .rooms
        .get(room)
        .is_none_or(|r| r.answered_since(message, |u| state.is_human(room, u)))
}

fn asker_name(state: &GroupState, room: &PlatformRoomId, message: &PlatformMessageId) -> String {
    state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message))
        .map_or_else(
            || "someone".to_owned(),
            |m| state.display_name(room, &m.author),
        )
}

/// Whether `emoji` is the canonical shush. A room's stand-in is read by
/// `reactions::means`.
pub fn is_shush(emoji: &str) -> bool {
    reactions::is_canonical(emoji, Meaning::Shush)
}
