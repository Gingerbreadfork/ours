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
use crate::state::{DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, GroupState};
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
pub const WEEK_SECONDS: i64 = 7 * DAY_SECONDS;
/// Why a change to memory is not undone.
pub const FORGOTTEN_NOTE: &str = "what is forgotten is gone; say remember that to keep it again";

/// A change to facts someone asked for, carried out at once or after a
/// lead's ✅.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryChange {
    Forget { facts: Vec<FactId> },
    Expire { facts: Vec<FactId>, at: Instant },
}

impl MemoryChange {
    fn facts(&self) -> &[FactId] {
        match self {
            MemoryChange::Forget { facts } | MemoryChange::Expire { facts, .. } => facts,
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
    redact(state, fx, said, Some(user.clone()), &message.id);
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
    crate::tab::forget(state, &user);
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

/// Asks the binary to scrub `messages`, and with `user` all their words,
/// from the journal, and drops the same messages from the rings so the
/// state matches what a replay of the scrubbed journal holds. The request
/// itself stays in the ring so its reply still names who asked.
fn redact(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    messages: Vec<(PlatformRoomId, PlatformMessageId)>,
    user: Option<PlatformUserId>,
    request: &PlatformMessageId,
) {
    if messages.is_empty() && user.is_none() {
        return;
    }
    // The words as a trace or an action record would quote them.
    let quoted: Vec<String> = state
        .rooms
        .iter()
        .flat_map(|(room_id, room)| room.messages.iter().map(move |m| (room_id, m)))
        .filter(|(room_id, m)| {
            m.id != *request
                && (user.as_ref() == Some(&m.author)
                    || messages.iter().any(|(r, id)| r == *room_id && id == &m.id))
        })
        .map(|(_, m)| crate::trace::shorten(&m.text, 80))
        .filter(|s| !s.is_empty())
        .collect();
    for (room_id, room) in &mut state.rooms {
        room.messages.retain(|m| {
            &m.id == request || !messages.iter().any(|(r, id)| r == room_id && id == &m.id)
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
    let trigger = addressed_trigger(state, message);
    match disposition(state, change.facts(), &message.author, role) {
        Disposition::Do => {
            let line = apply(state, &change, &message.author, &message.id, now, fx);
            let trace = Trace::new(
                trigger,
                "anyone may change their own facts; a lead anyone's",
                now,
            );
            let note = Recording::Note {
                why: FORGOTTEN_NOTE.to_owned(),
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
        Disposition::Defer => {
            let deferral = Deferral {
                room: room.clone(),
                asked_by: message.author.clone(),
                request: message.id.clone(),
                request_text: message.text.clone(),
                change: DeferredChange::Memory { change },
                expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
            };
            let trace = Trace::new(
                trigger,
                "a change to someone else's facts waits for a lead's ✅",
                now,
            );
            leads::open_deferral(state, fx, deferral, now, trace);
        }
        Disposition::Decline => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests may change only their own facts", now);
            send(state, fx, room, line, Some(message.id.clone()), now, trace);
        }
    }
}

/// Applies a change asked for in `request` and returns the one line that
/// says so. A forget also scrubs the messages the facts came from.
pub(crate) fn apply(
    state: &mut GroupState,
    change: &MemoryChange,
    by: &PlatformUserId,
    request: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    match change {
        MemoryChange::Forget { facts } => {
            let sources = memory::source_messages(state, facts);
            memory::retract(state, facts, FORGET_THAT, Some(by), now);
            redact(state, fx, sources, None, request);
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
    }
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
    let corrected = match &message.reply_to {
        Some(parent) => Some(parent.clone()),
        None => state
            .rooms
            .get(&message.room)
            .and_then(|r| r.traces.back())
            .map(|(id, _)| id.clone()),
    };
    let facts = match target(state, message) {
        Target::Own(facts) | Target::Message(facts) => facts,
        Target::Alone => state
            .rooms
            .get(&message.room)
            .and_then(|r| r.last_trace())
            .map(|t| unretracted(state, t.facts.clone()))
            .unwrap_or_default(),
    };
    if role == Role::Guest && disposition(state, &facts, &message.author, role) != Disposition::Do {
        let line = decline_line(state, &message.room);
        say(
            state,
            message,
            &line,
            "guests may correct only their own facts",
            now,
            fx,
        );
        return false;
    }
    let sources = memory::source_messages(state, &facts);
    let dropped = memory::retract(state, &facts, CORRECTED, Some(&message.author), now);
    redact(state, fx, sources, None, &message.id);
    if dropped > 0 {
        let by = state.display_name(&message.room, &message.author);
        fx.push(Effect::Notice(format!(
            "Memory: {by} corrected {}.",
            count(dropped)
        )));
    }
    if rest.trim().is_empty() {
        let line = if dropped > 0 { DROPPED } else { NOTHING_CITED };
        say(
            state,
            message,
            line,
            "\"that's wrong\" drops what was cited",
            now,
            fx,
        );
        return false;
    }
    let mut context = context_lines(state, message, EXTRACT_CONTEXT);
    if let Some(parent) = corrected
        && let Some(held) = state
            .rooms
            .get(&message.room)
            .and_then(|r| r.messages.iter().find(|m| m.id == parent))
        && !context.iter().any(|l| l.text == held.text)
    {
        if context.len() >= EXTRACT_CONTEXT {
            context.remove(0);
        }
        context.push(line_of(state, message, held));
    }
    let pending = PendingExtract {
        room: message.room.clone(),
        message: message.id.clone(),
        speaker: message.author.clone(),
        at: message.at,
        text: rest.trim().to_owned(),
        why: ExtractWhy::Correction {
            dropped: u32::try_from(dropped).unwrap_or(u32::MAX),
        },
        reply_to: Some(message.id.clone()),
        guard: None,
        asked_at: now,
    };
    extract::ask(state, fx, pending, context, now);
    true
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
        let pending = remember(message, message, rest, now);
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
            if role == Role::Guest
                && disposition(state, &facts, &message.author, role) != Disposition::Do
            {
                let line = decline_line(state, &message.room);
                say(
                    state,
                    message,
                    &line,
                    "guests may change only their own facts",
                    now,
                    fx,
                );
                return false;
            }
            for id in &facts {
                if let Some(fact) = state.facts.get_mut(id) {
                    fact.expires = None;
                    fact.confidence = fact.confidence.max(STATED);
                }
            }
            say(
                state,
                message,
                KEPT_FOR_GOOD,
                "\"remember that\" keeps the fact for good",
                now,
                fx,
            );
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
    if role == Role::Guest && source.author != message.author {
        let line = decline_line(state, &message.room);
        say(
            state,
            message,
            &line,
            "guests may change only their own facts",
            now,
            fx,
        );
        return false;
    }
    let context = context_lines(state, &source, EXTRACT_CONTEXT);
    let pending = remember(&source, message, &source.text, now);
    extract::ask(state, fx, pending, context, now);
    true
}

/// An extraction of `text` from `source`, answered under `asked`.
fn remember(source: &Message, asked: &Message, text: &str, now: Instant) -> PendingExtract {
    PendingExtract {
        room: source.room.clone(),
        message: source.id.clone(),
        speaker: source.author.clone(),
        at: source.at,
        text: text.to_owned(),
        why: ExtractWhy::Remember,
        reply_to: Some(asked.id.clone()),
        guard: None,
        asked_at: now,
    }
}

fn line_of(state: &GroupState, message: &Message, held: &Message) -> ContextLine {
    let room = &message.room;
    let own =
        state.is_me(&held.author) || state.rooms.get(room).is_some_and(|r| r.is_own(&held.id));
    ContextLine {
        speaker: if own {
            state.agent_name.clone()
        } else {
            state.display_name(room, &held.author)
        },
        text: render_mentions(state, room, &held.text),
        seconds_before: message.at.seconds_since(held.at).max(0),
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
