//! Recall (DESIGN.md section 9): which remembered facts matter to a message
//! and may be said where it was asked. Lexical and deterministic; a vector
//! index can stand behind the same function later.

use crate::memory::{Fact, FactId, FactScope, Place, Source, Viewer, day, link, source_label};
use crate::state::GroupState;
use ours_types::{Instant, PlatformUserId};
use std::collections::BTreeSet;

/// Facts offered to the model with one message.
pub const RECALLED: usize = 8;
/// Source links appended to a reply.
pub const SOURCE_LINES: usize = 2;
/// Extra weight for a fact about someone the query names.
const NAME_BOOST: usize = 2;

const STOP: [&str; 32] = [
    "the", "and", "for", "was", "were", "are", "you", "your", "what", "when", "where", "who",
    "why", "how", "that", "this", "with", "from", "have", "has", "had", "did", "does", "about",
    "any", "anyone", "know", "remember", "tell", "can", "again", "there",
];

const MEMORY_RULE: &str = "Use these only when they help answer. A quoted line is that person's own words about themselves: repeat it only as quoted, never reworded. Put the msg id of each fact your reply relies on in \"cites\", and leave cites empty when you use none.";

/// The facts that may be said at `place` and match `query`, best first:
/// query words found in the subject, predicate, object or quote, a boost
/// when the query names the fact's subject, then confidence, then recency.
/// Expired and retracted facts never come back.
pub fn recall<'a>(
    state: &'a GroupState,
    place: &Place,
    query: &str,
    now: Instant,
    limit: usize,
) -> Vec<&'a Fact> {
    let viewer = Viewer::new(state, place.clone());
    let words = tokens(query);
    let named = named_people(state, query);
    let mut hits: Vec<(usize, &Fact)> = state
        .facts
        .values()
        .filter(|f| f.is_live(now) && viewer.may_see(f))
        .filter_map(|f| {
            let own = fact_tokens(f);
            let mut score = words.iter().filter(|w| own.contains(*w)).count();
            if f.subject_user.as_ref().is_some_and(|u| named.contains(u)) {
                score += NAME_BOOST;
            }
            (score > 0).then_some((score, f))
        })
        .collect();
    hits.sort_by(|(a, fa), (b, fb)| {
        b.cmp(a)
            .then(fb.confidence.total_cmp(&fa.confidence))
            .then(fb.last_confirmed.cmp(&fa.last_confirmed))
            .then(fa.id.cmp(&fb.id))
    });
    hits.into_iter().take(limit).map(|(_, f)| f).collect()
}

/// Lower-cased words of three or more letters, without punctuation or the
/// stop list, with a trailing plural s dropped.
fn tokens(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3 && !STOP.contains(t))
        .map(stem)
        .collect()
}

fn stem(word: &str) -> String {
    match word.strip_suffix('s') {
        Some(rest) if word.len() >= 5 && !rest.ends_with('s') => rest.to_owned(),
        _ => word.to_owned(),
    }
}

fn fact_tokens(fact: &Fact) -> BTreeSet<String> {
    let mut all = tokens(&fact.subject);
    all.extend(tokens(&fact.predicate));
    all.extend(tokens(&fact.object));
    if let Some(quote) = &fact.quote {
        all.extend(tokens(quote));
    }
    all
}

/// Members whose display name, or its first word, appears in `query`.
fn named_people(state: &GroupState, query: &str) -> BTreeSet<PlatformUserId> {
    let lower = query.to_lowercase();
    let words: BTreeSet<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .collect();
    let mut people = BTreeSet::new();
    for room in state.rooms.values() {
        for m in room.members.values() {
            if m.is_bot || state.is_me(&m.id) {
                continue;
            }
            let name = m.display_name.to_lowercase();
            let first = name.split_whitespace().next().unwrap_or("");
            if words.contains(first) || words.contains(name.as_str()) {
                people.insert(m.id.clone());
            }
        }
    }
    people
}

/// The prompt block that offers recalled facts to the model, each with the
/// message it came from, self-stated ones as quoted.
pub(crate) fn block(state: &GroupState, place: &Place, facts: &[&Fact]) -> String {
    let viewer = Viewer::new(state, place.clone());
    let mut lines = vec!["What you remember that may matter:".to_owned()];
    for fact in facts {
        let cite = viewer.source(fact).map_or_else(String::new, |s| {
            format!(
                " ({}, {}, msg {})",
                state.display_name(&s.room, &s.speaker),
                day(s.at, &state.timezone),
                s.message
            )
        });
        lines.push(match (&fact.scope, &fact.quote) {
            (FactScope::SelfStated(_), Some(_)) => {
                format!("- {}{cite}; say this only in these words", fact.said())
            }
            _ => format!("- {}{cite}", fact.line()),
        });
    }
    lines.push(MEMORY_RULE.to_owned());
    lines.join("\n")
}

/// What a reply relied on: the offered facts whose visible source the plan
/// cited, and those sources in the order cited.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Citation {
    pub facts: Vec<FactId>,
    pub sources: Vec<(Source, bool)>,
}

/// Matches a plan's cites against the facts that were offered. Anything
/// cited that was not offered is ignored.
pub(crate) fn citation(
    state: &GroupState,
    place: &Place,
    offered: &[FactId],
    cites: &[String],
) -> Citation {
    let viewer = Viewer::new(state, place.clone());
    let mut out = Citation::default();
    for cite in cites {
        let cite = cite.trim();
        let cite = cite.strip_prefix("msg").unwrap_or(cite).trim();
        for fact in offered.iter().filter_map(|id| state.facts.get(id)) {
            let Some(source) = fact
                .sources
                .iter()
                .find(|s| s.message.as_str() == cite && viewer.may_see_source(s))
            else {
                continue;
            };
            if !out.facts.contains(&fact.id) {
                out.facts.push(fact.id.clone());
            }
            let quoted = matches!(fact.scope, FactScope::SelfStated(_)) && fact.quote.is_some();
            match out.sources.iter_mut().find(|(s, _)| s == source) {
                Some(slot) => slot.1 |= quoted,
                None => out.sources.push((source.clone(), quoted)),
            }
        }
    }
    out
}

/// The cites a reply implies when the model gave none: the source message
/// of each offered fact whose own words the reply repeats.
pub(crate) fn implied_cites(state: &GroupState, offered: &[FactId], text: &str) -> Vec<String> {
    let reply = text.to_lowercase();
    let mut out = Vec::new();
    for fact in offered.iter().filter_map(|id| state.facts.get(id)) {
        let words = fact.line().to_lowercase();
        let repeated = words
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 4 && !STOP.contains(w))
            .any(|w| reply.contains(w));
        if repeated && let Some(source) = fact.sources.first() {
            out.push(source.message.as_str().to_owned());
        }
    }
    out
}

/// "Source: <link>" for up to two cited messages. A quote is never repeated
/// without its source, so a quoted fact on a platform without links gets
/// who said it, where and when instead, and goes first.
pub(crate) fn source_lines(state: &GroupState, citation: &Citation) -> Vec<String> {
    let mut quoted = Vec::new();
    let mut plain = Vec::new();
    for (source, is_quote) in &citation.sources {
        match (link(state, source), is_quote) {
            (Some(url), true) => quoted.push(format!("Source: {url}")),
            (None, true) => quoted.push(format!("Source: {}", source_label(state, source))),
            (Some(url), false) => plain.push(format!("Source: {url}")),
            (None, false) => plain.push(format!("Source: {}", source_label(state, source))),
        }
    }
    quoted.into_iter().chain(plain).take(SOURCE_LINES).collect()
}

/// The fourth line of "why": the facts a message relied on. Facts since
/// forgotten are left out.
pub(crate) fn from_line(state: &GroupState, ids: &[FactId]) -> Option<String> {
    let parts: Vec<String> = ids
        .iter()
        .filter_map(|id| state.facts.get(id))
        .filter(|f| !f.is_forgotten())
        .map(|f| match &f.retracted {
            Some(r) => format!("{} (since {})", f.said(), r.reason),
            None => f.said(),
        })
        .collect();
    (!parts.is_empty()).then(|| format!("From: {}", parts.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_lower_cased_stemmed_and_stopped() {
        let words = tokens("What was that BAR, with the weird cocktails? Hobson's!");
        let expected: BTreeSet<String> = ["bar", "weird", "cocktail", "hobson"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(words, expected);
        assert_eq!(stem("glass"), "glass");
        assert_eq!(stem("bus"), "bus");
        assert_eq!(stem("drives"), "drive");
    }
}
