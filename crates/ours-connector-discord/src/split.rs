//! Fitting text into Discord's message length limit.

/// The most characters Discord accepts in one message.
pub const MESSAGE_LIMIT: usize = 2000;

const MORE_LINE: &str = "(more on the page)";

/// Byte offset just past the first `chars` characters of `s`.
fn byte_at(s: &str, chars: usize) -> usize {
    s.char_indices().nth(chars).map_or(s.len(), |(i, _)| i)
}

/// Split `text` into pieces of at most `limit` characters. A piece ends at
/// the last newline in range, else the last space, else hard at the limit.
/// The newline or space it ends on is dropped. No piece is empty.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.chars().count() > limit {
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

/// Fit an edit into `limit` characters. Over the limit, cut at the last
/// newline that leaves room and end with a line pointing to the page.
pub fn truncate_edit(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let suffix = format!("\n{MORE_LINE}");
    let budget = limit.saturating_sub(suffix.chars().count());
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
    fn exact_boundary_is_one_piece() {
        let text = "a".repeat(10);
        assert_eq!(split_message(&text, 10), vec![text]);
    }

    #[test]
    fn one_over_the_boundary_splits() {
        let text = "a".repeat(11);
        assert_eq!(split_message(&text, 10), vec!["a".repeat(10), "a".into()]);
    }

    #[test]
    fn breaks_on_the_last_newline() {
        assert_eq!(
            split_message("aaa bbb\nccc ddd\neee", 12),
            vec!["aaa bbb", "ccc ddd\neee"]
        );
    }

    #[test]
    fn breaks_on_the_last_space_without_a_newline() {
        assert_eq!(
            split_message("aaaa bbbb cccc", 12),
            vec!["aaaa bbbb", "cccc"]
        );
    }

    #[test]
    fn breaks_hard_on_a_long_word() {
        assert_eq!(
            split_message("abcdefghijklmno", 6),
            vec!["abcdef", "ghijkl", "mno"]
        );
    }

    #[test]
    fn counts_chars_not_bytes() {
        let text = "é".repeat(7);
        assert_eq!(split_message(&text, 5), vec!["é".repeat(5), "é".repeat(2)]);
    }

    #[test]
    fn never_yields_an_empty_piece() {
        for text in ["", "\n\n\n\n\n\n", "      ", "ab\n\n\n\n\ncd", "\nabcdefgh"] {
            for piece in split_message(text, 3) {
                assert!(!piece.is_empty(), "{text:?}");
                assert!(piece.chars().count() <= 3);
            }
        }
        assert!(split_message("", 5).is_empty());
    }

    #[test]
    fn every_piece_fits() {
        let text = "word ".repeat(1000);
        for piece in split_message(&text, MESSAGE_LIMIT) {
            assert!(piece.chars().count() <= MESSAGE_LIMIT);
        }
    }

    #[test]
    fn the_limit_is_discords() {
        assert_eq!(MESSAGE_LIMIT, 2000);
        let text = "a".repeat(2001);
        let pieces = split_message(&text, MESSAGE_LIMIT);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].chars().count(), 2000);
    }

    #[test]
    fn short_edit_is_unchanged() {
        assert_eq!(truncate_edit("hi", 2000), "hi");
    }

    #[test]
    fn long_edit_cuts_at_a_newline_and_points_to_the_page() {
        let text = format!("{}\n{}", "a".repeat(1000), "b".repeat(1500));
        let out = truncate_edit(&text, 2000);
        assert_eq!(out, format!("{}\n(more on the page)", "a".repeat(1000)));
        assert!(out.chars().count() <= 2000);
    }

    #[test]
    fn long_edit_without_a_newline_still_fits() {
        let out = truncate_edit(&"é".repeat(3000), 2000);
        assert!(out.chars().count() <= 2000);
        assert!(out.ends_with("\n(more on the page)"));
    }
}
