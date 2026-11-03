//! Availability (DESIGN.md section 12): a perishable fact such as "Alex is
//! away" or "I'm off 12 to 16" is a span in which nobody is chased, the
//! ballot marks the option, and the digest and day-of line say so.

use crate::memory::Fact;
use crate::state::GroupState;
use ours_types::{Instant, PlatformUserId};

/// Words that make a relation an absence on their own.
const AWAY_WORDS: [&str; 10] = [
    "away",
    "leave",
    "holiday",
    "holidays",
    "vacation",
    "travelling",
    "traveling",
    "unavailable",
    "ooo",
    "sick",
];

/// Words that make one only when nothing else is said: "off", "out of office".
const OFF_WORDS: [&str; 4] = ["out", "off", "work", "office"];

/// Whether a fact says its subject is away for a while.
pub fn is_availability(fact: &Fact) -> bool {
    if fact.expires.is_none() || fact.subject_user.is_none() {
        return false;
    }
    let words = crate::proposal::words(&format!("{} {}", fact.predicate, fact.object));
    let words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !matches!(*w, "is" | "are" | "be" | "will" | "on" | "of"))
        .collect();
    words.iter().any(|w| AWAY_WORDS.contains(w))
        || (!words.is_empty() && words.iter().all(|w| OFF_WORDS.contains(w)))
}

/// The start and end of each span in which `user` is away, from live facts.
fn spans<'a>(
    state: &'a GroupState,
    user: &'a PlatformUserId,
) -> impl Iterator<Item = (Instant, Instant)> + 'a {
    state
        .facts
        .values()
        .filter(move |f| {
            f.retracted.is_none() && f.subject_user.as_ref() == Some(user) && is_availability(f)
        })
        .filter_map(|f| Some((f.starts.unwrap_or(f.first_seen), f.expires?)))
}

/// Whether `user` is away at `at`.
pub fn away(state: &GroupState, user: &PlatformUserId, at: Instant) -> bool {
    spans(state, user).any(|(start, end)| start <= at && at < end)
}

/// Whether `user` is away at any point from `start` up to `end`.
pub fn away_during(
    state: &GroupState,
    user: &PlatformUserId,
    start: Instant,
    end: Instant,
) -> bool {
    if start >= end {
        return away(state, user, start);
    }
    spans(state, user).any(|(s, e)| s < end && start < e)
}

/// When `user` is back, following spans that run into each other, if they
/// are away at `at`.
pub fn back(state: &GroupState, user: &PlatformUserId, at: Instant) -> Option<Instant> {
    let mut until: Option<Instant> = None;
    let mut probe = at;
    for _ in 0..16 {
        let next = spans(state, user)
            .filter(|(s, e)| *s <= probe && probe < *e)
            .map(|(_, e)| e)
            .max();
        match next {
            Some(end) => {
                until = Some(end);
                probe = end;
            }
            None => break,
        }
    }
    until
}

/// The last moment of the span covering `at`, for "away until Friday".
pub fn until(state: &GroupState, user: &PlatformUserId, at: Instant) -> Option<Instant> {
    back(state, user, at).map(|end| end.plus_seconds(-1))
}
