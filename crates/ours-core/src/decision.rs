//! The decision record and the group's calendar, DESIGN.md sections 10 and
//! 12. A record is written once, when a proposal closes, and never changed;
//! the calendar holds the dated outcomes and who is coming.

use crate::memory::{Place, link, rooms_of};
use crate::proposal::{CloseRule, DecisionId, ProposalId, ProposalKind};
use crate::state::GroupState;
use crate::time::{self, bounds};
use crate::trace::shorten;
use crate::zone;
use ours_types::{Instant, PlatformMessageId, PlatformRoomId, PlatformUserId, TimeMeaning};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub id: DecisionId,
    pub proposal: ProposalId,
    pub room: PlatformRoomId,
    pub question: String,
    pub topic: Option<String>,
    pub kind: ProposalKind,
    pub options: Vec<DecidedOption>,
    /// The option chosen, by index.
    pub winner: usize,
    pub rule: CloseRule,
    pub outcome: Outcome,
    /// What the deciding lead gave as the reason, in their words.
    pub reason: Option<String>,
    /// Who said what, in order, with the message each said it in.
    pub said: Vec<Said>,
    /// The message that closed it, when a person's words did.
    pub closed_by: Option<PlatformMessageId>,
    /// The message that started it.
    pub thread_root: PlatformMessageId,
    pub opened_at: Instant,
    pub closed_at: Instant,
    pub open_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DecidedOption {
    pub label: String,
    pub when: Option<TimeMeaning>,
    pub echo: Option<String>,
    pub votes: BTreeSet<PlatformUserId>,
}

impl DecidedOption {
    pub fn display(&self) -> &str {
        self.echo.as_deref().unwrap_or(&self.label)
    }
}

/// The count or the decider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    /// Votes per option, in option order.
    Count {
        counts: Vec<u32>,
    },
    Decider {
        user: PlatformUserId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Said {
    pub user: PlatformUserId,
    /// Their name when it was said.
    pub name: String,
    pub message: PlatformMessageId,
    pub text: String,
    /// The option it named, if one.
    pub option: Option<usize>,
}

/// One dated outcome on the group's calendar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    pub when: TimeMeaning,
    /// "Dinner", or the option when there is no topic.
    pub label: String,
    pub decision: DecisionId,
    /// Who is coming, as far as anyone has said.
    pub attendees: BTreeSet<PlatformUserId>,
    #[serde(default)]
    pub room: Option<PlatformRoomId>,
    /// How to say the date, as it was resolved.
    #[serde(default)]
    pub echo: Option<String>,
    /// Who said they can't make it after it was decided.
    #[serde(default)]
    pub out: BTreeSet<PlatformUserId>,
}

impl Decision {
    pub fn chosen(&self) -> Option<&DecidedOption> {
        self.options.get(self.winner)
    }

    /// The topic, or the options when there is none.
    pub fn title(&self) -> String {
        self.topic.clone().unwrap_or_else(|| {
            crate::proposal::or_list(self.options.iter().map(|o| o.label.clone()).collect())
        })
    }

    /// "4 to 1", "3 to 2 to 1": the winner's count first, then the rest
    /// from most to fewest, trailing zeros dropped but one kept.
    pub fn count_line(&self) -> Option<String> {
        let Outcome::Count { counts } = &self.outcome else {
            return None;
        };
        Some(count_line(counts, self.winner))
    }
}

pub fn count_line(counts: &[u32], winner: usize) -> String {
    let first = counts.get(winner).copied().unwrap_or(0);
    let mut rest: Vec<u32> = counts
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != winner)
        .map(|(_, c)| *c)
        .collect();
    rest.sort_unstable_by(|a, b| b.cmp(a));
    while rest.len() > 1 && rest.last() == Some(&0) {
        rest.pop();
    }
    std::iter::once(first)
        .chain(rest)
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" to ")
}

/// Calendar entries that overlap the next `days` days from `now`, soonest
/// first. A day without a time stays upcoming until it ends.
pub fn upcoming(state: &GroupState, now: Instant, days: i64) -> Vec<&Entry> {
    let until = now.plus_seconds(days * 24 * 60 * 60);
    let mut entries: Vec<(Instant, &Entry)> = state
        .calendar
        .iter()
        .filter_map(|e| bounds(&e.when, &state.timezone).map(|(start, end)| (start, end, e)))
        .filter(|(start, end, _)| *end >= now && *start < until)
        .map(|(start, _, e)| (start, e))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.decision.cmp(&b.1.decision)));
    entries.into_iter().map(|(_, e)| e).collect()
}

/// Whether something from `room` may be told at `place`: in the room
/// itself, or in a DM with someone who belongs to it.
pub(crate) fn visible(state: &GroupState, room: &PlatformRoomId, place: &Place) -> bool {
    match place {
        Place::Room(here) => here == room,
        Place::Dm(user) => rooms_of(state, user).contains(room),
    }
}

/// How an entry's date is said in the group's zone.
pub fn entry_echo(state: &GroupState, entry: &Entry, now: Instant) -> String {
    entry
        .echo
        .clone()
        .or_else(|| time::echo(&entry.when, &state.timezone, now))
        .unwrap_or_default()
}

/// "Upcoming: Thursday the 12th, Dinner." for the next seven days, as the
/// place may see them.
pub(crate) fn upcoming_line(state: &GroupState, place: &Place, now: Instant) -> Option<String> {
    let parts: Vec<String> = upcoming(state, now, 7)
        .into_iter()
        .filter(|e| e.room.as_ref().is_none_or(|r| visible(state, r, place)))
        .map(|e| {
            let coming = if e.attendees.is_empty() {
                String::new()
            } else {
                format!(", {} coming", e.attendees.len())
            };
            format!("{}, {}{coming}", entry_echo(state, e, now), e.label)
        })
        .collect();
    (!parts.is_empty()).then(|| format!("Upcoming: {}.", parts.join("; ")))
}

/// Records matching the words of `query` that the place may see, best
/// first: words found in the question, the options, the reason or what
/// was said, with the chosen option counting double.
pub fn recall_decisions<'a>(
    state: &'a GroupState,
    place: &Place,
    query: &str,
    limit: usize,
) -> Vec<&'a Decision> {
    let words = query_words(query);
    let mut hits: Vec<(usize, &Decision)> = state
        .decisions
        .values()
        .filter(|d| visible(state, &d.room, place))
        .filter_map(|d| {
            let own = record_words(d);
            let chosen = d
                .chosen()
                .map(|o| query_words(&o.label))
                .unwrap_or_default();
            let score: usize = words
                .iter()
                .map(|w| {
                    if chosen.contains(w) {
                        2
                    } else {
                        usize::from(own.contains(w))
                    }
                })
                .sum();
            (score > 0).then_some((score, d))
        })
        .collect();
    hits.sort_by(|(a, da), (b, db)| b.cmp(a).then(db.closed_at.cmp(&da.closed_at)));
    hits.into_iter().take(limit).map(|(_, d)| d).collect()
}

/// The most recent record the place may see.
pub fn latest_decision<'a>(state: &'a GroupState, place: &Place) -> Option<&'a Decision> {
    state
        .decisions
        .values()
        .filter(|d| visible(state, &d.room, place))
        .max_by(|a, b| a.closed_at.cmp(&b.closed_at).then(a.id.cmp(&b.id)))
}

const QUERY_STOP: [&str; 30] = [
    "the", "and", "for", "was", "were", "are", "you", "what", "when", "where", "who", "why", "how",
    "did", "does", "about", "with", "decide", "decided", "pick", "picked", "choose", "chose",
    "chosen", "going", "went", "settle", "settled", "on", "we",
];

fn query_words(text: &str) -> BTreeSet<String> {
    crate::proposal::words(text)
        .into_iter()
        .filter(|w| w.chars().count() >= 2 && !QUERY_STOP.contains(&w.as_str()))
        .collect()
}

fn record_words(d: &Decision) -> BTreeSet<String> {
    let mut all = query_words(&d.question);
    if let Some(topic) = &d.topic {
        all.extend(query_words(topic));
    }
    for o in &d.options {
        all.extend(query_words(&o.label));
        if let Some(echo) = &o.echo {
            all.extend(query_words(echo));
        }
    }
    if let Some(reason) = &d.reason {
        all.extend(query_words(reason));
    }
    for s in &d.said {
        all.extend(query_words(&s.text));
    }
    all
}

/// The answer to "why did we pick Thursday": the outcome, the count or the
/// decider and their reason, when, and a link where the platform has one.
pub(crate) fn answer(state: &GroupState, d: &Decision, now: Instant) -> String {
    let tz = &state.timezone;
    let chosen = d.chosen().map_or("", DecidedOption::display);
    let day = time::day_echo(
        zone::local(d.closed_at, tz).date(),
        zone::local(now, tz).date(),
    );
    let mut lines = vec![match &d.outcome {
        Outcome::Count { .. } => format!(
            "{}: {chosen}, {} on {}, decided {day} after {}.",
            d.title(),
            d.count_line().unwrap_or_default(),
            d.rule.name(),
            duration(d.open_seconds)
        ),
        Outcome::Decider { user } => {
            let name = state.name_of(user);
            match &d.reason {
                Some(reason) => format!(
                    "{}: {chosen}, decided by {name} on {day}: \"{}\".",
                    d.title(),
                    shorten(reason, 160)
                ),
                None => format!("{}: {chosen}, decided by {name} on {day}.", d.title()),
            }
        }
    }];
    let others: Vec<&str> = d
        .options
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != d.winner)
        .map(|(_, o)| o.label.as_str())
        .collect();
    let source = d.closed_by.as_ref().unwrap_or(&d.thread_root);
    match message_url(state, &d.room, source) {
        Some(url) => lines.push(format!("Source: {url}")),
        None if !others.is_empty() => {
            lines.push(format!(
                "The other option was {}.",
                crate::proposal::or_list(others.iter().map(|s| (*s).to_owned()).collect())
            ));
        }
        None => {}
    }
    lines.join("\n")
}

fn message_url(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) -> Option<String> {
    let source = crate::memory::Source {
        room: room.clone(),
        message: message.clone(),
        speaker: PlatformUserId::new(""),
        at: Instant::UNIX_EPOCH,
    };
    link(state, &source)
}

/// The prompt block that offers matching records to the model.
pub(crate) fn block(state: &GroupState, decisions: &[&Decision]) -> String {
    let mut lines = vec!["Decisions that may matter:".to_owned()];
    for d in decisions {
        let chosen = d.chosen().map_or("", DecidedOption::display);
        let how = match &d.outcome {
            Outcome::Count { .. } => d.count_line().unwrap_or_default(),
            Outcome::Decider { user } => format!("by {}", state.name_of(user)),
        };
        let reason = d.reason.as_ref().map_or_else(String::new, |r| {
            format!(", because \"{}\"", shorten(r, 120))
        });
        lines.push(format!(
            "- {}: {chosen}, {how} ({}){reason}, closed {} (msg {})",
            d.question,
            d.rule.name(),
            zone::stamp(d.closed_at, &state.timezone),
            d.closed_by.as_ref().unwrap_or(&d.thread_root)
        ));
    }
    lines.join("\n")
}

/// "42 minutes", "3 hours", "2 days".
pub fn duration(seconds: i64) -> String {
    let minutes = (seconds / 60).max(1);
    let (n, one, many) = if minutes < 120 {
        (minutes, "minute", "minutes")
    } else if minutes < 48 * 60 {
        (minutes / 60, "hour", "hours")
    } else {
        (minutes / (24 * 60), "day", "days")
    };
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The group's decisions as a Markdown log, open proposals first, then
/// every record from the newest back. The binary writes it to a file.
pub fn render_decisions(state: &GroupState) -> String {
    let tz = state.timezone.as_str();
    let mut lines = vec![
        format!("# Decision log for group {}", state.id),
        String::new(),
    ];
    let open: Vec<_> = state.proposals.values().filter(|p| p.is_open()).collect();
    if !open.is_empty() {
        lines.push("## Open".to_owned());
        lines.push(String::new());
        for p in open {
            lines.push(format!(
                "- {} in {}, opened {}, closes {}, {}",
                p.question,
                state.room_label(&p.room),
                zone::stamp(p.opened_at, tz),
                zone::stamp(p.deadline, tz),
                p.rule.name()
            ));
        }
        lines.push(String::new());
    }
    let mut records: Vec<&Decision> = state.decisions.values().collect();
    records.sort_by(|a, b| b.closed_at.cmp(&a.closed_at).then(b.id.cmp(&a.id)));
    for d in records {
        let chosen = d.chosen().map_or("", DecidedOption::display);
        lines.push(format!(
            "## {}: {}",
            crate::memory::day(d.closed_at, tz),
            d.question
        ));
        lines.push(String::new());
        let how = match &d.outcome {
            Outcome::Count { .. } => d.count_line().unwrap_or_default(),
            Outcome::Decider { user } => format!("by {}", person(state, user)),
        };
        lines.push(format!("- Decided: {chosen}, {how}"));
        lines.push(format!("- Rule: {}", d.rule.name()));
        if let Some(reason) = &d.reason {
            lines.push(format!("- Reason: \"{reason}\""));
        }
        lines.push(format!("- Room: {}", state.room_label(&d.room)));
        lines.push(format!(
            "- Opened {}, closed {}, after {}",
            zone::stamp(d.opened_at, tz),
            zone::stamp(d.closed_at, tz),
            duration(d.open_seconds)
        ));
        let votes: Vec<String> = d
            .options
            .iter()
            .filter(|o| !o.votes.is_empty())
            .map(|o| {
                let names: Vec<String> = o.votes.iter().map(|u| state.name_of(u)).collect();
                format!("{}: {}", o.label, names.join(", "))
            })
            .collect();
        if !votes.is_empty() {
            lines.push(format!("- Votes: {}", votes.join("; ")));
        }
        if !d.said.is_empty() {
            lines.push("- Said:".to_owned());
            for s in &d.said {
                let at = message_url(state, &d.room, &s.message)
                    .unwrap_or_else(|| format!("msg {}", s.message));
                lines.push(format!("  - {}: \"{}\" ({at})", s.name, s.text));
            }
        }
        lines.push(String::new());
    }
    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn person(state: &GroupState, user: &PlatformUserId) -> String {
    let name = state.name_of(user);
    if name == user.as_str() {
        format!("`{user}`")
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_winner_first() {
        assert_eq!(count_line(&[4, 1], 0), "4 to 1");
        assert_eq!(count_line(&[1, 4], 1), "4 to 1");
        assert_eq!(count_line(&[3, 0, 0], 0), "3 to 0");
        assert_eq!(count_line(&[1, 3, 2], 1), "3 to 2 to 1");
        assert_eq!(count_line(&[2, 2], 0), "2 to 2");
        assert_eq!(duration(42 * 60), "42 minutes");
        assert_eq!(duration(5 * 3600), "5 hours");
        assert_eq!(duration(3 * 86400), "3 days");
    }
}
