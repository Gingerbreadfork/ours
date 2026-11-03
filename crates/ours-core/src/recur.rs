//! Recurrence (DESIGN.md section 12): "monthly dinner on the first
//! Thursday", "weekly planning every Monday 10am", "subs due on the
//! first". A small RFC 5545 RRULE evaluator finds each instance; the next
//! one opens its lead time ahead, a lead learned from how long past ones
//! took to close; one that gets no answer twice is paused with one line.

use crate::action::Recording;
use crate::commitment::{Commitment, CommitmentStatus, arm_nudges};
use crate::decide::{self, Opening};
use crate::effect::{Effect, send, send_as};
use crate::leads::decline_line;
use crate::memory::Source;
use crate::observe;
use crate::proposal::{CloseRule, Offered, ProposalId, ProposalKind, capitalise, offered, seq_id};
use crate::reactions;
use crate::settings::addressed_trigger;
use crate::state::GroupState;
use crate::time::{self, at_on, clock_echo, date_of, day_end, weekday_name};
use crate::timers::{self, TimerPurpose};
use crate::trace::Trace;
use crate::zone;
use jiff::civil::{Date, Weekday};
use ours_types::{
    Instant, Message, PlatformMessageId, PlatformRoomId, PlatformUserId, Role, TimeMeaning,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

seq_id!(
    /// A recurrence's id, minted from the group's sequence.
    RecurrenceId
);

const DAY: i64 = 24 * 60 * 60;
/// How far ahead a proposal's instance opens until there is a history.
pub const PROPOSAL_LEAD_SECONDS: i64 = 3 * DAY;
/// How far ahead commitments and reminders open.
pub const OTHER_LEAD_SECONDS: i64 = DAY;
/// The learned lead is this many times the median time to close.
const LEAD_FACTOR: i64 = 3;
/// The local hour an instance without a time opens.
const OPEN_HOUR: i8 = 10;
/// Instances with no answer in a row before it is paused.
pub const SILENCES: u8 = 2;
/// Days searched for the next instance.
const HORIZON_DAYS: usize = 800;

const RULE: &str = "a recurrence opens its next instance its lead time ahead";

/// What each instance is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecurKind {
    /// A ballot: who's in when there are no options.
    Proposal {
        question: String,
        options: Vec<String>,
    },
    Commitment {
        who: PlatformUserId,
        what: String,
    },
    Reminder {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Recurrence {
    pub id: RecurrenceId,
    /// RFC 5545, without DTSTART: "FREQ=MONTHLY;BYDAY=1TH".
    pub rrule: String,
    pub kind: RecurKind,
    /// The next instance.
    pub next: Instant,
    pub lead_seconds: i64,
    pub last_opened: Option<Instant>,
    pub paused: bool,
    /// "dinner", as said.
    pub label: String,
    pub room: PlatformRoomId,
    /// The sentence that set it up; ballots hang off it.
    pub source: PlatformMessageId,
    pub created_by: PlatformUserId,
    /// The first instance, which INTERVAL counts from.
    pub anchor: Instant,
    pub has_time: bool,
    /// The proposal the last instance opened.
    #[serde(default)]
    pub open: Option<ProposalId>,
    /// Instances in a row nobody answered.
    #[serde(default)]
    pub silent: u8,
    /// How long past instances took to close, in seconds.
    #[serde(default)]
    pub took: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
}

/// The parts of an RRULE this evaluator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub freq: Freq,
    pub interval: u32,
    /// Weekdays, each with its place in the month for monthly rules, as in
    /// 1TH or -1FR; zero means every one.
    pub by_day: Vec<(i8, Weekday)>,
    /// Days of the month; negative counts from the end.
    pub by_month_day: Vec<i8>,
    pub at: Option<(i8, i8)>,
}

const DAY_CODES: [(&str, Weekday); 7] = [
    ("MO", Weekday::Monday),
    ("TU", Weekday::Tuesday),
    ("WE", Weekday::Wednesday),
    ("TH", Weekday::Thursday),
    ("FR", Weekday::Friday),
    ("SA", Weekday::Saturday),
    ("SU", Weekday::Sunday),
];

fn code(w: Weekday) -> &'static str {
    DAY_CODES
        .iter()
        .find(|(_, d)| *d == w)
        .map_or("MO", |(c, _)| c)
}

impl Rule {
    /// Reads FREQ of DAILY, WEEKLY or MONTHLY, INTERVAL, BYDAY, BYMONTHDAY,
    /// BYHOUR and BYMINUTE; anything else makes it unreadable.
    pub fn parse(rrule: &str) -> Option<Rule> {
        let mut rule = Rule {
            freq: Freq::Daily,
            interval: 1,
            by_day: Vec::new(),
            by_month_day: Vec::new(),
            at: None,
        };
        let mut freq = None;
        let (mut hour, mut minute) = (None, 0);
        for part in rrule.trim().trim_start_matches("RRULE:").split(';') {
            let (key, value) = part.split_once('=')?;
            match key.trim().to_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match value.trim().to_uppercase().as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        _ => return None,
                    });
                }
                "INTERVAL" => rule.interval = value.trim().parse().ok().filter(|n| *n > 0)?,
                "BYDAY" => {
                    for d in value.split(',') {
                        let d = d.trim().to_uppercase();
                        let split = d.len().checked_sub(2)?;
                        let (n, c) = d.split_at(split);
                        let weekday = DAY_CODES.iter().find(|(k, _)| *k == c)?.1;
                        let n: i8 = if n.is_empty() { 0 } else { n.parse().ok()? };
                        rule.by_day.push((n, weekday));
                    }
                }
                "BYMONTHDAY" => {
                    for d in value.split(',') {
                        let n: i8 = d.trim().parse().ok()?;
                        if n == 0 || !(-31..=31).contains(&n) {
                            return None;
                        }
                        rule.by_month_day.push(n);
                    }
                }
                "BYHOUR" => {
                    hour = Some(
                        value
                            .trim()
                            .parse::<i8>()
                            .ok()
                            .filter(|h| (0..24).contains(h))?,
                    )
                }
                "BYMINUTE" => {
                    minute = value
                        .trim()
                        .parse::<i8>()
                        .ok()
                        .filter(|m| (0..60).contains(m))?
                }
                _ => return None,
            }
        }
        rule.freq = freq?;
        rule.at = hour.map(|h| (h, minute));
        Some(rule)
    }

    pub fn to_rrule(&self) -> String {
        let mut parts = vec![format!(
            "FREQ={}",
            match self.freq {
                Freq::Daily => "DAILY",
                Freq::Weekly => "WEEKLY",
                Freq::Monthly => "MONTHLY",
            }
        )];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        if !self.by_day.is_empty() {
            let days: Vec<String> = self
                .by_day
                .iter()
                .map(|(n, w)| {
                    if *n == 0 {
                        code(*w).to_owned()
                    } else {
                        format!("{n}{}", code(*w))
                    }
                })
                .collect();
            parts.push(format!("BYDAY={}", days.join(",")));
        }
        if !self.by_month_day.is_empty() {
            let days: Vec<String> = self.by_month_day.iter().map(i8::to_string).collect();
            parts.push(format!("BYMONTHDAY={}", days.join(",")));
        }
        if let Some((h, m)) = self.at {
            parts.push(format!("BYHOUR={h}"));
            parts.push(format!("BYMINUTE={m}"));
        }
        parts.join(";")
    }

    /// Whether the rule has an instance on `date`, counting INTERVAL from `anchor`.
    fn matches(&self, date: Date, anchor: Date) -> bool {
        let interval = i64::from(self.interval.max(1));
        match self.freq {
            Freq::Daily => time::days_from(anchor, date).rem_euclid(interval) == 0,
            Freq::Weekly => {
                let monday = |d: Date| {
                    d.checked_sub(
                        jiff::Span::new().days(i64::from(d.weekday().to_monday_zero_offset())),
                    )
                    .unwrap_or(d)
                };
                let weeks = time::days_from(monday(anchor), monday(date)) / 7;
                if weeks.rem_euclid(interval) != 0 {
                    return false;
                }
                if self.by_day.is_empty() {
                    date.weekday() == anchor.weekday()
                } else {
                    self.by_day.iter().any(|(_, w)| *w == date.weekday())
                }
            }
            Freq::Monthly => {
                let months = (i64::from(date.year()) * 12 + i64::from(date.month()))
                    - (i64::from(anchor.year()) * 12 + i64::from(anchor.month()));
                if months.rem_euclid(interval) != 0 {
                    return false;
                }
                let last = date.last_of_month().day();
                let by_day = self.by_day.iter().any(|(n, w)| {
                    *w == date.weekday()
                        && match *n {
                            0 => true,
                            n if n > 0 => (date.day() - 1) / 7 + 1 == n,
                            n => (last - date.day()) / 7 + 1 == -n,
                        }
                });
                let by_month_day = self.by_month_day.iter().any(|n| {
                    if *n > 0 {
                        date.day() == *n
                    } else {
                        date.day() == last + 1 + *n
                    }
                });
                if self.by_day.is_empty() && self.by_month_day.is_empty() {
                    date.day() == anchor.day()
                } else {
                    by_day || by_month_day
                }
            }
        }
    }
}

/// The first instance of `rrule` strictly after `after`, in `zone_name`,
/// counting INTERVAL from `anchor`.
pub fn next_after(
    rrule: &str,
    anchor: Instant,
    after: Instant,
    zone_name: &str,
) -> Option<Instant> {
    let rule = Rule::parse(rrule)?;
    let anchor_day = date_of(anchor, zone_name);
    let mut day = date_of(after, zone_name);
    for _ in 0..HORIZON_DAYS {
        if rule.matches(day, anchor_day) {
            let (h, m) = rule.at.unwrap_or((0, 0));
            if let Some(at) = at_on(day, h, m, zone_name)
                && at > after
            {
                return Some(at);
            }
        }
        day = day.tomorrow().ok()?;
    }
    None
}

/// A recurrence as said: what it is called, its rule and what each instance is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub label: String,
    pub freq: Freq,
    pub interval: u32,
    pub by_day: Vec<(i8, Weekday)>,
    pub by_month_day: Vec<i8>,
    pub at: Option<(i8, i8)>,
    pub kind: Said,
}

/// What kind of thing the sentence repeats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    Proposal,
    Reminder(String),
    /// The speaker's own: "my rent is due on the first".
    Mine(String),
}

/// Sentences about recurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurSentence {
    New(Schedule),
    Pause { what: String },
    Resume { what: String },
}

const ORDINALS: [(&str, i8); 10] = [
    ("first", 1),
    ("1st", 1),
    ("second", 2),
    ("2nd", 2),
    ("third", 3),
    ("3rd", 3),
    ("fourth", 4),
    ("4th", 4),
    ("last", -1),
    ("final", -1),
];

/// Words that belong to the schedule, not the name.
const FILLER: [&str; 38] = [
    "every",
    "each",
    "on",
    "the",
    "at",
    "of",
    "a",
    "an",
    "monthly",
    "weekly",
    "fortnightly",
    "daily",
    "other",
    "week",
    "weeks",
    "month",
    "months",
    "day",
    "days",
    "two",
    "in",
    "from",
    "starting",
    "please",
    "lets",
    "let's",
    "we",
    "have",
    "do",
    "our",
    "and",
    "am",
    "pm",
    "is",
    "are",
    "or",
    "for",
    "to",
];

/// A weekday a word names, and whether it is a plural such as "saturdays".
fn weekday_token(t: &str) -> Option<(Weekday, bool)> {
    let plural = t.ends_with("days");
    let t = if plural { &t[..t.len() - 1] } else { t };
    match time::weekday_word(t) {
        Some((w, false)) => Some((w, plural)),
        _ => None,
    }
}

/// "10am", "7:30pm", "19:00", or a bare hour after "at".
fn clock_token(t: &str, after_at: bool) -> Option<(i8, i8)> {
    let (body, pm) = if let Some(b) = t.strip_suffix("pm") {
        (b, Some(true))
    } else if let Some(b) = t.strip_suffix("am") {
        (b, Some(false))
    } else {
        (t, None)
    };
    let (h, m) = match body.split_once([':', '.']) {
        Some((h, m)) => (h, Some(m)),
        None => (body, None),
    };
    if h.is_empty() || !h.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour: i8 = h.parse().ok()?;
    let minute: i8 = match m {
        Some(m) if m.len() == 2 => m.parse().ok()?,
        Some(_) => return None,
        None => 0,
    };
    if pm.is_none() && m.is_none() && !after_at {
        return None;
    }
    let hour = match pm {
        Some(true) if hour == 12 => 12,
        Some(true) => hour + 12,
        Some(false) if hour == 12 => 0,
        Some(false) => hour,
        None if (1..=7).contains(&hour) && m.is_none() => hour + 12,
        None => hour,
    };
    ((0..24).contains(&hour) && (0..60).contains(&minute)).then_some((hour, minute))
}

/// Reads a recurrence sentence. Needs a word that says it repeats.
pub fn parse(text: &str) -> Option<RecurSentence> {
    let toks = time::tokens(text);
    let first = toks.first().map(String::as_str);
    let rest = |n: usize| toks.get(n..).unwrap_or(&[]).join(" ");
    match first {
        Some("pause") => {
            let what = rest(1);
            return (!what.is_empty()).then_some(RecurSentence::Pause {
                what: strip_the(&what),
            });
        }
        Some("resume" | "unpause" | "restart") => {
            return Some(RecurSentence::Resume {
                what: strip_the(&rest(1)),
            });
        }
        _ => {}
    }
    // A question about a schedule is not one being set.
    let asks = matches!(
        first,
        Some(
            "when"
                | "what"
                | "who"
                | "where"
                | "why"
                | "how"
                | "which"
                | "is"
                | "are"
                | "do"
                | "does"
                | "did"
                | "can"
                | "could"
                | "will"
                | "would"
                | "should"
                | "any"
        )
    );
    if asks || text.trim_end().ends_with('?') {
        return None;
    }
    schedule(&toks).map(RecurSentence::New)
}

fn strip_the(text: &str) -> String {
    text.strip_prefix("the ").unwrap_or(text).trim().to_owned()
}

fn schedule(toks: &[String]) -> Option<Schedule> {
    let word = |i: usize| toks.get(i).map_or("", String::as_str);
    let mut body: Vec<String> = toks.to_vec();
    let mut reminder: Option<String> = None;
    if word(0) == "remind" && matches!(word(1), "us" | "me" | "everyone" | "everybody" | "the") {
        let skip = if word(1) == "the" { 3 } else { 2 };
        let to = toks.iter().rposition(|t| t == "to")?;
        if to <= skip {
            return None;
        }
        reminder = Some(toks[to + 1..].join(" ")).filter(|t| !t.is_empty());
        reminder.as_ref()?;
        body = toks[skip..to].to_vec();
    }
    let has = |w: &str| body.iter().any(|t| t == w);
    let mut freq = None;
    let mut interval = 1;
    let mut repeats = reminder.is_some();
    if has("daily") || (has("every") && has("day")) {
        freq = Some(Freq::Daily);
        repeats = true;
    }
    if has("weekly") || (has("every") && has("week")) {
        freq = Some(Freq::Weekly);
        repeats = true;
    }
    if has("fortnightly")
        || (has("every") && (has("other") || has("two") || has("second")) && !has("month"))
    {
        freq = Some(Freq::Weekly);
        interval = 2;
        repeats = true;
    }
    if has("monthly") || (has("every") || has("each")) && has("month") {
        freq = Some(Freq::Monthly);
        repeats = true;
    }
    let mut by_day = Vec::new();
    let mut by_month_day = Vec::new();
    let mut at = None;
    let mut used = vec![false; body.len()];
    for (i, t) in body.iter().enumerate() {
        let before = i.checked_sub(1).map_or("", |b| body[b].as_str());
        if let Some((w, plural)) = weekday_token(t) {
            let ordinal = ORDINALS.iter().find(|(o, _)| *o == before).map(|(_, n)| *n);
            if plural && ordinal.is_none() {
                repeats = true;
            }
            if matches!(before, "every" | "each") {
                repeats = true;
            }
            by_day.push((ordinal.unwrap_or(0), w));
            used[i] = true;
            if ordinal.is_some() && i > 0 {
                used[i - 1] = true;
            }
            continue;
        }
        if let Some(clock) = clock_token(t, before == "at") {
            at = Some(clock);
            used[i] = true;
            continue;
        }
        let next = body.get(i + 1).map_or("", String::as_str);
        if before == "the" && weekday_token(next).is_none() {
            let n = ORDINALS
                .iter()
                .find(|(o, _)| *o == t.as_str())
                .map(|(_, n)| *n)
                .or_else(|| {
                    let digits = t.trim_end_matches(|c: char| c.is_ascii_alphabetic());
                    digits.parse::<i8>().ok().filter(|n| (1..=31).contains(n))
                });
            if let Some(n) = n {
                by_month_day.push(n);
                used[i] = true;
            }
        }
    }
    let label_words: Vec<&str> = body
        .iter()
        .enumerate()
        .filter(|(i, t)| !used[*i] && !FILLER.contains(&t.as_str()))
        .map(|(_, t)| t.as_str())
        .collect();
    let due = label_words.contains(&"due");
    if !by_month_day.is_empty() && due {
        repeats = true;
    }
    if !repeats {
        return None;
    }
    let freq = match freq {
        Some(f) => f,
        None if !by_month_day.is_empty() || by_day.iter().any(|(n, _)| *n != 0) => Freq::Monthly,
        None if !by_day.is_empty() => Freq::Weekly,
        None => return None,
    };
    if freq == Freq::Weekly {
        for d in &mut by_day {
            d.0 = 0;
        }
    }
    let label = label_words.join(" ");
    let kind = match reminder {
        Some(text) => Said::Reminder(text),
        None if due && label_words.first() == Some(&"my") => {
            let what: Vec<&str> = label_words
                .iter()
                .copied()
                .filter(|w| !matches!(*w, "my" | "due"))
                .collect();
            Said::Mine(what.join(" "))
        }
        None if due => Said::Reminder(label.clone()),
        None => Said::Proposal,
    };
    let label = match &kind {
        Said::Reminder(text) if label.is_empty() => text.clone(),
        _ => label,
    };
    if label.is_empty() {
        return None;
    }
    Some(Schedule {
        label,
        freq,
        interval,
        by_day,
        by_month_day,
        at,
        kind,
    })
}

/// "every month on the first Thursday", "every Monday at 10am".
fn schedule_words(rule: &Rule) -> String {
    let days = |rule: &Rule| -> String {
        let names: Vec<String> = rule
            .by_day
            .iter()
            .map(|(n, w)| {
                let ordinal = match n {
                    1 => "first ",
                    2 => "second ",
                    3 => "third ",
                    4 => "fourth ",
                    -1 => "last ",
                    _ => "",
                };
                format!("{ordinal}{}", weekday_name(*w))
            })
            .collect();
        crate::leads::join_names(&names, "and")
    };
    let mut text = match rule.freq {
        Freq::Daily => "every day".to_owned(),
        Freq::Weekly if rule.interval == 2 => format!("every other {}", days(rule)),
        Freq::Weekly => format!("every {}", days(rule)),
        Freq::Monthly if !rule.by_day.is_empty() => format!("every month on the {}", days(rule)),
        Freq::Monthly => {
            let n = rule.by_month_day.first().copied().unwrap_or(1);
            if n < 0 {
                "every month on the last day".to_owned()
            } else {
                format!("every month on the {}", ordinal(n))
            }
        }
    };
    if let Some((h, m)) = rule.at {
        text.push_str(" at ");
        text.push_str(&clock_echo(h, m));
    }
    text
}

fn ordinal(n: i8) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// "three days", "a day".
fn lead_words(seconds: i64) -> String {
    let days = (seconds / DAY).max(1);
    if days == 1 {
        "a day".to_owned()
    } else if days % 7 == 0 {
        let weeks = days / 7;
        if weeks == 1 {
            "a week".to_owned()
        } else {
            format!("{} weeks", time::number_word(weeks))
        }
    } else {
        format!("{} days", time::number_word(days))
    }
}

/// When the instance at `rec.next` opens: its lead time ahead, at ten for
/// one without a time, moved out of quiet hours, never before `now`.
fn open_at(state: &GroupState, rec: &Recurrence, now: Instant) -> Instant {
    let tz = state.timezone.as_str();
    let raw = rec.next.plus_seconds(-rec.lead_seconds);
    let mut at = if rec.has_time {
        raw
    } else {
        at_on(date_of(raw, tz), OPEN_HOUR, 0, tz).unwrap_or(raw)
    };
    let quiet = state.profile.quiet_hours;
    for _ in 0..24 {
        let local = zone::local(at, tz);
        if !quiet.contains(u8::try_from(local.hour()).unwrap_or(0)) {
            break;
        }
        at = at.plus_seconds(3600 - i64::from(local.minute()) * 60 - i64::from(local.second()));
    }
    at.max(now)
}

fn arm(state: &mut GroupState, id: &RecurrenceId, now: Instant, fx: &mut Vec<Effect>) {
    timers::cancel_where(
        state,
        fx,
        |p| matches!(p, TimerPurpose::Recur { id: r } if r == id),
    );
    let Some(rec) = state.recurrences.iter().find(|r| &r.id == id) else {
        return;
    };
    if rec.paused {
        return;
    }
    let at = open_at(state, rec, now);
    timers::arm(state, fx, at, TimerPurpose::Recur { id: id.clone() });
}

/// The first instance after `now` whose lead still fits, skipping one too
/// close to open in time.
fn first_instance(
    rrule: &str,
    anchor: Instant,
    now: Instant,
    lead: i64,
    zone_name: &str,
) -> Option<Instant> {
    let next = next_after(rrule, anchor, now, zone_name)?;
    if next.seconds_since(now) < lead.min(DAY) {
        return next_after(rrule, anchor, next, zone_name);
    }
    Some(next)
}

/// Handles a recurrence sentence.
pub(crate) fn handle(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    sentence: RecurSentence,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = message.room.clone();
    let trigger = addressed_trigger(state, message);
    let reply = |state: &mut GroupState, fx: &mut Vec<Effect>, text: String| {
        let trace = Trace::new(trigger.clone(), RULE, now);
        let note = Recording::Note {
            why: "say pause and its name to stop a repeating one".to_owned(),
        };
        send_as(
            state,
            fx,
            &room,
            text,
            Some(message.id.clone()),
            now,
            trace,
            note,
        );
    };
    if role < Role::Member {
        let text = decline_line(state, &room);
        reply(state, fx, text);
        return;
    }
    if state.rooms.get(&room).is_some_and(|r| r.is_dm()) {
        reply(state, fx, "Ask me in the room it's for.".to_owned());
        return;
    }
    let text = match sentence {
        RecurSentence::New(schedule) => create(state, message, schedule, now, fx),
        RecurSentence::Pause { what } => match find(state, &room, &what, false) {
            Some(id) => {
                timers::cancel_where(
                    state,
                    fx,
                    |p| matches!(p, TimerPurpose::Recur { id: r } if *r == id),
                );
                let rec = state.recurrences.iter_mut().find(|r| r.id == id);
                let label = rec.map(|r| {
                    r.paused = true;
                    r.label.clone()
                });
                format!("Paused the {}.", label.unwrap_or_default())
            }
            None => "Nothing like that repeats here.".to_owned(),
        },
        RecurSentence::Resume { what } => match find(state, &room, &what, true) {
            Some(id) => resume(state, &id, now, fx),
            None => "Nothing like that is paused here.".to_owned(),
        },
    };
    reply(state, fx, text);
}

/// The room's recurrence `what` names, or its only one, paused or not.
fn find(
    state: &GroupState,
    room: &PlatformRoomId,
    what: &str,
    paused: bool,
) -> Option<RecurrenceId> {
    let words = crate::proposal::words(what);
    let here: Vec<&Recurrence> = state
        .recurrences
        .iter()
        .filter(|r| &r.room == room && r.paused == paused)
        .collect();
    let named: Vec<&&Recurrence> = here
        .iter()
        .filter(|r| {
            crate::proposal::words(&r.label)
                .iter()
                .any(|w| words.contains(w))
        })
        .collect();
    match (named.as_slice(), here.as_slice()) {
        ([one, ..], _) => Some(one.id.clone()),
        ([], [one]) if words.is_empty() || words.iter().all(|w| w == "it") => Some(one.id.clone()),
        _ => None,
    }
}

fn create(
    state: &mut GroupState,
    message: &Message,
    schedule: Schedule,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    let tz = state.timezone.clone();
    let today = date_of(now, &tz);
    let mut rule = Rule {
        freq: schedule.freq,
        interval: schedule.interval,
        by_day: schedule.by_day,
        by_month_day: schedule.by_month_day,
        at: schedule.at,
    };
    if rule.freq == Freq::Weekly && rule.by_day.is_empty() {
        rule.by_day.push((0, today.weekday()));
    }
    if rule.freq == Freq::Monthly && rule.by_day.is_empty() && rule.by_month_day.is_empty() {
        rule.by_month_day.push(today.day());
    }
    let rrule = rule.to_rrule();
    let (kind, lead) = match schedule.kind {
        Said::Proposal => (
            RecurKind::Proposal {
                question: format!("Who's in for {}?", schedule.label),
                options: Vec::new(),
            },
            PROPOSAL_LEAD_SECONDS,
        ),
        Said::Reminder(text) => (RecurKind::Reminder { text }, OTHER_LEAD_SECONDS),
        Said::Mine(what) => (
            RecurKind::Commitment {
                who: message.author.clone(),
                what: if what.is_empty() {
                    schedule.label.clone()
                } else {
                    what
                },
            },
            OTHER_LEAD_SECONDS,
        ),
    };
    let Some(next) = first_instance(&rrule, now, now, lead, &tz) else {
        return "I couldn't work out when that falls.".to_owned();
    };
    let id = state.mint_recurrence();
    let rec = Recurrence {
        id: id.clone(),
        rrule,
        kind,
        next,
        lead_seconds: lead,
        last_opened: None,
        paused: false,
        label: schedule.label,
        room: message.room.clone(),
        source: message.id.clone(),
        created_by: message.author.clone(),
        anchor: next,
        has_time: rule.at.is_some(),
        open: None,
        silent: 0,
        took: Vec::new(),
    };
    let echo = instance_echo(state, &rec, now);
    let action = match &rec.kind {
        RecurKind::Proposal { .. } => "ask who's in",
        RecurKind::Reminder { .. } => "remind the room",
        RecurKind::Commitment { .. } => "put it on your list",
    };
    let text = format!(
        "Set: {}, {}. Next is {echo}; I'll {action} {} before.",
        rec.label,
        schedule_words(&rule),
        lead_words(lead)
    );
    fx.push(Effect::Notice(format!(
        "Recurrence in {}: {} ({}).",
        state.room_label(&rec.room),
        rec.label,
        rec.rrule
    )));
    state.recurrences.push(rec);
    arm(state, &id, now, fx);
    text
}

fn instance_echo(state: &GroupState, rec: &Recurrence, now: Instant) -> String {
    let meaning = TimeMeaning::Instant {
        at: rec.next,
        has_time: rec.has_time,
    };
    time::echo(&meaning, &state.timezone, now).unwrap_or_default()
}

fn resume(state: &mut GroupState, id: &RecurrenceId, now: Instant, fx: &mut Vec<Effect>) -> String {
    let tz = state.timezone.clone();
    let Some(rec) = state.recurrences.iter_mut().find(|r| &r.id == id) else {
        return String::new();
    };
    rec.paused = false;
    rec.silent = 0;
    if let Some(next) = first_instance(&rec.rrule, rec.anchor, now, rec.lead_seconds, &tz) {
        rec.next = next;
    }
    let rec = rec.clone();
    arm(state, id, now, fx);
    format!(
        "Resumed the {}. Next is {}.",
        rec.label,
        instance_echo(state, &rec, now)
    )
}

/// A recurrence's timer fired: open this instance and schedule the next.
pub(crate) fn on_recur(
    state: &mut GroupState,
    id: &RecurrenceId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(rec) = state.recurrences.iter().find(|r| &r.id == id).cloned() else {
        return;
    };
    if rec.paused {
        return;
    }
    let tz = state.timezone.clone();
    let echo = instance_echo(state, &rec, now);
    let trace = Trace::new(
        format!("the {} comes round again on {echo}", rec.label),
        RULE,
        now,
    );
    match &rec.kind {
        RecurKind::Proposal { options, .. } => {
            if state.is_observing() {
                let text = format!(
                    "In {} I would have asked who's in for the {}, {echo}.\n{}",
                    state.room_label(&rec.room),
                    rec.label,
                    reactions::feedback_line(state, &rec.room)
                );
                observe::report(state, fx, &text, trace);
            } else {
                let pid = open_instance(state, &rec, options, now, trace, fx);
                if let Some(slot) = state.recurrences.iter_mut().find(|r| &r.id == id) {
                    slot.open = pid;
                }
            }
        }
        RecurKind::Reminder { text } => {
            let line = format!("Reminder for {echo}: {text}.");
            if state.is_observing() {
                let report = format!(
                    "In {} I would have said: {line}\n{}",
                    state.room_label(&rec.room),
                    reactions::feedback_line(state, &rec.room)
                );
                observe::report(state, fx, &report, trace);
            } else {
                send(state, fx, &rec.room, line, None, now, trace);
            }
        }
        RecurKind::Commitment { who, what } => {
            let (due, has_time) = if rec.has_time {
                (Some(rec.next), true)
            } else {
                (day_end(date_of(rec.next, &tz), &tz), false)
            };
            let cid = state.mint_commitment();
            state.commitments.insert(
                cid.clone(),
                Commitment {
                    id: cid.clone(),
                    who: who.clone(),
                    what: what.clone(),
                    for_whom: None,
                    due,
                    due_has_time: has_time,
                    event: None,
                    status: CommitmentStatus::Open,
                    source: Source {
                        room: rec.room.clone(),
                        message: rec.source.clone(),
                        speaker: rec.created_by.clone(),
                        at: now,
                    },
                    created_at: now,
                    confidence: crate::commitment::STATED,
                    quote: None,
                    ledger: None,
                    nudged: 0,
                    nudges: Vec::new(),
                    nudged_at: None,
                    moved_by: Vec::new(),
                    changed_at: Some(now),
                },
            );
            arm_nudges(state, &cid, now, fx);
        }
    }
    let after = next_after(&rec.rrule, rec.anchor, rec.next, &tz);
    if let Some(slot) = state.recurrences.iter_mut().find(|r| &r.id == id) {
        slot.last_opened = Some(now);
        if let Some(next) = after {
            slot.next = next;
        }
    }
    if after.is_some() {
        arm(state, id, now, fx);
    }
}

fn open_instance(
    state: &mut GroupState,
    rec: &Recurrence,
    options: &[String],
    now: Instant,
    trace: Trace,
    fx: &mut Vec<Effect>,
) -> Option<ProposalId> {
    let tz = state.timezone.clone();
    let when = TimeMeaning::Instant {
        at: rec.next,
        has_time: rec.has_time,
    };
    let echo = time::echo(&when, &tz, now);
    let (kind, offers, rule) = if options.len() >= 2 {
        let offers: Vec<Offered> = options
            .iter()
            .filter_map(|o| offered(o, &tz, now))
            .collect();
        (ProposalKind::Choice, offers, decide::default_rule(state))
    } else {
        let offer = Offered {
            label: capitalise(&rec.label),
            when: Some(when),
            echo,
            unresolved: false,
        };
        (
            ProposalKind::Attendance,
            vec![offer],
            CloseRule::WhoeverIsComing,
        )
    };
    if offers.is_empty() {
        return None;
    }
    let pid = decide::open(
        state,
        Opening {
            room: rec.room.clone(),
            thread_root: rec.source.clone(),
            reply_to: None,
            topic: Some(capitalise(&rec.label)),
            options: offers,
            kind,
            organiser: Some(rec.created_by.clone()),
            rule,
            ballot: true,
            source: rec.source.clone(),
            zone: tz,
            reopens: None,
            trace,
        },
        now,
        fx,
    );
    Some(pid)
}

/// A proposal a recurrence opened was decided: its time to close teaches the lead.
pub(crate) fn on_closed(state: &mut GroupState, pid: &ProposalId, open_seconds: i64) {
    let Some(rec) = state
        .recurrences
        .iter_mut()
        .find(|r| r.open.as_ref() == Some(pid))
    else {
        return;
    };
    rec.open = None;
    rec.silent = 0;
    rec.took.push(open_seconds.max(0));
    let mut took = rec.took.clone();
    took.sort_unstable();
    if let Some(median) = took.get(took.len() / 2) {
        rec.lead_seconds = (median * LEAD_FACTOR).max(DAY);
    }
}

/// A proposal a recurrence opened lapsed with no answer. The second in a
/// row pauses it; returns the words that say so.
pub(crate) fn on_lapsed(
    state: &mut GroupState,
    pid: &ProposalId,
    fx: &mut Vec<Effect>,
) -> Option<String> {
    let rec = state
        .recurrences
        .iter_mut()
        .find(|r| r.open.as_ref() == Some(pid))?;
    rec.open = None;
    rec.silent = rec.silent.saturating_add(1);
    if rec.silent < SILENCES {
        return None;
    }
    rec.paused = true;
    let (id, label) = (rec.id.clone(), rec.label.clone());
    timers::cancel_where(
        state,
        fx,
        |p| matches!(p, TimerPurpose::Recur { id: r } if *r == id),
    );
    fx.push(Effect::Notice(format!(
        "Paused the {label} after {SILENCES} with no answer."
    )));
    Some(format!(
        "Nobody answered the last two, so I've paused the {label}; say resume the {label} to bring it back."
    ))
}

/// One line for the charter: "dinner, every month on the first Thursday,
/// next 2026-12-03".
pub fn describe(state: &GroupState, rec: &Recurrence) -> String {
    let rule = Rule::parse(&rec.rrule)
        .map(|r| schedule_words(&r))
        .unwrap_or_default();
    let next = crate::memory::day(rec.next, &state.timezone);
    format!("{}, {rule}, next {next}", rec.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Instant {
        Instant(text.parse().unwrap())
    }

    #[test]
    fn rules_round_trip_and_find_their_days() {
        let rule = Rule::parse("FREQ=MONTHLY;BYDAY=1TH").unwrap();
        assert_eq!(rule.to_rrule(), "FREQ=MONTHLY;BYDAY=1TH");
        let next = next_after(
            "FREQ=MONTHLY;BYDAY=1TH",
            at("2026-11-01T00:00:00Z"),
            at("2026-11-09T00:00:00Z"),
            "UTC",
        );
        assert_eq!(next, Some(at("2026-12-03T00:00:00Z")));
        let next = next_after(
            "FREQ=WEEKLY;BYDAY=MO;BYHOUR=10;BYMINUTE=0",
            at("2026-11-02T00:00:00Z"),
            at("2026-11-03T00:00:00Z"),
            "Europe/London",
        );
        assert_eq!(next, Some(at("2026-11-09T10:00:00Z")));
        let next = next_after(
            "FREQ=MONTHLY;BYMONTHDAY=-1",
            at("2026-11-01T00:00:00Z"),
            at("2026-11-03T00:00:00Z"),
            "UTC",
        );
        assert_eq!(next, Some(at("2026-11-30T00:00:00Z")));
        let fortnight = "FREQ=WEEKLY;INTERVAL=2;BYDAY=SA";
        let first = at("2026-11-07T00:00:00Z");
        let next = next_after(fortnight, first, first, "UTC");
        assert_eq!(next, Some(at("2026-11-21T00:00:00Z")));
        let daily = next_after("FREQ=DAILY;INTERVAL=3", first, first, "UTC");
        assert_eq!(daily, Some(at("2026-11-10T00:00:00Z")));
        assert!(Rule::parse("FREQ=YEARLY").is_none());
        assert!(Rule::parse("FREQ=WEEKLY;BYSETPOS=1").is_none());
    }

    fn new(text: &str) -> Schedule {
        match parse(text) {
            Some(RecurSentence::New(s)) => s,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn sentences_read_as_schedules() {
        let dinner = new("monthly dinner on the first Thursday");
        assert_eq!(dinner.label, "dinner");
        assert_eq!(dinner.freq, Freq::Monthly);
        assert_eq!(dinner.by_day, vec![(1, Weekday::Thursday)]);
        assert_eq!(dinner.kind, Said::Proposal);
        let planning = new("weekly planning every Monday 10am");
        assert_eq!(planning.label, "planning");
        assert_eq!(planning.freq, Freq::Weekly);
        assert_eq!(planning.at, Some((10, 0)));
        let subs = new("subs due on the first");
        assert_eq!(subs.freq, Freq::Monthly);
        assert_eq!(subs.by_month_day, vec![1]);
        assert_eq!(subs.kind, Said::Reminder("subs due".into()));
        let boots = new("remind us every Saturday 9am to bring boots");
        assert_eq!(boots.kind, Said::Reminder("bring boots".into()));
        assert_eq!(boots.by_day, vec![(0, Weekday::Saturday)]);
        assert_eq!(boots.at, Some((9, 0)));
        let training = new("training saturdays");
        assert_eq!(training.label, "training");
        assert_eq!(training.freq, Freq::Weekly);
        assert_eq!(
            parse("pause the dinner"),
            Some(RecurSentence::Pause {
                what: "dinner".into()
            })
        );
        assert_eq!(
            parse("resume"),
            Some(RecurSentence::Resume {
                what: String::new()
            })
        );
        for text in [
            "dinner on the 1st",
            "who's in for saturday",
            "thursday at 7",
            "thurs or fri?",
            "when does the wall close on sundays?",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
