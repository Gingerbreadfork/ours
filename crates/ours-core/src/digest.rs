//! The digest and the day-of summary (DESIGN.md sections 6 and 11). The
//! digest is one scheduled message a day for a room where something
//! happened: what was decided, what is due, what slipped, what closed, what
//! is open and who is away. The day-of summary is one line on a day with
//! something on the calendar. Both are scheduled, so neither counts
//! against the budget; while observing they go to leads as "would have".

use crate::away;
use crate::commitment::{Commitment, CommitmentStatus, doing, due_words};
use crate::decide::open_items;
use crate::decision::Entry;
use crate::effect::{Effect, send};
use crate::leads::join_names;
use crate::memory::Place;
use crate::observe;
use crate::profile::ClockTime;
use crate::proposal::DecisionId;
use crate::reactions;
use crate::state::GroupState;
use crate::time::{self, at_on, bounds, date_of, day_echo};
use crate::timers::{self, TimerPurpose};
use crate::trace::Trace;
use crate::zone;
use jiff::civil::Date;
use ours_types::{Instant, PlatformRoomId, PlatformUserId};
use tracing::debug;

/// Lines in a digest, at most.
pub const DIGEST_LINES: usize = 6;
/// Items on one digest line before "and N more".
pub const DIGEST_ITEMS: usize = 5;
/// The day-of line goes this long before the earliest timed entry.
pub const DAY_OF_LEAD_SECONDS: i64 = 2 * 60 * 60;
/// The local hour it goes when nothing that day has a time.
pub const DAY_OF_HOUR: i8 = 9;

const DIGEST_RULE: &str = "the digest goes out once a day where something happened";
const DAY_OF_RULE: &str = "the day-of summary is one line on a day with something on";

/// Arms the digest and day-of timers a step may have called for.
pub(crate) fn reconcile(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    digests(state, now, fx);
    days(state, now, fx);
}

fn rooms(state: &GroupState) -> Vec<PlatformRoomId> {
    state
        .rooms
        .iter()
        .filter(|(_, r)| r.info.is_some() && !r.is_dm())
        .map(|(id, _)| id.clone())
        .collect()
}

fn digests(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    let Some(at) = state.jobs().digest else {
        return;
    };
    for room in rooms(state) {
        // Counting starts from the first step that sees the room.
        let since = *state
            .digests
            .entry(room.clone())
            .or_insert(now.plus_seconds(-1));
        let armed = timers::any_where(
            state,
            |p| matches!(p, TimerPurpose::Digest { room: r } if *r == room),
        );
        if armed || !pending(state, &room, since, now) {
            continue;
        }
        if let Some(when) = next_digest(state, at, now) {
            timers::arm(state, fx, when, TimerPurpose::Digest { room });
        }
    }
}

/// Whether the room's next digest may have something to say.
fn pending(state: &GroupState, room: &PlatformRoomId, since: Instant, now: Instant) -> bool {
    let place = Place::Room(room.clone());
    state
        .decisions
        .values()
        .any(|d| &d.room == room && d.closed_at > since)
        || state
            .proposals
            .values()
            .any(|p| &p.room == room && p.opened_at > since)
        || state.commitments.values().any(|c| {
            c.visible(state, &place)
                && (c.changed_at.is_some_and(|t| t > since)
                    || (c.is_open() && c.due.is_some_and(|d| d > now)))
        })
}

/// The next digest time after `now`, on a working day where the profile
/// keeps working hours.
fn next_digest(state: &GroupState, at: ClockTime, now: Instant) -> Option<Instant> {
    let tz = state.timezone.as_str();
    let hour = i8::try_from(at.hour % 24).ok()?;
    let minute = i8::try_from(at.minute % 60).ok()?;
    let mut day = date_of(now, tz);
    for _ in 0..8 {
        let weekend = day.weekday().to_monday_one_offset() > 5;
        let skip = weekend && state.profile.working_hours.is_some_and(|w| w.weekdays_only);
        if !skip
            && let Some(t) = at_on(day, hour, minute, tz)
            && t > now
        {
            return Some(t);
        }
        day = day.tomorrow().ok()?;
    }
    None
}

/// The digest timer fired: post it if anything happened, then count from now.
pub(crate) fn on_digest(
    state: &mut GroupState,
    room: &PlatformRoomId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if state.jobs().digest.is_none() {
        return;
    }
    let since = state
        .digests
        .get(room)
        .copied()
        .unwrap_or(now.plus_seconds(-24 * 60 * 60));
    let text = digest(state, room, since, now);
    state.digests.insert(room.clone(), now);
    let Some(text) = text else {
        debug!(%room, "nothing happened; no digest");
        return;
    };
    let trace = Trace::new(
        format!("the digest time in {}", state.room_label(room)),
        DIGEST_RULE,
        now,
    );
    if state.is_observing() {
        let label = state.room_label(room);
        let report = format!(
            "In {label} I would have posted the digest:\n{text}\n{}",
            reactions::feedback_line(state, room)
        );
        observe::report(state, fx, &report, trace);
        return;
    }
    send(state, fx, room, text, None, now, trace);
    fx.push(Effect::Notice(format!(
        "Posted the digest in {}.",
        state.room_label(room)
    )));
}

/// Six lines at most, or none when nothing was decided, due, slipped or
/// closed since `since`.
pub fn digest(
    state: &GroupState,
    room: &PlatformRoomId,
    since: Instant,
    now: Instant,
) -> Option<String> {
    let tz = state.timezone.as_str();
    let place = Place::Room(room.clone());
    let today = date_of(now, tz);
    let tomorrow = today.tomorrow().ok()?;
    let mut decisions: Vec<_> = state
        .decisions
        .values()
        .filter(|d| &d.room == room && d.closed_at > since)
        .collect();
    decisions.sort_by_key(|d| (d.closed_at, d.id.seq()));
    let decided: Vec<String> = decisions
        .iter()
        .map(|d| {
            let chosen = d.chosen().map_or("", |o| o.display());
            match &d.topic {
                Some(topic) => format!("{topic}, {chosen}"),
                None => chosen.to_owned(),
            }
        })
        .collect();
    let mut mine: Vec<&Commitment> = state
        .commitments
        .values()
        .filter(|c| c.visible(state, &place))
        .collect();
    mine.sort_by_key(|c| (c.due.is_none(), c.due, c.id.seq()));
    let name = |c: &Commitment| state.name_of(&c.who);
    let mut due = Vec::new();
    let mut slipped = Vec::new();
    let mut closed = Vec::new();
    for c in &mine {
        let day = c.due_date(tz);
        if c.is_open() && !c.overdue(now) && (day == Some(today) || day == Some(tomorrow)) {
            let when = if day == Some(today) {
                "today"
            } else {
                "tomorrow"
            };
            due.push(format!("{}, {}, {when}", name(c), c.what));
        }
        match &c.status {
            CommitmentStatus::Moved { at: Some(at), .. } if *at > since => {
                let to = due_words(c, tz, now).unwrap_or_default();
                slipped.push(format!("{}, {}, to {to}", name(c), c.what));
            }
            CommitmentStatus::Open | CommitmentStatus::Moved { .. }
                if c.due.is_some_and(|d| d > since && d <= now) =>
            {
                let was = due_words(c, tz, now).unwrap_or_default();
                slipped.push(format!("{}, {}, was due {was}", name(c), c.what));
            }
            CommitmentStatus::Done { at } if *at > since => {
                closed.push(format!("{}, {}", name(c), c.what));
            }
            _ => {}
        }
    }
    if decided.is_empty() && due.is_empty() && slipped.is_empty() && closed.is_empty() {
        return None;
    }
    let open = open_items(state, room);
    let away = away_names(state, room, now)
        .into_iter()
        .map(|(user, name)| match away::until(state, &user, now) {
            Some(end) => format!("{name} until {}", day_echo(date_of(end, tz), today)),
            None => name,
        })
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for (label, items) in [
        ("Decided", decided),
        ("Due", due),
        ("Slipped", slipped),
        ("Closed", closed),
        ("Open", open),
        ("Away", away),
    ] {
        if items.is_empty() {
            continue;
        }
        let mut shown = items.iter().take(DIGEST_ITEMS).cloned().collect::<Vec<_>>();
        if items.len() > DIGEST_ITEMS {
            shown.push(format!("and {} more", items.len() - DIGEST_ITEMS));
        }
        lines.push(format!("{label}: {}.", shown.join("; ")));
    }
    lines.truncate(DIGEST_LINES);
    Some(lines.join("\n"))
}

/// Human members of the room who are away at `at`, with their names.
fn away_names(
    state: &GroupState,
    room: &PlatformRoomId,
    at: Instant,
) -> Vec<(PlatformUserId, String)> {
    let Some(r) = state.rooms.get(room) else {
        return Vec::new();
    };
    let mut out: Vec<(PlatformUserId, String)> = r
        .members
        .values()
        .filter(|m| !m.is_bot && !state.is_me(&m.id) && away::away(state, &m.id, at))
        .map(|m| (m.id.clone(), m.display_name.clone()))
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// The calendar entries in `room` on `day`, earliest first.
fn entries_on<'a>(state: &'a GroupState, room: &PlatformRoomId, day: Date) -> Vec<&'a Entry> {
    let tz = state.timezone.as_str();
    let mut out: Vec<(Instant, &Entry)> = state
        .calendar
        .iter()
        .filter(|e| e.room.as_ref() == Some(room))
        .filter_map(|e| bounds(&e.when, tz).map(|(start, _)| (start, e)))
        .filter(|(start, _)| date_of(*start, tz) == day)
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.decision.cmp(&b.1.decision)));
    out.into_iter().map(|(_, e)| e).collect()
}

fn has_time(entry: &Entry) -> bool {
    matches!(
        entry.when,
        ours_types::TimeMeaning::Instant { has_time: true, .. }
    )
}

fn days(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    if !state.jobs().day_of {
        return;
    }
    let tz = state.timezone.clone();
    let today = date_of(now, &tz);
    let mut wanted: Vec<(PlatformRoomId, Date, DecisionId)> = Vec::new();
    for e in &state.calendar {
        let (Some(room), Some((start, _))) = (e.room.as_ref(), bounds(&e.when, &tz)) else {
            continue;
        };
        if state.rooms.get(room).is_none_or(|r| r.is_dm()) {
            continue;
        }
        let day = date_of(start, &tz);
        let done = state
            .day_of_sent
            .get(room)
            .is_some_and(|d| *d >= day.to_string());
        if day < today || done || wanted.iter().any(|(r, d, _)| r == room && *d == day) {
            continue;
        }
        wanted.push((room.clone(), day, e.decision.clone()));
    }
    for (room, day, decision) in wanted {
        let armed = timers::any_where(state, |p| match p {
            TimerPurpose::DayOf { room: r, entry } => {
                *r == room
                    && state
                        .calendar
                        .iter()
                        .find(|e| &e.decision == entry)
                        .and_then(|e| bounds(&e.when, &tz))
                        .is_some_and(|(start, _)| date_of(start, &tz) == day)
            }
            _ => false,
        });
        if armed {
            continue;
        }
        let Some(at) = day_of_time(state, &room, day) else {
            continue;
        };
        if at <= now {
            continue;
        }
        let purpose = TimerPurpose::DayOf {
            room,
            entry: decision,
        };
        timers::arm(state, fx, at, purpose);
    }
}

/// Two hours before the day's earliest timed entry, or nine; never in quiet
/// hours unless that would be after the entry starts.
fn day_of_time(state: &GroupState, room: &PlatformRoomId, day: Date) -> Option<Instant> {
    let tz = state.timezone.as_str();
    let entries = entries_on(state, room, day);
    let timed = entries
        .iter()
        .filter(|e| has_time(e))
        .filter_map(|e| bounds(&e.when, tz).map(|(s, _)| s))
        .min();
    let at = match timed {
        Some(start) => start.plus_seconds(-DAY_OF_LEAD_SECONDS),
        None => at_on(day, DAY_OF_HOUR, 0, tz)?,
    };
    let quiet = state.profile.quiet_hours;
    let hour = |t: Instant| u8::try_from(zone::local(t, tz).hour()).unwrap_or(0);
    if !quiet.contains(hour(at)) {
        return Some(at);
    }
    let wake = at_on(day, i8::try_from(quiet.end).ok()?, 0, tz)?;
    match timed {
        Some(start) if wake >= start => None,
        _ if wake > at => Some(wake),
        _ => Some(at),
    }
}

/// The day-of timer fired: one line about the day's entries.
pub(crate) fn on_day_of(
    state: &mut GroupState,
    room: &PlatformRoomId,
    entry: &DecisionId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if !state.jobs().day_of {
        return;
    }
    let tz = state.timezone.clone();
    let Some(day) = state
        .calendar
        .iter()
        .find(|e| &e.decision == entry)
        .and_then(|e| bounds(&e.when, &tz))
        .map(|(start, _)| date_of(start, &tz))
    else {
        return;
    };
    let Some(text) = day_line(state, room, day) else {
        return;
    };
    state.day_of_sent.insert(room.clone(), day.to_string());
    let trace = Trace::new(
        format!("{} has something on today", state.room_label(room)),
        DAY_OF_RULE,
        now,
    );
    if state.is_observing() {
        let label = state.room_label(room);
        let report = format!(
            "In {label} I would have said: {text}\n{}",
            reactions::feedback_line(state, room)
        );
        observe::report(state, fx, &report, trace);
        return;
    }
    send(state, fx, room, text, None, now, trace);
}

/// "Priya's at 7:30pm, Alex is away, Marcus out. Jo is bringing dessert."
pub fn day_line(state: &GroupState, room: &PlatformRoomId, day: Date) -> Option<String> {
    let tz = state.timezone.as_str();
    let entries = entries_on(state, room, day);
    if entries.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = entries
        .iter()
        .map(|e| {
            let at = bounds(&e.when, tz).map(|(s, _)| zone::local(s, tz));
            match at.filter(|_| has_time(e)) {
                Some(local) => format!(
                    "{} at {}",
                    e.label,
                    time::clock_echo(local.hour(), local.minute())
                ),
                None => format!("{} today", e.label),
            }
        })
        .collect();
    let start = at_on(day, 0, 0, tz)?;
    let end = time::day_end(day, tz)?;
    let away: Vec<String> = state
        .rooms
        .get(room)
        .map(|r| {
            let mut names: Vec<String> = r
                .members
                .values()
                .filter(|m| !m.is_bot && !state.is_me(&m.id))
                .filter(|m| away::away_during(state, &m.id, start, end))
                .map(|m| m.display_name.clone())
                .collect();
            names.sort();
            names
        })
        .unwrap_or_default();
    match away.len() {
        0 => {}
        1 => parts.push(format!("{} is away", away[0])),
        _ => parts.push(format!("{} are away", join_names(&away, "and"))),
    }
    let mut out: Vec<String> = entries
        .iter()
        .flat_map(|e| e.out.iter())
        .map(|u| state.display_name(room, u))
        .filter(|n| !away.contains(n))
        .collect();
    out.sort();
    out.dedup();
    if !out.is_empty() {
        parts.push(format!("{} out", join_names(&out, "and")));
    }
    let mut first = parts.join(", ");
    first.push('.');
    let decisions: Vec<&DecisionId> = entries.iter().map(|e| &e.decision).collect();
    let mut bringing: Vec<(u64, String)> = state
        .commitments
        .values()
        .filter(|c| c.is_open() && c.event.as_ref().is_some_and(|d| decisions.contains(&d)))
        .map(|c| {
            (
                c.id.seq(),
                format!("{} is {}", state.name_of(&c.who), doing(&c.what)),
            )
        })
        .collect();
    bringing.sort();
    if bringing.is_empty() {
        return Some(first);
    }
    let said: Vec<String> = bringing.into_iter().map(|(_, s)| s).collect();
    Some(format!("{first} {}.", said.join(", ")))
}
