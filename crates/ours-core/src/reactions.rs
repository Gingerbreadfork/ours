//! The reactions the engine relies on, by meaning. A platform that allows
//! only a fixed set of emoji (Telegram) gets a stand-in from its set for
//! each one it lacks, and inbound reactions are read back through the same
//! table, so the rest of the engine asks for a meaning, never an emoji.

use crate::effect::{self, Effect};
use crate::state::GroupState;
use ours_types::{ActionId, Platform, PlatformMessageId, PlatformRoomId, PlatformUserId};

/// What a reaction says to or from the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Meaning {
    /// Working on it.
    Eyes,
    /// A lead approves, a payment is made or confirmed, a promise is kept.
    Check,
    /// Stop speaking unprompted for the day.
    Shush,
    /// The agent has an answer to an open question.
    RaisedHand,
    /// A promise was noted.
    Pin,
    /// Agreement, and the only answer a shush gets.
    ThumbsUp,
    ThumbsDown,
    /// A vote on an attendance ballot.
    In,
    /// Takes the reactor out of a tab line's split.
    NotMe,
    /// Counts the reactor twice in a tab line's split.
    Double,
    /// Drops a promise or disputes a payment.
    Cross,
}

impl Meaning {
    pub const ALL: [Meaning; 11] = [
        Meaning::Eyes,
        Meaning::Check,
        Meaning::Shush,
        Meaning::RaisedHand,
        Meaning::Pin,
        Meaning::ThumbsUp,
        Meaning::ThumbsDown,
        Meaning::In,
        Meaning::NotMe,
        Meaning::Double,
        Meaning::Cross,
    ];

    /// The emoji the engine uses where the platform allows any.
    pub fn canonical(self) -> &'static str {
        match self {
            Meaning::Eyes => crate::step::EYES,
            Meaning::Check => crate::step::CHECK,
            Meaning::Shush => crate::attention::SHUSH,
            Meaning::RaisedHand => crate::attention::RAISED_HAND,
            Meaning::Pin => crate::commitment::PIN,
            Meaning::ThumbsUp => crate::attention::THUMBS_UP,
            Meaning::ThumbsDown => "👎",
            Meaning::In => crate::proposal::IN,
            Meaning::NotMe => crate::tab::NOT_ME,
            Meaning::Double => crate::tab::DOUBLE,
            Meaning::Cross => "❌",
        }
    }

    /// Other spellings that carry the meaning: platform names such as
    /// Slack's and close variants of the emoji.
    fn aliases(self) -> &'static [&'static str] {
        match self {
            Meaning::Eyes => &["eyes"],
            Meaning::Check => &[
                "✔",
                "☑",
                "white_check_mark",
                "heavy_check_mark",
                "ballot_box_with_check",
            ],
            Meaning::Shush => &["shushing_face", "shush"],
            Meaning::RaisedHand => &["raising_hand"],
            Meaning::Pin => &["pushpin"],
            Meaning::ThumbsUp | Meaning::In => &["thumbsup", "thumbs_up", "+1"],
            Meaning::ThumbsDown => &["thumbsdown", "thumbs_down", "-1"],
            Meaning::NotMe => &["no_good", "person_gesturing_no"],
            Meaning::Double => &["v", "victory_hand"],
            Meaning::Cross => &["✖", "x", "cross_mark", "heavy_multiplication_x"],
        }
    }

    /// Stand-ins in order of preference, for a platform that lacks the
    /// canonical emoji. Each reads right at every dial.
    fn stand_ins(self) -> &'static [&'static str] {
        match self {
            Meaning::Eyes => &["🤔", "👌"],
            Meaning::Check => &["👍", "👌", "🆒", "💯"],
            Meaning::Shush => &["😴", "🙊", "🥱"],
            Meaning::RaisedHand => &["🫡", "🤓", "⚡"],
            Meaning::Pin => &["✍", "👌", "💯"],
            Meaning::ThumbsUp => &["👌", "💯", "🔥"],
            Meaning::ThumbsDown => &["💔", "😐"],
            Meaning::In => &["👌", "🔥", "🎉"],
            Meaning::NotMe => &["🙈", "🙊", "🤷"],
            Meaning::Double => &["🤝", "🔥", "💯"],
            Meaning::Cross => &["👎", "💔"],
        }
    }

    /// What the reaction does, for the charter's line.
    fn gloss(self) -> &'static str {
        match self {
            Meaning::Eyes => "means I'm on it",
            Meaning::Check => "approves",
            Meaning::Shush => "stands me down",
            Meaning::RaisedHand => "means I have the answer",
            Meaning::Pin => "a promise noted",
            Meaning::ThumbsUp => "agrees",
            Meaning::ThumbsDown => "disagrees",
            Meaning::In => "means you're in",
            Meaning::NotMe => "takes you out of a split",
            Meaning::Double => "counts you twice",
            Meaning::Cross => "drops a promise",
        }
    }
}

/// The emoji for `meaning` in `room`: the canonical one where the room's
/// platform allows it, otherwise a stand-in from the platform's set.
pub fn emoji_for(state: &GroupState, room: &PlatformRoomId, meaning: Meaning) -> &'static str {
    pick(reaction_set(state, room), meaning)
}

/// What an inbound reaction in `room` means: the canonical emoji or an
/// alias first, then the stand-in the room's platform uses. Where one emoji
/// carries two meanings, as 👍 does on Telegram, `means` tests for the one
/// a caller expects.
pub fn meaning_of(state: &GroupState, room: &PlatformRoomId, emoji: &str) -> Option<Meaning> {
    let set = reaction_set(state, room);
    let emoji = bare(emoji);
    Meaning::ALL
        .into_iter()
        .find(|m| own_spelling(*m, &emoji))
        .or_else(|| {
            Meaning::ALL
                .into_iter()
                .find(|m| substituted(set, *m).is_some_and(|e| same(&emoji, e)))
        })
}

/// Whether `emoji`, reacted in `room`, carries `meaning`.
pub fn means(state: &GroupState, room: &PlatformRoomId, emoji: &str, meaning: Meaning) -> bool {
    let emoji = bare(emoji);
    own_spelling(meaning, &emoji)
        || substituted(reaction_set(state, room), meaning).is_some_and(|e| same(&emoji, e))
}

/// Whether `emoji` is the canonical emoji for `meaning` or one of its
/// aliases, whatever the platform.
pub fn is_canonical(emoji: &str, meaning: Meaning) -> bool {
    own_spelling(meaning, &bare(emoji))
}

/// Whether the room's platform accepts `emoji` as a reaction.
pub fn allowed(state: &GroupState, room: &PlatformRoomId, emoji: &str) -> bool {
    reaction_set(state, room).is_none_or(|set| allows(set, emoji))
}

/// Each meaning whose emoji differs in `room`, with the one used there.
pub fn substitutions(state: &GroupState, room: &PlatformRoomId) -> Vec<(Meaning, &'static str)> {
    substitutions_in(reaction_set(state, room))
}

/// "Reactions here: 👍 approves, 😴 stands me down.", for the charter, or
/// none when every reaction is the canonical one.
pub fn charter_line(subs: &[(Meaning, &'static str)], label: &str) -> Option<String> {
    if subs.is_empty() {
        return None;
    }
    let parts: Vec<String> = subs
        .iter()
        .map(|(m, e)| format!("{e} {}", m.gloss()))
        .collect();
    Some(format!("Reactions {label}: {}.", parts.join(", ")))
}

/// The substitutions on each connected platform that has any.
pub fn by_platform(state: &GroupState) -> Vec<(Platform, Vec<(Meaning, &'static str)>)> {
    state
        .me
        .iter()
        .map(|m| {
            let set = m.capabilities.reaction_set.as_deref();
            (m.platform, substitutions_in(set))
        })
        .filter(|(_, subs)| !subs.is_empty())
        .collect()
}

/// The only reactions the room's platform allows, when it restricts them.
pub fn reaction_set<'a>(state: &'a GroupState, room: &PlatformRoomId) -> Option<&'a [String]> {
    state.me_in(room)?.capabilities.reaction_set.as_deref()
}

/// The emoji for `meaning` on the platform `user` is reached on, for DMs
/// whose room may not be synced yet.
pub(crate) fn emoji_for_user(
    state: &GroupState,
    user: &PlatformUserId,
    meaning: Meaning,
) -> &'static str {
    let room = state.dm_room_of(user).or_else(|| {
        state
            .rooms
            .iter()
            .find(|(_, r)| r.members.contains_key(user))
            .map(|(id, _)| id.clone())
    });
    match room {
        Some(room) => emoji_for(state, &room, meaning),
        None => pick(
            state
                .me
                .first()
                .and_then(|m| m.capabilities.reaction_set.as_deref()),
            meaning,
        ),
    }
}

/// A reaction the model chose, as the room's platform can show it: as is
/// when allowed, else the stand-in for what it means, else nothing.
pub(crate) fn fit(state: &GroupState, room: &PlatformRoomId, emoji: &str) -> Option<String> {
    if allowed(state, room, emoji) {
        return Some(emoji.to_owned());
    }
    let meaning = meaning_of(state, room, emoji)?;
    let stand_in = emoji_for(state, room, meaning);
    allowed(state, room, stand_in).then(|| stand_in.to_owned())
}

/// "👍 if I should have, 👎 if not.", in the room's emoji.
pub(crate) fn feedback_line(state: &GroupState, room: &PlatformRoomId) -> String {
    format!(
        "{} if I should have, {} if not.",
        emoji_for(state, room, Meaning::ThumbsUp),
        emoji_for(state, room, Meaning::ThumbsDown)
    )
}

/// Reacts with `meaning`'s emoji for the room.
pub(crate) fn add(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    meaning: Meaning,
) -> ActionId {
    let emoji = emoji_for(state, room, meaning);
    effect::react(state, fx, room, message, emoji)
}

/// Takes `meaning`'s emoji for the room off a message.
pub(crate) fn remove(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    meaning: Meaning,
) {
    let emoji = emoji_for(state, room, meaning);
    effect::unreact(state, fx, room, message, emoji);
}

fn pick(set: Option<&[String]>, meaning: Meaning) -> &'static str {
    substituted(set, meaning).unwrap_or_else(|| meaning.canonical())
}

/// The stand-in in use for `meaning` under `set`, when there is one. A
/// platform with nothing that fits keeps the canonical emoji.
fn substituted(set: Option<&[String]>, meaning: Meaning) -> Option<&'static str> {
    let set = set?;
    if allows(set, meaning.canonical()) {
        return None;
    }
    meaning.stand_ins().iter().copied().find(|e| allows(set, e))
}

fn substitutions_in(set: Option<&[String]>) -> Vec<(Meaning, &'static str)> {
    Meaning::ALL
        .into_iter()
        .filter_map(|m| substituted(set, m).map(|e| (m, e)))
        .collect()
}

fn allows(set: &[String], emoji: &str) -> bool {
    let wanted = bare(emoji);
    set.iter().any(|e| bare(e) == wanted)
}

fn own_spelling(meaning: Meaning, bare_emoji: &str) -> bool {
    same(bare_emoji, meaning.canonical()) || meaning.aliases().iter().any(|a| same(bare_emoji, a))
}

/// Whether an already bare emoji is `target`, or a gendered variant of it.
fn same(bare_emoji: &str, target: &str) -> bool {
    let target = bare(target);
    bare_emoji == target
        || bare_emoji
            .strip_prefix(target.as_str())
            .is_some_and(|rest| rest.starts_with('\u{200d}'))
}

/// `emoji` without colons, variation selectors and skin tones.
fn bare(emoji: &str) -> String {
    emoji
        .trim()
        .trim_matches(':')
        .chars()
        .filter(|c| !matches!(c, '\u{fe0e}' | '\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Me;
    use ours_types::{Capabilities, GroupId, Kind, Room, RoomKind};

    const TELEGRAM: [&str; 20] = [
        "❤", "👍", "👎", "👀", "🔥", "🎉", "😁", "🤔", "🫡", "✍", "🙈", "🤝", "😴", "🙏", "👌",
        "💯", "🤓", "⚡", "🏆", "🙊",
    ];

    fn group(set: Option<&[&str]>) -> (GroupState, PlatformRoomId) {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        state.me.push(Me {
            platform: Platform::Telegram,
            id: "bot".into(),
            name: "Q".into(),
            capabilities: Capabilities {
                reaction_set: set.map(|s| s.iter().map(|e| (*e).to_owned()).collect()),
                ..Capabilities::default()
            },
        });
        let room = PlatformRoomId::new("r");
        state.room_mut(&room).info = Some(Room {
            platform: Platform::Telegram,
            id: room.clone(),
            name: Some("general".into()),
            kind: RoomKind::Channel,
            parent: None,
            container: None,
        });
        (state, room)
    }

    #[test]
    fn any_emoji_keeps_every_canonical_one() {
        let (state, room) = group(None);
        for m in Meaning::ALL {
            assert_eq!(emoji_for(&state, &room, m), m.canonical());
        }
        assert!(substitutions(&state, &room).is_empty());
        assert_eq!(meaning_of(&state, &room, "✅"), Some(Meaning::Check));
        assert_eq!(meaning_of(&state, &room, "😴"), None);
        assert!(!means(&state, &room, "👍", Meaning::Check));
    }

    #[test]
    fn a_fixed_set_gets_stand_ins() {
        let (state, room) = group(Some(&TELEGRAM));
        let subs = substitutions(&state, &room);
        assert_eq!(
            subs,
            vec![
                (Meaning::Check, "👍"),
                (Meaning::Shush, "😴"),
                (Meaning::RaisedHand, "🫡"),
                (Meaning::Pin, "✍"),
                (Meaning::NotMe, "🙈"),
                (Meaning::Double, "🤝"),
                (Meaning::Cross, "👎"),
            ]
        );
        assert_eq!(emoji_for(&state, &room, Meaning::Eyes), "👀");
        assert_eq!(emoji_for(&state, &room, Meaning::ThumbsUp), "👍");
        assert!(means(&state, &room, "👍", Meaning::Check));
        assert!(means(&state, &room, "👍", Meaning::ThumbsUp));
        assert!(means(&state, &room, "✅", Meaning::Check));
        assert!(means(&state, &room, "✍️", Meaning::Pin));
        assert_eq!(meaning_of(&state, &room, "😴"), Some(Meaning::Shush));
        assert_eq!(meaning_of(&state, &room, "🫡"), Some(Meaning::RaisedHand));
        assert_eq!(meaning_of(&state, &room, "👍"), Some(Meaning::ThumbsUp));
        assert_eq!(meaning_of(&state, &room, "🙏"), None);
        assert_eq!(
            charter_line(&subs[..4], "here").as_deref(),
            Some(
                "Reactions here: 👍 approves, 😴 stands me down, 🫡 means I have the answer, ✍ a promise noted."
            )
        );
    }

    #[test]
    fn a_missing_stand_in_falls_back_down_its_list() {
        let (state, room) = group(Some(&["👍", "🙊", "🤓"]));
        assert_eq!(emoji_for(&state, &room, Meaning::Shush), "🙊");
        assert_eq!(emoji_for(&state, &room, Meaning::RaisedHand), "🤓");
        // Nothing fits, so the canonical one stays and the platform refuses it.
        assert_eq!(emoji_for(&state, &room, Meaning::Pin), "📌");
    }

    #[test]
    fn variation_selectors_skin_tones_and_names_are_ignored() {
        let (state, room) = group(None);
        assert!(means(&state, &room, "👍🏽", Meaning::ThumbsUp));
        assert!(means(&state, &room, ":+1:", Meaning::ThumbsUp));
        assert!(means(&state, &room, "✌", Meaning::Double));
        assert!(means(&state, &room, "🙅\u{200d}♂\u{fe0f}", Meaning::NotMe));
        assert!(means(&state, &room, ":white_check_mark:", Meaning::Check));
        assert!(means(&state, &room, "✔️", Meaning::Check));
        assert!(!means(&state, &room, "👎", Meaning::ThumbsUp));
        assert!(is_canonical("🤫", Meaning::Shush));
        assert!(!is_canonical("😴", Meaning::Shush));
    }

    #[test]
    fn a_model_reaction_is_fitted_or_dropped() {
        let (state, room) = group(Some(&TELEGRAM));
        assert_eq!(fit(&state, &room, "🔥").as_deref(), Some("🔥"));
        assert_eq!(fit(&state, &room, "✅").as_deref(), Some("👍"));
        assert_eq!(fit(&state, &room, "😂"), None);
    }
}
