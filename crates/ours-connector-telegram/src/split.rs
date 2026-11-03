//! Fitting text into Telegram's message length limit. Telegram measures
//! text in UTF-16 code units, so every length here does too.

/// The most UTF-16 code units Telegram accepts in one message.
pub const MESSAGE_LIMIT: usize = 4096;

const MORE_LINE: &str = "(more on the page)";

/// Length of `s` as Telegram counts it.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Byte offset just past the longest prefix of `s` within `units`.
fn byte_at(s: &str, units: usize) -> usize {
    let mut used = 0;
    for (at, c) in s.char_indices() {
        used += c.len_utf16();
        if used > units {
            return at;
        }
    }
    s.len()
}

/// Split `text` into pieces of at most `limit` units. A piece ends at the
/// last newline in range, else the last space, else hard at the limit. The
/// newline or space it ends on is dropped. No piece is empty.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(2);
    let mut pieces = Vec::new();
    let mut rest = text;
    while utf16_len(rest) > limit {
        let window = &rest[..byte_at(rest, limit)];
        let cut = b"\n "
            .iter()
            .find_map(|sep| window.bytes().rposition(|b| b == *sep).filter(|i| *i > 0));
        let (piece, next) = match cut {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (window, &rest[window.len()..]),
        };
        if !piece.trim().is_empty() {
            pieces.push(piece.to_owned());
        }
        rest = next;
    }
    if !rest.trim().is_empty() {
        pieces.push(rest.to_owned());
    }
    pieces
}

/// Fit an edit into `limit` units. Over the limit, cut at the last newline
/// that leaves room and end with a line pointing to the page.
pub fn truncate_edit(text: &str, limit: usize) -> String {
    if utf16_len(text) <= limit {
        return text.to_owned();
    }
    let suffix = format!("\n{MORE_LINE}");
    let budget = limit.saturating_sub(utf16_len(&suffix));
    let window = &text[..byte_at(text, budget)];
    let head = window.rfind('\n').map_or(window, |i| &window[..i]);
    format!("{head}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_the_limit_is_one_piece() {
        assert_eq!(split_message("hello", 10), vec!["hello"]);
    }

    #[test]
    fn exactly_4096_is_one_piece_and_4097_is_two() {
        let text = "a".repeat(MESSAGE_LIMIT);
        assert_eq!(split_message(&text, MESSAGE_LIMIT), vec![text.clone()]);
        let over = "a".repeat(MESSAGE_LIMIT + 1);
        let pieces = split_message(&over, MESSAGE_LIMIT);
        assert_eq!(pieces.len(), 2);
        assert_eq!(utf16_len(&pieces[0]), MESSAGE_LIMIT);
        assert_eq!(pieces[1], "a");
    }

    #[test]
    fn breaks_on_the_last_newline_then_the_last_space() {
        assert_eq!(
            split_message("aaa bbb\nccc ddd\neee", 12),
            vec!["aaa bbb", "ccc ddd\neee"]
        );
        assert_eq!(
            split_message("aaaa bbbb cccc", 12),
            vec!["aaaa bbbb", "cccc"]
        );
    }

    #[test]
    fn a_long_paragraph_breaks_at_its_last_newline_under_the_limit() {
        let first = "word ".repeat(700);
        let second = "more ".repeat(300);
        let text = format!("{}\n{}", first.trim_end(), second.trim_end());
        let pieces = split_message(&text, MESSAGE_LIMIT);
        assert_eq!(pieces, vec![first.trim_end(), second.trim_end()]);
    }

    #[test]
    fn breaks_hard_on_a_long_word() {
        assert_eq!(
            split_message("abcdefghijklmno", 6),
            vec!["abcdef", "ghijkl", "mno"]
        );
    }

    #[test]
    fn emoji_count_as_two_units() {
        let text = "😀".repeat(5);
        assert_eq!(utf16_len(&text), 10);
        assert_eq!(
            split_message(&text, 4),
            vec!["😀😀".to_owned(), "😀😀".to_owned(), "😀".to_owned()]
        );
        for piece in split_message(&"😀".repeat(3000), MESSAGE_LIMIT) {
            assert!(utf16_len(&piece) <= MESSAGE_LIMIT);
        }
    }

    #[test]
    fn never_yields_an_empty_piece() {
        for text in ["", "\n\n\n\n\n\n", "      ", "ab\n\n\n\n\ncd", "\nabcdefgh"] {
            for piece in split_message(text, 3) {
                assert!(!piece.trim().is_empty(), "{text:?}");
                assert!(utf16_len(&piece) <= 3);
            }
        }
        assert!(split_message("", 5).is_empty());
    }

    #[test]
    fn a_short_edit_is_unchanged() {
        assert_eq!(truncate_edit("hi", MESSAGE_LIMIT), "hi");
    }

    #[test]
    fn a_long_edit_cuts_at_a_newline_and_points_to_the_page() {
        let text = format!("{}\n{}", "a".repeat(3000), "b".repeat(2000));
        let out = truncate_edit(&text, MESSAGE_LIMIT);
        assert_eq!(out, format!("{}\n(more on the page)", "a".repeat(3000)));
        let wide = truncate_edit(&"é".repeat(5000), MESSAGE_LIMIT);
        assert!(utf16_len(&wide) <= MESSAGE_LIMIT);
        assert!(wide.ends_with("\n(more on the page)"));
    }
}
