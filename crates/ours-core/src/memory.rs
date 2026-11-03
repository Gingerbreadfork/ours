//! Group memory (DESIGN.md section 9): facts with scope, rooms, provenance,
//! confidence and expiry, folded into the group's state like everything
//! else. A forget, a correction or a deleted message removes a fact's words
//! and keeps an empty row; a superseded fact keeps its words as history.

use crate::sentence::SELF_WORDS;
use crate::state::GroupState;
use crate::trace::shorten;
use crate::zone;
use ours_types::{
    FactDraft, Instant, PlatformMessageId, PlatformRoomId, PlatformUserId, SubjectKind,
    message_link,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Confidence of a fact said outright.
pub const STATED: f32 = 0.9;
/// Confidence of a fact only implied.
pub const INFERRED: f32 = 0.6;
pub const DAY_SECONDS: i64 = 24 * 60 * 60;
/// A perishable fact with no date lasts this long after it was said.
pub const PERISHABLE_SECONDS: i64 = 30 * DAY_SECONDS;
/// Drafts kept from one message.
pub const DRAFTS_KEPT: usize = 10;
/// The longest quote kept for a self-stated fact.
pub const QUOTE_CHARS: usize = 200;

pub const SUPERSEDED: &str = "superseded";
pub const CORRECTED: &str = "corrected";
pub const FORGET_THAT: &str = "forget that";
pub const FORGET_ME: &str = "forget me";
pub const DELETED: &str = "message deleted";
/// Past its expiry and swept by the daily retention job.
pub const EXPIRED: &str = "expired";
/// Taken back by "undo": retracted, with its words kept as the record.
pub const UNDONE: &str = "undo";

/// A fact's id, minted from the group's sequence so replay gives the same one.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct FactId(pub String);

impl FactId {
    pub fn new(s: impl Into<String>) -> FactId {
        FactId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for FactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FactId({})", self.0)
    }
}

impl fmt::Display for FactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who may hear a fact (DESIGN.md section 9, "Shape").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FactScope {
    /// Said in the group, or derived from what was.
    Group,
    /// Said to the agent in a DM by this person; only ever used with them.
    Private(PlatformUserId),
    /// This person said it about themselves in a room; repeated only in
    /// their own words.
    SelfStated(PlatformUserId),
}

/// One message a fact came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    pub speaker: PlatformUserId,
    pub at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Retraction {
    pub at: Instant,
    /// "superseded", "corrected", "forget that", "forget me", "message
    /// deleted", "expired" or "undo".
    pub reason: String,
    pub by: Option<PlatformUserId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fact {
    pub id: FactId,
    pub subject: String,
    pub subject_kind: SubjectKind,
    /// The member the subject names, when it names one.
    pub subject_user: Option<PlatformUserId>,
    pub predicate: String,
    pub object: String,
    pub scope: FactScope,
    /// Every room it was seen in.
    pub rooms: BTreeSet<PlatformRoomId>,
    /// The messages it came from; the first is the primary source.
    pub sources: Vec<Source>,
    pub confidence: f32,
    /// When it was first said.
    pub first_seen: Instant,
    /// When it was last said again or used without challenge.
    pub last_confirmed: Instant,
    /// When it stops being true; never for durable facts.
    pub expires: Option<Instant>,
    /// When a span such as "away 12 to 16" starts; it holds from then until it expires.
    #[serde(default)]
    pub starts: Option<Instant>,
    /// When the agent stored it.
    pub learned_at: Instant,
    pub retracted: Option<Retraction>,
    /// The words that carry it.
    pub quote: Option<String>,
}

impl Fact {
    pub fn is_expired(&self, now: Instant) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }

    /// Neither retracted nor expired.
    pub fn is_live(&self, now: Instant) -> bool {
        self.retracted.is_none() && !self.is_expired(now)
    }

    pub fn is_durable(&self) -> bool {
        self.expires.is_none()
    }

    /// "Priya is vegetarian".
    pub fn line(&self) -> String {
        [&self.subject, &self.predicate, &self.object]
            .into_iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// How the fact may be repeated: a self-stated one only as quoted.
    pub fn said(&self) -> String {
        match (&self.scope, &self.quote) {
            (FactScope::SelfStated(_), Some(quote)) => format!("{}: \"{quote}\"", self.subject),
            _ => self.line(),
        }
    }

    /// Whether `user` is its subject or one of its sources.
    pub fn concerns(&self, user: &PlatformUserId) -> bool {
        self.subject_user.as_ref() == Some(user) || self.sources.iter().any(|s| &s.speaker == user)
    }

    /// Whether it was removed: forgotten, corrected or deleted with its
    /// message. A removed fact keeps its row but none of its words.
    pub fn is_forgotten(&self) -> bool {
        self.retracted.as_ref().is_some_and(|r| removes(&r.reason))
    }

    fn same_subject(&self, user: Option<&PlatformUserId>, subject: &str) -> bool {
        match (self.subject_user.as_ref(), user) {
            (Some(a), Some(b)) => a == b,
            _ => self.subject.eq_ignore_ascii_case(subject),
        }
    }
}

/// Where something is being said: a room, or a DM with one person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Room(PlatformRoomId),
    Dm(PlatformUserId),
}

impl Place {
    /// The place `room` is: a DM with its person, or the room itself.
    pub fn of(state: &GroupState, room: &PlatformRoomId) -> Place {
        let partner = state
            .rooms
            .get(room)
            .filter(|r| r.is_dm())
            .and_then(|r| {
                r.members
                    .values()
                    .find(|m| !m.is_bot && !state.is_me(&m.id))
            })
            .map(|m| m.id.clone());
        match partner {
            Some(user) => Place::Dm(user),
            None => Place::Room(room.clone()),
        }
    }
}

/// Who is looking, with the rooms they belong to worked out once.
pub(crate) struct Viewer {
    pub place: Place,
    rooms: BTreeSet<PlatformRoomId>,
}

impl Viewer {
    pub fn new(state: &GroupState, place: Place) -> Viewer {
        let rooms = match &place {
            Place::Dm(user) => rooms_of(state, user),
            Place::Room(_) => BTreeSet::new(),
        };
        Viewer { place, rooms }
    }

    /// Whether the fact may be said here, whatever its expiry (DESIGN.md
    /// section 9).
    pub fn may_see(&self, fact: &Fact) -> bool {
        match (&fact.scope, &self.place) {
            (FactScope::Private(owner), Place::Dm(user)) => owner == user,
            (FactScope::Private(_), Place::Room(_)) => false,
            (_, Place::Room(room)) => fact.rooms.contains(room),
            (FactScope::SelfStated(owner), Place::Dm(user)) if owner == user => true,
            (_, Place::Dm(_)) => fact.rooms.iter().any(|r| self.rooms.contains(r)),
        }
    }

    /// Whether this source may be shown here.
    pub fn may_see_source(&self, source: &Source) -> bool {
        match &self.place {
            Place::Room(room) => &source.room == room,
            Place::Dm(user) => &source.speaker == user || self.rooms.contains(&source.room),
        }
    }

    /// The first source that may be shown here.
    pub fn source<'a>(&self, fact: &'a Fact) -> Option<&'a Source> {
        fact.sources.iter().find(|s| self.may_see_source(s))
    }
}

/// The rooms other than DMs that `user` is a member of.
pub fn rooms_of(state: &GroupState, user: &PlatformUserId) -> BTreeSet<PlatformRoomId> {
    state
        .rooms
        .iter()
        .filter(|(_, r)| !r.is_dm() && r.members.contains_key(user))
        .map(|(id, _)| id.clone())
        .collect()
}

/// What storing one draft did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stored {
    New(FactId),
    Reconfirmed(FactId),
}

/// Stores a draft heard in `source`. A draft with the same subject,
/// predicate and scope as a live fact reconfirms it when the object is the
/// same and supersedes it otherwise. `text` is what was said, kept as the
/// quote of a self-stated fact the extractor gave none for.
pub(crate) fn store(
    state: &mut GroupState,
    source: Source,
    text: &str,
    draft: FactDraft,
    now: Instant,
) -> Option<Stored> {
    let subject = draft.subject.trim();
    let predicate = draft.predicate.trim().to_owned();
    if subject.is_empty() || predicate.is_empty() {
        return None;
    }
    let object = draft.object.trim().to_owned();
    let in_dm = state.rooms.get(&source.room).is_some_and(|r| r.is_dm());
    let about_self = draft.about_self || SELF_WORDS.contains(&subject.to_lowercase().as_str());
    let speaker = source.speaker.clone();
    let scope = if in_dm {
        FactScope::Private(speaker.clone())
    } else if about_self {
        FactScope::SelfStated(speaker.clone())
    } else {
        FactScope::Group
    };
    let (subject, subject_user) = if about_self {
        (state.display_name(&source.room, &speaker), Some(speaker))
    } else {
        let user = resolve_subject(state, &source.room, subject);
        (subject.to_owned(), user)
    };
    let confidence = if draft.stated { STATED } else { INFERRED };
    let expires = if draft.durable {
        None
    } else {
        Some(
            draft
                .expires
                .as_deref()
                .and_then(|d| end_of_day(d, &state.timezone))
                .unwrap_or_else(|| source.at.plus_seconds(PERISHABLE_SECONDS)),
        )
    };
    let starts = draft
        .starts
        .as_deref()
        .and_then(|d| start_of_day(d, &state.timezone));
    let quote = draft
        .quote
        .map(|q| q.trim().to_owned())
        .filter(|q| !q.is_empty())
        .or_else(|| {
            matches!(scope, FactScope::SelfStated(_))
                .then(|| shorten(text, QUOTE_CHARS))
                .filter(|q| !q.is_empty())
        });

    let matching: Vec<FactId> = state
        .facts
        .values()
        .filter(|f| {
            f.is_live(now)
                && f.scope == scope
                && f.predicate.eq_ignore_ascii_case(&predicate)
                && f.same_subject(subject_user.as_ref(), &subject)
                && (scope != FactScope::Group || f.rooms.contains(&source.room))
        })
        .map(|f| f.id.clone())
        .collect();
    let mut same = None;
    for id in matching {
        let Some(fact) = state.facts.get_mut(&id) else {
            continue;
        };
        if same.is_none() && fact.object.eq_ignore_ascii_case(&object) {
            same = Some(id);
        } else {
            fact.retracted = Some(Retraction {
                at: now,
                reason: SUPERSEDED.to_owned(),
                by: Some(source.speaker.clone()),
            });
        }
    }
    if let Some(id) = same {
        let fact = state.facts.get_mut(&id)?;
        fact.last_confirmed = fact.last_confirmed.max(source.at);
        fact.confidence = fact.confidence.max(confidence);
        fact.rooms.insert(source.room.clone());
        fact.expires = match (fact.expires, expires) {
            (None, _) | (_, None) => None,
            (Some(a), Some(b)) => Some(a.max(b)),
        };
        if starts.is_some() {
            fact.starts = starts;
        }
        if fact.quote.is_none() {
            fact.quote = quote;
        }
        let known = fact
            .sources
            .iter()
            .any(|s| s.room == source.room && s.message == source.message);
        if !known {
            fact.sources.push(source);
        }
        return Some(Stored::Reconfirmed(id));
    }
    let id = state.mint_fact();
    let fact = Fact {
        id: id.clone(),
        subject,
        subject_kind: draft.subject_kind,
        subject_user,
        predicate,
        object,
        scope,
        rooms: BTreeSet::from([source.room.clone()]),
        first_seen: source.at,
        last_confirmed: source.at,
        sources: vec![source],
        confidence,
        expires,
        starts,
        learned_at: now,
        retracted: None,
        quote,
    };
    state.facts.insert(id.clone(), fact);
    Some(Stored::New(id))
}

/// Whether a retraction for `reason` removes the fact's content. Only a
/// superseded fact keeps its words, so "what did we think before" still works.
fn removes(reason: &str) -> bool {
    matches!(
        reason,
        FORGET_THAT | FORGET_ME | CORRECTED | DELETED | EXPIRED
    )
}

/// Retracts each fact not already retracted and returns how many were. A
/// removal keeps the row, its rooms and its dates for replay and "why", and
/// clears every word and every link to who said it (DESIGN.md section 9).
pub(crate) fn retract(
    state: &mut GroupState,
    ids: &[FactId],
    reason: &str,
    by: Option<&PlatformUserId>,
    now: Instant,
) -> usize {
    // Records that name these facts in their summary lose the words too.
    let named: Vec<(usize, Vec<FactId>, String)> = if removes(reason) {
        state
            .actions
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match &r.undo {
                crate::action::Undo::RetractFacts { ids: of }
                    if of.iter().any(|f| ids.contains(f)) =>
                {
                    Some((i, of.clone(), crate::extract::facts_summary(state, of)))
                }
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut n = 0;
    for id in ids {
        if let Some(fact) = state.facts.get_mut(id)
            && fact.retracted.is_none()
        {
            fact.retracted = Some(Retraction {
                at: now,
                reason: reason.to_owned(),
                by: by.cloned(),
            });
            if removes(reason) {
                fact.subject.clear();
                fact.predicate.clear();
                fact.object.clear();
                fact.quote = None;
                fact.subject_user = None;
                fact.sources.clear();
            }
            n += 1;
        }
    }
    for (i, of, before) in named {
        let after = crate::extract::facts_summary(state, &of);
        if let Some(record) = state.actions.get_mut(i)
            && record.summary == before
        {
            record.summary = after;
        }
    }
    n
}

/// The messages these facts came from, by room, in order and without repeats.
pub(crate) fn source_messages(
    state: &GroupState,
    ids: &[FactId],
) -> Vec<(PlatformRoomId, PlatformMessageId)> {
    let mut out: Vec<(PlatformRoomId, PlatformMessageId)> = Vec::new();
    for source in ids
        .iter()
        .filter_map(|id| state.facts.get(id))
        .flat_map(|f| &f.sources)
    {
        let key = (source.room.clone(), source.message.clone());
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

/// Marks facts as used without challenge.
pub(crate) fn confirm(state: &mut GroupState, ids: &[FactId], now: Instant) {
    for id in ids {
        if let Some(fact) = state.facts.get_mut(id) {
            fact.last_confirmed = fact.last_confirmed.max(now);
        }
    }
}

/// A deleted message takes the facts that came only from it, and any read
/// of it still in flight.
pub(crate) fn on_deleted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
) -> usize {
    state
        .extractions
        .retain(|p| p.reply_to.is_some() || &p.room != room || &p.message != message);
    let ids: Vec<FactId> = state
        .facts
        .values()
        .filter(|f| {
            !f.sources.is_empty()
                && f.sources
                    .iter()
                    .all(|s| &s.room == room && &s.message == message)
        })
        .map(|f| f.id.clone())
        .collect();
    retract(state, &ids, DELETED, None, now)
}

/// Facts not retracted that came from `message` in `room`.
pub(crate) fn from_message(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) -> Vec<FactId> {
    state
        .facts
        .values()
        .filter(|f| f.retracted.is_none())
        .filter(|f| {
            f.sources
                .iter()
                .any(|s| &s.room == room && &s.message == message)
        })
        .map(|f| f.id.clone())
        .collect()
}

/// The facts last stored from `user`'s words: every one learned at the
/// latest instant any of theirs was.
pub(crate) fn latest_from(state: &GroupState, user: &PlatformUserId) -> Vec<FactId> {
    let theirs: Vec<&Fact> = state
        .facts
        .values()
        .filter(|f| f.retracted.is_none() && f.sources.iter().any(|s| &s.speaker == user))
        .collect();
    let Some(latest) = theirs.iter().map(|f| f.learned_at).max() else {
        return Vec::new();
    };
    theirs
        .into_iter()
        .filter(|f| f.learned_at == latest)
        .map(|f| f.id.clone())
        .collect()
}

/// The member a written subject names in `room`: an exact display name,
/// then a unique one it begins, ignoring case. In a DM, any member of the
/// group.
pub fn resolve_subject(
    state: &GroupState,
    room: &PlatformRoomId,
    subject: &str,
) -> Option<PlatformUserId> {
    let lower = subject.trim().trim_start_matches('@').to_lowercase();
    let name = lower
        .strip_suffix("'s")
        .or_else(|| lower.strip_suffix("\u{2019}s"))
        .unwrap_or(&lower)
        .trim();
    if name.is_empty() {
        return None;
    }
    let in_dm = state.rooms.get(room).is_some_and(|r| r.is_dm());
    let mut people: Vec<(&PlatformUserId, String)> = Vec::new();
    for (id, r) in &state.rooms {
        if !in_dm && id != room {
            continue;
        }
        for m in r.members.values() {
            if !m.is_bot && !state.is_me(&m.id) && !people.iter().any(|(u, _)| *u == &m.id) {
                people.push((&m.id, m.display_name.to_lowercase()));
            }
        }
    }
    let exact: Vec<&PlatformUserId> = people
        .iter()
        .filter(|(_, n)| n == name)
        .map(|(u, _)| *u)
        .collect();
    let found = if exact.is_empty() && name.chars().count() >= 3 {
        people
            .iter()
            .filter(|(_, n)| n.starts_with(name))
            .map(|(u, _)| *u)
            .collect()
    } else {
        exact
    };
    match found.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// The instant a day given as YYYY-MM-DD ends in the named zone.
pub fn end_of_day(date: &str, zone_name: &str) -> Option<Instant> {
    let day: jiff::civil::Date = date.trim().parse().ok()?;
    let next = day
        .tomorrow()
        .ok()?
        .to_zoned(zone::or_utc(zone_name))
        .ok()?;
    Some(Instant(next.timestamp()))
}

/// The instant a day given as YYYY-MM-DD starts in the named zone.
pub fn start_of_day(date: &str, zone_name: &str) -> Option<Instant> {
    let day: jiff::civil::Date = date.trim().parse().ok()?;
    let start = day.to_zoned(zone::or_utc(zone_name)).ok()?;
    Some(Instant(start.timestamp()))
}

/// `at` as 2026-11-02 in the named zone.
pub fn day(at: Instant, zone_name: &str) -> String {
    zone::local(at, zone_name).strftime("%Y-%m-%d").to_string()
}

/// The last day a fact holds: the day before the instant it expires.
pub fn last_day(expires: Instant, zone_name: &str) -> String {
    day(expires.plus_seconds(-1), zone_name)
}

/// A link to a source message where the platform has one. Mail has none.
pub fn link(state: &GroupState, source: &Source) -> Option<String> {
    if source
        .message
        .as_str()
        .starts_with(crate::mail::MAIL_PREFIX)
    {
        return None;
    }
    let info = state.rooms.get(&source.room)?.info.as_ref()?;
    message_link(
        info.platform,
        info.container.as_deref(),
        &source.room,
        &source.message,
    )
}

/// "Priya in #general, 2026-11-02".
pub fn source_label(state: &GroupState, source: &Source) -> String {
    format!(
        "{} in {}, {}",
        state.display_name(&source.room, &source.speaker),
        state.room_label(&source.room),
        day(source.at, &state.timezone)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_ends_at_midnight_in_the_groups_zone() {
        let end = end_of_day("2026-11-14", "Europe/Lisbon").unwrap();
        assert_eq!(end.to_string(), "2026-11-15T00:00:00Z");
        let end = end_of_day("2026-07-01", "Europe/Lisbon").unwrap();
        assert_eq!(end.to_string(), "2026-07-01T23:00:00Z");
        assert_eq!(last_day(end, "Europe/Lisbon"), "2026-07-01");
        assert!(end_of_day("next friday", "UTC").is_none());
    }

    #[test]
    fn a_line_skips_empty_parts() {
        let fact = Fact {
            id: FactId::new("f1"),
            subject: "Priya".into(),
            subject_kind: SubjectKind::Person,
            subject_user: None,
            predicate: "is vegetarian".into(),
            object: String::new(),
            scope: FactScope::SelfStated("priya".into()),
            rooms: BTreeSet::new(),
            sources: Vec::new(),
            confidence: STATED,
            first_seen: Instant::UNIX_EPOCH,
            last_confirmed: Instant::UNIX_EPOCH,
            expires: None,
            starts: None,
            learned_at: Instant::UNIX_EPOCH,
            retracted: None,
            quote: Some("I'm veggie now".into()),
        };
        assert_eq!(fact.line(), "Priya is vegetarian");
        assert_eq!(fact.said(), "Priya: \"I'm veggie now\"");
    }
}
