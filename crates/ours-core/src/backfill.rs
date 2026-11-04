//! Backfill (DESIGN.md section 18, "The group's record"): on a lead's
//! say-so, read the last 90 days of a room once, page by page from the
//! newest back, keep the facts with provenance and no transcript.

use crate::action::{self, ActionClass, ActionRecord, ActionStatus, Origin, Recording, Undo};
use crate::authority::{ActionKind, Disposition, dispose};
use crate::effect::{Effect, send, send_as};
use crate::extract::{self, EXTRACT_CONTEXT, ExtractWhy, PendingExtract};
use crate::leads::{self, decline_line};
use crate::memory::{self, DAY_SECONDS, FactId, UNDONE, day};
use crate::settings::addressed_trigger;
use crate::state::{DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, GroupState};
use crate::timers::{self, TimerPurpose};
use crate::trace::Trace;
use crate::triage::rules_score;
use ours_types::{
    ActionId, ContextLine, Instant, Message, Outbound, OutboundAction, PlatformMessageId,
    PlatformRoomId, PlatformUserId, Role, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::info;

pub const BACKFILL_DAYS: i64 = 90;
pub const PAGE: u32 = 100;
/// Messages read in one backfill at most.
pub const MAX_MESSAGES: u32 = 2000;
/// Extractions asked for in one backfill at most.
pub const MAX_EXTRACTS: u32 = 200;
/// How long a page may take to arrive.
pub const PAGE_GUARD_SECONDS: i64 = 120;
/// How long the last facts may take once every page is in.
pub const EXTRACT_WAIT_SECONDS: i64 = 10 * 60;

pub const START_LINE: &str =
    "Reading the last 90 days of this room; say forget that on anything I get wrong.";
pub const BUSY_LINE: &str = "Already reading this room's history; I'll say when I'm done.";
pub const IN_DM_LINE: &str = "Ask me in the room you want me to read.";
pub const NOTHING_LINE: &str = "Found no history to read in this room.";

/// One room's history being read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Backfill {
    pub started: Instant,
    pub by: PlatformUserId,
    /// The message that asked; the summary answers it.
    pub request: PlatformMessageId,
    /// Nothing older than this is read.
    pub cutoff: Instant,
    /// Still asking for pages.
    pub paging: bool,
    /// The page request in flight.
    #[serde(default)]
    pub fetch: Option<ActionId>,
    #[serde(default)]
    pub guard: Option<TimerId>,
    /// Messages read so far.
    pub read: u32,
    /// When the oldest message read was sent.
    #[serde(default)]
    pub reached: Option<Instant>,
    /// Extractions asked for.
    pub extracts: u32,
    /// New facts kept.
    pub kept: u32,
    /// The newest page read, held until the page before it arrives so its
    /// first lines have context. Dropped when the backfill ends.
    #[serde(default)]
    pub held: Vec<Message>,
    /// The action record that holds every fact it keeps, so one undo takes
    /// them all back.
    #[serde(default)]
    pub record: Option<ActionId>,
}

/// "backfill" from someone in `message.room`: started for a lead or host,
/// deferred for a member, declined for a guest.
pub(crate) fn handle(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    let reply_to = Some(message.id.clone());
    let trigger = addressed_trigger(state, message);
    let in_dm = state.rooms.get(room).is_some_and(|r| r.is_dm());
    if in_dm || state.backfills.contains_key(room) {
        let (line, rule) = if in_dm {
            (IN_DM_LINE, "a backfill reads the room it is asked in")
        } else {
            (BUSY_LINE, "one backfill per room at a time")
        };
        let trace = Trace::new(trigger, rule, now);
        send(state, fx, room, line, reply_to, now, trace);
        return;
    }
    match dispose(role, ActionKind::ChangeMemory) {
        Disposition::Do => start(state, room, &message.author, &message.id, None, now, fx),
        Disposition::Defer => {
            let deferral = Deferral {
                room: room.clone(),
                asked_by: message.author.clone(),
                request: message.id.clone(),
                request_text: message.text.clone(),
                change: DeferredChange::Backfill,
                expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
            };
            let trace = Trace::new(trigger, "a member's backfill waits for a lead's ✅", now);
            leads::open_deferral(state, fx, deferral, now, trace);
        }
        Disposition::Decline => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
    }
}

/// Says one line to the room and asks for the newest page.
pub(crate) fn start(
    state: &mut GroupState,
    room: &PlatformRoomId,
    by: &PlatformUserId,
    request: &PlatformMessageId,
    credit: Option<&str>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if state.backfills.contains_key(room) {
        let trace = Trace::new(
            "a lead approved a backfill",
            "one backfill per room at a time",
            now,
        );
        send(
            state,
            fx,
            room,
            BUSY_LINE,
            Some(request.clone()),
            now,
            trace,
        );
        return;
    }
    let line = match credit {
        Some(credit) => format!("{START_LINE} {credit}"),
        None => START_LINE.to_owned(),
    };
    let name = state.display_name(room, by);
    let trace = Trace::new(
        format!("{name} asked me to read this room's history"),
        "a lead's backfill reads the last 90 days once",
        now,
    );
    let record = state.mint_action();
    let entry = ActionRecord::new(
        record.clone(),
        ActionClass::Fact,
        room.clone(),
        now,
        "what I kept from reading this room's history",
        Undo::RetractFacts { ids: Vec::new() },
    )
    .from_origin(&Origin::asked(by, request));
    action::push(state, entry);
    let as_line = Recording::Line {
        action: record.clone(),
    };
    send_as(
        state,
        fx,
        room,
        line,
        Some(request.clone()),
        now,
        trace,
        as_line,
    );
    state.backfills.insert(
        room.clone(),
        Backfill {
            started: now,
            by: by.clone(),
            request: request.clone(),
            cutoff: now.plus_seconds(-BACKFILL_DAYS * DAY_SECONDS),
            paging: true,
            fetch: None,
            guard: None,
            read: 0,
            reached: None,
            extracts: 0,
            kept: 0,
            held: Vec::new(),
            record: Some(record),
        },
    );
    fetch(state, room, None, now, fx);
    info!(%room, "backfill started");
    fx.push(Effect::Notice(format!(
        "Backfill started in {} by {name}.",
        state.room_label(room)
    )));
}

fn fetch(
    state: &mut GroupState,
    room: &PlatformRoomId,
    before: Option<PlatformMessageId>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let id = state.mint_action();
    fx.push(Effect::Send(Outbound {
        id: id.clone(),
        action: OutboundAction::FetchHistory {
            room: room.clone(),
            limit: PAGE,
            before,
        },
    }));
    let purpose = TimerPurpose::BackfillGuard { room: room.clone() };
    let guard = timers::arm(state, fx, now.plus_seconds(PAGE_GUARD_SECONDS), purpose);
    if let Some(bf) = state.backfills.get_mut(room) {
        bf.fetch = Some(id);
        bf.guard = Some(guard);
    }
}

/// Whether a backfill in `room` is waiting for a page. Pages nobody is
/// waiting for go to the ring buffer.
pub(crate) fn waiting(state: &GroupState, room: &PlatformRoomId) -> bool {
    state.backfills.get(room).is_some_and(|b| b.paging)
}

/// A page of history for a backfill: read what is inside the window, then
/// ask for the page before it or stop.
pub(crate) fn on_history(
    state: &mut GroupState,
    room: &PlatformRoomId,
    messages: Vec<Message>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(bf) = state.backfills.get_mut(room).filter(|b| b.paging) else {
        return;
    };
    bf.fetch = None;
    let guard = bf.guard.take();
    let cutoff = bf.cutoff;
    let cursor = messages.iter().min_by_key(|m| m.at).map(|m| m.id.clone());
    let older = messages.iter().any(|m| m.at < cutoff);
    // Paging counts every message; a card number is never read or held.
    let mut page: Vec<Message> = messages
        .into_iter()
        .filter(|m| m.at >= cutoff && !crate::tab::has_card_number(&m.text))
        .collect();
    page.sort_by_key(|m| m.at);
    let left = usize::try_from(MAX_MESSAGES.saturating_sub(bf.read)).unwrap_or(0);
    if page.len() > left {
        page = page.split_off(page.len() - left);
    }
    bf.read += u32::try_from(page.len()).unwrap_or(0);
    if let Some(first) = page.first() {
        bf.reached = Some(bf.reached.map_or(first.at, |r| r.min(first.at)));
    }
    let full = bf.read >= MAX_MESSAGES;
    let newer = std::mem::take(&mut bf.held);
    if let Some(guard) = guard {
        timers::cancel(state, fx, &guard);
    }
    read(state, room, &page, &newer, now, fx);
    match cursor.filter(|_| !older && !full) {
        Some(before) => {
            if let Some(bf) = state.backfills.get_mut(room) {
                bf.held = page;
            }
            fetch(state, room, Some(before), now, fx);
        }
        None => {
            read(state, room, &[], &page, now, fx);
            stop_paging(state, room, now, fx);
        }
    }
}

/// Asks for the facts in `batch`, oldest first, with context from `older`
/// and the batch itself. Messages the ring holds were heard live.
fn read(
    state: &mut GroupState,
    room: &PlatformRoomId,
    older: &[Message],
    batch: &[Message],
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let all: Vec<&Message> = older.iter().chain(batch.iter()).collect();
    let offset = older.len();
    for (i, message) in batch.iter().enumerate() {
        let Some(bf) = state.backfills.get(room) else {
            return;
        };
        if bf.extracts >= MAX_EXTRACTS {
            return;
        }
        let live = state
            .rooms
            .get(room)
            .is_some_and(|r| r.messages.iter().any(|m| m.id == message.id));
        if live || rules_score(state, room, message).fact < crate::attention::ACT_THRESHOLD {
            continue;
        }
        let before = &all[..offset + i];
        let context = before
            .iter()
            .skip(before.len().saturating_sub(EXTRACT_CONTEXT))
            .map(|m| ContextLine {
                speaker: if state.is_me(&m.author) {
                    state.agent_name.clone()
                } else {
                    state.display_name(room, &m.author)
                },
                text: crate::step::render_mentions(state, room, &m.text),
                seconds_before: message.at.seconds_since(m.at).max(0),
            })
            .collect();
        let pending = PendingExtract {
            room: room.clone(),
            message: message.id.clone(),
            speaker: message.author.clone(),
            at: message.at,
            text: message.text.clone(),
            why: ExtractWhy::Backfill,
            reply_to: None,
            guard: None,
            asked_at: now,
        };
        let asked = state.extractions.len();
        extract::ask(state, fx, pending, context, now);
        if state.extractions.len() > asked
            && let Some(bf) = state.backfills.get_mut(room)
        {
            bf.extracts += 1;
        }
    }
}

fn stop_paging(state: &mut GroupState, room: &PlatformRoomId, now: Instant, fx: &mut Vec<Effect>) {
    if let Some(bf) = state.backfills.get_mut(room) {
        bf.paging = false;
        bf.fetch = None;
        bf.held.clear();
    }
    settle(state, room, now, fx);
}

/// Ends the backfill once every page is in and every extraction answered;
/// until then waits a while for the last of them.
fn settle(state: &mut GroupState, room: &PlatformRoomId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(bf) = state.backfills.get(room) else {
        return;
    };
    if bf.paging {
        return;
    }
    if outstanding(state, room) == 0 {
        finish(state, room, now, fx);
    } else if bf.guard.is_none() {
        let purpose = TimerPurpose::BackfillGuard { room: room.clone() };
        let guard = timers::arm(state, fx, now.plus_seconds(EXTRACT_WAIT_SECONDS), purpose);
        if let Some(bf) = state.backfills.get_mut(room) {
            bf.guard = Some(guard);
        }
    }
}

fn outstanding(state: &GroupState, room: &PlatformRoomId) -> usize {
    state
        .extractions
        .iter()
        .filter(|p| &p.room == room && p.why == ExtractWhy::Backfill)
        .count()
}

/// Facts from one backfilled message are in.
pub(crate) fn on_extracted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    kept: u32,
    new: &[FactId],
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let mut record = None;
    if let Some(bf) = state.backfills.get_mut(room) {
        bf.kept += kept;
        record = bf.record.clone();
    }
    if let Some(id) = record {
        keep_in_record(state, &id, new, now);
    }
    settle(state, room, now, fx);
}

/// Adds facts to the backfill's record; once that was undone, they are
/// retracted as they arrive.
fn keep_in_record(state: &mut GroupState, id: &ActionId, new: &[FactId], now: Instant) {
    let Some(record) = action::find_mut(state, id) else {
        return;
    };
    if let ActionStatus::Undone { by, .. } = &record.status {
        let by = by.clone();
        memory::retract(state, new, UNDONE, by.as_ref(), now);
        return;
    }
    if let Undo::RetractFacts { ids } = &mut record.undo {
        ids.extend(new.iter().cloned());
    }
}

/// A page request the connector could not carry out ends the paging.
pub(crate) fn on_fetch_failed(
    state: &mut GroupState,
    id: &ActionId,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let Some(room) = state
        .backfills
        .iter()
        .find(|(_, b)| b.fetch.as_ref() == Some(id))
        .map(|(r, _)| r.clone())
    else {
        return false;
    };
    let held = state
        .backfills
        .get_mut(&room)
        .map(|b| std::mem::take(&mut b.held))
        .unwrap_or_default();
    if let Some(guard) = state.backfills.get_mut(&room).and_then(|b| b.guard.take()) {
        timers::cancel(state, fx, &guard);
    }
    read(state, &room, &[], &held, now, fx);
    stop_paging(state, &room, now, fx);
    true
}

/// The guard fired: a page never came, or the last facts never did.
pub(crate) fn on_guard(
    state: &mut GroupState,
    room: &PlatformRoomId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(bf) = state.backfills.get_mut(room) else {
        return;
    };
    bf.guard = None;
    if bf.paging {
        let held = std::mem::take(&mut bf.held);
        read(state, room, &[], &held, now, fx);
        fx.push(Effect::Notice(format!(
            "No history page arrived for {} within {PAGE_GUARD_SECONDS} s; stopped reading.",
            state.room_label(room)
        )));
        stop_paging(state, room, now, fx);
        return;
    }
    state
        .extractions
        .retain(|p| !(&p.room == room && p.why == ExtractWhy::Backfill));
    finish(state, room, now, fx);
}

/// "Read N messages back to <date>: kept X facts. Tell me what's wrong."
fn finish(state: &mut GroupState, room: &PlatformRoomId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(bf) = state.backfills.remove(room) else {
        return;
    };
    if let Some(guard) = &bf.guard {
        timers::cancel(state, fx, guard);
    }
    let text = match bf.reached {
        Some(at) if bf.read > 0 => format!(
            "Read {} back to {}: kept {}. Tell me what's wrong.",
            count(bf.read, "message", "messages"),
            day(at, &state.timezone),
            count(bf.kept, "fact", "facts")
        ),
        _ => NOTHING_LINE.to_owned(),
    };
    let trace = Trace::new(
        format!(
            "{} asked me to read this room's history",
            state.display_name(room, &bf.by)
        ),
        "a backfill ends with what it kept",
        now,
    );
    let recording = match &bf.record {
        Some(id) => {
            if let Some(record) = action::find_mut(state, id) {
                record.summary = format!(
                    "{} from reading this room's history",
                    count(bf.kept, "fact", "facts")
                );
            }
            Recording::Line { action: id.clone() }
        }
        None => Recording::Own,
    };
    send_as(
        state,
        fx,
        room,
        text.clone(),
        Some(bf.request),
        now,
        trace,
        recording,
    );
    fx.push(Effect::Notice(format!(
        "Backfill of {} done: {text}",
        state.room_label(room)
    )));
}

fn count(n: u32, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
