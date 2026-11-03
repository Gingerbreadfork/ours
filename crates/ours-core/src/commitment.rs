//! Commitments (DESIGN.md section 11): who said they would do what, for
//! whom and by when. They come from the extractor, are confirmed with a 📌,
//! nudged by DM the day before and at due and never a third time, and
//! listed for anyone in the room they were made in.

use crate::action::{self, ActionClass, ActionRecord, Origin, Undo};
use crate::away;
use crate::decision::visible;
use crate::effect::{Effect, send, send_dm_then};
use crate::extract::PendingExtract;
use crate::memory::{Place, Source, resolve_subject};
use crate::patterns::usual_hour;
use crate::proposal::{DecisionId, capitalise, seq_id};
use crate::reactions::{self, Meaning};
use crate::sentence::SELF_WORDS;
use crate::settings::addressed_trigger;
use crate::state::{DmFollowUp, GroupState};
use crate::tab::LedgerId;
use crate::time::{self, at_on, date_of, day_echo, day_end, days_from, resolve_time_rules};
use crate::timers::{self, DueAt, TimerPurpose};
use crate::trace::{Trace, shorten};
use crate::zone::{self, zone_of};
use jiff::civil::Date;
use ours_types::{
    CommitmentDraft, Instant, Message, PlatformMessageId, PlatformRoomId, PlatformUserId,
    TimeMeaning,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::debug;

seq_id!(
    /// A commitment's id, minted from the group's sequence.
    CommitmentId
);

/// The reaction that says a commitment was kept.
pub const PIN: &str = "📌";
/// Confidence of a commitment said outright.
pub const STATED: f32 = 0.9;
/// Confidence of one only implied.
pub const IMPLIED: f32 = 0.6;
/// The local hour a nudge goes out when the person has no usual hour yet.
pub const NUDGE_HOUR: i8 = 9;
/// Lines in one answer before "and N more".
pub const LINES: usize = 6;
/// The question every nudge ends with.
pub const NUDGE_ASK: &str = "Done, slipping, or drop it?";
pub const ASK_WHEN: &str = "To when? Say a day, like Friday.";

const NUDGE_RULE: &str = "one nudge the day before and one at due, by DM, never a third";
const OUTSTANDING_RULE: &str = "commitments made in a room are listed for anyone in it";

/// Whom a commitment is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ForWhom {
    Member { user: PlatformUserId },
    Group,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommitmentStatus {
    Open,
    Done {
        at: Instant,
    },
    /// Slipped to `to` and still open.
    Moved {
        to: Instant,
        #[serde(default)]
        at: Option<Instant>,
    },
    Dropped {
        at: Instant,
        by: Option<PlatformUserId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Commitment {
    pub id: CommitmentId,
    pub who: PlatformUserId,
    /// What they will do, short: "draft to legal", "bring dessert".
    pub what: String,
    pub for_whom: Option<ForWhom>,
    /// When it is due; a date alone is the end of that day.
    pub due: Option<Instant>,
    pub due_has_time: bool,
    /// The calendar entry it belongs to, by its decision.
    pub event: Option<DecisionId>,
    pub status: CommitmentStatus,
    pub source: Source,
    pub created_at: Instant,
    pub confidence: f32,
    pub quote: Option<String>,
    /// The tab line it paid for, when it had an amount.
    pub ledger: Option<LedgerId>,
    /// Nudges sent; never more than two.
    pub nudged: u8,
    /// The nudge DMs, so a reply or reaction there finds this one.
    #[serde(default)]
    pub nudges: Vec<PlatformMessageId>,
    #[serde(default)]
    pub nudged_at: Option<Instant>,
    /// Messages in the room that moved it.
    #[serde(default)]
    pub moved_by: Vec<PlatformMessageId>,
    /// When its status or due date last changed, for the digest.
    #[serde(default)]
    pub changed_at: Option<Instant>,
}

impl Commitment {
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            CommitmentStatus::Open | CommitmentStatus::Moved { .. }
        )
    }

    /// Whether it may be told at `place`: in the room it was made in, or a
    /// DM with someone in that room. One made in a DM stays with its people.
    pub fn visible(&self, state: &GroupState, place: &Place) -> bool {
        let private = state.rooms.get(&self.source.room).is_none_or(|r| r.is_dm());
        if private {
            return matches!(place, Place::Dm(u) if *u == self.who || *u == self.source.speaker);
        }
        visible(state, &self.source.room, place)
    }

    /// The day it is due on, in `zone_name`.
    pub fn due_date(&self, zone_name: &str) -> Option<Date> {
        let due = self.due?;
        let at = if self.due_has_time {
            due
        } else {
            due.plus_seconds(-1)
        };
        Some(date_of(at, zone_name))
    }

    /// Whether it is past due at `now`.
    pub fn overdue(&self, now: Instant) -> bool {
        self.is_open() && self.due.is_some_and(|d| d <= now)
    }
}

/// The member a written name means: the speaker for "I", their own name
/// or nothing, else someone in the room.
pub(crate) fn person(
    state: &GroupState,
    room: &PlatformRoomId,
    speaker: &PlatformUserId,
    name: &str,
) -> Option<PlatformUserId> {
    let n = name.trim().to_lowercase();
    if n.is_empty()
        || SELF_WORDS.contains(&n.as_str())
        || n == state.display_name(room, speaker).to_lowercase()
    {
        return Some(speaker.clone());
    }
    resolve_subject(state, room, name)
}

/// A resolved time as a due date: its moment, or the end of its day or span.
pub(crate) fn due_from(meaning: &TimeMeaning, zone_name: &str) -> Option<(Instant, bool)> {
    match meaning {
        TimeMeaning::Instant { at, has_time: true } => Some((*at, true)),
        TimeMeaning::Instant {
            at,
            has_time: false,
        } => Some((day_end(date_of(*at, zone_name), zone_name)?, false)),
        TimeMeaning::Span { end, .. } => Some((*end, false)),
        _ => None,
    }
}

/// The due date in the speaker's words, read by the time rules in their
/// zone, else the extractor's date.
fn due_of(
    draft: &CommitmentDraft,
    text: &str,
    zone_name: &str,
    at: Instant,
) -> (Option<Instant>, bool) {
    for words in [draft.quote.as_deref(), Some(text)].into_iter().flatten() {
        if let Some(found) = resolve_time_rules(words, zone_name, at)
            && let Some((due, has_time)) = due_from(&found.meaning, zone_name)
        {
            return (Some(due), has_time);
        }
    }
    match draft
        .due
        .as_deref()
        .and_then(|d| time::parse_local(d, zone_name))
    {
        Some((at, true)) => (Some(at), true),
        Some((at, false)) => (day_end(date_of(at, zone_name), zone_name), false),
        None => (None, false),
    }
}

/// The calendar entry an event's name or the due day points to.
pub(crate) fn event_of(
    state: &GroupState,
    room: &PlatformRoomId,
    named: Option<&str>,
    due: Option<Instant>,
    now: Instant,
) -> Option<DecisionId> {
    let place = Place::of(state, room);
    let entries: Vec<_> = state
        .calendar
        .iter()
        .filter(|e| e.room.as_ref().is_none_or(|r| visible(state, r, &place)))
        .filter(|e| time::bounds(&e.when).is_some_and(|(_, end)| end >= now))
        .collect();
    if let Some(named) = named {
        let wanted = crate::proposal::words(named);
        let hit = entries.iter().find(|e| {
            crate::proposal::words(&e.label)
                .iter()
                .any(|w| w.len() > 2 && wanted.contains(w))
        });
        if let Some(e) = hit {
            return Some(e.decision.clone());
        }
    }
    let due = due?;
    let tz = &state.timezone;
    let day = date_of(due.plus_seconds(-1), tz);
    entries
        .iter()
        .find(|e| time::bounds(&e.when).is_some_and(|(start, _)| date_of(start, tz) == day))
        .map(|e| e.decision.clone())
}

/// Keeps the commitments the extractor found in one message and reacts 📌
/// on it, unless the same message put a line on the tab, which it then
/// points to. Returns how many were kept.
pub(crate) fn store_drafts(
    state: &mut GroupState,
    pending: &PendingExtract,
    drafts: Vec<CommitmentDraft>,
    tab_lines: &[LedgerId],
    now: Instant,
    fx: &mut Vec<Effect>,
) -> usize {
    let quiet = !tab_lines.is_empty();
    let ledger = match tab_lines {
        [one] => Some(one.clone()),
        _ => None,
    };
    let room = pending.room.clone();
    let moved_here = state
        .commitments
        .values()
        .any(|c| c.moved_by.contains(&pending.message));
    if moved_here {
        return 0;
    }
    let zone_name = zone_of(state, &pending.speaker);
    let mut kept = 0;
    let pinned = !quiet && !state.is_observing();
    for draft in drafts.into_iter().take(crate::memory::DRAFTS_KEPT) {
        let Some(who) = person(state, &room, &pending.speaker, &draft.who) else {
            debug!(who = %draft.who, "a commitment from someone not in the room");
            continue;
        };
        let what = draft
            .what
            .trim()
            .trim_end_matches(['.', '!'])
            .trim()
            .to_owned();
        if what.is_empty() {
            continue;
        }
        let duplicate = state
            .commitments
            .values()
            .any(|c| c.is_open() && c.who == who && c.what.eq_ignore_ascii_case(&what));
        if duplicate {
            continue;
        }
        let for_whom = draft.for_whom.as_deref().and_then(|f| {
            let f = f.trim().to_lowercase();
            if matches!(
                f.as_str(),
                "the group" | "group" | "us" | "everyone" | "all of us"
            ) {
                Some(ForWhom::Group)
            } else {
                person(state, &room, &pending.speaker, &f).map(|user| ForWhom::Member { user })
            }
        });
        let (due, due_has_time) = due_of(&draft, &pending.text, &zone_name, pending.at);
        let event = event_of(state, &room, draft.event.as_deref(), due, now);
        let id = state.mint_commitment();
        let quote = draft
            .quote
            .map(|q| q.trim().to_owned())
            .filter(|q| !q.is_empty())
            .or_else(|| Some(shorten(&pending.text, 200)));
        let commitment = Commitment {
            id: id.clone(),
            who,
            what,
            for_whom,
            due,
            due_has_time,
            event,
            status: CommitmentStatus::Open,
            source: Source {
                room: room.clone(),
                message: pending.message.clone(),
                speaker: pending.speaker.clone(),
                at: pending.at,
            },
            created_at: now,
            confidence: if draft.stated { STATED } else { IMPLIED },
            quote,
            ledger: ledger.clone(),
            nudged: 0,
            nudges: Vec::new(),
            nudged_at: None,
            moved_by: Vec::new(),
            changed_at: Some(now),
        };
        let line = line(state, &commitment, &state.timezone, now);
        let summary = format!(
            "{}'s commitment: {}",
            state.display_name(&room, &commitment.who),
            commitment.what
        );
        state.commitments.insert(id.clone(), commitment);
        let record = ActionRecord::new(
            state.mint_action(),
            ActionClass::Commitment,
            room.clone(),
            now,
            summary,
            Undo::DropCommitment { id: id.clone() },
        )
        .from_origin(&Origin::asked(&pending.speaker, &pending.message));
        action::push(state, if pinned { record } else { record.hidden() });
        arm_nudges(state, &id, now, fx);
        fx.push(Effect::Notice(format!(
            "Commitment: {line}, from {}.",
            state.room_label(&room)
        )));
        kept += 1;
    }
    if kept > 0 && pinned {
        reactions::add(state, fx, &room, &pending.message, Meaning::Pin);
    }
    kept
}

/// "Alex: draft to legal, due Friday the 7th", in `zone_name`.
pub fn line(state: &GroupState, c: &Commitment, zone_name: &str, now: Instant) -> String {
    let name = state.name_of(&c.who);
    match due_words(c, zone_name, now) {
        Some(due) if c.overdue(now) => format!("{name}: {}, was due {due}", c.what),
        Some(due) => format!("{name}: {}, due {due}", c.what),
        None => format!("{name}: {}", c.what),
    }
}

/// "Friday the 7th", or "Friday the 7th at 5pm".
pub fn due_words(c: &Commitment, zone_name: &str, now: Instant) -> Option<String> {
    let day = c.due_date(zone_name)?;
    let today = date_of(now, zone_name);
    let mut words = day_echo(day, today);
    if c.due_has_time
        && let Some(due) = c.due
    {
        let local = zone::local(due, zone_name);
        words.push_str(" at ");
        words.push_str(&time::clock_echo(local.hour(), local.minute()));
    }
    Some(words)
}

/// `at` moved out of `user`'s quiet hours and, where the profile keeps
/// them, into working hours, at the top of the first hour that allows it.
fn allowed(state: &GroupState, user: &PlatformUserId, at: Instant) -> Instant {
    let theirs = zone_of(state, user);
    let ok = |t: Instant| {
        let local = zone::local(t, &theirs);
        if state
            .profile
            .quiet_hours
            .contains(u8::try_from(local.hour()).unwrap_or(0))
        {
            return false;
        }
        match state.profile.working_hours {
            Some(hours) => {
                let group = zone::local(t, &state.timezone);
                let weekday = u8::try_from(group.weekday().to_monday_one_offset()).unwrap_or(1);
                hours.contains(weekday, u8::try_from(group.hour()).unwrap_or(0))
            }
            None => true,
        }
    };
    let mut t = at;
    for _ in 0..(24 * 8) {
        if ok(t) {
            return t;
        }
        let local = zone::local(t, &theirs);
        t = t.plus_seconds(3600 - i64::from(local.minute()) * 60 - i64::from(local.second()));
    }
    at
}

/// `user`'s usual hour on `date` in their zone, or nine there.
fn hour_on(state: &GroupState, user: &PlatformUserId, date: Date) -> Option<Instant> {
    match usual_hour(state, user).and_then(|h| i8::try_from(h).ok()) {
        Some(hour) => at_on(date, hour, 0, &state.timezone),
        None => at_on(date, NUDGE_HOUR, 0, &zone_of(state, user)),
    }
}

/// When the nudge at due goes: at the time for a timed due, at the
/// person's hour on the day for a date.
fn at_moment(state: &GroupState, c: &Commitment) -> Option<Instant> {
    let due = c.due?;
    if c.due_has_time {
        return Some(allowed(state, &c.who, due));
    }
    let day = c.due_date(&zone_of(state, &c.who))?;
    Some(allowed(state, &c.who, hour_on(state, &c.who, day)?))
}

/// The day before the due day, at the person's hour.
fn before_moment(state: &GroupState, c: &Commitment) -> Option<Instant> {
    let day = c.due_date(&zone_of(state, &c.who))?.yesterday().ok()?;
    Some(allowed(state, &c.who, hour_on(state, &c.who, day)?))
}

/// Arms whichever of the two nudges are still to come.
pub(crate) fn arm_nudges(
    state: &mut GroupState,
    id: &CommitmentId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    disarm(state, id, fx);
    let Some(c) = state.commitments.get(id).filter(|c| c.is_open()) else {
        return;
    };
    if c.due.is_none() || c.nudged >= 2 {
        return;
    }
    let Some(at) = at_moment(state, c) else {
        return;
    };
    let theirs = zone_of(state, &c.who);
    let before = before_moment(state, c).filter(|b| {
        c.nudged == 0 && *b > now && *b < at && date_of(*b, &theirs) != date_of(at, &theirs)
    });
    if let Some(b) = before {
        let purpose = TimerPurpose::Due {
            commitment: id.clone(),
            which: DueAt::Before,
        };
        timers::arm(state, fx, b, purpose);
    }
    if at > now {
        let purpose = TimerPurpose::Due {
            commitment: id.clone(),
            which: DueAt::At,
        };
        timers::arm(state, fx, at, purpose);
    }
}

fn disarm(state: &mut GroupState, id: &CommitmentId, fx: &mut Vec<Effect>) {
    timers::cancel_where(
        state,
        fx,
        |p| matches!(p, TimerPurpose::Due { commitment, .. } if commitment == id),
    );
}

/// A nudge timer fired: one DM, unless the person is away, in which case
/// the first nudge waits for the day they are back while that is before due.
pub(crate) fn on_due(
    state: &mut GroupState,
    id: &CommitmentId,
    which: DueAt,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(c) = state.commitments.get(id).filter(|c| c.is_open()).cloned() else {
        return;
    };
    if c.nudged >= 2 || state.is_observing() {
        return;
    }
    if away::away(state, &c.who, now) {
        if which == DueAt::Before {
            after_away(state, &c, now, fx);
        }
        debug!(commitment = %id, "away, so not chased");
        return;
    }
    let text = nudge_text(state, &c, now);
    let name = state.name_of(&c.who);
    let trace = Trace::new(
        format!(
            "{name}'s commitment from {}: {}",
            state.room_label(&c.source.room),
            c.what
        ),
        NUDGE_RULE,
        now,
    );
    let then = DmFollowUp::Due {
        commitment: id.clone(),
    };
    send_dm_then(state, fx, &c.who, text, now, trace, then);
    if let Some(slot) = state.commitments.get_mut(id) {
        slot.nudged += 1;
        slot.nudged_at = Some(now);
    }
}

/// Moves the first nudge to the day the person is back, or folds it into
/// the nudge at due when that is the same day.
fn after_away(state: &mut GroupState, c: &Commitment, now: Instant, fx: &mut Vec<Effect>) {
    let (Some(back), Some(at)) = (away::back(state, &c.who, now), at_moment(state, c)) else {
        return;
    };
    let theirs = zone_of(state, &c.who);
    let Some(moment) = hour_on(state, &c.who, date_of(back, &theirs)) else {
        return;
    };
    let moment = allowed(state, &c.who, moment.max(back));
    if moment >= at {
        return;
    }
    let which = if date_of(moment, &theirs) == date_of(at, &theirs) {
        timers::cancel_where(
            state,
            fx,
            |p| matches!(p, TimerPurpose::Due { commitment, which: DueAt::At } if *commitment == c.id),
        );
        DueAt::At
    } else {
        DueAt::Before
    };
    let purpose = TimerPurpose::Due {
        commitment: c.id.clone(),
        which,
    };
    timers::arm(state, fx, moment, purpose);
    fx.push(Effect::Notice(format!(
        "{} is away; the nudge about {} waits until {}.",
        state.name_of(&c.who),
        c.what,
        zone::stamp(moment, &state.timezone)
    )));
}

/// "Draft to legal is due today. Done, slipping, or drop it?"
fn nudge_text(state: &GroupState, c: &Commitment, now: Instant) -> String {
    let theirs = zone_of(state, &c.who);
    let what = capitalise(&c.what);
    let Some(day) = c.due_date(&theirs) else {
        return format!("{what}: {NUDGE_ASK}");
    };
    let today = date_of(now, &theirs);
    let when = match days_from(today, day) {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        _ => day_echo(day, today),
    };
    let clock = match c.due.filter(|_| c.due_has_time) {
        Some(due) => {
            let local = zone::local(due, &theirs);
            format!(" at {}", time::clock_echo(local.hour(), local.minute()))
        }
        None => String::new(),
    };
    let verb = if c.overdue(now) { "was" } else { "is" };
    format!("{what} {verb} due{clock} {when}. {NUDGE_ASK}")
}

/// Remembers a nudge DM's id once it is known.
pub(crate) fn nudge_posted(state: &mut GroupState, id: &CommitmentId, message: PlatformMessageId) {
    if let Some(c) = state.commitments.get_mut(id) {
        c.nudges.push(message);
    }
}

const DONE: [&str; 16] = [
    "done",
    "done it",
    "did it",
    "all done",
    "it's done",
    "its done",
    "finished",
    "sent",
    "sent it",
    "complete",
    "completed",
    "yes done",
    "yep done",
    "done thanks",
    "done, thanks",
    "✅",
];

const DROP: [&str; 12] = [
    "drop it",
    "drop",
    "drop that",
    "dropping it",
    "cancel it",
    "cancel that",
    "scrap it",
    "not doing it",
    "forget it",
    "nah drop it",
    "drop it please",
    "❌",
];

const SLIP: [&str; 10] = [
    "slipping",
    "slip",
    "slipped",
    "it's slipping",
    "push it",
    "pushing it",
    "move it",
    "moving it",
    "need more time",
    "delayed",
];

fn plain(text: &str) -> String {
    text.trim()
        .to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .trim_end_matches(['.', '!', '?'])
        .trim()
        .to_owned()
}

/// The commitment a DM is answering: the nudge it replies to, or the
/// agent's last message in the DM when that was a nudge or asked when to.
fn awaiting(state: &GroupState, message: &Message) -> Option<CommitmentId> {
    let find = |id: &PlatformMessageId| {
        state
            .commitments
            .values()
            .find(|c| c.is_open() && c.who == message.author && c.nudges.contains(id))
            .map(|c| c.id.clone())
    };
    if let Some(found) = message.reply_to.as_ref().and_then(find) {
        return Some(found);
    }
    let room = state.rooms.get(&message.room)?;
    let last = room
        .messages
        .iter()
        .rev()
        .filter(|m| room.is_own(&m.id) || state.is_me(&m.author))
        .find(|m| m.text != ASK_WHEN)?;
    find(&last.id)
}

/// The person's open commitment nudged last, or their only open one.
fn latest_nudged(state: &GroupState, user: &PlatformUserId) -> Option<CommitmentId> {
    let open: Vec<&Commitment> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && &c.who == user)
        .collect();
    open.iter()
        .filter(|c| c.nudged_at.is_some())
        .max_by_key(|c| (c.nudged_at, c.id.seq()))
        .or(match open.as_slice() {
            [one] => Some(one),
            _ => None,
        })
        .map(|c| c.id.clone())
}

/// A DM that answers a nudge: "done", "drop it", "slipping", or a day.
/// Returns whether it was one.
pub(crate) fn dm_reply(
    state: &mut GroupState,
    message: &Message,
    body: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let words = plain(body);
    let waiting = awaiting(state, message);
    let reply = |state: &mut GroupState, fx: &mut Vec<Effect>, text: String, rule: &str| {
        let trace = Trace::new(addressed_trigger(state, message), rule, now);
        send(
            state,
            fx,
            &message.room,
            text,
            Some(message.id.clone()),
            now,
            trace,
        );
    };
    if DONE.contains(&words.as_str()) {
        let Some(id) = waiting.or_else(|| latest_nudged(state, &message.author)) else {
            return false;
        };
        let what = finish(state, &id, now, fx);
        reply(
            state,
            fx,
            format!("Marked done: {what}."),
            "\"done\" closes a commitment",
        );
        return true;
    }
    if DROP.contains(&words.as_str()) {
        let Some(id) = waiting.or_else(|| latest_nudged(state, &message.author)) else {
            return false;
        };
        let what = abandon(state, &id, Some(&message.author), now, fx);
        reply(
            state,
            fx,
            format!("Dropped: {what}."),
            "\"drop it\" drops a commitment",
        );
        return true;
    }
    let slipping = SLIP.iter().any(|s| {
        words == *s || words.starts_with(&format!("{s} ")) || words.starts_with(&format!("{s},"))
    });
    let Some(id) = waiting.or_else(|| {
        slipping
            .then(|| latest_nudged(state, &message.author))
            .flatten()
    }) else {
        return false;
    };
    // A bare day answers the nudge; a longer line or a question is something else.
    if !slipping && (words.split_whitespace().count() > 5 || body.trim_end().ends_with('?')) {
        return false;
    }
    let theirs = zone_of(state, &message.author);
    match resolve_time_rules(body, &theirs, now) {
        Some(found) => {
            let Some((due, has_time)) = due_from(&found.meaning, &theirs) else {
                reply(
                    state,
                    fx,
                    ASK_WHEN.to_owned(),
                    "a slipped commitment needs a day",
                );
                return true;
            };
            move_to(state, &id, due, has_time, None, now, fx);
            reply(
                state,
                fx,
                format!("Moved to {}.", found.echo),
                "a day in reply to a nudge moves the commitment",
            );
            true
        }
        None if slipping => {
            reply(
                state,
                fx,
                ASK_WHEN.to_owned(),
                "a slipped commitment needs a day",
            );
            true
        }
        None => false,
    }
}

/// Marks a commitment done and returns what it was.
pub(crate) fn finish(
    state: &mut GroupState,
    id: &CommitmentId,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    disarm(state, id, fx);
    let Some(c) = state.commitments.get_mut(id) else {
        return String::new();
    };
    c.status = CommitmentStatus::Done { at: now };
    c.changed_at = Some(now);
    let what = c.what.clone();
    let name = state.name_of(&state.commitments[id].who);
    fx.push(Effect::Notice(format!("{name} finished: {what}.")));
    what
}

/// Drops a commitment and returns what it was.
pub(crate) fn abandon(
    state: &mut GroupState,
    id: &CommitmentId,
    by: Option<&PlatformUserId>,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    disarm(state, id, fx);
    let Some(c) = state.commitments.get_mut(id) else {
        return String::new();
    };
    c.status = CommitmentStatus::Dropped {
        at: now,
        by: by.cloned(),
    };
    c.changed_at = Some(now);
    let what = c.what.clone();
    let name = state.name_of(&state.commitments[id].who);
    fx.push(Effect::Notice(format!("{name} dropped: {what}.")));
    what
}

/// Moves a commitment to a new due date, re-arming whatever nudges are left.
pub(crate) fn move_to(
    state: &mut GroupState,
    id: &CommitmentId,
    due: Instant,
    has_time: bool,
    by: Option<&PlatformMessageId>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(c) = state.commitments.get_mut(id) else {
        return;
    };
    c.due = Some(due);
    c.due_has_time = has_time;
    c.status = CommitmentStatus::Moved {
        to: due,
        at: Some(now),
    };
    c.changed_at = Some(now);
    if let Some(message) = by {
        c.moved_by.push(message.clone());
    }
    let c = c.clone();
    arm_nudges(state, id, now, fx);
    let tz = state.timezone.clone();
    fx.push(Effect::Notice(format!(
        "Moved: {}.",
        line(state, &c, &tz, now)
    )));
}

/// A ✅ or ❌ from the person on one of their nudge DMs. Returns whether
/// the reaction was about a commitment.
pub(crate) fn on_dm_reaction(
    state: &mut GroupState,
    dm: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let Some(id) = state
        .commitments
        .values()
        .find(|c| c.is_open() && &c.who == user && c.nudges.contains(message))
        .map(|c| c.id.clone())
    else {
        return false;
    };
    let Some(room) = state.dm_room_of(user) else {
        return false;
    };
    let text = if reactions::means(state, dm, emoji, Meaning::Check) {
        format!("Marked done: {}.", finish(state, &id, now, fx))
    } else if reactions::means(state, dm, emoji, Meaning::Cross) {
        format!("Dropped: {}.", abandon(state, &id, Some(user), now, fx))
    } else {
        return false;
    };
    let trace = Trace::new(
        format!("{} reacted {emoji} on a nudge", state.name_of(user)),
        "✅ on a nudge closes it and ❌ drops it",
        now,
    );
    send(state, fx, &room, text, None, now, trace);
    true
}

/// A ❌ on a message the agent pinned as a commitment drops it when it
/// comes from whoever promised or said it, and does nothing from anyone
/// else. Returns whether the message carried a commitment.
pub(crate) fn on_room_reaction(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    if !reactions::means(state, room, emoji, Meaning::Cross) {
        return false;
    }
    let ids: Vec<CommitmentId> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && &c.source.room == room && &c.source.message == message)
        .map(|c| c.id.clone())
        .collect();
    if ids.is_empty() {
        return false;
    }
    let mine: Vec<CommitmentId> = ids
        .into_iter()
        .filter(|id| {
            state
                .commitments
                .get(id)
                .is_some_and(|c| &c.who == user || &c.source.speaker == user)
        })
        .collect();
    if mine.is_empty() {
        debug!(%user, "a ❌ on someone else's commitment");
        return true;
    }
    for id in &mine {
        abandon(state, id, Some(user), now, fx);
    }
    reactions::remove(state, fx, room, message, Meaning::Pin);
    true
}

/// Promise words that, with "it" and a day, move a commitment already made.
const MOVE_WORDS: [&str; 14] = [
    "i'll",
    "i will",
    "i can",
    "i should",
    "will have",
    "have it",
    "it'll",
    "it will",
    "slipping",
    "pushing it",
    "push it",
    "move it",
    "moving it",
    "be ready",
];

/// "I'll have it by Friday" from someone with an open commitment in the
/// room moves it. Returns whether a commitment moved.
pub(crate) fn heard(
    state: &mut GroupState,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let mine: Vec<&Commitment> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && c.who == message.author && c.source.room == message.room)
        .filter(|c| c.source.message != message.id)
        .collect();
    if mine.is_empty() || message.text.trim_end().ends_with('?') {
        return false;
    }
    let words = crate::proposal::words(&message.text);
    let padded = format!(" {} ", words.join(" "));
    let has = |p: &str| padded.contains(&format!(" {p} "));
    if !MOVE_WORDS.iter().any(|w| has(w)) {
        return false;
    }
    let named: Vec<&&Commitment> = mine
        .iter()
        .filter(|c| {
            crate::proposal::words(&c.what)
                .iter()
                .any(|w| w.len() > 3 && words.contains(w))
        })
        .collect();
    let target = match (named.as_slice(), mine.as_slice()) {
        ([one], _) => (**one).clone(),
        ([], [one]) if has("it") || has("that") || has("this") => (*one).clone(),
        _ => return false,
    };
    let theirs = zone_of(state, &message.author);
    let Some(found) = resolve_time_rules(&message.text, &theirs, message.at) else {
        return false;
    };
    let Some((due, has_time)) = due_from(&found.meaning, &theirs) else {
        return false;
    };
    if target.due == Some(due) {
        return false;
    }
    move_to(state, &target.id, due, has_time, Some(&message.id), now, fx);
    if !state.is_observing() {
        reactions::add(state, fx, &message.room, &message.id, Meaning::Pin);
    }
    true
}

/// What an outstanding question asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    All,
    By(PlatformUserId),
    /// Due before the end of this week, overdue included.
    ThisWeek,
}

/// Open commitments the place may see, soonest due first, one line each,
/// six at most and then "and N more".
pub(crate) fn outstanding(
    state: &GroupState,
    place: &Place,
    filter: &Filter,
    zone_name: &str,
    now: Instant,
) -> Option<String> {
    let week_end = resolve_time_rules("this week", zone_name, now).and_then(|r| match r.meaning {
        TimeMeaning::Span { end, .. } => Some(end),
        _ => None,
    });
    let mut open: Vec<&Commitment> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && c.visible(state, place))
        .filter(|c| match filter {
            Filter::All => true,
            Filter::By(user) => &c.who == user,
            Filter::ThisWeek => c.due.zip(week_end).is_some_and(|(due, end)| due <= end),
        })
        .collect();
    if open.is_empty() {
        return None;
    }
    open.sort_by_key(|c| (c.due.is_none(), c.due, c.id.seq()));
    let mut lines: Vec<String> = open
        .iter()
        .take(LINES)
        .map(|c| line(state, c, zone_name, now))
        .collect();
    if open.len() > LINES {
        lines.push(format!("and {} more", open.len() - LINES));
    }
    Some(lines.join("\n"))
}

/// Answers "what's outstanding", "what did Alex commit to" and "what's due
/// this week" in the room or DM they were asked in.
pub(crate) fn answer(
    state: &mut GroupState,
    message: &Message,
    ask: Ask,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    let place = Place::of(state, room);
    let zone_name = match &place {
        Place::Dm(user) => zone_of(state, user),
        Place::Room(_) => state.timezone.clone(),
    };
    let (filter, empty) = match &ask {
        Ask::Outstanding => (Filter::All, "Nothing outstanding.".to_owned()),
        Ask::DueThisWeek => (Filter::ThisWeek, "Nothing due this week.".to_owned()),
        Ask::By { name } => match person(state, room, &message.author, name) {
            Some(user) => {
                let empty = if user == message.author {
                    "Nothing outstanding from you.".to_owned()
                } else {
                    format!("Nothing outstanding from {}.", state.name_of(&user))
                };
                (Filter::By(user), empty)
            }
            None => {
                let text = format!("I don't know anyone called {} here.", capitalise(name));
                let trace = Trace::new(addressed_trigger(state, message), OUTSTANDING_RULE, now);
                send(state, fx, room, text, Some(message.id.clone()), now, trace);
                return;
            }
        },
    };
    let text = outstanding(state, &place, &filter, &zone_name, now).unwrap_or(empty);
    let trace = Trace::new(addressed_trigger(state, message), OUTSTANDING_RULE, now);
    send(state, fx, room, text, Some(message.id.clone()), now, trace);
}

/// "Outstanding: Alex: draft to legal, due Thursday the 5th; ..." for the
/// system prompt, as the place may see, five at most.
pub(crate) fn prompt_line(state: &GroupState, place: &Place, now: Instant) -> Option<String> {
    let zone_name = match place {
        Place::Dm(user) => zone_of(state, user),
        Place::Room(_) => state.timezone.clone(),
    };
    let mut open: Vec<&Commitment> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && c.visible(state, place))
        .collect();
    if open.is_empty() {
        return None;
    }
    open.sort_by_key(|c| (c.due.is_none(), c.due, c.id.seq()));
    let parts: Vec<String> = open
        .iter()
        .take(5)
        .map(|c| line(state, c, &zone_name, now))
        .collect();
    Some(format!("Outstanding: {}.", parts.join("; ")))
}

/// The outstanding questions by sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    Outstanding,
    By { name: String },
    DueThisWeek,
}

/// A deleted message takes the commitments it made.
pub(crate) fn on_deleted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let ids: Vec<CommitmentId> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && &c.source.room == room && &c.source.message == message)
        .map(|c| c.id.clone())
        .collect();
    for id in ids {
        abandon(state, &id, None, now, fx);
    }
}

/// "forget me": their open commitments are dropped.
pub(crate) fn forget(
    state: &mut GroupState,
    user: &PlatformUserId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let ids: Vec<CommitmentId> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && &c.who == user)
        .map(|c| c.id.clone())
        .collect();
    for id in ids {
        abandon(state, &id, Some(user), now, fx);
    }
}

/// The present participle of what someone will do: "bring dessert" reads
/// "bringing dessert".
pub fn doing(what: &str) -> String {
    let what = what.trim();
    let (verb, rest) = what.split_once(' ').unwrap_or((what, ""));
    let lower = verb.to_lowercase();
    let ing = match lower.as_str() {
        "be" => "being".to_owned(),
        "see" => "seeing".to_owned(),
        "get" | "put" | "run" | "sit" | "set" | "shop" | "plan" | "drop" | "stop" | "chop" => {
            let last = lower.chars().last().unwrap_or('t');
            format!("{lower}{last}ing")
        }
        v if v.ends_with("ie") => format!("{}ying", &v[..v.len() - 2]),
        v if v.ends_with('e') && !v.ends_with("ee") && v.len() > 2 => {
            format!("{}ing", &v[..v.len() - 1])
        }
        v => format!("{v}ing"),
    };
    if rest.is_empty() {
        ing
    } else {
        format!("{ing} {rest}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_read_as_doing() {
        assert_eq!(doing("bring dessert"), "bringing dessert");
        assert_eq!(doing("drive"), "driving");
        assert_eq!(doing("get the wine"), "getting the wine");
        assert_eq!(doing("book the pitch"), "booking the pitch");
        assert_eq!(doing("tie up loose ends"), "tying up loose ends");
        assert_eq!(doing("see to it"), "seeing to it");
    }
}
