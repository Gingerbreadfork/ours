//! The extraction round trip (DESIGN.md sections 9, 11 and 14). The engine
//! asks for the facts, commitments and tab lines in one message with
//! `Effect::Extract`; the binary always answers with `Input::Extracted`,
//! empty when the extractor failed, and the drafts are stored from here.

use crate::action::{self, ActionClass, ActionRecord, Origin, Recording, Undo};
use crate::attention::context_lines;
use crate::backfill;
use crate::commitment;
use crate::effect::{Effect, send, send_as};
use crate::memory::{self, DRAFTS_KEPT, FactId, Source, Stored};
use crate::reactions::{self, Meaning};
use crate::sentence::SELF_WORDS;
use crate::state::GroupState;
use crate::step::{FAILED_REPLY, model_for, render_mentions, trigger_for};
use crate::tab;
use crate::timers::{self, TimerPurpose};
use crate::trace::Trace;
use ours_types::{
    ContextLine, ExtractRequest, Extraction, Instant, Message, PlatformMessageId, PlatformRoomId,
    PlatformUserId, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Earlier lines the extractor sees.
pub const EXTRACT_CONTEXT: usize = 6;
/// How long a "remember that" waits for the extractor.
pub const EXTRACT_GUARD_SECONDS: i64 = 60;
/// Background extractions nobody answered are dropped after this long.
pub const EXTRACT_TTL_SECONDS: i64 = 60 * 60;
/// What a "remember that" says when nothing could be kept.
pub const NOTHING_TO_KEEP: &str = "I couldn't find a fact in that to keep.";
/// What a correction says when it dropped facts and found nothing new.
pub const DROPPED: &str = "Dropped that.";

/// An extraction in flight for one message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingExtract {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    /// Who the facts are credited to.
    pub speaker: PlatformUserId,
    /// When the words were said.
    pub at: Instant,
    pub text: String,
    pub why: ExtractWhy,
    /// The addressed message to answer once the facts are in.
    #[serde(default)]
    pub reply_to: Option<PlatformMessageId>,
    #[serde(default)]
    pub guard: Option<TimerId>,
    pub asked_at: Instant,
}

/// Why facts are being pulled out of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtractWhy {
    /// It scored as a fact.
    Heard,
    /// "remember that": kept durable and stated.
    Remember,
    /// "that's wrong" with what is right; `dropped` facts were retracted.
    Correction { dropped: u32 },
    /// Read from the room's history.
    Backfill,
    /// Addressed and sounding like a promise or money paid: the model is
    /// asked only when neither turns up.
    Addressed,
    /// A reply to the group's mail. Its facts are the group's, credited to
    /// the sender; an unverified sender's are only implied.
    Mail { verified: bool },
}

/// Asks for the facts in a message the agent heard.
pub(crate) fn heard(state: &mut GroupState, message: &Message, now: Instant, fx: &mut Vec<Effect>) {
    let context = context_lines(state, message, EXTRACT_CONTEXT);
    let pending = PendingExtract {
        room: message.room.clone(),
        message: message.id.clone(),
        speaker: message.author.clone(),
        at: message.at,
        text: message.text.clone(),
        why: ExtractWhy::Heard,
        reply_to: None,
        guard: None,
        asked_at: now,
    };
    ask(state, fx, pending, context, now);
}

/// Asks for what an addressed promise or payment holds before any model
/// call; the 👀 stays until one of them answers.
pub(crate) fn addressed(
    state: &mut GroupState,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let context = context_lines(state, message, EXTRACT_CONTEXT);
    let pending = PendingExtract {
        room: message.room.clone(),
        message: message.id.clone(),
        speaker: message.author.clone(),
        at: message.at,
        text: message.text.clone(),
        why: ExtractWhy::Addressed,
        reply_to: Some(message.id.clone()),
        guard: None,
        asked_at: now,
    };
    ask(state, fx, pending, context, now);
}

/// Emits the request and remembers it. A message already being read is
/// not asked about twice; a "remember that" takes over the one in flight.
pub(crate) fn ask(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    mut pending: PendingExtract,
    context: Vec<ContextLine>,
    now: Instant,
) {
    if pending.reply_to.is_some() {
        let purpose = TimerPurpose::ExtractGuard {
            room: pending.room.clone(),
            message: pending.message.clone(),
        };
        pending.guard = Some(timers::arm(
            state,
            fx,
            now.plus_seconds(EXTRACT_GUARD_SECONDS),
            purpose,
        ));
    }
    if let Some(at) = state
        .extractions
        .iter()
        .position(|p| p.room == pending.room && p.message == pending.message)
    {
        if pending.reply_to.is_some() {
            let existing = &mut state.extractions[at];
            existing.why = pending.why;
            let asked = std::mem::replace(&mut existing.reply_to, pending.reply_to);
            let guard = std::mem::replace(&mut existing.guard, pending.guard);
            if let Some(guard) = guard {
                timers::cancel(state, fx, &guard);
            }
            // Only the latest asker is answered; the earlier one's 👀 comes off.
            if let Some(asked) = asked {
                reactions::remove(state, fx, &pending.room, &asked, Meaning::Eyes);
            }
        }
        return;
    }
    let request = ExtractRequest {
        room: pending.room.clone(),
        message: pending.message.clone(),
        agent_name: state.agent_name.clone(),
        speaker: state.display_name(&pending.room, &pending.speaker),
        text: render_mentions(state, &pending.room, &pending.text),
        private: state.rooms.get(&pending.room).is_some_and(|r| r.is_dm()),
        context,
        date: memory::day(pending.at, &state.timezone),
        timezone: state.timezone.clone(),
    };
    fx.push(Effect::Extract(request));
    state.extractions.push(pending);
}

/// The extractor's answer: store each draft, a notice per fact, and the
/// one line a "remember that" or a correction is owed.
pub(crate) fn on_extracted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    extraction: Extraction,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Extraction {
        facts: drafts,
        commitments,
        ledger,
    } = extraction;
    let Some(at) = state
        .extractions
        .iter()
        .position(|p| &p.room == room && &p.message == message)
    else {
        debug!(%message, "facts for a message no longer being read");
        return;
    };
    let pending = state.extractions.remove(at);
    if let Some(guard) = &pending.guard {
        timers::cancel(state, fx, guard);
    }
    let label = state.room_label(room);
    let mut lines: Vec<String> = Vec::new();
    let mut kept = 0;
    let mut new: Vec<FactId> = Vec::new();
    let weak = pending.why == ExtractWhy::Mail { verified: false };
    for mut draft in drafts.into_iter().take(DRAFTS_KEPT) {
        if pending.why == ExtractWhy::Remember {
            draft.durable = true;
            draft.stated = true;
            draft.expires = None;
        }
        if let ExtractWhy::Mail { verified } = pending.why {
            draft.about_self = false;
            draft.stated &= verified;
            if SELF_WORDS.contains(&draft.subject.trim().to_lowercase().as_str()) {
                draft.subject = pending.speaker.to_string();
            }
        }
        let source = Source {
            room: room.clone(),
            message: message.clone(),
            speaker: pending.speaker.clone(),
            at: pending.at,
        };
        let (id, verb) = match memory::store(state, source, &pending.text, draft, weak, now) {
            Some(Stored::New(id)) => (id, "remembered"),
            Some(Stored::Reconfirmed(id)) => (id, "reconfirmed"),
            None => continue,
        };
        let Some(fact) = state.facts.get(&id) else {
            continue;
        };
        if verb == "remembered" && !fact.is_expired(now) {
            kept += 1;
        }
        if verb == "remembered" {
            new.push(id.clone());
        }
        let line = fact.line();
        fx.push(Effect::Notice(format!("{verb}: {line}, from {label}")));
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    if pending.why == ExtractWhy::Backfill {
        backfill::on_extracted(state, room, kept, &new, now, fx);
    }
    let record = (pending.why != ExtractWhy::Backfill && !new.is_empty())
        .then(|| record_facts(state, &pending, &new, now));
    let promises = matches!(
        pending.why,
        ExtractWhy::Heard | ExtractWhy::Remember | ExtractWhy::Addressed
    );
    let mut answered = false;
    if promises {
        let tab_lines = tab::store_drafts(state, &pending, ledger, now, fx);
        let kept = commitment::store_drafts(state, &pending, commitments, &tab_lines, now, fx);
        answered = !tab_lines.is_empty() || kept > 0;
    }
    let Some(reply_to) = pending.reply_to else {
        return;
    };
    if pending.why == ExtractWhy::Addressed && !answered {
        model_for(state, room, &reply_to, &pending.speaker, now, fx);
        return;
    }
    // A promise or a tab line already answered with its own 📌 or line.
    if answered && (pending.why == ExtractWhy::Addressed || lines.is_empty()) {
        reactions::remove(state, fx, room, &reply_to, Meaning::Eyes);
        return;
    }
    let text = match (pending.why, lines.is_empty()) {
        (ExtractWhy::Correction { .. }, false) => format!("Corrected: {}.", lines.join("; ")),
        (ExtractWhy::Correction { dropped }, true) if dropped > 0 => DROPPED.to_owned(),
        (_, false) => format!("Noted: {}.", lines.join("; ")),
        (_, true) => NOTHING_TO_KEEP.to_owned(),
    };
    let rule = match pending.why {
        ExtractWhy::Correction { .. } => {
            "\"that's wrong\" drops what was cited and keeps the correction"
        }
        _ => "\"remember that\" keeps the fact for good",
    };
    let recording = match (record, lines.is_empty()) {
        (Some(action), _) => Recording::Line { action },
        (None, false) => Recording::Note {
            why: "I already knew it; say forget that to drop it".to_owned(),
        },
        (None, true) if text == DROPPED => Recording::Note {
            why: "what was dropped stays dropped".to_owned(),
        },
        (None, true) => Recording::Unrecorded,
    };
    let trace = Trace::new(trigger_for(state, room, &reply_to), rule, now);
    send_as(
        state,
        fx,
        room,
        text,
        Some(reply_to.clone()),
        now,
        trace,
        recording,
    );
    reactions::remove(state, fx, room, &reply_to, Meaning::Eyes);
}

/// One record for the facts one message gave, undone by retracting them.
/// Only those with a line in the room are what a bare "undo" may mean.
fn record_facts(
    state: &mut GroupState,
    pending: &PendingExtract,
    ids: &[FactId],
    now: Instant,
) -> ours_types::ActionId {
    let id = state.mint_action();
    let summary = facts_summary(state, ids);
    let mail = matches!(pending.why, ExtractWhy::Mail { .. });
    let origin = if mail {
        Origin::default()
    } else {
        Origin::asked(&pending.speaker, &pending.message)
    };
    let mut record = ActionRecord::new(
        id.clone(),
        ActionClass::Fact,
        pending.room.clone(),
        now,
        summary,
        Undo::RetractFacts { ids: ids.to_vec() },
    )
    .from_origin(&origin);
    record.visible = pending.reply_to.is_some() && !mail;
    action::push(state, record);
    id
}

/// "the fact Priya is vegetarian", "3 facts: a; b; c". A forgotten fact
/// keeps no words here either.
pub(crate) fn facts_summary(state: &GroupState, ids: &[FactId]) -> String {
    let facts: Vec<&memory::Fact> = ids.iter().filter_map(|id| state.facts.get(id)).collect();
    let line = |f: &memory::Fact| {
        if f.is_forgotten() {
            FORGOTTEN_FACT.to_owned()
        } else {
            f.line()
        }
    };
    match facts.as_slice() {
        [one] if one.is_forgotten() => FORGOTTEN_FACT.to_owned(),
        [one] => format!("the fact {}", one.line()),
        many => {
            let lines: Vec<String> = many.iter().map(|f| line(f)).collect();
            crate::trace::shorten(&format!("{} facts: {}", lines.len(), lines.join("; ")), 120)
        }
    }
}

/// How a removed fact appears in the action record.
pub const FORGOTTEN_FACT: &str = "a forgotten fact";

/// The extractor did not answer a "remember that" in time: say so.
pub(crate) fn on_guard(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(at) = state
        .extractions
        .iter()
        .position(|p| &p.room == room && &p.message == message)
    else {
        return;
    };
    let pending = state.extractions.remove(at);
    let Some(reply_to) = pending.reply_to else {
        return;
    };
    if pending.why == ExtractWhy::Addressed {
        model_for(state, room, &reply_to, &pending.speaker, now, fx);
        return;
    }
    let trace = Trace::new(
        trigger_for(state, room, &reply_to),
        "a failed extraction is owned up to",
        now,
    );
    send(
        state,
        fx,
        room,
        FAILED_REPLY,
        Some(reply_to.clone()),
        now,
        trace,
    );
    reactions::remove(state, fx, room, &reply_to, Meaning::Eyes);
    fx.push(Effect::Notice(format!(
        "The extractor did not answer within {EXTRACT_GUARD_SECONDS} s; said so in the room."
    )));
}
