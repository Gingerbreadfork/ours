//! Time expressions read by rule (DESIGN.md section 12): weekdays, today
//! and tomorrow, weeks, weekends and months, dates, times of day and
//! ranges, each in the speaker's zone and echoed back as the date chosen.
//! What the rules cannot read goes to the resolver as `Effect::ResolveTime`.

use crate::zone;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Zoned};
use ours_types::{Instant, PlatformMessageId, PlatformRoomId, TimeMeaning, TimeResolution};

/// On a Sunday from this hour on, "next week" has two readings.
pub const SUNDAY_EVENING_HOUR: i8 = 17;
/// The local hour a day without a time is taken to start, for deadlines.
pub const EVENT_HOUR: i8 = 18;
/// Dates further away than this are echoed with their month.
const NEAR_DAYS: i64 = 27;

/// A time expression the rules understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The words that carried it, lower-cased.
    pub phrase: String,
    pub meaning: TimeMeaning,
    /// How to say it back: "Thursday the 14th", "the week of the 9th".
    pub echo: String,
}

impl Resolved {
    pub fn resolution(self, room: PlatformRoomId, message: PlatformMessageId) -> TimeResolution {
        TimeResolution {
            room,
            message,
            phrase: Some(self.phrase),
            meaning: self.meaning,
            echo: Some(self.echo),
        }
    }
}

/// Words that say a time the rules do not read, such as "after the
/// holidays" or "when the exams finish". Used only on short phrases.
const HINTS: [&str; 40] = [
    "after",
    "before",
    "until",
    "till",
    "holiday",
    "holidays",
    "hols",
    "easter",
    "christmas",
    "xmas",
    "new year",
    "term",
    "semester",
    "exams",
    "exam",
    "finish",
    "finishes",
    "finished",
    "over",
    "back",
    "returns",
    "summer",
    "winter",
    "spring",
    "autumn",
    "soon",
    "later",
    "sometime",
    "when",
    "once",
    "birthday",
    "payday",
    "bank holiday",
    "long weekend",
    "early",
    "mid",
    "late",
    "week after",
    "eve",
    "lunchtime",
];

/// Whether a short phrase carries a time the rules could not read, so the
/// resolver should be asked.
pub fn wants_resolver(text: &str) -> bool {
    let padded = format!(" {} ", tokens(text).join(" "));
    HINTS.iter().any(|h| padded.contains(&format!(" {h} ")))
}

/// Resolves the first time expression in `text` as the speaker in
/// `zone_name` means it at `now`. The conventional reading wins; only
/// "next week" on a Sunday evening comes back ambiguous.
pub fn resolve_time_rules(text: &str, zone_name: &str, now: Instant) -> Option<Resolved> {
    let cx = Cx::new(zone_name, now);
    let toks = tokens(text);
    let evening = toks.iter().any(|t| EVENING_WORDS.contains(&t.as_str()));
    for i in 0..toks.len() {
        let Some(found) = date_at(&cx, &toks, i) else {
            continue;
        };
        let near = time_near(&toks, &found);
        let (start, end, clock, part) = match near {
            Some(n) => (
                found.start.min(n.start),
                found.end.max(n.end),
                Some(n.clock),
                n.part,
            ),
            None => (found.start, found.end, None, found.part),
        };
        let part = if part == Part::None && evening {
            Part::Evening
        } else {
            part
        };
        let meaning = cx.meaning(found.when, clock, part)?;
        let echo = echo(&meaning, zone_name, now)?;
        return Some(Resolved {
            phrase: toks[start..end].join(" "),
            meaning,
            echo,
        });
    }
    // A time with no day: today if it is still ahead, else tomorrow.
    for i in 0..toks.len() {
        let Some((len, clock, strong)) = clock_at(&toks, i) else {
            continue;
        };
        if !strong {
            continue;
        }
        let part = if evening { Part::Evening } else { Part::None };
        let (h, m) = clock.resolve(part);
        let today = cx.at(cx.today, h, m)?;
        let at = if today > now.0 {
            today
        } else {
            cx.at(cx.plus(cx.today, 1)?, h, m)?
        };
        let meaning = TimeMeaning::Instant {
            at: Instant(at),
            has_time: true,
        };
        let echo = echo(&meaning, zone_name, now)?;
        return Some(Resolved {
            phrase: toks[i..i + len].join(" "),
            meaning,
            echo,
        });
    }
    None
}

/// How to say a meaning back, in `zone_name` as of `now`.
pub fn echo(meaning: &TimeMeaning, zone_name: &str, now: Instant) -> Option<String> {
    let tz = zone::or_utc(zone_name);
    let today = now.0.to_zoned(tz.clone()).date();
    let date_of = |at: Instant| at.0.to_zoned(tz.clone()).date();
    match meaning {
        TimeMeaning::Instant { at, has_time } => {
            let local = at.0.to_zoned(tz.clone());
            let day = day_echo(local.date(), today);
            if *has_time {
                Some(format!(
                    "{day} at {}",
                    clock_echo(local.hour(), local.minute())
                ))
            } else {
                Some(day)
            }
        }
        TimeMeaning::Span { start, end } => {
            let start = date_of(*start);
            let last = date_of(end.plus_seconds(-1));
            Some(span_echo(start, last, today))
        }
        TimeMeaning::Ambiguous { candidates } => {
            let dates: Vec<Date> = candidates.iter().map(|c| date_of(*c)).collect();
            if dates.is_empty() {
                return None;
            }
            if dates.iter().all(|d| d.weekday() == Weekday::Monday) {
                let days: Vec<String> = dates.iter().map(|d| day_of_month(*d, today)).collect();
                Some(format!("the week of {}", days.join(" or ")))
            } else {
                let days: Vec<String> = dates.iter().map(|d| day_echo(*d, today)).collect();
                Some(days.join(" or "))
            }
        }
        TimeMeaning::Recurrence { first, .. } => {
            Some(format!("from {}", day_echo(date_of(*first), today)))
        }
        TimeMeaning::None => None,
    }
}

/// When a meaning starts, as an instant: a day without a time counts from
/// `EVENT_HOUR` that day. Used for deadlines and for ties.
pub fn event_start(meaning: &TimeMeaning, zone_name: &str) -> Option<Instant> {
    let tz = zone::or_utc(zone_name);
    let evening = |at: Instant| -> Option<Instant> {
        let date = at.0.to_zoned(tz.clone()).date();
        let ts = date
            .at(EVENT_HOUR, 0, 0, 0)
            .to_zoned(tz.clone())
            .ok()?
            .timestamp();
        Some(Instant(ts))
    };
    match meaning {
        TimeMeaning::Instant { at, has_time: true } => Some(*at),
        TimeMeaning::Instant {
            at,
            has_time: false,
        } => evening(*at),
        TimeMeaning::Span { start, .. } => evening(*start),
        TimeMeaning::Ambiguous { candidates } => candidates.first().and_then(|c| evening(*c)),
        TimeMeaning::Recurrence { first, .. } => Some(*first),
        TimeMeaning::None => None,
    }
}

/// The first and last instants a meaning covers: a whole day for a day
/// without a time, the moment itself otherwise.
pub fn bounds(meaning: &TimeMeaning) -> Option<(Instant, Instant)> {
    const DAY: i64 = 24 * 60 * 60;
    match meaning {
        TimeMeaning::Instant { at, has_time: true } => Some((*at, *at)),
        TimeMeaning::Instant {
            at,
            has_time: false,
        } => Some((*at, at.plus_seconds(DAY))),
        TimeMeaning::Span { start, end } => Some((*start, *end)),
        TimeMeaning::Ambiguous { candidates } => {
            let first = *candidates.first()?;
            Some((first, first.plus_seconds(DAY)))
        }
        TimeMeaning::Recurrence { first, .. } => Some((*first, *first)),
        TimeMeaning::None => None,
    }
}

/// "today", "tomorrow", "in three days", "in two weeks", or nothing for a
/// day in the past.
pub fn relative(at: Instant, zone_name: &str, now: Instant) -> Option<String> {
    let tz = zone::or_utc(zone_name);
    let today = now.0.to_zoned(tz.clone()).date();
    let day = at.0.to_zoned(tz).date();
    let days = days_between(today, day);
    Some(match days {
        0 => "today".to_owned(),
        1 => "tomorrow".to_owned(),
        2..=13 => format!("in {} days", number_word(days)),
        14.. => format!("in {} weeks", number_word(days / 7)),
        _ => return None,
    })
}

/// The civil date `at` falls on in the named zone.
pub fn date_of(at: Instant, zone_name: &str) -> Date {
    zone::local(at, zone_name).date()
}

/// `date` at `hour:minute` in the named zone.
pub fn at_on(date: Date, hour: i8, minute: i8, zone_name: &str) -> Option<Instant> {
    let local = date.at(hour, minute, 0, 0);
    let ts = local.to_zoned(zone::or_utc(zone_name)).ok()?.timestamp();
    Some(Instant(ts))
}

/// The instant `date` ends in the named zone, which is when the next day starts.
pub fn day_end(date: Date, zone_name: &str) -> Option<Instant> {
    at_on(date.tomorrow().ok()?, 0, 0, zone_name)
}

/// "2026-11-12" or "2026-11-12T17:00" read in the named zone, with whether
/// it carried a time of day.
pub fn parse_local(text: &str, zone_name: &str) -> Option<(Instant, bool)> {
    let t = text.trim();
    if t.contains(['T', ' ']) {
        let dt: jiff::civil::DateTime = t.parse().ok()?;
        let ts = dt.to_zoned(zone::or_utc(zone_name)).ok()?.timestamp();
        return Some((Instant(ts), true));
    }
    let date: Date = t.parse().ok()?;
    Some((at_on(date, 0, 0, zone_name)?, false))
}

/// Whole days from `from` to `to`, negative when `to` is earlier.
pub fn days_from(from: Date, to: Date) -> i64 {
    days_between(from, to)
}

/// "Thursday the 14th" within four weeks of `today`, else "Saturday 14
/// March", with the year when it is not this one.
pub fn day_echo(date: Date, today: Date) -> String {
    let weekday = weekday_name(date.weekday());
    if days_between(today, date).abs() <= NEAR_DAYS {
        format!("{weekday} the {}", ordinal(date.day()))
    } else if date.year() == today.year() {
        format!("{weekday} {} {}", date.day(), month_name(date.month()))
    } else {
        format!(
            "{weekday} {} {} {}",
            date.day(),
            month_name(date.month()),
            date.year()
        )
    }
}

/// "7:30pm", "7pm", "noon", "midnight".
pub fn clock_echo(hour: i8, minute: i8) -> String {
    match (hour, minute) {
        (0, 0) => "midnight".to_owned(),
        (12, 0) => "noon".to_owned(),
        _ => {
            let suffix = if hour < 12 { "am" } else { "pm" };
            let h = match hour % 12 {
                0 => 12,
                h => h,
            };
            if minute == 0 {
                format!("{h}{suffix}")
            } else {
                format!("{h}:{minute:02}{suffix}")
            }
        }
    }
}

pub fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

/// Small counts as words, larger ones as digits.
pub fn number_word(n: i64) -> String {
    const WORDS: [&str; 11] = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    usize::try_from(n)
        .ok()
        .and_then(|i| WORDS.get(i))
        .map_or_else(|| n.to_string(), |w| (*w).to_owned())
}

/// The weekday a word names, and whether the word is also an everyday one
/// like "sat" or "sun" that needs context to count.
pub fn weekday_word(word: &str) -> Option<(Weekday, bool)> {
    Some(match word {
        "monday" | "mon" => (Weekday::Monday, false),
        "tuesday" | "tue" | "tues" => (Weekday::Tuesday, false),
        "wednesday" | "weds" => (Weekday::Wednesday, false),
        "wed" => (Weekday::Wednesday, true),
        "thursday" | "thu" | "thur" | "thurs" => (Weekday::Thursday, false),
        "friday" | "fri" => (Weekday::Friday, false),
        "saturday" => (Weekday::Saturday, false),
        "sat" => (Weekday::Saturday, true),
        "sunday" => (Weekday::Sunday, false),
        "sun" => (Weekday::Sunday, true),
        _ => return None,
    })
}

/// The words of a text as the resolver reads them: lower case, curly
/// apostrophes straightened, punctuation gone, "fri-sun" split into
/// "fri to sun" and possessives dropped.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    let lower = text.to_lowercase().replace(['\u{2018}', '\u{2019}'], "'");
    let cleaned: String = lower
        .chars()
        .map(|c| {
            if matches!(
                c,
                ',' | ';' | '!' | '?' | '(' | ')' | '"' | '[' | ']' | '*' | '~' | '_'
            ) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut out = Vec::new();
    for raw in cleaned.split_whitespace() {
        if raw.starts_with('@') {
            out.push("at".to_owned());
        }
        let t = raw
            .trim_start_matches('@')
            .trim_end_matches(['.', ':'])
            .trim_matches('\'');
        let t = t.strip_suffix("'s").unwrap_or(t);
        if t.is_empty() {
            continue;
        }
        if is_iso(t) {
            out.push(t.to_owned());
            continue;
        }
        match t.split_once(['-', '\u{2013}']) {
            Some((a, b)) if !a.is_empty() && !b.is_empty() => {
                out.push(a.to_owned());
                out.push("to".to_owned());
                out.push(b.to_owned());
            }
            _ => out.push(t.trim_matches('-').to_owned()),
        }
    }
    out.retain(|t| !t.is_empty());
    out
}

const EVENING_WORDS: [&str; 9] = [
    "tonight", "night", "evening", "dinner", "drinks", "pub", "bar", "party", "supper",
];
const TO_WORDS: [&str; 5] = ["to", "till", "until", "through", "thru"];
/// Words that may sit between a day and its time: "friday at 7".
const TIME_LINKS: [&str; 5] = ["at", "around", "about", "from", "by"];
/// Words before a risky weekday that make it one: "on sat", "fri or sat".
const DAY_LEADS: [&str; 11] = [
    "on", "this", "next", "coming", "or", "to", "and", "vs", "till", "until", "from",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    None,
    Morning,
    Evening,
}

#[derive(Debug, Clone, Copy)]
enum When {
    Day(Date),
    /// From `start` up to `end`, exclusive.
    Days {
        start: Date,
        end: Date,
    },
    /// Two readings of a week, by their Mondays.
    Weeks([Date; 2]),
    At(jiff::Timestamp),
}

#[derive(Debug, Clone, Copy)]
struct Found {
    start: usize,
    end: usize,
    when: When,
    part: Part,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clock {
    hour: i8,
    minute: i8,
    /// On the 24-hour clock already: am or pm was given, or the hour says so.
    explicit: bool,
}

impl Clock {
    /// Hours without am or pm: one to seven are in the afternoon or
    /// evening, eight to eleven in the morning unless the text says evening.
    fn resolve(self, part: Part) -> (i8, i8) {
        let h = if self.explicit || self.hour >= 12 || part == Part::Morning {
            self.hour
        } else if part == Part::Evening || (1..=7).contains(&self.hour) {
            self.hour + 12
        } else {
            self.hour
        };
        (h, self.minute)
    }
}

struct Near {
    start: usize,
    end: usize,
    clock: Clock,
    part: Part,
}

struct Cx {
    tz: TimeZone,
    now: Zoned,
    today: Date,
    month_first: bool,
}

impl Cx {
    fn new(zone_name: &str, now: Instant) -> Cx {
        let tz = zone::or_utc(zone_name);
        let local = now.0.to_zoned(tz.clone());
        Cx {
            today: local.date(),
            now: local,
            tz,
            month_first: zone::month_first(zone_name),
        }
    }

    fn plus(&self, date: Date, days: i64) -> Option<Date> {
        date.checked_add(SignedDuration::from_hours(24 * days)).ok()
    }

    fn start(&self, date: Date) -> Option<Instant> {
        Some(Instant(date.to_zoned(self.tz.clone()).ok()?.timestamp()))
    }

    fn at(&self, date: Date, hour: i8, minute: i8) -> Option<jiff::Timestamp> {
        let (date, hour) = if hour >= 24 {
            (self.plus(date, 1)?, hour - 24)
        } else {
            (date, hour)
        };
        Some(
            date.at(hour, minute, 0, 0)
                .to_zoned(self.tz.clone())
                .ok()?
                .timestamp(),
        )
    }

    fn offset(date: Date) -> i64 {
        i64::from(date.weekday().to_monday_zero_offset())
    }

    fn next_monday(&self) -> Option<Date> {
        self.plus(self.today, 7 - Cx::offset(self.today))
    }

    /// The weekday as a bare "friday" means it, the next one after today, or
    /// as "this friday" does, which may be today.
    fn coming(&self, weekday: Weekday, this: bool) -> Option<Date> {
        let ahead =
            (i64::from(weekday.to_monday_zero_offset()) - Cx::offset(self.today)).rem_euclid(7);
        let ahead = if ahead == 0 && !this { 7 } else { ahead };
        self.plus(self.today, ahead)
    }

    /// "next friday": the Friday of next week, Monday to Sunday. Before
    /// Friday that skips the coming one; from Saturday it is the coming one.
    fn next(&self, weekday: Weekday) -> Option<Date> {
        self.plus(
            self.next_monday()?,
            i64::from(weekday.to_monday_zero_offset()),
        )
    }

    fn this_weekend(&self) -> Option<Date> {
        match Cx::offset(self.today) {
            5 => Some(self.today),
            6 => self.plus(self.today, -1),
            off => self.plus(self.today, 5 - off),
        }
    }

    fn month_start(&self, date: Date, months: i64) -> Option<Date> {
        date.first_of_month()
            .checked_add(Span::new().months(months))
            .ok()
    }

    /// A day and month with no year: this year's if it has not passed,
    /// else next year's.
    fn upcoming(&self, month: i8, day: i8, year: Option<i16>) -> Option<Date> {
        if let Some(year) = year {
            return Date::new(year, month, day).ok();
        }
        let this = Date::new(self.today.year(), month, day).ok();
        match this {
            Some(d) if d >= self.today => Some(d),
            _ => Date::new(self.today.year() + 1, month, day).ok(),
        }
    }

    fn meaning(&self, when: When, clock: Option<Clock>, part: Part) -> Option<TimeMeaning> {
        Some(match (when, clock) {
            (When::Day(date), Some(clock)) => {
                let (h, m) = clock.resolve(part);
                TimeMeaning::Instant {
                    at: Instant(self.at(date, h, m)?),
                    has_time: true,
                }
            }
            (When::Day(date), None) => TimeMeaning::Instant {
                at: self.start(date)?,
                has_time: false,
            },
            (When::Days { start, end }, _) => TimeMeaning::Span {
                start: self.start(start)?,
                end: self.start(end)?,
            },
            (When::Weeks(mondays), _) => TimeMeaning::Ambiguous {
                candidates: vec![self.start(mondays[0])?, self.start(mondays[1])?],
            },
            (When::At(ts), _) => TimeMeaning::Instant {
                at: Instant(ts),
                has_time: true,
            },
        })
    }
}

fn word(toks: &[String], i: usize) -> &str {
    toks.get(i).map_or("", String::as_str)
}

fn is_iso(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn month_word(t: &str) -> Option<i8> {
    Some(match t {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

fn month_name(month: i8) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    usize::try_from(month - 1)
        .ok()
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("")
}

/// "14", "14th", "1st": a day of the month, and whether it had a suffix.
fn day_number(t: &str) -> Option<(i8, bool)> {
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    let suffix = &t[digits.len()..];
    if digits.is_empty() || digits.len() > 2 || !matches!(suffix, "" | "st" | "nd" | "rd" | "th") {
        return None;
    }
    let n: i8 = digits.parse().ok()?;
    (1..=31).contains(&n).then_some((n, !suffix.is_empty()))
}

fn year_number(t: &str) -> Option<i16> {
    if t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()) {
        t.parse().ok().filter(|y| (2000..=2100).contains(y))
    } else {
        None
    }
}

fn count_word(t: &str) -> Option<i64> {
    Some(match t {
        "a" | "an" | "one" => 1,
        "two" | "couple" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        other => {
            let n: i64 = other.parse().ok()?;
            return (1..=365).contains(&n).then_some(n);
        }
    })
}

fn days_between(from: Date, to: Date) -> i64 {
    to.duration_since(from).as_secs() / (24 * 60 * 60)
}

fn ordinal(day: i8) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

/// "the 14th" near `today`, "14 March" further away.
fn day_of_month(date: Date, today: Date) -> String {
    if days_between(today, date).abs() <= NEAR_DAYS {
        format!("the {}", ordinal(date.day()))
    } else {
        format!("{} {}", date.day(), month_name(date.month()))
    }
}

fn span_echo(start: Date, last: Date, today: Date) -> String {
    let days = days_between(start, last) + 1;
    if days == 7 && start.weekday() == Weekday::Monday {
        return format!("the week of {}", day_of_month(start, today));
    }
    if days == 2 && start.weekday() == Weekday::Saturday {
        return format!("the weekend of {}", day_of_month(start, today));
    }
    if start.day() == 1 && last == start.last_of_month() {
        return if start.year() == today.year() {
            month_name(start.month()).to_owned()
        } else {
            format!("{} {}", month_name(start.month()), start.year())
        };
    }
    if days <= 1 {
        return day_echo(start, today);
    }
    let far = days_between(today, start).abs() > NEAR_DAYS;
    if far && start.month() == last.month() && start.year() == last.year() {
        let year = if start.year() == today.year() {
            String::new()
        } else {
            format!(" {}", start.year())
        };
        return format!(
            "{} to {} {}{year}",
            start.day(),
            last.day(),
            month_name(start.month())
        );
    }
    format!("{} to {}", day_echo(start, today), day_echo(last, today))
}

/// A weekday at `i`, with whether its word needs context and whether that
/// context is there.
fn weekday_at(toks: &[String], i: usize) -> Option<Weekday> {
    let (weekday, risky) = weekday_word(word(toks, i))?;
    if !risky {
        return Some(weekday);
    }
    let before = i.checked_sub(1).map_or("", |b| word(toks, b));
    let after = word(toks, i + 1);
    let alone = toks.len() == 1;
    let fits = alone
        || DAY_LEADS.contains(&before)
        || matches!(
            after,
            "or" | "to" | "night" | "morning" | "afternoon" | "evening" | "at"
        )
        || clock_at(toks, i + 1).is_some_and(|(_, _, strong)| strong);
    fits.then_some(weekday)
}

/// A date such as "14 march", "march 14th", "the 14th of march" or
/// "2026-03-14" at `i`, with how many words it took.
fn date_words(cx: &Cx, toks: &[String], i: usize) -> Option<(usize, Date)> {
    let t = word(toks, i);
    if is_iso(t) {
        return Some((1, t.parse().ok()?));
    }
    if let Some(found) = numeric_date(cx, t) {
        return Some((1, found));
    }
    if let Some((day, _)) = day_number(t) {
        let of = usize::from(word(toks, i + 1) == "of");
        if let Some(month) = month_word(word(toks, i + 1 + of)) {
            let year = year_number(word(toks, i + 2 + of));
            let len = 2 + of + usize::from(year.is_some());
            return Some((len, cx.upcoming(month, day, year)?));
        }
    }
    if let Some(month) = month_word(t)
        && let Some((day, _)) = day_number(word(toks, i + 1))
    {
        let year = year_number(word(toks, i + 2));
        let len = 2 + usize::from(year.is_some());
        return Some((len, cx.upcoming(month, day, year)?));
    }
    None
}

/// "14/3", "3/14" in a month-first zone, "14/3/27", "14/03/2027".
fn numeric_date(cx: &Cx, t: &str) -> Option<Date> {
    let parts: Vec<&str> = t.split('/').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || p.len() > 4) {
        return None;
    }
    let a: i8 = parts[0].parse().ok()?;
    let b: i8 = parts[1].parse().ok()?;
    let year = match parts.get(2) {
        Some(y) if y.len() == 2 => Some(2000 + y.parse::<i16>().ok()?),
        Some(y) => Some(y.parse::<i16>().ok()?),
        None => None,
    };
    let (day, month) = if a > 12 {
        (a, b)
    } else if b > 12 || cx.month_first {
        (b, a)
    } else {
        (a, b)
    };
    if !(1..=12).contains(&month) {
        return None;
    }
    cx.upcoming(month, day, year)
}

/// "the 14th" or "on the 14th": that day this month, or next month once
/// it has passed.
fn day_of_month_at(cx: &Cx, toks: &[String], i: usize) -> Option<Date> {
    let (day, suffixed) = day_number(word(toks, i))?;
    let before = i.checked_sub(1).map_or("", |b| word(toks, b));
    if !suffixed || !matches!(before, "the" | "on") {
        return None;
    }
    for months in 0..3 {
        let first = cx.month_start(cx.today, months)?;
        if let Ok(date) = Date::new(first.year(), first.month(), day)
            && date >= cx.today
        {
            return Some(date);
        }
    }
    None
}

fn found(start: usize, len: usize, when: When) -> Found {
    Found {
        start,
        end: start + len,
        when,
        part: Part::None,
    }
}

/// The time expression that starts at `i`, longest first.
fn date_at(cx: &Cx, toks: &[String], i: usize) -> Option<Found> {
    if let Some(f) = range_at(cx, toks, i) {
        return Some(f);
    }
    let t = word(toks, i);
    let next = word(toks, i + 1);
    let day = |len: usize, date: Date| Some(found(i, len, When::Day(date)));
    match t {
        "today" => return day(1, cx.today),
        "tonight" => {
            return Some(Found {
                part: Part::Evening,
                ..found(i, 1, When::Day(cx.today))
            });
        }
        "tomorrow" | "tmrw" | "tmr" | "tomoz" | "tomorow" => {
            let date = cx.plus(cx.today, 1)?;
            let part = part_of(next);
            let len = 1 + usize::from(part != Part::None);
            return Some(Found {
                part,
                ..found(i, len, When::Day(date))
            });
        }
        "day" if next == "after" => {
            let before = i.checked_sub(1).map_or("", |b| word(toks, b));
            let tomorrow = word(toks, i + 2) == "tomorrow";
            if tomorrow || before == "the" {
                return day(2 + usize::from(tomorrow), cx.plus(cx.today, 2)?);
            }
        }
        "weekend" => {
            let sat = cx.this_weekend()?;
            return Some(found(
                i,
                1,
                When::Days {
                    start: sat,
                    end: cx.plus(sat, 2)?,
                },
            ));
        }
        "week" if next == "after" && word(toks, i + 2) == "next" => {
            let monday = cx.plus(cx.next_monday()?, 7)?;
            return Some(found(
                i,
                3,
                When::Days {
                    start: monday,
                    end: cx.plus(monday, 7)?,
                },
            ));
        }
        "eom" => return day(1, cx.today.last_of_month()),
        "month" if next == "end" => return day(2, cx.today.last_of_month()),
        "end" if next == "of" => return end_of(cx, toks, i),
        "this" | "next" | "coming" => return qualified(cx, toks, i),
        "in" => return in_from_now(cx, toks, i),
        _ => {}
    }
    if let Some((len, date)) = date_words(cx, toks, i) {
        return day(len, date);
    }
    if let Some(date) = day_of_month_at(cx, toks, i) {
        return day(1, date);
    }
    if let Some(weekday) = weekday_at(toks, i) {
        // "thursday 14 march" and "thursday the 14th" name the date outright.
        if let Some((len, date)) = date_words(cx, toks, i + 1) {
            return day(1 + len, date);
        }
        if next == "the"
            && let Some(date) = day_of_month_at(cx, toks, i + 2)
        {
            return day(3, date);
        }
        let part = part_of(next);
        let len = 1 + usize::from(part != Part::None);
        return Some(Found {
            part,
            ..found(i, len, When::Day(cx.coming(weekday, false)?))
        });
    }
    None
}

fn part_of(word: &str) -> Part {
    match word {
        "night" | "evening" => Part::Evening,
        "morning" => Part::Morning,
        _ => Part::None,
    }
}

/// "this friday", "next week", "this coming saturday", "next month".
fn qualified(cx: &Cx, toks: &[String], i: usize) -> Option<Found> {
    let q = word(toks, i);
    let mut k = i + 1;
    if q == "this" && word(toks, k) == "coming" {
        k += 1;
    }
    let target = word(toks, k);
    let len = k - i + 1;
    let days = |start: Date, end: Date| Some(found(i, len, When::Days { start, end }));
    match (q, target) {
        ("this" | "coming", "weekend") => {
            let sat = cx.this_weekend()?;
            days(sat, cx.plus(sat, 2)?)
        }
        ("next", "weekend") => {
            let sat = cx.plus(cx.this_weekend()?, 7)?;
            days(sat, cx.plus(sat, 2)?)
        }
        ("this", "week") => days(cx.today, cx.next_monday()?),
        ("next" | "coming", "week") => {
            let monday = cx.next_monday()?;
            let sunday_evening = Cx::offset(cx.today) == 6 && cx.now.hour() >= SUNDAY_EVENING_HOUR;
            if q == "next" && sunday_evening {
                return Some(found(i, len, When::Weeks([monday, cx.plus(monday, 7)?])));
            }
            days(monday, cx.plus(monday, 7)?)
        }
        ("this", "month") => days(cx.today, cx.month_start(cx.today, 1)?),
        ("next", "month") => days(cx.month_start(cx.today, 1)?, cx.month_start(cx.today, 2)?),
        _ => {
            let weekday = weekday_word(target)?.0;
            let date = match q {
                "next" => cx.next(weekday)?,
                "this" => cx.coming(weekday, true)?,
                _ => cx.coming(weekday, false)?,
            };
            let part = part_of(word(toks, k + 1));
            Some(Found {
                part,
                ..found(i, len + usize::from(part != Part::None), When::Day(date))
            })
        }
    }
}

/// "end of the month", "end of next month", "end of the week".
fn end_of(cx: &Cx, toks: &[String], i: usize) -> Option<Found> {
    let mut k = i + 2;
    if word(toks, k) == "the" || word(toks, k) == "this" {
        k += 1;
    }
    let next = word(toks, k) == "next";
    if next {
        k += 1;
    }
    let date = match word(toks, k) {
        "month" if next => cx.month_start(cx.today, 1)?.last_of_month(),
        "month" => cx.today.last_of_month(),
        "week" if next => cx.plus(cx.next_monday()?, 4)?,
        "week" => {
            let friday = cx.plus(cx.today, 4 - Cx::offset(cx.today))?;
            if friday >= cx.today { friday } else { cx.today }
        }
        _ => return None,
    };
    Some(found(i, k - i + 1, When::Day(date)))
}

/// "in 3 days", "in a week", "in a fortnight", "in a couple of weeks",
/// "in 2 hours", "in june".
fn in_from_now(cx: &Cx, toks: &[String], i: usize) -> Option<Found> {
    let first = word(toks, i + 1);
    if (first == "a" && word(toks, i + 2) == "fortnight") || first == "fortnight" {
        let len = if first == "a" { 3 } else { 2 };
        return Some(found(i, len, When::Day(cx.plus(cx.today, 14)?)));
    }
    if let Some(month) = month_word(first)
        && first != "may"
        && first != "mar"
    {
        let mut start = Date::new(cx.today.year(), month, 1).ok()?;
        if start.last_of_month() < cx.today {
            start = Date::new(cx.today.year() + 1, month, 1).ok()?;
        }
        let end = cx.month_start(start, 1)?;
        return Some(found(i, 2, When::Days { start, end }));
    }
    let (n, mut k) = if first == "a" && word(toks, i + 2) == "couple" {
        (2, i + 3)
    } else {
        (count_word(first)?, i + 2)
    };
    if word(toks, k) == "of" {
        k += 1;
    }
    let unit = word(toks, k);
    let len = k - i + 1;
    let date = match unit {
        "day" | "days" => cx.plus(cx.today, n)?,
        "week" | "weeks" => cx.plus(cx.today, 7 * n)?,
        "month" | "months" => cx.today.checked_add(Span::new().months(n)).ok()?,
        "hour" | "hours" | "hr" | "hrs" => {
            let at = cx.now.timestamp() + SignedDuration::from_hours(n);
            return Some(found(i, len, When::At(at)));
        }
        "minute" | "minutes" | "min" | "mins" => {
            let at = cx.now.timestamp() + SignedDuration::from_mins(n);
            return Some(found(i, len, When::At(at)));
        }
        _ => return None,
    };
    Some(found(i, len, When::Day(date)))
}

/// "fri to sun", "12 to 16 june", "june 12 to 16", "14 march to 2 april".
fn range_at(cx: &Cx, toks: &[String], i: usize) -> Option<Found> {
    let days = |len: usize, start: Date, last: Date| {
        (last >= start).then(|| {
            cx.plus(last, 1)
                .map(|end| found(i, len, When::Days { start, end }))
        })?
    };
    if let Some(first) = weekday_word(word(toks, i)).map(|w| w.0)
        && TO_WORDS.contains(&word(toks, i + 1))
        && let Some(last) = weekday_word(word(toks, i + 2)).map(|w| w.0)
    {
        let start = cx.coming(first, true)?;
        let ahead = (i64::from(last.to_monday_zero_offset())
            - i64::from(first.to_monday_zero_offset()))
        .rem_euclid(7);
        return days(3, start, cx.plus(start, ahead)?);
    }
    if let Some((d1, _)) = day_number(word(toks, i))
        && TO_WORDS.contains(&word(toks, i + 1))
        && let Some((d2, _)) = day_number(word(toks, i + 2))
    {
        let of = usize::from(word(toks, i + 3) == "of");
        if let Some(month) = month_word(word(toks, i + 3 + of)) {
            let start = cx.upcoming(month, d1, None)?;
            let last = Date::new(start.year(), month, d2).ok()?;
            return days(4 + of, start, last);
        }
    }
    if let Some(month) = month_word(word(toks, i))
        && let Some((d1, _)) = day_number(word(toks, i + 1))
        && TO_WORDS.contains(&word(toks, i + 2))
        && let Some((d2, _)) = day_number(word(toks, i + 3))
        && month_word(word(toks, i + 4)).is_none()
    {
        let start = cx.upcoming(month, d1, None)?;
        let last = Date::new(start.year(), month, d2).ok()?;
        return days(4, start, last);
    }
    if let Some((len1, start)) = date_words(cx, toks, i)
        && TO_WORDS.contains(&word(toks, i + len1))
        && let Some((len2, last)) = date_words(cx, toks, i + len1 + 1)
    {
        let last = if last < start {
            Date::new(last.year() + 1, last.month(), last.day()).ok()?
        } else {
            last
        };
        return days(len1 + 1 + len2, start, last);
    }
    None
}

/// A time of day at `i`: "7pm", "7:30", "7.30 pm", "noon", "half seven",
/// "at 7". Strong when it cannot be anything but a time.
fn clock_at(toks: &[String], i: usize) -> Option<(usize, Clock, bool)> {
    let t = word(toks, i);
    let before = i.checked_sub(1).map_or("", |b| word(toks, b));
    let linked = TIME_LINKS.contains(&before);
    match t {
        "noon" | "midday" => {
            return Some((
                1,
                Clock {
                    hour: 12,
                    minute: 0,
                    explicit: true,
                },
                true,
            ));
        }
        "midnight" => {
            return Some((
                1,
                Clock {
                    hour: 24,
                    minute: 0,
                    explicit: true,
                },
                true,
            ));
        }
        "half" => {
            let skip = if word(toks, i + 1) == "past" { 2 } else { 1 };
            let hour = hour_word(word(toks, i + skip))?;
            return Some((
                skip + 1,
                Clock {
                    hour,
                    minute: 30,
                    explicit: false,
                },
                true,
            ));
        }
        _ => {}
    }
    let t = t.strip_suffix("ish").unwrap_or(t);
    let (body, mut suffix) = match t {
        _ if t.ends_with("am") || t.ends_with("pm") => (&t[..t.len() - 2], Some(&t[t.len() - 2..])),
        _ if t.ends_with("a.m") || t.ends_with("p.m") => {
            (&t[..t.len() - 3], Some(&t[t.len() - 3..t.len() - 2]))
        }
        _ => (t, None),
    };
    let mut len = 1;
    if suffix.is_none() {
        match word(toks, i + 1) {
            "am" | "a.m" => suffix = Some("a"),
            "pm" | "p.m" => suffix = Some("p"),
            _ => {}
        }
        if suffix.is_some() {
            len = 2;
        }
    }
    let (hour_text, minute_text) = match body.split_once([':', '.']) {
        Some((h, m)) => (h, Some(m)),
        None => (body, None),
    };
    if hour_text.is_empty() || hour_text.len() > 2 || !hour_text.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let hour: i8 = hour_text.parse().ok()?;
    let minute: i8 = match minute_text {
        Some(m) if m.len() == 2 && m.chars().all(|c| c.is_ascii_digit()) => m.parse().ok()?,
        Some(_) => return None,
        None => 0,
    };
    if minute > 59 {
        return None;
    }
    let pm = suffix.map(|s| s.starts_with('p'));
    let hour = match pm {
        Some(_) if !(1..=12).contains(&hour) => return None,
        Some(true) if hour == 12 => 12,
        Some(true) => hour + 12,
        Some(false) if hour == 12 => 0,
        Some(false) => hour,
        None if hour > 23 => return None,
        None => hour,
    };
    if word(toks, i + len) == "o'clock" {
        len += 1;
    }
    let colon = minute_text.is_some() && body.contains(':');
    let explicit = pm.is_some() || hour >= 13 || hour == 0 || hour_text.starts_with('0');
    let strong = pm.is_some() || colon || linked || word(toks, i + len - 1) == "o'clock";
    // A bare number is a time only after "at"; "7.30" only next to a day.
    if !strong && minute_text.is_none() {
        return None;
    }
    Some((
        len,
        Clock {
            hour,
            minute,
            explicit,
        },
        strong,
    ))
}

fn hour_word(t: &str) -> Option<i8> {
    Some(match t {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        other => {
            let n: i8 = other.parse().ok()?;
            return (1..=12).contains(&n).then_some(n);
        }
    })
}

/// A time of day just after the day, as in "friday night at 8", or just
/// before it, as in "7pm on friday".
fn time_near(toks: &[String], day: &Found) -> Option<Near> {
    let mut k = day.end;
    let mut part = day.part;
    for _ in 0..3 {
        let w = word(toks, k);
        if TIME_LINKS.contains(&w) {
            k += 1;
        } else if part_of(w) != Part::None || w == "afternoon" {
            if part == Part::None {
                part = part_of(w);
            }
            k += 1;
        } else {
            break;
        }
    }
    if let Some((len, clock, _)) = clock_at(toks, k) {
        return Some(Near {
            start: day.start,
            end: k + len,
            clock,
            part,
        });
    }
    for back in 1..=4 {
        let Some(j) = day.start.checked_sub(back) else {
            break;
        };
        let Some((len, clock, strong)) = clock_at(toks, j) else {
            continue;
        };
        let end = j + len;
        let joined = end == day.start || (end + 1 == day.start && word(toks, end) == "on");
        if strong && joined {
            let start = if j > 0 && TIME_LINKS.contains(&word(toks, j - 1)) {
                j - 1
            } else {
                j
            };
            return Some(Near {
                start,
                end: day.end,
                clock,
                part,
            });
        }
    }
    None
}
