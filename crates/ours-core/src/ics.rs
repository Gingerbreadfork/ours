//! The group's calendar as an ICS feed (DESIGN.md section 12): one VEVENT
//! per calendar entry, and for one member only the entries they are in and
//! their own dated commitments. The binary serves it at the feed links.

use crate::decision::{DecidedOption, Entry, Outcome};
use crate::state::GroupState;
use crate::time::{at_on, bounds, date_of, day_end};
use ours_types::{Instant, PlatformUserId, TimeMeaning};
use sha2::{Digest, Sha256};

/// A timed entry is shown as lasting this long.
const EVENT_SECONDS: i64 = 2 * 60 * 60;
/// A timed due date is shown as lasting this long.
const DUE_SECONDS: i64 = 30 * 60;
/// RFC 5545 folds lines longer than this many octets.
const FOLD: usize = 75;

/// The path segment that opens `user`'s own feed: the first 16 hex digits
/// of sha256 of "<secret>:<user>", so one member's link opens nobody else's.
pub fn member_feed_token(secret: &str, user: &str) -> String {
    let digest = Sha256::digest(format!("{secret}:{user}").as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The calendar as RFC 5545 text: the group's, or `member`'s own.
pub fn render_ics(state: &GroupState, member: Option<&PlatformUserId>, now: Instant) -> String {
    let name = match member {
        Some(user) => format!("{} for {}", state.agent_name, state.name_of(user)),
        None => format!("{} for the group", state.agent_name),
    };
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".to_owned(),
        "PRODID:-//Ours//Group calendar//EN".to_owned(),
        "CALSCALE:GREGORIAN".to_owned(),
        "METHOD:PUBLISH".to_owned(),
        format!("X-WR-CALNAME:{}", escape(&name)),
    ];
    let tz = state.timezone.as_str();
    let mut entries: Vec<(Instant, &Entry)> = state
        .calendar
        .iter()
        .filter(|e| member.is_none_or(|m| is_in(state, e, m)))
        .filter_map(|e| bounds(&e.when, tz).map(|(start, _)| (start, e)))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.decision.cmp(&b.1.decision)));
    for (_, entry) in entries {
        let Some((start, end)) = span(&entry.when, tz) else {
            continue;
        };
        lines.extend(event(
            &format!("{}@{}", entry.decision, state.id),
            now,
            start,
            end,
            &entry.label,
            &description(state, entry),
        ));
    }
    if let Some(user) = member {
        let mut mine: Vec<_> = state
            .commitments
            .values()
            .filter(|c| c.is_open() && &c.who == user)
            .filter_map(|c| c.due.map(|due| (due, c)))
            .collect();
        mine.sort_by_key(|(due, c)| (*due, c.id.seq()));
        for (due, c) in mine {
            let (start, end) = if c.due_has_time {
                (due, due.plus_seconds(DUE_SECONDS))
            } else {
                (day_before(due, tz), due)
            };
            let from = format!("Said in {}", state.room_label(&c.source.room));
            let said = c
                .quote
                .as_deref()
                .map_or(from.clone(), |q| format!("{from}: \"{q}\""));
            lines.extend(event(
                &format!("{}@{}", c.id, state.id),
                now,
                start,
                end,
                &format!("Due: {}", c.what),
                &said,
            ));
        }
    }
    lines.push("END:VCALENDAR".to_owned());
    let mut out = String::new();
    for line in lines {
        out.push_str(&fold(&line));
    }
    out
}

/// Whether `member` is in an entry: one of its attendees, or anyone in its
/// room who has not said they're out when a lead decided and nobody voted.
fn is_in(state: &GroupState, entry: &Entry, member: &PlatformUserId) -> bool {
    if !entry.attendees.is_empty() {
        return entry.attendees.contains(member);
    }
    if entry.out.contains(member) {
        return false;
    }
    entry.room.as_ref().is_none_or(|room| {
        state
            .rooms
            .get(room)
            .is_some_and(|r| r.members.contains_key(member))
    })
}

/// The start of the local day that ends at `end`, which is not always 24
/// hours earlier when the clocks change.
fn day_before(end: Instant, zone_name: &str) -> Instant {
    let date = date_of(end.plus_seconds(-1), zone_name);
    match (at_on(date, 0, 0, zone_name), day_end(date, zone_name)) {
        (Some(start), Some(day_end)) if day_end == end => start,
        _ => end.plus_seconds(-24 * 60 * 60),
    }
}

/// When an entry starts and ends: a timed one lasts two hours, a day the
/// whole day, a span its span.
fn span(when: &TimeMeaning, zone_name: &str) -> Option<(Instant, Instant)> {
    match when {
        TimeMeaning::Instant { at, has_time: true } => Some((*at, at.plus_seconds(EVENT_SECONDS))),
        TimeMeaning::Recurrence { first, .. } => Some((*first, first.plus_seconds(EVENT_SECONDS))),
        other => bounds(other, zone_name),
    }
}

fn event(
    uid: &str,
    now: Instant,
    start: Instant,
    end: Instant,
    summary: &str,
    description: &str,
) -> Vec<String> {
    vec![
        "BEGIN:VEVENT".to_owned(),
        format!("UID:{}", escape(uid)),
        format!("DTSTAMP:{}", utc(now)),
        format!("DTSTART:{}", utc(start)),
        format!("DTEND:{}", utc(end)),
        format!("SUMMARY:{}", escape(summary)),
        format!("DESCRIPTION:{}", escape(description)),
        "END:VEVENT".to_owned(),
    ]
}

/// The decision's outcome in one line: "Dinner: Thursday the 12th, 4 to 1
/// on majority of active members."
fn description(state: &GroupState, entry: &Entry) -> String {
    let Some(d) = state.decisions.get(&entry.decision) else {
        return entry.label.clone();
    };
    let chosen = d.chosen().map_or("", DecidedOption::display);
    match &d.outcome {
        Outcome::Count { .. } => format!(
            "{}: {chosen}, {} on {}.",
            d.title(),
            d.count_line().unwrap_or_default(),
            d.rule.name()
        ),
        Outcome::Decider { user } => {
            format!(
                "{}: {chosen}, decided by {}.",
                d.title(),
                state.name_of(user)
            )
        }
    }
}

/// 20261112T083000Z.
fn utc(at: Instant) -> String {
    at.0.strftime("%Y%m%dT%H%M%SZ").to_string()
}

/// Text as RFC 5545 wants it: backslashes, commas, semicolons and line
/// breaks escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            other => out.push(other),
        }
    }
    out
}

/// One content line, folded at 75 octets on character boundaries, with CRLF.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > FOLD {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_and_folded() {
        assert_eq!(escape("a, b; c\\d\ne"), "a\\, b\\; c\\\\d\\ne");
        let long = format!("DESCRIPTION:{}", "x".repeat(100));
        let folded = fold(&long);
        let lines: Vec<&str> = folded.split("\r\n").collect();
        assert_eq!(lines[0].len(), 75);
        assert!(lines[1].starts_with(' '));
        assert!(lines.iter().all(|l| l.len() <= 75));
    }

    #[test]
    fn member_tokens_are_stable_and_differ_per_member() {
        let sam = member_feed_token("s3cret", "sam");
        assert_eq!(sam, "35de6da1183cae89");
        assert_eq!(sam, member_feed_token("s3cret", "sam"));
        assert_ne!(sam, member_feed_token("s3cret", "priya"));
        assert_ne!(sam, member_feed_token("other", "sam"));
    }
}
