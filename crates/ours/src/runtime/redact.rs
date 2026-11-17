//! Scrubbing forgotten words out of the journal. A redacted record keeps
//! its place and hash; only the words and attachments are gone.

use crate::runtime::journal::{Journal, JournalError};
use ours_core::{GroupState, Input};
use ours_types::{Extraction, PlatformMessageId, PlatformRoomId, TimeMeaning, TimeResolution};
use std::cell::RefCell;
use std::collections::BTreeSet;

pub const FORGOTTEN: &str = "[forgotten]";

/// Shorter texts are scrubbed only where they sit as a message; quoting
/// them elsewhere cannot be told from ordinary words.
const QUOTE_MIN_CHARS: usize = 16;

/// A message to scrub: its room and its id. An id alone is not enough,
/// because some platforms number messages per room.
pub type Target = (String, String);

/// Rewrites every logged message in `messages` or whose author is `user`,
/// then every other record that quotes those words (the extractor's
/// drafts, the model's plans), drops the snapshots that might still hold
/// them, and returns how many records changed.
pub fn redact_journal(
    journal: &Journal<Input, GroupState>,
    messages: &BTreeSet<Target>,
    user: Option<&str>,
) -> Result<u64, JournalError> {
    // First pass: find the words before they go.
    let words: RefCell<Vec<String>> = RefCell::new(Vec::new());
    journal.redact(&|_, _, payload| {
        let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
        collect(&value, messages, user, &mut words.borrow_mut());
        None
    })?;
    let words = words.into_inner();
    let changed = journal.redact(&|_, _, payload| {
        let mut value: serde_json::Value = serde_json::from_slice(payload).ok()?;
        let hit = scrub(&mut value, messages, user) | scrub_quotes(&mut value, &words);
        hit.then(|| serde_json::to_vec(&value).ok())?
    })?;
    if changed > 0
        && let Some(seq) = journal.earliest_redacted()?
    {
        journal.drop_snapshots_from(seq)?;
    }
    Ok(changed)
}

/// An answer about a message `gone` says was redacted while the answer
/// was on its way, emptied so the journal never gains the words it quotes.
/// Every other input passes unchanged.
pub fn without_forgotten(
    input: Input,
    gone: impl Fn(&PlatformRoomId, &PlatformMessageId) -> bool,
) -> Input {
    match input {
        Input::Extracted { room, message, .. } if gone(&room, &message) => Input::Extracted {
            room,
            message,
            extraction: Extraction::default(),
        },
        Input::TimeResolved(resolution) if gone(&resolution.room, &resolution.message) => {
            Input::TimeResolved(TimeResolution {
                phrase: None,
                meaning: TimeMeaning::None,
                echo: None,
                ..resolution
            })
        }
        other => other,
    }
}

fn is_target(
    object: &serde_json::Map<String, serde_json::Value>,
    messages: &BTreeSet<Target>,
    user: Option<&str>,
) -> bool {
    let field = |name: &str| object.get(name).and_then(|v| v.as_str());
    let named = match (field("room"), field("id")) {
        (Some(room), Some(id)) => messages.contains(&(room.to_owned(), id.to_owned())),
        _ => false,
    };
    let author = field("author");
    field("text").is_some_and(|t| t != FORGOTTEN) && (named || (author.is_some() && author == user))
}

/// The texts of every matching message object in a logged input.
fn collect(
    value: &serde_json::Value,
    messages: &BTreeSet<Target>,
    user: Option<&str>,
    out: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(object) => {
            if is_target(object, messages, user)
                && let Some(text) = object.get("text").and_then(|t| t.as_str())
                && text.chars().count() >= QUOTE_MIN_CHARS
                && !out.iter().any(|w| w == text)
            {
                out.push(text.to_owned());
            }
            for child in object.values() {
                collect(child, messages, user, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect(item, messages, user, out);
            }
        }
        _ => {}
    }
}

/// Walks a logged input and blanks the text of any message object that
/// matches. True when something changed.
fn scrub(value: &mut serde_json::Value, messages: &BTreeSet<Target>, user: Option<&str>) -> bool {
    let mut changed = false;
    match value {
        serde_json::Value::Object(object) => {
            if is_target(object, messages, user) {
                object.insert(
                    "text".to_owned(),
                    serde_json::Value::String(FORGOTTEN.to_owned()),
                );
                if object.contains_key("attachments") {
                    object.insert(
                        "attachments".to_owned(),
                        serde_json::Value::Array(Vec::new()),
                    );
                }
                changed = true;
            }
            for child in object.values_mut() {
                changed |= scrub(child, messages, user);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                changed |= scrub(item, messages, user);
            }
        }
        _ => {}
    }
    changed
}

/// Blanks any string that quotes one of the forgotten texts, or a long
/// enough prefix of one, wherever it sits in the record.
fn scrub_quotes(value: &mut serde_json::Value, words: &[String]) -> bool {
    if words.is_empty() {
        return false;
    }
    let mut changed = false;
    match value {
        serde_json::Value::String(s) => {
            if s.as_str() != FORGOTTEN && quotes_any(s, words) {
                *s = FORGOTTEN.to_owned();
                changed = true;
            }
        }
        serde_json::Value::Object(object) => {
            for child in object.values_mut() {
                changed |= scrub_quotes(child, words);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                changed |= scrub_quotes(item, words);
            }
        }
        _ => {}
    }
    changed
}

fn quotes_any(s: &str, words: &[String]) -> bool {
    words.iter().any(|w| {
        let head: String = w.chars().take(QUOTE_MIN_CHARS).collect();
        s.contains(w.as_str()) || s.contains(head.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(pairs: &[(&str, &str)]) -> BTreeSet<Target> {
        pairs
            .iter()
            .map(|(room, id)| ((*room).to_owned(), (*id).to_owned()))
            .collect()
    }

    #[test]
    fn a_late_answer_about_a_forgotten_message_is_emptied() {
        let gone = |room: &PlatformRoomId, message: &PlatformMessageId| {
            room.as_str() == "a" && message.as_str() == "42"
        };
        let extracted = |room: &str| Input::Extracted {
            room: room.into(),
            message: "42".into(),
            extraction: Extraction {
                facts: vec![ours_types::FactDraft {
                    subject: "Sam".into(),
                    subject_kind: ours_types::SubjectKind::Person,
                    predicate: "lives at".into(),
                    object: "5 Elm Street".into(),
                    about_self: true,
                    stated: true,
                    durable: true,
                    expires: None,
                    starts: None,
                    quote: Some("my new place is 5 Elm Street".into()),
                }],
                ..Extraction::default()
            },
        };
        let emptied = without_forgotten(extracted("a"), gone);
        assert!(
            matches!(&emptied, Input::Extracted { extraction, .. } if extraction == &Extraction::default()),
            "{emptied:?}"
        );
        // The same id in another room is someone else's message.
        assert_eq!(without_forgotten(extracted("b"), gone), extracted("b"));

        let resolved = Input::TimeResolved(TimeResolution {
            room: "a".into(),
            message: "42".into(),
            phrase: Some("my place on thursday".into()),
            meaning: TimeMeaning::Ambiguous {
                candidates: Vec::new(),
            },
            echo: Some("Thursday".into()),
        });
        let Input::TimeResolved(blank) = without_forgotten(resolved, gone) else {
            panic!("still a time resolution");
        };
        assert_eq!(
            (blank.phrase, blank.meaning, blank.echo),
            (None, TimeMeaning::None, None)
        );
    }

    #[test]
    fn the_same_id_in_another_room_is_left_alone() {
        let mut value = serde_json::json!({"inbound": {"history": {"room": "a", "messages": [
            {"id": "42", "room": "a", "author": "sam", "text": "forget this one"},
            {"id": "42", "room": "b", "author": "jo", "text": "keep this one"}]}}});
        assert!(scrub(&mut value, &targets(&[("a", "42")]), None));
        let messages = &value["inbound"]["history"]["messages"];
        assert_eq!(messages[0]["text"], FORGOTTEN);
        assert_eq!(messages[1]["text"], "keep this one");
    }

    #[test]
    fn matching_messages_lose_their_text_and_nothing_else_changes() {
        let mut value = serde_json::json!({"inbound": {"message": {
            "id": "m1", "room": "r", "author": "sam", "text": "I'm vegan", "attachments": [{"name": "x"}], "at": 5}}});
        let ids = targets(&[("r", "m1")]);
        assert!(scrub(&mut value, &ids, None));
        assert_eq!(value["inbound"]["message"]["text"], FORGOTTEN);
        assert_eq!(
            value["inbound"]["message"]["attachments"],
            serde_json::json!([])
        );
        assert_eq!(value["inbound"]["message"]["at"], 5);
        assert!(!scrub(&mut value, &ids, None), "already forgotten");
    }

    #[test]
    fn a_users_messages_match_by_author() {
        let mut value = serde_json::json!({"inbound": {"message": {
            "id": "m9", "room": "r", "author": "jo", "text": "hi"}}});
        assert!(!scrub(&mut value, &BTreeSet::new(), Some("sam")));
        assert!(scrub(&mut value, &BTreeSet::new(), Some("jo")));
        assert_eq!(value["inbound"]["message"]["text"], FORGOTTEN);
    }

    #[test]
    fn quotes_of_the_words_go_too_but_short_texts_only_as_messages() {
        let long = "I have been vegan since the spring of 2024";
        let message = serde_json::json!({"inbound": {"message": {
            "id": "m1", "room": "r", "author": "sam", "text": long}}});
        let ids = targets(&[("r", "m1")]);
        let mut words = Vec::new();
        collect(&message, &ids, None, &mut words);
        assert_eq!(words, vec![long.to_owned()]);
        let mut extracted = serde_json::json!({"extracted": {"message": "m1", "extraction": {"facts": [
            {"subject": "Sam", "predicate": "is", "object": "vegan", "quote": long}]}}});
        assert!(scrub_quotes(&mut extracted, &words));
        assert_eq!(
            extracted["extracted"]["extraction"]["facts"][0]["quote"],
            FORGOTTEN
        );
        assert_eq!(
            extracted["extracted"]["extraction"]["facts"][0]["object"],
            "vegan"
        );
        let short = serde_json::json!({"inbound": {"message": {
            "id": "m2", "room": "r", "author": "sam", "text": "yes"}}});
        let mut none = Vec::new();
        collect(&short, &targets(&[("r", "m2")]), None, &mut none);
        assert!(none.is_empty(), "three letters are not a quote to hunt for");
    }
}
