//! Reactions. Telegram lets a bot set one reaction per message and only
//! from a fixed set of emoji; members' reactions arrive as the full list
//! before and after each change.

use frankenstein::types::ReactionType;

/// Every emoji Telegram accepts as a reaction, as the Bot API spells them.
pub const ALLOWED: [&str; 73] = [
    "❤",
    "👍",
    "👎",
    "🔥",
    "🥰",
    "👏",
    "😁",
    "🤔",
    "🤯",
    "😱",
    "🤬",
    "😢",
    "🎉",
    "🤩",
    "🤮",
    "💩",
    "🙏",
    "👌",
    "🕊",
    "🤡",
    "🥱",
    "🥴",
    "😍",
    "🐳",
    "❤\u{200d}🔥",
    "🌚",
    "🌭",
    "💯",
    "🤣",
    "⚡",
    "🍌",
    "🏆",
    "💔",
    "🤨",
    "😐",
    "🍓",
    "🍾",
    "💋",
    "🖕",
    "😈",
    "😴",
    "😭",
    "🤓",
    "👻",
    "👨\u{200d}💻",
    "👀",
    "🎃",
    "🙈",
    "😇",
    "😨",
    "🤝",
    "✍",
    "🤗",
    "🫡",
    "🎅",
    "🎄",
    "☃",
    "💅",
    "🤪",
    "🗿",
    "🆒",
    "💘",
    "🙉",
    "🦄",
    "😘",
    "💊",
    "🙊",
    "😎",
    "👾",
    "🤷\u{200d}♂",
    "🤷",
    "🤷\u{200d}♀",
    "😡",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReactionError {
    #[error("Telegram has no digit reactions; use a poll")]
    Digit,
    #[error("Telegram does not allow {0} as a reaction")]
    NotAllowed(String),
}

/// `emoji` without variation selectors and skin tones, for comparing.
fn bare(emoji: &str) -> String {
    emoji
        .chars()
        .filter(|c| !matches!(c, '\u{fe0e}' | '\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}'))
        .collect()
}

/// A keycap digit such as 1️⃣, or 🔟.
fn is_digit(emoji: &str) -> bool {
    emoji == "🔟" || emoji.ends_with('\u{20e3}')
}

/// The spelling Telegram accepts for `emoji`, or why it cannot be sent.
pub fn telegram_form(emoji: &str) -> Result<&'static str, ReactionError> {
    let emoji = emoji.trim();
    if is_digit(emoji) {
        return Err(ReactionError::Digit);
    }
    let wanted = bare(emoji);
    ALLOWED
        .iter()
        .find(|allowed| bare(allowed) == wanted)
        .copied()
        .ok_or_else(|| ReactionError::NotAllowed(emoji.to_owned()))
}

fn emojis(list: &[ReactionType]) -> Vec<&str> {
    list.iter()
        .filter_map(|r| match r {
            ReactionType::Emoji(e) => Some(e.emoji.as_str()),
            ReactionType::CustomEmoji(_) | ReactionType::Paid(_) => None,
        })
        .collect()
}

/// The emoji a member added and removed, in the order they were listed.
/// Custom and paid reactions are left out.
pub fn diff(old: &[ReactionType], new: &[ReactionType]) -> (Vec<String>, Vec<String>) {
    let (old, new) = (emojis(old), emojis(new));
    let added = new
        .iter()
        .filter(|e| !old.contains(e))
        .map(|e| (*e).to_owned())
        .collect();
    let removed = old
        .iter()
        .filter(|e| !new.contains(e))
        .map(|e| (*e).to_owned())
        .collect();
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankenstein::types::{ReactionTypeCustomEmoji, ReactionTypeEmoji};

    fn emoji(e: &str) -> ReactionType {
        ReactionType::Emoji(ReactionTypeEmoji {
            emoji: e.to_owned(),
        })
    }

    fn custom(id: &str) -> ReactionType {
        ReactionType::CustomEmoji(ReactionTypeCustomEmoji {
            custom_emoji_id: id.to_owned(),
        })
    }

    #[test]
    fn the_engines_emoji_that_telegram_has_go_out_as_telegram_spells_them() {
        for (engine, telegram) in [
            ("👍", "👍"),
            ("👎", "👎"),
            ("👀", "👀"),
            ("🔥", "🔥"),
            ("❤️", "❤"),
            ("❤", "❤"),
            ("🎉", "🎉"),
            ("😁", "😁"),
            ("👍🏽", "👍"),
            ("✍️", "✍"),
            ("❤️\u{200d}🔥", "❤\u{200d}🔥"),
        ] {
            assert_eq!(telegram_form(engine), Ok(telegram), "{engine}");
        }
    }

    #[test]
    fn the_engines_emoji_that_telegram_lacks_are_named() {
        for missing in ["✅", "🙋", "📌", "🤫", "🙅", "✌️", "🗳️"] {
            let error = telegram_form(missing).unwrap_err();
            assert_eq!(error, ReactionError::NotAllowed(missing.to_owned()));
            assert_eq!(
                error.to_string(),
                format!("Telegram does not allow {missing} as a reaction")
            );
        }
    }

    #[test]
    fn digits_point_to_a_poll() {
        for digit in ["1️⃣", "2️⃣", "9️⃣", "🔟", "3\u{20e3}"] {
            assert_eq!(telegram_form(digit), Err(ReactionError::Digit), "{digit}");
        }
        assert_eq!(
            ReactionError::Digit.to_string(),
            "Telegram has no digit reactions; use a poll"
        );
    }

    #[test]
    fn every_allowed_emoji_maps_to_itself() {
        for allowed in ALLOWED {
            assert_eq!(telegram_form(allowed), Ok(allowed));
        }
        let mut sorted = ALLOWED.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ALLOWED.len());
    }

    #[test]
    fn a_change_is_split_into_added_and_removed() {
        let (added, removed) = diff(&[emoji("👍")], &[emoji("🔥"), emoji("👀")]);
        assert_eq!(added, ["🔥", "👀"]);
        assert_eq!(removed, ["👍"]);

        let (added, removed) = diff(&[], &[emoji("👍")]);
        assert_eq!((added, removed), (vec!["👍".to_owned()], vec![]));

        let (added, removed) = diff(&[emoji("👍")], &[]);
        assert_eq!((added, removed), (vec![], vec!["👍".to_owned()]));

        let (added, removed) = diff(&[emoji("👍")], &[emoji("👍")]);
        assert!(added.is_empty() && removed.is_empty());
    }

    #[test]
    fn custom_emoji_are_ignored() {
        let (added, removed) = diff(&[custom("5368")], &[custom("9999"), emoji("🎉")]);
        assert_eq!(added, ["🎉"]);
        assert!(removed.is_empty());
    }
}
