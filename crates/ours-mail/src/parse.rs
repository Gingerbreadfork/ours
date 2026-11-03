//! Turning raw RFC 5322 bytes into an `InboundMail`. Pure: the only clock
//! is the `now` the caller passes for messages without a usable date.

use crate::error::MailError;
use mail_parser::{Message, MessageParser};
use ours_types::{InboundMail, Instant};

/// More than this many ids in In-Reply-To and References are ignored.
const MAX_REFERENCES: usize = 50;

/// Parse one message. The sender address is lowercased; compare it with
/// addresses the group wrote to in the same way. Message ids carry no
/// angle brackets. A missing, invalid or future Date becomes `now`.
pub fn parse_eml(
    bytes: &[u8],
    now: Instant,
    receiver: Option<&str>,
) -> Result<InboundMail, MailError> {
    let message = MessageParser::new()
        .parse(bytes)
        .ok_or_else(|| MailError::Parse("not a mail message".to_owned()))?;

    let sender = message
        .from()
        .and_then(|list| list.mailboxes().find(|m| m.address().is_some()))
        .ok_or_else(|| MailError::Parse("no From address".to_owned()))?;
    let from = sender
        .address()
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| MailError::Parse("empty From address".to_owned()))?;
    let from_name = sender
        .name()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());

    let body = message
        .body_text(0)
        .map(|text| plain_body(&text))
        .unwrap_or_default();

    Ok(InboundMail {
        from,
        from_name,
        subject: message
            .subject()
            .map(|s| s.trim().to_owned())
            .unwrap_or_default(),
        body,
        at: message_date(&message).map_or(now, |at| at.min(now)),
        message_id: message
            .message_id()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty()),
        in_reply_to: reply_ids(&message),
        authenticated: authenticated(&message, receiver),
    })
}

fn message_date(message: &Message<'_>) -> Option<Instant> {
    let date = message.date().filter(|d| d.is_valid())?;
    Instant::from_millis(date.to_timestamp().checked_mul(1000)?)
}

/// In-Reply-To first, then References from the newest id back to the
/// oldest, without repeats.
fn reply_ids(message: &Message<'_>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut add = |id: &str| {
        let id = id.trim();
        if !id.is_empty() && !ids.iter().any(|seen| seen == id) {
            ids.push(id.to_owned());
        }
    };
    if let Some(list) = message.in_reply_to() {
        list.iter().rev().for_each(&mut add);
    }
    if let Some(list) = message.references() {
        list.iter().rev().for_each(&mut add);
    }
    ids.truncate(MAX_REFERENCES);
    ids
}

/// True only when the topmost Authentication-Results header written by
/// the receiver reports both dkim=pass and spf=pass. Headers whose
/// authserv-id is not the receiver may come from the sender and are
/// ignored; with no receiver nothing is trusted.
fn authenticated(message: &Message<'_>, receiver: Option<&str>) -> bool {
    let Some(receiver) = receiver.map(str::trim).filter(|r| !r.is_empty()) else {
        return false;
    };
    message
        .headers()
        .all("Authentication-Results")
        .map(|h| String::from_utf8_lossy(h.raw_value()).into_owned())
        .find(|value| authserv_id(value).is_some_and(|id| id.eq_ignore_ascii_case(receiver)))
        .is_some_and(|value| results_pass(&value))
}

/// The authserv-id: the first token before the first ';'.
fn authserv_id(value: &str) -> Option<String> {
    let value = strip_comments(value);
    let head = value.split(';').next()?;
    head.split_whitespace().next().map(str::to_owned)
}

/// Reads `method=result` pairs from an Authentication-Results value.
fn results_pass(value: &str) -> bool {
    let value = strip_comments(value);
    let mut dkim = false;
    let mut spf = false;
    // The first segment is the server's identity; each later one is a result.
    for part in value.split(';').skip(1) {
        let Some(first) = part.split_whitespace().next() else {
            continue;
        };
        let Some((method, result)) = first.split_once('=') else {
            continue;
        };
        if !result.eq_ignore_ascii_case("pass") {
            continue;
        }
        match method.to_ascii_lowercase().as_str() {
            "dkim" => dkim = true,
            "spf" => spf = true,
            _ => {}
        }
    }
    dkim && spf
}

fn strip_comments(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    for c in value.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// The text body with line endings made plain and quoted replies removed.
/// When nothing is left after stripping, the whole text is returned.
fn plain_body(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let stripped = strip_quoted(&text);
    if stripped.is_empty() {
        text.trim().to_owned()
    } else {
        stripped
    }
}

const ATTRIBUTION_ENDINGS: [&str; 6] = [
    "wrote:",
    "wrote :",
    "a écrit :",
    "schrieb:",
    "escribió:",
    "ha scritto:",
];

fn strip_quoted(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![true; lines.len()];

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if is_original_message_marker(trimmed) {
            keep[i..].fill(false);
            break;
        }
        if trimmed.starts_with('>') {
            keep[i] = false;
            continue;
        }
        if is_attribution_end(trimmed) && next_non_blank_is_quote(&lines, i) {
            keep[i] = false;
            // The attribution may wrap onto a second line.
            if i > 0 && lines[i - 1].trim_start().starts_with("On ") {
                keep[i - 1] = false;
            }
        }
    }

    let mut out: Vec<&str> = Vec::new();
    for (line, keep) in lines.iter().zip(&keep) {
        if !keep {
            continue;
        }
        let line = line.trim_end();
        if line.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

fn is_original_message_marker(line: &str) -> bool {
    let inner = line.trim_matches('-').trim();
    line.starts_with("--") && inner.eq_ignore_ascii_case("original message")
}

fn is_attribution_end(line: &str) -> bool {
    let lower = line.to_lowercase();
    ATTRIBUTION_ENDINGS.iter().any(|end| lower.ends_with(end))
}

fn next_non_blank_is_quote(lines: &[&str], from: usize) -> bool {
    lines[from + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .is_some_and(|l| l.starts_with('>'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIVER: &str = "mx.example.org";

    fn now() -> Instant {
        Instant::from_millis(1_800_000_000_000).unwrap()
    }

    fn parse(text: &str) -> InboundMail {
        parse_eml(text.replace('\n', "\r\n").as_bytes(), now(), Some(RECEIVER)).unwrap()
    }

    #[test]
    fn plain_reply_with_threading_headers() {
        let m = parse(
            "From: Priya Rao <Priya@Venue.example>\n\
             To: q@example.org\n\
             Subject: Re: Table for six\n\
             Date: Mon, 02 Nov 2026 18:30:00 +0000\n\
             Message-ID: <reply-3@venue.example>\n\
             In-Reply-To: <mine-2@example.org>\n\
             References: <mine-1@example.org> <theirs-1@venue.example> <mine-2@example.org>\n\
             \n\
             Thursday at 7:30 is fine.\n",
        );
        assert_eq!(m.from, "priya@venue.example");
        assert_eq!(m.from_name.as_deref(), Some("Priya Rao"));
        assert_eq!(m.subject, "Re: Table for six");
        assert_eq!(m.body, "Thursday at 7:30 is fine.");
        assert_eq!(m.message_id.as_deref(), Some("reply-3@venue.example"));
        assert_eq!(
            m.in_reply_to,
            [
                "mine-2@example.org",
                "theirs-1@venue.example",
                "mine-1@example.org"
            ]
        );
        assert_eq!(m.at.as_millis(), 1_793_644_200_000);
        assert!(!m.authenticated);
    }

    #[test]
    fn multipart_prefers_the_text_part() {
        let m = parse(
            "From: a@x.example\n\
             Subject: Both\n\
             MIME-Version: 1.0\n\
             Content-Type: multipart/alternative; boundary=\"b1\"\n\
             \n\
             --b1\n\
             Content-Type: text/plain; charset=utf-8\n\
             \n\
             Plain words.\n\
             --b1\n\
             Content-Type: text/html; charset=utf-8\n\
             \n\
             <p>Html <b>words</b>.</p>\n\
             --b1--\n",
        );
        assert_eq!(m.body, "Plain words.");
    }

    #[test]
    fn html_only_message_is_converted_to_text() {
        let m = parse(
            "From: a@x.example\n\
             Subject: Html\n\
             Content-Type: text/html; charset=utf-8\n\
             \n\
             <p>Booked for <b>Thursday</b>.</p>\n",
        );
        assert!(m.body.contains("Booked for"));
        assert!(m.body.contains("Thursday"));
        assert!(!m.body.contains('<'));
    }

    #[test]
    fn dkim_and_spf_pass_is_authenticated() {
        let m = parse(
            "Authentication-Results: mx.example.org (Postfix);\n \
             spf=pass (sender ok) smtp.mailfrom=venue.example;\n \
             dkim=pass header.d=venue.example header.s=s1;\n \
             dmarc=pass header.from=venue.example\n\
             From: a@venue.example\n\
             Subject: Hi\n\
             \n\
             Hello\n",
        );
        assert!(m.authenticated);
    }

    #[test]
    fn dkim_alone_is_not_authenticated() {
        let m = parse(
            "Authentication-Results: mx.example.org; dkim=pass header.d=venue.example; spf=fail\n\
             From: a@venue.example\n\
             Subject: Hi\n\
             \n\
             Hello\n",
        );
        assert!(!m.authenticated);
    }

    const TAIL: &str = "From: a@venue.example\nSubject: Hi\n\nHello\n";

    #[test]
    fn a_forged_header_alone_is_unverified() {
        let m = parse(&format!(
            "Authentication-Results: evil.example; dkim=pass; spf=pass\n{TAIL}"
        ));
        assert!(!m.authenticated);
    }

    #[test]
    fn the_receivers_header_passes() {
        let m = parse(&format!(
            "Authentication-Results: MX.example.org; dkim=pass; spf=pass\n{TAIL}"
        ));
        assert!(m.authenticated);
    }

    #[test]
    fn a_receiver_header_below_a_forged_one_passes() {
        let m = parse(&format!(
            "Authentication-Results: evil.example; dkim=pass; spf=pass\n\
             Authentication-Results: mx.example.org; dkim=pass; spf=pass\n{TAIL}"
        ));
        assert!(m.authenticated);
    }

    #[test]
    fn a_failing_receiver_header_is_not_overridden_by_a_forged_one() {
        let m = parse(&format!(
            "Authentication-Results: mx.example.org; dkim=none; spf=fail\n\
             Authentication-Results: evil.example; dkim=pass; spf=pass\n{TAIL}"
        ));
        assert!(!m.authenticated);
    }

    #[test]
    fn a_configured_authserv_id_differing_from_the_host_works() {
        let eml = format!("Authentication-Results: mx.google.com; dkim=pass; spf=pass\n{TAIL}")
            .replace('\n', "\r\n");
        let ok = parse_eml(eml.as_bytes(), now(), Some("mx.google.com")).unwrap();
        assert!(ok.authenticated);
        let host_only = parse_eml(eml.as_bytes(), now(), Some("imap.gmail.com")).unwrap();
        assert!(!host_only.authenticated);
    }

    #[test]
    fn no_receiver_means_unverified() {
        let eml = format!("Authentication-Results: mx.example.org; dkim=pass; spf=pass\n{TAIL}")
            .replace('\n', "\r\n");
        assert!(
            !parse_eml(eml.as_bytes(), now(), None)
                .unwrap()
                .authenticated
        );
    }

    #[test]
    fn a_pass_word_inside_a_comment_does_not_count() {
        let m = parse(
            "Authentication-Results: mx.example.org; dkim=fail (was dkim=pass); spf=pass\n\
             From: a@venue.example\n\
             Subject: Hi\n\
             \n\
             Hello\n",
        );
        assert!(!m.authenticated);
    }

    #[test]
    fn quoted_reply_block_is_stripped() {
        let m = parse(
            "From: a@venue.example\n\
             Subject: Re: Table\n\
             \n\
             Yes, 7:30 works.\n\
             \n\
             On Mon, 2 Nov 2026 at 18:00, Q for The Six <q@example.org>\n\
             wrote:\n\
             > Can we have a table for six on Thursday?\n\
             > Around 7:30.\n\
             >\n\
             > Thanks\n",
        );
        assert_eq!(m.body, "Yes, 7:30 works.");
    }

    #[test]
    fn interleaved_answers_are_kept() {
        let m = parse(
            "From: a@venue.example\n\
             Subject: Re: Table\n\
             \n\
             On Mon, Q wrote:\n\
             > Thursday?\n\
             Yes.\n\
             > Seven thirty?\n\
             Also yes.\n",
        );
        assert_eq!(m.body, "Yes.\nAlso yes.");
    }

    #[test]
    fn original_message_marker_cuts_the_rest() {
        let m = parse(
            "From: a@venue.example\n\
             Subject: Re: Table\n\
             \n\
             Confirmed.\n\
             \n\
             -----Original Message-----\n\
             From: Q\n\
             Sent: Monday\n\
             Can we have a table?\n",
        );
        assert_eq!(m.body, "Confirmed.");
    }

    #[test]
    fn a_body_that_is_all_quote_is_kept_whole() {
        let m = parse(
            "From: a@venue.example\n\
             Subject: Fwd\n\
             \n\
             > the entire message\n",
        );
        assert_eq!(m.body, "> the entire message");
    }

    #[test]
    fn missing_date_uses_now() {
        let m = parse("From: a@venue.example\nSubject: No date\n\nHi\n");
        assert_eq!(m.at, now());
        assert!(m.message_id.is_none());
        assert!(m.in_reply_to.is_empty());
    }

    #[test]
    fn a_future_date_is_clamped_to_now() {
        let m = parse("From: a@venue.example\nDate: Sat, 01 Jan 2050 00:00:00 +0000\n\nHi\n");
        assert_eq!(m.at, now());
    }

    #[test]
    fn missing_from_is_a_parse_error() {
        let err = parse_eml(b"Subject: Hi\r\n\r\nHello\r\n", now(), Some(RECEIVER)).unwrap_err();
        assert!(matches!(err, MailError::Parse(_)));
    }

    #[test]
    fn encoded_headers_are_decoded() {
        let m = parse(
            "From: =?utf-8?B?SsO8cmdlbg==?= <j@x.example>\n\
             Subject: =?utf-8?Q?Caf=C3=A9_Thursday?=\n\
             \n\
             Hi\n",
        );
        assert_eq!(m.from_name.as_deref(), Some("Jürgen"));
        assert_eq!(m.subject, "Café Thursday");
    }
}
