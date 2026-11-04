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

    let authenticated = authenticated(&message, receiver, &from);
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
        authenticated,
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
/// the receiver vouches for the From domain. Headers whose authserv-id is
/// not the receiver may come from the sender and are ignored; with no
/// receiver nothing is trusted.
fn authenticated(message: &Message<'_>, receiver: Option<&str>, from: &str) -> bool {
    let Some(receiver) = receiver.map(str::trim).filter(|r| !r.is_empty()) else {
        return false;
    };
    let Some((_, from_domain)) = from.rsplit_once('@') else {
        return false;
    };
    message
        .headers()
        .all("Authentication-Results")
        .map(|h| tokens(&String::from_utf8_lossy(h.raw_value())))
        .find(|tokens| authserv_id(tokens).is_some_and(|id| id.eq_ignore_ascii_case(receiver)))
        .and_then(|tokens| results(&tokens))
        .is_some_and(|results| vouches(&results, from_domain))
}

/// A dmarc=pass for the From domain is enough, and any dmarc verdict other
/// than pass or none refuses the mail. Otherwise a dkim or spf pass must
/// align with the From domain.
fn vouches(results: &[AuthResult], from_domain: &str) -> bool {
    let dmarc: Vec<&AuthResult> = results.iter().filter(|r| r.method == "dmarc").collect();
    if dmarc
        .iter()
        .any(|r| r.result != "pass" && r.result != "none")
    {
        return false;
    }
    let dmarc_pass = dmarc.iter().any(|r| {
        r.result == "pass"
            && r.prop("header.from")
                .is_some_and(|d| same_domain(d, from_domain))
    });
    dmarc_pass
        || results.iter().filter(|r| r.result == "pass").any(|r| {
            let props: &[&str] = match r.method.as_str() {
                "dkim" => &["header.d", "header.i"],
                "spf" => &["smtp.mailfrom"],
                _ => &[],
            };
            props
                .iter()
                .filter_map(|p| r.prop(p))
                .any(|v| aligned(domain_of(v), from_domain))
        })
}

/// The part after the last '@', or all of it when there is none.
fn domain_of(value: &str) -> &str {
    value.rsplit_once('@').map_or(value, |(_, domain)| domain)
}

fn same_domain(a: &str, b: &str) -> bool {
    a.trim_end_matches('.')
        .eq_ignore_ascii_case(b.trim_end_matches('.'))
}

/// The same domain, or one a subdomain of the other. Without a public
/// suffix list this stands in for DMARC's relaxed alignment.
fn aligned(domain: &str, from_domain: &str) -> bool {
    let a = domain.trim_end_matches('.').to_ascii_lowercase();
    let b = from_domain.trim_end_matches('.').to_ascii_lowercase();
    !a.is_empty()
        && !b.is_empty()
        && (a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Eq,
    Semi,
    /// A quoted string or comment that is never closed.
    Open,
}

/// Splits an Authentication-Results value into words, '=' and ';'. A
/// quoted string joins the word it sits in, so a ';' or '(' inside it is
/// plain text. Comments are dropped.
fn tokens(value: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        let next = match c {
            '"' => {
                if quoted(&mut chars, &mut word) {
                    continue;
                }
                Some(Token::Open)
            }
            '(' => (!comment(&mut chars)).then_some(Token::Open),
            ';' => Some(Token::Semi),
            '=' => Some(Token::Eq),
            c if c.is_whitespace() => None,
            c => {
                word.push(c);
                continue;
            }
        };
        if !word.is_empty() {
            out.push(Token::Word(std::mem::take(&mut word)));
        }
        out.extend(next);
    }
    if !word.is_empty() {
        out.push(Token::Word(word));
    }
    out
}

/// Reads a quoted string, after its opening quote, into `word`. False when
/// it is never closed.
fn quoted(chars: &mut std::str::Chars<'_>, word: &mut String) -> bool {
    while let Some(c) = chars.next() {
        match c {
            '"' => return true,
            '\\' => word.extend(chars.next()),
            c => word.push(c),
        }
    }
    false
}

/// Skips a comment, nested ones included, after its opening '('. False
/// when it is never closed.
fn comment(chars: &mut std::str::Chars<'_>) -> bool {
    let mut depth = 1usize;
    while let Some(c) = chars.next() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            '\\' => {
                chars.next();
            }
            _ => {}
        }
    }
    false
}

/// The authserv-id: the first word.
fn authserv_id(tokens: &[Token]) -> Option<&str> {
    match tokens.first() {
        Some(Token::Word(id)) => Some(id),
        _ => None,
    }
}

/// The results after the authserv-id. None at all when a quote or comment
/// is left open, since the rest of the header cannot be read.
fn results(tokens: &[Token]) -> Option<Vec<AuthResult>> {
    if tokens.contains(&Token::Open) {
        return None;
    }
    Some(
        tokens
            .split(|t| *t == Token::Semi)
            .skip(1)
            .filter_map(AuthResult::read)
            .collect(),
    )
}

/// One `method=result` and the `name=value` properties after it. The
/// method, result and names are lowercased.
#[derive(Debug)]
struct AuthResult {
    method: String,
    result: String,
    props: Vec<(String, String)>,
}

impl AuthResult {
    fn read(segment: &[Token]) -> Option<Self> {
        let [
            Token::Word(method),
            Token::Eq,
            Token::Word(result),
            rest @ ..,
        ] = segment
        else {
            return None;
        };
        let mut rest = rest;
        let mut props = Vec::new();
        while !rest.is_empty() {
            if let [Token::Word(name), Token::Eq, Token::Word(value), tail @ ..] = rest {
                props.push((name.to_ascii_lowercase(), value.clone()));
                rest = tail;
            } else {
                rest = &rest[1..];
            }
        }
        Some(Self {
            method: method.to_ascii_lowercase(),
            result: result.to_ascii_lowercase(),
            props,
        })
    }

    fn prop(&self, name: &str) -> Option<&str> {
        self.props
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
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
    fn the_receivers_dmarc_pass_is_authenticated() {
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

    const TAIL: &str = "From: a@venue.example\nSubject: Hi\n\nHello\n";

    /// Passes that align with the From domain of `TAIL`.
    const ALIGNED: &str = "dkim=pass header.d=venue.example; spf=pass smtp.mailfrom=venue.example";

    /// Whether `TAIL` counts as authenticated under one receiver header.
    fn authenticated_with(results: &str) -> bool {
        parse(&format!("Authentication-Results: {results}\n{TAIL}")).authenticated
    }

    #[test]
    fn an_aligned_dkim_pass_is_enough_without_dmarc() {
        assert!(authenticated_with(
            "mx.example.org; dkim=pass header.d=venue.example; spf=fail"
        ));
    }

    #[test]
    fn an_spf_pass_for_a_subdomain_aligns() {
        assert!(authenticated_with(
            "mx.example.org; spf=pass smtp.mailfrom=bounce@mail.venue.example"
        ));
    }

    #[test]
    fn a_dkim_identity_aligns_and_padding_in_a_value_is_harmless() {
        assert!(authenticated_with(
            "mx.example.org; dkim=pass header.i=@venue.example header.s=s1 header.b=Ab+/cd==; \
             spf=neutral"
        ));
    }

    #[test]
    fn passes_for_other_domains_are_unverified() {
        assert!(!authenticated_with("mx.example.org; dkim=pass; spf=pass"));
        assert!(!authenticated_with(
            "mx.example.org; dkim=pass header.d=mailer.example; spf=pass smtp.mailfrom=mailer.example"
        ));
        assert!(!authenticated_with(
            "mx.example.org; dkim=pass header.d=notvenue.example"
        ));
        assert!(!authenticated_with(
            "mx.example.org; dkim=pass header.d=venue.example.attacker.example"
        ));
    }

    #[test]
    fn a_forged_from_with_attacker_aligned_passes_is_unverified() {
        assert!(!authenticated_with(
            "mx.example.org; spf=pass smtp.mailfrom=attacker.example; \
             dkim=pass header.d=attacker.example; dmarc=fail (p=none) header.from=venue.example"
        ));
    }

    #[test]
    fn a_dmarc_fail_outweighs_an_aligned_pass() {
        assert!(!authenticated_with(&format!(
            "mx.example.org; {ALIGNED}; dmarc=fail header.from=venue.example"
        )));
    }

    #[test]
    fn a_dmarc_pass_for_another_from_domain_does_not_count() {
        assert!(!authenticated_with(
            "mx.example.org; dkim=pass header.d=attacker.example; \
             dmarc=pass header.from=attacker.example"
        ));
    }

    #[test]
    fn a_quoted_value_cannot_inject_results() {
        assert!(!authenticated_with(
            "mx.example.org; spf=fail smtp.mailfrom=\"x; dkim=pass header.d=venue.example; \
             spf=pass smtp.mailfrom=venue.example\"@attacker.example; dkim=none"
        ));
        let read = results(&tokens(
            "mx.example.org; spf=fail smtp.mailfrom=\"a; dkim=pass; spf=pass\"@x.example",
        ))
        .unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(
            (read[0].method.as_str(), read[0].result.as_str()),
            ("spf", "fail")
        );
        assert_eq!(
            read[0].prop("smtp.mailfrom"),
            Some("a; dkim=pass; spf=pass@x.example")
        );
    }

    #[test]
    fn parentheses_inside_quotes_are_not_a_comment() {
        // Read as a comment, the "(" would hide the dmarc=fail up to the last ")".
        assert!(!authenticated_with(
            "mx.example.org; dkim=pass header.d=venue.example header.s=\"s(1\"; \
             dmarc=fail header.from=venue.example; x=y z=\")\""
        ));
        assert!(authenticated_with(
            "mx.example.org; dkim=pass header.d=venue.example header.s=\"s(1\"; x=y z=\")\""
        ));
    }

    #[test]
    fn an_unclosed_quote_fails_the_whole_header() {
        assert!(!authenticated_with(&format!(
            "mx.example.org; {ALIGNED} x=\"open"
        )));
        // A receiver header further down does not stand in for the broken one.
        let m = parse(&format!(
            "Authentication-Results: mx.example.org; {ALIGNED} x=\"open\n\
             Authentication-Results: mx.example.org; {ALIGNED}\n{TAIL}"
        ));
        assert!(!m.authenticated);
    }

    #[test]
    fn a_forged_header_alone_is_unverified() {
        assert!(!authenticated_with(&format!("evil.example; {ALIGNED}")));
    }

    #[test]
    fn the_receivers_header_passes() {
        assert!(authenticated_with(&format!("MX.example.org; {ALIGNED}")));
    }

    #[test]
    fn a_receiver_header_below_a_forged_one_passes() {
        let m = parse(&format!(
            "Authentication-Results: evil.example; {ALIGNED}\n\
             Authentication-Results: mx.example.org; {ALIGNED}\n{TAIL}"
        ));
        assert!(m.authenticated);
    }

    #[test]
    fn a_failing_receiver_header_is_not_overridden_by_a_forged_one() {
        let m = parse(&format!(
            "Authentication-Results: mx.example.org; dkim=none; spf=fail\n\
             Authentication-Results: evil.example; {ALIGNED}\n{TAIL}"
        ));
        assert!(!m.authenticated);
    }

    #[test]
    fn a_configured_authserv_id_differing_from_the_host_works() {
        let eml = format!("Authentication-Results: mx.google.com; {ALIGNED}\n{TAIL}")
            .replace('\n', "\r\n");
        let ok = parse_eml(eml.as_bytes(), now(), Some("mx.google.com")).unwrap();
        assert!(ok.authenticated);
        let host_only = parse_eml(eml.as_bytes(), now(), Some("imap.gmail.com")).unwrap();
        assert!(!host_only.authenticated);
    }

    #[test]
    fn no_receiver_means_unverified() {
        let eml = format!("Authentication-Results: mx.example.org; {ALIGNED}\n{TAIL}")
            .replace('\n', "\r\n");
        assert!(
            !parse_eml(eml.as_bytes(), now(), None)
                .unwrap()
                .authenticated
        );
    }

    #[test]
    fn a_pass_inside_a_comment_does_not_count() {
        assert!(!authenticated_with(
            "mx.example.org; dkim=fail (was; dkim=pass header.d=venue.example) \
             header.d=venue.example; spf=softfail smtp.mailfrom=venue.example"
        ));
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
