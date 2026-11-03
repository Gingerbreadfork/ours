//! Whether a message is addressed to the agent (DESIGN.md section 7): a
//! mention, its name at the start, a reply to one of its messages, or a DM.

use crate::state::GroupState;
use ours_types::Message;

pub fn is_addressed(state: &GroupState, message: &Message) -> bool {
    if message.mentions_me || message.mentions.iter().any(|u| state.is_me(u)) {
        return true;
    }
    if name_prefix(message.text.trim_start(), &state.agent_name).is_some() {
        return true;
    }
    let room = state.rooms.get(&message.room);
    if let Some(parent) = &message.reply_to
        && room.is_some_and(|r| r.is_own(parent))
    {
        return true;
    }
    room.is_some_and(|r| r.is_dm())
}

/// The text after the agent's name or a leading mention of it, with the
/// separator removed. Returns the whole text when there is no address.
pub fn after_address<'a>(state: &GroupState, text: &'a str) -> &'a str {
    let text = text.trim_start();
    if let Some(end) = name_prefix(text, &state.agent_name) {
        return trim_separator(&text[end..]);
    }
    // A connector rewrites a mention of the bot into the default name; the
    // group may have chosen another since.
    if state.agent_name != crate::state::DEFAULT_AGENT_NAME
        && let Some(end) = name_prefix(text, crate::state::DEFAULT_AGENT_NAME)
    {
        return trim_separator(&text[end..]);
    }
    let (first, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let names_me = state.me.iter().any(|m| {
        first.contains(m.id.as_str()) || first.trim_start_matches('@').eq_ignore_ascii_case(&m.name)
    });
    if names_me { trim_separator(rest) } else { text }
}

/// If `text` starts with `name` (optionally after "@") followed by a comma,
/// colon, whitespace or the end, the byte offset just past the name.
fn name_prefix(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let body = text.strip_prefix('@').unwrap_or(text);
    let offset = text.len() - body.len();
    let mut chars = body.char_indices();
    for expected in name.chars() {
        let (_, got) = chars.next()?;
        if !expected.to_lowercase().eq(got.to_lowercase()) {
            return None;
        }
    }
    match chars.next() {
        None => Some(text.len()),
        Some((i, c)) if c == ',' || c == ':' || c.is_whitespace() => Some(offset + i),
        Some(_) => None,
    }
}

fn trim_separator(text: &str) -> &str {
    text.trim_start_matches(|c: char| c == ',' || c == ':' || c == '-' || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_must_stand_alone() {
        for text in [
            "Q, when was that",
            "q undo the booking",
            "Q: hi",
            "Q",
            "@q what's up",
            "q\tthere",
        ] {
            assert!(name_prefix(text, "Q").is_some(), "{text}");
        }
        for text in [
            "Q&A is at four",
            "queue's long",
            "quick one",
            "the Q, though",
            "@ours hi",
        ] {
            assert!(name_prefix(text, "Q").is_none(), "{text}");
        }
        assert!(name_prefix("Robin, hi", "robin").is_some());
        assert!(name_prefix("anything", "").is_none());
    }

    #[test]
    fn the_address_is_stripped() {
        let mut state = GroupState::new("g".into(), ours_types::Kind::Friends);
        state.agent_name = crate::sim::SIM_AGENT_NAME.to_owned();
        assert_eq!(
            after_address(&state, "Q, who do you listen to?"),
            "who do you listen to?"
        );
        assert_eq!(after_address(&state, "@q: undo"), "undo");
        assert_eq!(after_address(&state, "q"), "");
        assert_eq!(after_address(&state, "thanks"), "thanks");
    }
}
