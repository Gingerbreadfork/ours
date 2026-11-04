//! Helpers for putting chat text into a prompt.

pub(crate) const MAX_LINE_CHARS: usize = 800;

/// One line of at most `MAX_LINE_CHARS`, so a chat line cannot start a
/// line of its own in the prompt or swamp the model's context.
pub(crate) fn flat(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(MAX_LINE_CHARS)
        .map(|c| if breaks_line(c) { ' ' } else { c })
        .collect();
    if text.chars().count() > MAX_LINE_CHARS {
        out.push_str("...");
    }
    out
}

/// True for control characters and the Unicode line and paragraph separators.
fn breaks_line(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}

/// At most `max` characters of `text`, without a trailing space.
pub(crate) fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].trim_end().to_owned(),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_turns_every_line_break_into_a_space() {
        assert_eq!(
            flat("a\nb\r\nc\u{85}d\u{2028}e\u{2029}f\tg"),
            "a b  c d e f g"
        );
    }

    #[test]
    fn clip_never_splits_a_character() {
        assert_eq!(clip("héllo wörld", 5), "héllo");
        assert_eq!(clip("héllo wörld", 6), "héllo");
        assert_eq!(clip("short", 120), "short");
    }
}
