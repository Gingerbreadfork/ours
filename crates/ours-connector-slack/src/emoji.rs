//! Slack names reactions by shortcode (`white_check_mark`); the engine names
//! them by the emoji itself (✅). The table covers what the engine reads and
//! sends; the `emojis` crate's GitHub shortcodes cover most of the rest.

/// Slack names and their emoji. Where a name has aliases, the first entry for
/// an emoji is the one sent back to Slack.
const TABLE: &[(&str, &str)] = &[
    ("zero", "0\u{fe0f}\u{20e3}"),
    ("one", "1\u{fe0f}\u{20e3}"),
    ("two", "2\u{fe0f}\u{20e3}"),
    ("three", "3\u{fe0f}\u{20e3}"),
    ("four", "4\u{fe0f}\u{20e3}"),
    ("five", "5\u{fe0f}\u{20e3}"),
    ("six", "6\u{fe0f}\u{20e3}"),
    ("seven", "7\u{fe0f}\u{20e3}"),
    ("eight", "8\u{fe0f}\u{20e3}"),
    ("nine", "9\u{fe0f}\u{20e3}"),
    ("keycap_ten", "🔟"),
    ("white_check_mark", "✅"),
    ("heavy_check_mark", "✔\u{fe0f}"),
    ("ballot_box_with_check", "☑\u{fe0f}"),
    ("x", "❌"),
    ("heavy_multiplication_x", "✖\u{fe0f}"),
    ("eyes", "👀"),
    ("raised_hand", "✋"),
    ("hand", "✋"),
    ("raising_hand", "🙋"),
    ("no_good", "🙅"),
    ("v", "✌\u{fe0f}"),
    ("shushing_face", "🤫"),
    ("+1", "👍"),
    ("thumbsup", "👍"),
    ("-1", "👎"),
    ("thumbsdown", "👎"),
    ("pushpin", "📌"),
    ("tada", "🎉"),
    ("heart", "❤\u{fe0f}"),
    ("fire", "🔥"),
    ("joy", "😂"),
];

/// Skin tone modifiers, U+1F3FB to U+1F3FF.
fn is_skin_tone(c: char) -> bool {
    ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)
}

/// The emoji with variation selectors and skin tones removed, for matching.
fn bare(emoji: &str) -> String {
    emoji
        .chars()
        .filter(|c| *c != '\u{fe0f}' && !is_skin_tone(*c))
        .collect()
}

/// A Slack reaction name as the engine names it: the emoji where it is
/// known, else `:name:`. Skin tones (`+1::skin-tone-3`) are dropped.
pub fn from_slack(name: &str) -> String {
    let name = name.trim().trim_matches(':');
    let name = name.split_once("::").map_or(name, |(base, _)| base);
    if let Some((_, emoji)) = TABLE.iter().find(|(n, _)| *n == name) {
        return (*emoji).to_owned();
    }
    match emojis::get_by_shortcode(name) {
        Some(emoji) => emoji.as_str().to_owned(),
        None => format!(":{name}:"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmojiError {
    #[error("no reaction emoji was given")]
    Empty,
    #[error("Slack has no reaction name for `{0}`")]
    Unknown(String),
}

/// The Slack reaction name for an emoji from the engine. A name already in
/// Slack's form (`:partyparrot:` or `thumbsup`) passes through.
pub fn to_slack(emoji: &str) -> Result<String, EmojiError> {
    let trimmed = emoji.trim();
    if trimmed.is_empty() {
        return Err(EmojiError::Empty);
    }
    if trimmed.is_ascii() {
        let name = trimmed.trim_matches(':');
        let name = name.split_once("::").map_or(name, |(base, _)| base);
        return if name.is_empty() {
            Err(EmojiError::Empty)
        } else {
            Ok(name.to_owned())
        };
    }
    let wanted = bare(trimmed);
    if let Some((name, _)) = TABLE.iter().find(|(_, e)| bare(e) == wanted) {
        return Ok((*name).to_owned());
    }
    let toneless: String = trimmed.chars().filter(|c| !is_skin_tone(*c)).collect();
    emojis::get(&toneless)
        .or_else(|| emojis::get(&wanted))
        .and_then(|e| e.shortcode())
        .map(str::to_owned)
        .ok_or_else(|| EmojiError::Unknown(trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_one_to_ten_are_keycaps() {
        let names = [
            "one",
            "two",
            "three",
            "four",
            "five",
            "six",
            "seven",
            "eight",
            "nine",
            "keycap_ten",
        ];
        for (n, name) in names.iter().enumerate() {
            let emoji = from_slack(name);
            if n < 9 {
                assert_eq!(emoji, format!("{}\u{fe0f}\u{20e3}", n + 1), "{name}");
            } else {
                assert_eq!(emoji, "🔟");
            }
            assert_eq!(to_slack(&emoji).unwrap(), *name);
        }
    }

    #[test]
    fn keycaps_without_the_variation_selector_still_match() {
        assert_eq!(to_slack("3\u{20e3}").unwrap(), "three");
    }

    #[test]
    fn the_common_set_goes_both_ways() {
        for (name, emoji) in [
            ("white_check_mark", "✅"),
            ("x", "❌"),
            ("eyes", "👀"),
            ("raised_hand", "✋"),
            ("shushing_face", "🤫"),
            ("pushpin", "📌"),
            ("tada", "🎉"),
            ("fire", "🔥"),
            ("joy", "😂"),
        ] {
            assert_eq!(from_slack(name), emoji, "{name}");
            assert_eq!(to_slack(emoji).unwrap(), name, "{emoji}");
        }
        assert_eq!(from_slack("heart"), "❤\u{fe0f}");
        assert_eq!(to_slack("❤").unwrap(), "heart");
        assert_eq!(to_slack("❤\u{fe0f}").unwrap(), "heart");
    }

    #[test]
    fn thumbs_have_two_names_each_and_go_back_as_the_short_one() {
        assert_eq!(from_slack("+1"), "👍");
        assert_eq!(from_slack("thumbsup"), "👍");
        assert_eq!(from_slack("-1"), "👎");
        assert_eq!(from_slack("thumbsdown"), "👎");
        assert_eq!(to_slack("👍").unwrap(), "+1");
        assert_eq!(to_slack("👎").unwrap(), "-1");
    }

    #[test]
    fn skin_tones_are_stripped_both_ways() {
        assert_eq!(from_slack("+1::skin-tone-3"), "👍");
        assert_eq!(from_slack("raised_hand::skin-tone-6"), "✋");
        assert_eq!(to_slack("👍🏽").unwrap(), "+1");
        assert_eq!(to_slack("✋🏿").unwrap(), "raised_hand");
    }

    #[test]
    fn colons_around_a_name_are_ignored() {
        assert_eq!(from_slack(":tada:"), "🎉");
        assert_eq!(to_slack(":tada:").unwrap(), "tada");
    }

    #[test]
    fn unknown_names_stay_names() {
        assert_eq!(from_slack("partyparrot"), ":partyparrot:");
        assert_eq!(to_slack(":partyparrot:").unwrap(), "partyparrot");
        assert_eq!(to_slack("white_check_mark").unwrap(), "white_check_mark");
    }

    #[test]
    fn the_shortcode_list_covers_names_the_table_lacks() {
        assert_eq!(from_slack("rocket"), "🚀");
        assert_eq!(to_slack("🚀").unwrap(), "rocket");
        assert_eq!(to_slack("🤔").unwrap(), "thinking");
    }

    #[test]
    fn an_emoji_with_no_name_is_an_error() {
        assert_eq!(to_slack("  "), Err(EmojiError::Empty));
        assert_eq!(to_slack("::"), Err(EmojiError::Empty));
        assert!(matches!(to_slack("é"), Err(EmojiError::Unknown(_))));
    }
}
