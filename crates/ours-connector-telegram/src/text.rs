//! Message text and its entities: who a mention points at, whether the
//! agent was addressed, and the one shape the engine reads. Entity offsets
//! count UTF-16 code units.

use frankenstein::types::{MessageEntity, MessageEntityType};
use std::ops::Range;

/// Who the agent is on Telegram and the name it answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub id: u64,
    pub username: Option<String>,
    pub agent_name: String,
}

impl Me {
    fn is_username(&self, name: &str) -> bool {
        self.username
            .as_deref()
            .is_some_and(|u| u.eq_ignore_ascii_case(name))
    }
}

/// The text the engine sees, who it mentions, and whether it is for the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub text: String,
    pub mentions: Vec<u64>,
    pub mentions_me: bool,
}

/// The byte range of an entity in `text`, or `None` when it does not fit.
pub fn byte_range(text: &str, offset: u16, length: u16) -> Option<Range<usize>> {
    let (start, end) = (
        usize::from(offset),
        usize::from(offset) + usize::from(length),
    );
    let mut units = 0;
    let mut range = (None, None);
    for (at, c) in text.char_indices() {
        if units == start {
            range.0 = Some(at);
        }
        if units == end {
            range.1 = Some(at);
            break;
        }
        units += c.len_utf16();
    }
    if units == start && range.0.is_none() {
        range.0 = Some(text.len());
    }
    if units == end && range.1.is_none() {
        range.1 = Some(text.len());
    }
    match range {
        (Some(s), Some(e)) if s <= e => Some(s..e),
        _ => None,
    }
}

/// An entity with its place in the text.
struct Span<'a> {
    entity: &'a MessageEntity,
    range: Range<usize>,
}

fn spans<'a>(text: &str, entities: &'a [MessageEntity]) -> Vec<Span<'a>> {
    let mut spans: Vec<Span<'a>> = entities
        .iter()
        .filter_map(|entity| {
            byte_range(text, entity.offset, entity.length).map(|range| Span { entity, range })
        })
        .collect();
    spans.sort_by_key(|s| s.range.start);
    spans
}

/// A command and the bot it names: `/start@ours_bot` is `("/start", Some("ours_bot"))`.
fn command(token: &str) -> (&str, Option<&str>) {
    match token.split_once('@') {
        Some((name, bot)) => (name, Some(bot)),
        None => (token, None),
    }
}

/// Read a message's text. `lookup` finds a user by username, without the
/// "@". In a private chat `/start` reads as "hello", so the first
/// conversation can begin. A command's "@bot" suffix naming the agent is a
/// mention and is dropped; a leading "@bot" or "@name" becomes "name, ".
pub fn read(
    text: &str,
    entities: &[MessageEntity],
    me: &Me,
    private: bool,
    lookup: impl Fn(&str) -> Option<u64>,
) -> Read {
    let spans = spans(text, entities);
    let mut mentions: Vec<u64> = Vec::new();
    let mut mentions_me = false;
    for span in &spans {
        let token = &text[span.range.clone()];
        match span.entity.type_field {
            MessageEntityType::TextMention => {
                if let Some(user) = &span.entity.user {
                    mentions_me |= user.id == me.id;
                    note(&mut mentions, user.id);
                }
            }
            MessageEntityType::Mention => {
                let name = token.trim_start_matches('@');
                if me.is_username(name) {
                    mentions_me = true;
                    note(&mut mentions, me.id);
                } else if let Some(id) = lookup(name) {
                    note(&mut mentions, id);
                }
            }
            MessageEntityType::BotCommand
                if command(token).1.is_some_and(|bot| me.is_username(bot)) =>
            {
                mentions_me = true;
            }
            _ => {}
        }
    }

    let first = spans.first().filter(|s| s.range.start == 0);
    if private
        && let Some(first) = first
        && first.entity.type_field == MessageEntityType::BotCommand
    {
        let (name, bot) = command(&text[first.range.clone()]);
        if name.eq_ignore_ascii_case("/start") && bot.is_none_or(|b| me.is_username(b)) {
            return Read {
                text: "hello".to_owned(),
                mentions,
                mentions_me,
            };
        }
    }

    // Rebuild the text without the agent's command suffixes, noting where a
    // leading mention of the agent ends.
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut lead_end = None;
    for span in &spans {
        if span.range.start < copied {
            continue;
        }
        let token = &text[span.range.clone()];
        match span.entity.type_field {
            MessageEntityType::BotCommand => {
                let (name, bot) = command(token);
                if bot.is_some_and(|b| me.is_username(b)) {
                    out.push_str(&text[copied..span.range.start]);
                    out.push_str(name);
                    copied = span.range.end;
                }
            }
            MessageEntityType::TextMention
                if span.entity.user.as_ref().is_some_and(|u| u.id == me.id)
                    && text[..span.range.start].trim().is_empty() =>
            {
                out.push_str(&text[copied..span.range.end]);
                copied = span.range.end;
                lead_end = Some(out.len());
            }
            _ => {}
        }
    }
    out.push_str(&text[copied..]);

    if let Some(end) = lead_end {
        return Read {
            text: addressed_text(&me.agent_name, &out[end..], &out),
            mentions,
            mentions_me: true,
        };
    }
    let addressed = address(&out, mentions_me, me);
    Read {
        text: addressed.0,
        mentions,
        mentions_me: addressed.1,
    }
}

fn note(mentions: &mut Vec<u64>, id: u64) {
    if !mentions.contains(&id) {
        mentions.push(id);
    }
}

/// A leading "@bot" or "@name" rewritten to "name, rest"; the agent's
/// name at the start counts as addressing it.
fn address(text: &str, mentioned: bool, me: &Me) -> (String, bool) {
    let trimmed = text.trim_start();
    if let Some(after) = trimmed.strip_prefix('@') {
        let handle = me
            .username
            .as_deref()
            .and_then(|username| strip_name(after, username));
        let named = strip_name(after, &me.agent_name).filter(|_| !me.agent_name.is_empty());
        if let Some(rest) = handle.or(named) {
            return (addressed_text(&me.agent_name, rest, text), true);
        }
    }
    let named = !me.agent_name.is_empty() && strip_name(trimmed, &me.agent_name).is_some();
    (text.to_owned(), mentioned || named)
}

/// `name, rest` with the punctuation after a mention removed.
fn addressed_text(agent_name: &str, rest: &str, original: &str) -> String {
    let rest = rest.trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace());
    if agent_name.is_empty() {
        original.to_owned()
    } else if rest.is_empty() {
        agent_name.to_owned()
    } else {
        format!("{agent_name}, {rest}")
    }
}

/// If `s` starts with `name`, ignoring case, and then a comma, colon,
/// whitespace or the end, return what follows the name.
fn strip_name<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    if name.is_empty() {
        return None;
    }
    let mut chars = s.char_indices();
    for expected in name.chars() {
        let (_, actual) = chars.next()?;
        if !actual.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
    }
    let rest = chars.next().map_or("", |(at, _)| &s[at..]);
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c == ',' || c == ':' || c.is_whitespace() => Some(rest),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn me() -> Me {
        Me {
            id: 900,
            username: Some("ours_q_bot".to_owned()),
            agent_name: "Q".to_owned(),
        }
    }

    fn entity(value: serde_json::Value) -> MessageEntity {
        serde_json::from_value(value).unwrap()
    }

    fn mention(offset: u16, length: u16) -> MessageEntity {
        entity(json!({ "type": "mention", "offset": offset, "length": length }))
    }

    fn command_at(offset: u16, length: u16) -> MessageEntity {
        entity(json!({ "type": "bot_command", "offset": offset, "length": length }))
    }

    fn lookup(name: &str) -> Option<u64> {
        match name.to_lowercase().as_str() {
            "sam_k" => Some(11),
            "jo" => Some(12),
            _ => None,
        }
    }

    fn read_group(text: &str, entities: &[MessageEntity]) -> Read {
        read(text, entities, &me(), false, lookup)
    }

    #[test]
    fn ranges_count_utf16_units() {
        let text = "😀 @sam_k hi";
        assert_eq!(byte_range(text, 3, 6), Some(5..11));
        assert_eq!(&text[byte_range(text, 3, 6).unwrap()], "@sam_k");
        assert_eq!(byte_range(text, 0, 2), Some(0..4));
        assert_eq!(byte_range(text, 1, 1), None);
        assert_eq!(byte_range("abc", 1, 2), Some(1..3));
        assert_eq!(byte_range("abc", 3, 0), Some(3..3));
        assert_eq!(byte_range("abc", 2, 5), None);
    }

    #[test]
    fn a_leading_bot_mention_becomes_the_agents_name() {
        let read = read_group("@ours_q_bot when is dinner?", &[mention(0, 11)]);
        assert_eq!(read.text, "Q, when is dinner?");
        assert!(read.mentions_me);
        assert_eq!(read.mentions, [900]);

        let bare = read_group("@Ours_Q_Bot", &[mention(0, 11)]);
        assert_eq!(bare.text, "Q");
        let punctuated = read_group("@ours_q_bot: book it", &[mention(0, 11)]);
        assert_eq!(punctuated.text, "Q, book it");
    }

    #[test]
    fn a_mention_later_in_the_text_addresses_without_a_rewrite() {
        let read = read_group("thanks @ours_q_bot", &[mention(7, 11)]);
        assert_eq!(read.text, "thanks @ours_q_bot");
        assert!(read.mentions_me);
    }

    #[test]
    fn the_agents_name_addresses_it() {
        let named = read_group("Q, what time was it?", &[]);
        assert!(named.mentions_me);
        assert_eq!(named.text, "Q, what time was it?");
        let at_name = read_group("@q what time", &[]);
        assert_eq!(
            (at_name.text.as_str(), at_name.mentions_me),
            ("Q, what time", true)
        );
        assert!(!read_group("Quincy is late", &[]).mentions_me);
        assert!(!read_group("dinner tonight?", &[]).mentions_me);
    }

    #[test]
    fn mentions_resolve_by_username_and_by_text_mention() {
        let text = "@sam_k and Priya, plus @stranger";
        let entities = [
            mention(0, 6),
            entity(json!({ "type": "text_mention", "offset": 11, "length": 5,
                "user": { "id": 13, "is_bot": false, "first_name": "Priya" } })),
            mention(23, 9),
        ];
        let read = read_group(text, &entities);
        assert_eq!(read.mentions, [11, 13]);
        assert!(!read.mentions_me);
        assert_eq!(read.text, text);
    }

    #[test]
    fn a_text_mention_of_the_agent_at_the_start_is_rewritten() {
        let entities = [entity(
            json!({ "type": "text_mention", "offset": 0, "length": 1,
            "user": { "id": 900, "is_bot": true, "first_name": "Q" } }),
        )];
        let read = read_group("Q remind me at 4", &entities);
        assert_eq!(read.text, "Q, remind me at 4");
        assert!(read.mentions_me);
    }

    #[test]
    fn start_in_a_private_chat_is_hello() {
        for (text, length) in [
            ("/start", 6),
            ("/start abc123", 6),
            ("/start@ours_q_bot", 17),
        ] {
            let read = read(text, &[command_at(0, length)], &me(), true, lookup);
            assert_eq!(read.text, "hello", "{text}");
        }
        let in_group = read("/start", &[command_at(0, 6)], &me(), false, lookup);
        assert_eq!(in_group.text, "/start");
        let other_bot = read(
            "/start@other_bot",
            &[command_at(0, 16)],
            &me(),
            true,
            lookup,
        );
        assert_eq!(other_bot.text, "/start@other_bot");
    }

    #[test]
    fn commands_are_plain_text_and_the_agents_suffix_is_a_mention() {
        let plain = read_group("/q what's on", &[command_at(0, 2)]);
        assert_eq!(plain.text, "/q what's on");
        assert!(!plain.mentions_me);

        let named = read_group("/q@ours_q_bot what's on", &[command_at(0, 13)]);
        assert_eq!(named.text, "/q what's on");
        assert!(named.mentions_me);

        let other = read_group("/poll@other_bot x", &[command_at(0, 15)]);
        assert_eq!(other.text, "/poll@other_bot x");
        assert!(!other.mentions_me);
    }

    #[test]
    fn entities_that_do_not_fit_are_skipped() {
        let read = read_group("hi", &[mention(1, 40)]);
        assert_eq!(read.text, "hi");
        assert!(read.mentions.is_empty());
    }
}
