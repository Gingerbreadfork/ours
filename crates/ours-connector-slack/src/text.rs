//! Slack's message markup turned into the plain text the engine reads.
//! User mentions stay as `<@U...>` for the engine to render; links,
//! channel references and special mentions become readable words; the
//! three escapes Slack applies are undone.

use ours_types::PlatformUserId;

/// Text with the user ids it mentions, in order of first mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub text: String,
    pub mentions: Vec<PlatformUserId>,
}

/// Rewrite Slack markup into plain text.
pub fn normalise(raw: &str) -> Parsed {
    let mut text = String::with_capacity(raw.len());
    let mut mentions: Vec<PlatformUserId> = Vec::new();
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        text.push_str(&unescape(&rest[..open]));
        let after = &rest[open + 1..];
        let Some(close) = after.find('>') else {
            text.push_str(&unescape(&rest[open..]));
            rest = "";
            break;
        };
        let token = &after[..close];
        if let Some(user) = user_token(token) {
            let id = PlatformUserId::new(user);
            text.push_str(&format!("<@{user}>"));
            if !mentions.contains(&id) {
                mentions.push(id);
            }
        } else {
            text.push_str(&render(token));
        }
        rest = &after[close + 1..];
    }
    text.push_str(&unescape(rest));
    Parsed { text, mentions }
}

/// Undo Slack's escaping of `&`, `<` and `>` in one pass, so an escaped
/// ampersand followed by `lt;` stays as typed.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let (replacement, used) = if tail.starts_with("&amp;") {
            ("&", 5)
        } else if tail.starts_with("&lt;") {
            ("<", 4)
        } else if tail.starts_with("&gt;") {
            (">", 4)
        } else {
            ("&", 1)
        };
        out.push_str(replacement);
        rest = &tail[used..];
    }
    out.push_str(rest);
    out
}

/// The user id in `@U123` or `@U123|name`.
fn user_token(token: &str) -> Option<&str> {
    let id = token.strip_prefix('@')?;
    let id = id.split_once('|').map_or(id, |(id, _)| id);
    is_user_id(id).then_some(id)
}

/// A Slack user id: `U` or `W` and then capitals and digits.
pub fn is_user_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('U' | 'W'))
        && id.len() > 1
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Everything in angle brackets that is not a user mention.
fn render(token: &str) -> String {
    let (target, label) = match token.split_once('|') {
        Some((target, label)) => (target, unescape(label)),
        None => (token, String::new()),
    };
    if let Some(channel) = target.strip_prefix('#') {
        return if label.is_empty() {
            format!("#{channel}")
        } else {
            format!("#{label}")
        };
    }
    if let Some(special) = target.strip_prefix('!') {
        return special_mention(special, label);
    }
    link(&unescape(target), label)
}

/// `<!here>`, `<!subteam^S1|@leads>`, `<!date^...|fallback>` and the like.
fn special_mention(special: &str, label: String) -> String {
    let (kind, _) = special.split_once('^').unwrap_or((special, ""));
    match kind {
        "here" | "channel" | "everyone" => format!("@{kind}"),
        "subteam" if !label.is_empty() => {
            if label.starts_with('@') {
                label
            } else {
                format!("@{label}")
            }
        }
        "subteam" => special
            .split_once('^')
            .map_or_else(|| "@group".to_owned(), |(_, id)| format!("@{id}")),
        _ if !label.is_empty() => label,
        _ => kind.to_owned(),
    }
}

/// `label (url)`, or just the label when it only repeats the address.
fn link(url: &str, label: String) -> String {
    let bare = ["https://", "http://", "mailto:"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
        .unwrap_or(url);
    if label.is_empty() {
        if url.starts_with("mailto:") {
            bare.to_owned()
        } else {
            url.to_owned()
        }
    } else if label == url || label == bare {
        label
    } else {
        format!("{label} ({url})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(raw: &str) -> String {
        normalise(raw).text
    }

    #[test]
    fn the_three_escapes_are_undone_once() {
        assert_eq!(unescape("a &amp; b &lt;3 &gt;"), "a & b <3 >");
        assert_eq!(
            unescape("&amp;lt; is how you write &lt;"),
            "&lt; is how you write <"
        );
        assert_eq!(unescape("fish & chips &nbsp;"), "fish & chips &nbsp;");
        assert_eq!(text("1 &lt; 2 &amp;&amp; 3 &gt; 2"), "1 < 2 && 3 > 2");
    }

    #[test]
    fn labelled_links_read_as_label_then_address() {
        assert_eq!(
            text("see <https://example.com/plan|the plan> today"),
            "see the plan (https://example.com/plan) today"
        );
        assert_eq!(
            text("<https://example.com/?a=1&amp;b=2|a &amp; b>"),
            "a & b (https://example.com/?a=1&b=2)"
        );
    }

    #[test]
    fn bare_and_auto_links_keep_one_copy_of_the_address() {
        assert_eq!(text("<https://example.com>"), "https://example.com");
        assert_eq!(text("<http://example.com|example.com>"), "example.com");
        assert_eq!(
            text("<mailto:jo@example.com|jo@example.com>"),
            "jo@example.com"
        );
        assert_eq!(text("<mailto:jo@example.com>"), "jo@example.com");
    }

    #[test]
    fn user_mentions_stay_as_tokens_and_are_collected() {
        let parsed = normalise("<@U1> and <@W2|sam> and <@U1> again");
        assert_eq!(parsed.text, "<@U1> and <@W2> and <@U1> again");
        assert_eq!(
            parsed.mentions,
            vec![PlatformUserId::new("U1"), PlatformUserId::new("W2")]
        );
    }

    #[test]
    fn channels_and_special_mentions_become_words() {
        assert_eq!(text("in <#C1|launch>"), "in #launch");
        assert_eq!(text("in <#C1>"), "in #C1");
        assert_eq!(text("in <#C1|>"), "in #C1");
        assert_eq!(text("<!here> lunch"), "@here lunch");
        assert_eq!(text("<!channel|channel> lunch"), "@channel lunch");
        assert_eq!(text("<!subteam^S1|@leads> look"), "@leads look");
        assert_eq!(text("<!subteam^S1> look"), "@S1 look");
        assert_eq!(text("due <!date^1700000000^{date}|Nov 14>"), "due Nov 14");
        assert!(normalise("<!here>").mentions.is_empty());
    }

    #[test]
    fn an_unclosed_bracket_is_left_as_written() {
        assert_eq!(text("a <b"), "a <b");
        assert_eq!(text("plain"), "plain");
        assert_eq!(text(""), "");
    }

    #[test]
    fn user_ids_are_u_or_w_then_capitals_and_digits() {
        assert!(is_user_id("U012AB"));
        assert!(is_user_id("W9"));
        assert!(!is_user_id("U"));
        assert!(!is_user_id("C012"));
        assert!(!is_user_id("Uabc"));
    }
}
