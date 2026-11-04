//! A person's rights over what the agent holds on them (DESIGN.md sections
//! 9 and 15): "what do you know about me", "forget me", "forget that",
//! "that's wrong", "remember that" and "just for this week". All handled by
//! rule, with no model call for the sentence itself. Anyone may change
//! their own data; someone else's needs a lead.

use crate::action::Recording;
use crate::attention::context_lines;
use crate::authority::{ActionKind, Disposition, dispose};
use crate::effect::{Effect, send, send_as, send_dm};
use crate::extract::{self, DROPPED, EXTRACT_CONTEXT, ExtractWhy, PendingExtract};
use crate::leads::{self, decline_line};
use crate::memory::{
    self, CORRECTED, DAY_SECONDS, FORGET_ME, FORGET_THAT, Fact, FactId, Place, STATED, Viewer,
    last_day, link, source_label,
};
use crate::reactions::{self, Meaning};
use crate::settings::addressed_trigger;
use crate::state::{
    DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, DmFollowUp, FollowUp, GroupState,
};
use crate::step::render_mentions;
use crate::timers;
use crate::trace::Trace;
use crate::zone;
use ours_types::{
    ContextLine, Instant, Message, PlatformMessageId, PlatformRoomId, PlatformUserId, Role,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const ILL_DM_YOU: &str = "I'll DM you.";
pub const FORGOT_THAT: &str = "Forgot that.";
pub const NOTHING_KEPT: &str = "I kept nothing from that.";
pub const NOTHING_FROM_YOU: &str = "I haven't kept anything from you.";
pub const NOTHING_CITED: &str = "Nothing I remember went into that, so there's nothing to drop.";
pub const NOTHING_HELD: &str = "I don't hold anything about you.";
/// Patterns are not facts and are never reported; the dump says they exist.
pub const PATTERN_LINE: &str = "I also keep the hour you usually reply, for timing only.";
pub const FORGET_ALL_LINE: &str = "Say forget me to clear all of it.";
pub const CANT_SEE: &str = "I can't see that message any more.";
pub const REMEMBER_WHAT: &str = "Say remember that and the fact, or reply to the message with it.";
pub const KEPT_FOR_GOOD: &str = "Keeping that for good.";
/// A lone "that's wrong" whose target is neither recent nor the latest.
pub const WHICH_ONE: &str = "Reply to the message that's wrong and I'll drop what it used.";
pub const WEEK_SECONDS: i64 = 7 * DAY_SECONDS;
/// A lone "that's wrong" means the agent's last message only this soon after it.
pub const LONE_CORRECTION_SECONDS: i64 = 10 * 60;
/// Why a change to memory is not undone.
pub const FORGOTTEN_NOTE: &str = "what is forgotten is gone; say remember that to keep it again";
/// Why "just for this week" is not undone.
pub const EXPIRY_NOTE: &str = "say remember that to keep it for good";
/// Why "remember that" is not undone.
pub const KEPT_NOTE: &str = "say forget that or just for this week to change it";

/// A change to facts someone asked for, carried out at once or after a
/// lead's ✅.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryChange {
    Forget {
        facts: Vec<FactId>,
    },
    Expire {
        facts: Vec<FactId>,
        at: Instant,
    },
    /// Kept for good and as stated.
    Keep {
        facts: Vec<FactId>,
    },
    /// Retracted as corrected, with `rest` read as the right version.
    Correct {
        facts: Vec<FactId>,
        #[serde(default)]
        rest: String,
        /// The message the correction answers.
        #[serde(default)]
        corrected: Option<PlatformMessageId>,
    },
    /// Someone else's message, read and kept for good.
    Remember {
        message: PlatformMessageId,
    },
}

impl MemoryChange {
    fn facts(&self) -> &[FactId] {
        match self {
            MemoryChange::Forget { facts }
            | MemoryChange::Expire { facts, .. }
            | MemoryChange::Keep { facts }
            | MemoryChange::Correct { facts, .. } => facts,
            MemoryChange::Remember { .. } => &[],
        }
    }

    /// Why the line reporting it cannot be undone.
    pub fn note(&self) -> &'static str {
        match self {
            MemoryChange::Forget { .. } | MemoryChange::Correct { .. } => FORGOTTEN_NOTE,
            MemoryChange::Expire { .. } => EXPIRY_NOTE,
            MemoryChange::Keep { .. } | MemoryChange::Remember { .. } => KEPT_NOTE,
        }
    }
}

/// What a sentence about memory points at.
enum Target {
    /// One of the agent's own messages: the facts it cited.
    Own(Vec<FactId>),
    /// Someone's message: the facts that came from it.
    Message(Vec<FactId>),
    /// Nothing replied to.
    Alone,
}

fn target(state: &GroupState, message: &Message) -> Target {
    let Some(parent) = &message.reply_to else {
        return Target::Alone;
    };
    let room = state.rooms.get(&message.room);
    if room.is_some_and(|r| r.is_own(parent)) {
        let cited = room
            .and_then(|r| r.trace_of(parent))
            .map(|t| t.facts.clone())
            .unwrap_or_default();
        Target::Own(unretracted(state, cited))
    } else {
        Target::Message(memory::from_message(state, &message.room, parent))
    }
}

fn unretracted(state: &GroupState, ids: Vec<FactId>) -> Vec<FactId> {
    ids.into_iter()
        .filter(|id| state.facts.get(id).is_some_and(|f| f.retracted.is_none()))
        .collect()
}

/// Whether `user` may change these facts directly, must wait for a lead,
/// or is declined.
fn disposition(
    state: &GroupState,
    facts: &[FactId],
    user: &PlatformUserId,
    role: Role,
) -> Disposition {
    let theirs = facts
        .iter()
        .all(|id| state.facts.get(id).is_some_and(|f| f.concerns(user)));
    if theirs {
        Disposition::Do
    } else {
        dispose(role, ActionKind::ChangeMemory)
    }
}

/// "what do you know about me": the full dump, by DM.
pub(crate) fn what_do_you_know(
    state: &mut GroupState,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let user = &message.author;
    let text = dump(state, user, now);
    let trace = Trace::new(
        addressed_trigger(state, message),
        "\"what do you know about me\" is answered in full, by DM",
        now,
    );
    let room = &message.room;
    if state.rooms.get(room).is_some_and(|r| r.is_dm()) {
        send(state, fx, room, text, Some(message.id.clone()), now, trace);
    } else {
        send(
            state,
            fx,
            room,
            ILL_DM_YOU,
            Some(message.id.clone()),
            now,
            trace.clone(),
        );
        send_dm(state, fx, user, text, now, trace, false);
    }
    fx.push(Effect::Notice(format!(
        "Sent {} what I hold about them.",
        state.display_name(room, user)
    )));
}

/// Every fact about `user` they may see, lasting ones first, each with its
/// source. Patterns are mentioned, never reported.
pub fn dump(state: &GroupState, user: &PlatformUserId, now: Instant) -> String {
    let viewer = Viewer::new(state, Place::Dm(user.clone()));
    let facts: Vec<&Fact> = state
        .facts
        .values()
        .filter(|f| f.retracted.is_none() && f.concerns(user) && viewer.may_see(f))
        .collect();
    let (mut lasting, mut perishable): (Vec<&Fact>, Vec<&Fact>) =
        facts.into_iter().partition(|f| f.is_durable());
    lasting.sort_by(|a, b| a.first_seen.cmp(&b.first_seen).then(a.id.cmp(&b.id)));
    perishable.sort_by(|a, b| a.expires.cmp(&b.expires).then(a.id.cmp(&b.id)));
    let any_facts = !lasting.is_empty() || !perishable.is_empty();
    let patterns = state.patterns.members.contains_key(user);
    let mut lines = vec![if any_facts {
        "Everything I hold about you:".to_owned()
    } else {
        NOTHING_HELD.to_owned()
    }];
    for (title, group) in [("Lasting:", &lasting), ("For now:", &perishable)] {
        if group.is_empty() {
            continue;
        }
        lines.push(title.to_owned());
        lines.extend(group.iter().map(|f| dump_line(state, &viewer, f, now)));
    }
    if patterns {
        lines.push(PATTERN_LINE.to_owned());
    }
    if any_facts || patterns {
        lines.push(FORGET_ALL_LINE.to_owned());
    }
    lines.join("\n")
}

fn dump_line(state: &GroupState, viewer: &Viewer, fact: &Fact, now: Instant) -> String {
    let until = match fact.expires {
        Some(at) if at <= now => format!(", expired {}", last_day(at, &state.timezone)),
        Some(at) => format!(", until {}", last_day(at, &state.timezone)),
        None => String::new(),
    };
    let source = viewer.source(fact).map_or_else(String::new, |s| {
        let at = link(state, s).unwrap_or_else(|| format!("msg {}", s.message));
        format!(" ({}, {at})", source_label(state, s))
    });
    format!("{}{until}{source}", fact.line())
}

/// "forget me": every fact with them as subject or source is retracted,
/// their words leave the journal and the rings, their reply hour is
/// dropped, and one line says so without detail.
pub(crate) fn forget_me(
    state: &mut GroupState,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let user = message.author.clone();
    let ids: Vec<FactId> = state
        .facts
        .values()
        .filter(|f| f.retracted.is_none() && f.concerns(&user))
        .map(|f| f.id.clone())
        .collect();
    let mut said = held_from(state, &user);
    for key in memory::source_messages(state, &ids) {
        if !said.contains(&key) {
            said.push(key);
        }
    }
    let n = memory::retract(state, &ids, FORGET_ME, Some(&user), now);
    redact(state, fx, said, Some(user.clone()), Some(&message.id));
    let (theirs, rest): (Vec<PendingExtract>, Vec<PendingExtract>) =
        std::mem::take(&mut state.extractions)
            .into_iter()
            .partition(|p| p.speaker == user);
    state.extractions = rest;
    for pending in theirs {
        if let Some(guard) = &pending.guard {
            timers::cancel(state, fx, guard);
        }
        if let Some(asked) = &pending.reply_to {
            reactions::remove(state, fx, &pending.room, asked, Meaning::Eyes);
        }
    }
    state.patterns.forget(&user);
    state.zones.remove(&user);
    crate::tab::forget(state, &user, now);
    crate::commitment::forget(state, &user, now, fx);
    let name = state.display_name(&message.room, &user);
    let trace = Trace::new(
        addressed_trigger(state, message),
        "\"forget me\" retracts everything about them, in one line without detail",
        now,
    );
    let line = format!("Forgot what I knew about {name}, as asked.");
    let note = Recording::Note {
        why: FORGOTTEN_NOTE.to_owned(),
    };
    send_as(
        state,
        fx,
        &message.room,
        line,
        Some(message.id.clone()),
        now,
        trace,
        note,
    );
    fx.push(Effect::Notice(format!(
        "Forgot {n} {} about {name}, as they asked.",
        if n == 1 { "fact" } else { "facts" }
    )));
}

/// What a trace or record says once the words it quoted are forgotten.
pub const FORGOTTEN_TRIGGER: &str = "a message since forgotten";
/// Shorter words are matched only by message id; inside other text they
/// cannot be told from ordinary words.
const QUOTE_MIN_CHARS: usize = 16;

/// Every message `user` wrote that a room's ring still holds, by room.
fn held_from(
    state: &GroupState,
    user: &PlatformUserId,
) -> Vec<(PlatformRoomId, PlatformMessageId)> {
    state
        .rooms
        .iter()
        .flat_map(|(id, r)| r.messages.iter().map(move |m| (id, m)))
        .filter(|(_, m)| &m.author == user)
        .map(|(id, m)| (id.clone(), m.id.clone()))
        .collect()
}

/// Takes one message out of its room and asks the binary to scrub it from
/// the journal, with any trace or record that quotes it.
pub(crate) fn redact_message(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) {
    redact(state, fx, vec![(room.clone(), message.clone())], None, None);
}

/// Asks the binary to scrub `messages`, and with `user` all their words,
/// from the journal, and drops the same messages from the rings so the
/// state matches what a replay of the scrubbed journal holds. The request
/// in `keep` stays in the ring so its reply still names who asked.
fn redact(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    messages: Vec<(PlatformRoomId, PlatformMessageId)>,
    user: Option<PlatformUserId>,
    keep: Option<&PlatformMessageId>,
) {
    if messages.is_empty() && user.is_none() {
        return;
    }
    let gone: Vec<&Message> = state
        .rooms
        .iter()
        .flat_map(|(room_id, room)| room.messages.iter().map(move |m| (room_id, m)))
        .filter(|(room_id, m)| {
            Some(&m.id) != keep
                && (user.as_ref() == Some(&m.author)
                    || messages.iter().any(|(r, id)| r == *room_id && id == &m.id))
        })
        .map(|(_, m)| m)
        .collect();
    let words: Vec<String> = gone
        .iter()
        .map(|m| m.text.trim().to_owned())
        .filter(|t| t.chars().count() >= QUOTE_MIN_CHARS)
        .collect();
    // The words as a trace or an action record would quote them.
    let quoted: Vec<String> = gone
        .iter()
        .map(|m| crate::trace::shorten(&m.text, 80))
        .filter(|s| !s.is_empty())
        .collect();
    let spent = |request: &PlatformMessageId, by: &PlatformUserId, text: &str| {
        messages.iter().any(|(_, id)| id == request)
            || user.as_ref() == Some(by)
            || words.iter().any(|w| text.contains(w.as_str()))
    };
    // A deferral that quotes the words would put them back when approved.
    state
        .deferrals
        .retain(|_, d| !spent(&d.request, &d.asked_by, &d.request_text));
    for pending in state.pending_actions.values_mut() {
        if let Some(FollowUp::OpenDeferral { deferral: d }) = &pending.then
            && spent(&d.request, &d.asked_by, &d.request_text)
        {
            pending.then = None;
        }
    }
    for pending in state.pending_dms.values_mut() {
        if let Some(DmFollowUp::Request { deferral: d }) = &pending.then
            && spent(&d.request, &d.asked_by, &d.request_text)
        {
            pending.then = None;
        }
    }
    for (room_id, room) in &mut state.rooms {
        room.messages.retain(|m| {
            Some(&m.id) == keep || !messages.iter().any(|(r, id)| r == room_id && id == &m.id)
        });
        for (_, trace) in &mut room.traces {
            if quoted.iter().any(|q| trace.trigger.contains(q.as_str())) {
                trace.trigger = FORGOTTEN_TRIGGER.to_owned();
            }
        }
    }
    for record in &mut state.actions {
        if quoted.iter().any(|q| record.summary.contains(q.as_str())) {
            record.summary = FORGOTTEN_TRIGGER.to_owned();
        }
        if let Some(outcome) = &record.outcome
            && quoted.iter().any(|q| outcome.contains(q.as_str()))
        {
            record.outcome = Some(FORGOTTEN_TRIGGER.to_owned());
        }
    }
    let mut ids: Vec<PlatformMessageId> = Vec::new();
    for (_, id) in messages {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    fx.push(Effect::Redact {
        messages: ids,
        user,
    });
}

/// "forget that": the facts from the message replied to, the facts one of
/// the agent's messages cited, or alone the last ones from the speaker.
pub(crate) fn forget_that(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let (facts, nothing) = match target(state, message) {
        Target::Own(facts) => (facts, NOTHING_CITED),
        Target::Message(facts) => (facts, NOTHING_KEPT),
        Target::Alone => (
            memory::latest_from(state, &message.author),
            NOTHING_FROM_YOU,
        ),
    };
    if facts.is_empty() {
        say(
            state,
            message,
            nothing,
            "\"forget that\" found nothing to forget",
            now,
            fx,
        );
        return;
    }
    act(
        state,
        message,
        role,
        MemoryChange::Forget { facts },
        now,
        fx,
    );
}

/// "just for this week": the same facts "forget that" would take now
/// expire in seven days.
pub(crate) fn just_this_week(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let (facts, nothing) = match target(state, message) {
        Target::Own(facts) => (facts, NOTHING_CITED),
        Target::Message(facts) => (facts, NOTHING_KEPT),
        Target::Alone => (
            memory::latest_from(state, &message.author),
            NOTHING_FROM_YOU,
        ),
    };
    if facts.is_empty() {
        say(
            state,
            message,
            nothing,
            "\"just for this week\" found nothing to change",
            now,
            fx,
        );
        return;
    }
    let at = now.plus_seconds(WEEK_SECONDS);
    act(
        state,
        message,
        role,
        MemoryChange::Expire { facts, at },
        now,
        fx,
    );
}

/// Carries out a change to facts, or defers or declines it by the rule.
fn act(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    change: MemoryChange,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    match disposition(state, change.facts(), &message.author, role) {
        Disposition::Do => {
            let Some(line) = apply(state, &change, room, &message.author, &message.id, now, fx)
            else {
                return;
            };
            let trace = Trace::new(
                addressed_trigger(state, message),
                "anyone may change their own facts; a lead anyone's",
                now,
            );
            let note = Recording::Note {
                why: change.note().to_owned(),
            };
            send_as(
                state,
                fx,
                room,
                line.clone(),
                Some(message.id.clone()),
                now,
                trace,
                note,
            );
            let by = state.display_name(room, &message.author);
            fx.push(Effect::Notice(format!(
                "Memory: {line} {} by {by}.",
                count(change.facts().len())
            )));
        }
        Disposition::Defer => defer(state, message, change, now, fx),
        Disposition::Decline => decline(state, message, now, fx),
    }
}

/// Replies "Needs a lead's ✅" and waits for one to approve the change.
fn defer(
    state: &mut GroupState,
    message: &Message,
    change: MemoryChange,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let deferral = Deferral {
        room: message.room.clone(),
        asked_by: message.author.clone(),
        request: message.id.clone(),
        request_text: message.text.clone(),
        change: DeferredChange::Memory { change },
        expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
    };
    let trace = Trace::new(
        addressed_trigger(state, message),
        "a change to someone else's facts waits for a lead's ✅",
        now,
    );
    leads::open_deferral(state, fx, deferral, now, trace);
}

fn decline(state: &mut GroupState, message: &Message, now: Instant, fx: &mut Vec<Effect>) {
    let line = decline_line(state, &message.room);
    say(
        state,
        message,
        &line,
        "guests may change only their own facts",
        now,
        fx,
    );
}

/// Applies a change asked for in `request` and returns the one line that
/// says so, or none when the extractor will answer instead. A forget or a
/// correction also scrubs the messages the facts came from.
pub(crate) fn apply(
    state: &mut GroupState,
    change: &MemoryChange,
    room: &PlatformRoomId,
    by: &PlatformUserId,
    request: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> Option<String> {
    let line = match change {
        MemoryChange::Forget { facts } => {
            let sources = memory::source_messages(state, facts);
            memory::retract(state, facts, FORGET_THAT, Some(by), now);
            redact(state, fx, sources, None, Some(request));
            FORGOT_THAT.to_owned()
        }
        MemoryChange::Expire { facts, at } => {
            for id in facts {
                if let Some(fact) = state.facts.get_mut(id)
                    && fact.retracted.is_none()
                {
                    fact.expires = Some(*at);
                }
            }
            format!(
                "Keeping that until {}.",
                zone::local(*at, &state.timezone).strftime("%A %Y-%m-%d")
            )
        }
        MemoryChange::Keep { facts } => {
            for id in facts {
                if let Some(fact) = state.facts.get_mut(id)
                    && fact.retracted.is_none()
                {
                    fact.expires = None;
                    fact.confidence = fact.confidence.max(STATED);
                }
            }
            KEPT_FOR_GOOD.to_owned()
        }
        MemoryChange::Correct {
            facts,
            rest,
            corrected,
        } => {
            let asked = Correction {
                room,
                request,
                by,
                rest,
                corrected: corrected.as_ref(),
            };
            let (dropped, waiting) = correct(state, fx, asked, facts, now);
            if waiting {
                return None;
            }
            if dropped > 0 { DROPPED } else { NOTHING_CITED }.to_owned()
        }
        MemoryChange::Remember { message } => {
            let source = state
                .rooms
                .get(room)
                .and_then(|r| r.messages.iter().find(|m| &m.id == message))
                .cloned();
            let Some(source) = source else {
                return Some(CANT_SEE.to_owned());
            };
            let context = context_lines(state, &source, EXTRACT_CONTEXT);
            let pending = remember(&source, request, &source.text, now);
            extract::ask(state, fx, pending, context, now);
            return None;
        }
    };
    Some(line)
}

/// "that's wrong": the facts the agent's message cited are retracted as
/// corrected, and anything said after the phrase is read as the right
/// version, credited to the corrector. Returns whether a reply waits on
/// the extractor.
pub(crate) fn thats_wrong(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    rest: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let last = state
        .rooms
        .get(&message.room)
        .and_then(|r| r.traces.back())
        .map(|(id, t)| (id.clone(), t.clone()));
    let corrected = match &message.reply_to {
        Some(parent) => Some(parent.clone()),
        None => last.as_ref().map(|(id, _)| id.clone()),
    };
    let facts = match target(state, message) {
        Target::Own(facts) | Target::Message(facts) => facts,
        Target::Alone => match last {
            Some((id, trace)) if lone_target(state, message, &id, &trace, now) => {
                unretracted(state, trace.facts)
            }
            Some(_) => {
                say(
                    state,
                    message,
                    WHICH_ONE,
                    "a lone \"that's wrong\" means only a recent or the latest message",
                    now,
                    fx,
                );
                return false;
            }
            None => Vec::new(),
        },
    };
    match disposition(state, &facts, &message.author, role) {
        Disposition::Do => {}
        Disposition::Defer => {
            let change = MemoryChange::Correct {
                facts,
                rest: rest.trim().to_owned(),
                corrected,
            };
            defer(state, message, change, now, fx);
            return false;
        }
        Disposition::Decline => {
            decline(state, message, now, fx);
            return false;
        }
    }
    let asked = Correction {
        room: &message.room,
        request: &message.id,
        by: &message.author,
        rest,
        corrected: corrected.as_ref(),
    };
    let (dropped, waiting) = correct(state, fx, asked, &facts, now);
    if !waiting {
        let line = if dropped > 0 { DROPPED } else { NOTHING_CITED };
        say(
            state,
            message,
            line,
            "\"that's wrong\" drops what was cited",
            now,
            fx,
        );
    }
    waiting
}

/// Whether the agent's last traced message in the room is what a lone
/// "that's wrong" means: sent recently, or the latest message there.
fn lone_target(
    state: &GroupState,
    message: &Message,
    id: &PlatformMessageId,
    trace: &Trace,
    now: Instant,
) -> bool {
    if now.seconds_since(trace.at) <= LONE_CORRECTION_SECONDS {
        return true;
    }
    state.rooms.get(&message.room).is_some_and(|r| {
        r.messages
            .iter()
            .rev()
            .find(|m| m.id != message.id)
            .is_some_and(|m| &m.id == id)
    })
}

/// Who asked for a correction, where, and what they said was right.
struct Correction<'a> {
    room: &'a PlatformRoomId,
    request: &'a PlatformMessageId,
    by: &'a PlatformUserId,
    rest: &'a str,
    corrected: Option<&'a PlatformMessageId>,
}

/// Retracts `facts` as corrected and scrubs their sources, then asks the
/// extractor for the right version when one was said. Returns how many
/// facts were dropped and whether a reply waits on the extractor.
fn correct(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    asked: Correction,
    facts: &[FactId],
    now: Instant,
) -> (usize, bool) {
    let Correction {
        room,
        request,
        by,
        rest,
        corrected,
    } = asked;
    let sources = memory::source_messages(state, facts);
    let dropped = memory::retract(state, facts, CORRECTED, Some(by), now);
    redact(state, fx, sources, None, Some(request));
    if dropped > 0 {
        let name = state.display_name(room, by);
        fx.push(Effect::Notice(format!(
            "Memory: {name} corrected {}.",
            count(dropped)
        )));
    }
    let rest = rest.trim();
    if rest.is_empty() {
        return (dropped, false);
    }
    let held = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == request))
        .cloned();
    let at = held.as_ref().map_or(now, |m| m.at);
    let mut context = match &held {
        Some(m) => context_lines(state, m, EXTRACT_CONTEXT),
        None => Vec::new(),
    };
    if let Some(parent) = corrected
        && let Some(parent) = state
            .rooms
            .get(room)
            .and_then(|r| r.messages.iter().find(|m| &m.id == parent))
        && !context.iter().any(|l| l.text == parent.text)
    {
        if context.len() >= EXTRACT_CONTEXT {
            context.remove(0);
        }
        context.push(line_of(state, room, at, parent));
    }
    let pending = PendingExtract {
        room: room.clone(),
        message: request.clone(),
        speaker: by.clone(),
        at,
        text: rest.to_owned(),
        why: ExtractWhy::Correction {
            dropped: u32::try_from(dropped).unwrap_or(u32::MAX),
        },
        reply_to: Some(request.clone()),
        guard: None,
        asked_at: now,
    };
    extract::ask(state, fx, pending, context, now);
    (dropped, true)
}

/// "remember that": what follows, the message replied to, or alone the
/// message before, read and kept durable and stated. On one of the agent's
/// own messages, the facts it cited are kept for good. Returns whether a
/// reply waits on the extractor.
pub(crate) fn remember_that(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    rest: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let rest = rest.trim();
    if !rest.is_empty() {
        let context = context_lines(state, message, EXTRACT_CONTEXT);
        let pending = remember(message, &message.id, rest, now);
        extract::ask(state, fx, pending, context, now);
        return true;
    }
    let room = state.rooms.get(&message.room);
    let source = match &message.reply_to {
        Some(parent) if room.is_some_and(|r| r.is_own(parent)) => {
            let Target::Own(facts) = target(state, message) else {
                return false;
            };
            if facts.is_empty() {
                say(
                    state,
                    message,
                    REMEMBER_WHAT,
                    "\"remember that\" needs a fact",
                    now,
                    fx,
                );
                return false;
            }
            act(state, message, role, MemoryChange::Keep { facts }, now, fx);
            return false;
        }
        Some(parent) => room.and_then(|r| r.messages.iter().find(|m| &m.id == parent)),
        None => room.and_then(|r| {
            r.messages
                .iter()
                .rev()
                .skip_while(|m| m.id != message.id)
                .skip(1)
                .find(|m| state.is_human(&message.room, &m.author))
        }),
    };
    let Some(source) = source.cloned() else {
        let line = if message.reply_to.is_some() {
            CANT_SEE
        } else {
            REMEMBER_WHAT
        };
        say(
            state,
            message,
            line,
            "\"remember that\" needs a message it can see",
            now,
            fx,
        );
        return false;
    };
    if source.author != message.author {
        match dispose(role, ActionKind::ChangeMemory) {
            Disposition::Do => {}
            Disposition::Defer => {
                let change = MemoryChange::Remember { message: source.id };
                defer(state, message, change, now, fx);
                return false;
            }
            Disposition::Decline => {
                decline(state, message, now, fx);
                return false;
            }
        }
    }
    let context = context_lines(state, &source, EXTRACT_CONTEXT);
    let pending = remember(&source, &message.id, &source.text, now);
    extract::ask(state, fx, pending, context, now);
    true
}

/// An extraction of `text` from `source`, answered under `asked`.
fn remember(
    source: &Message,
    asked: &PlatformMessageId,
    text: &str,
    now: Instant,
) -> PendingExtract {
    PendingExtract {
        room: source.room.clone(),
        message: source.id.clone(),
        speaker: source.author.clone(),
        at: source.at,
        text: text.to_owned(),
        why: ExtractWhy::Remember,
        reply_to: Some(asked.clone()),
        guard: None,
        asked_at: now,
    }
}

/// `held` as a line of context for something said at `at` in `room`.
fn line_of(state: &GroupState, room: &PlatformRoomId, at: Instant, held: &Message) -> ContextLine {
    let own =
        state.is_me(&held.author) || state.rooms.get(room).is_some_and(|r| r.is_own(&held.id));
    ContextLine {
        speaker: if own {
            state.agent_name.clone()
        } else {
            state.display_name(room, &held.author)
        },
        text: render_mentions(state, room, &held.text),
        seconds_before: at.seconds_since(held.at).max(0),
    }
}

fn say(
    state: &mut GroupState,
    message: &Message,
    line: &str,
    rule: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let trace = Trace::new(addressed_trigger(state, message), rule, now);
    send(
        state,
        fx,
        &message.room,
        line,
        Some(message.id.clone()),
        now,
        trace,
    );
}

fn count(n: usize) -> String {
    format!("{n} {}", if n == 1 { "fact" } else { "facts" })
}
