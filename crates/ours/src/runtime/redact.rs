//! Scrubbing forgotten words out of the journal. A redacted record keeps
//! its place and hash; only the words and attachments are gone.

use crate::runtime::journal::{Journal, JournalError};
use ours_core::{GroupState, Input};
use std::cell::RefCell;
use std::collections::BTreeSet;

pub const FORGOTTEN: &str = "[forgotten]";

/// Shorter texts are scrubbed only where they sit as a message; quoting
/// them elsewhere cannot be told from ordinary words.
const QUOTE_MIN_CHARS: usize = 16;

/// Rewrites every logged message whose id is in `messages` or whose author
/// is `user`, then every other record that quotes those words (the
/// extractor's drafts, the model's plans), drops the snapshots that might
/// still hold them, and returns how many records changed.
pub fn redact_journal(
    journal: &Journal<Input, GroupState>,
    messages: &BTreeSet<String>,
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

fn is_target(
    object: &serde_json::Map<String, serde_json::Value>,
    messages: &BTreeSet<String>,
    user: Option<&str>,
) -> bool {
    let id = object.get("id").and_then(|v| v.as_str());
    let author = object.get("author").and_then(|v| v.as_str());
    object
        .get("text")
        .and_then(|t| t.as_str())
        .is_some_and(|t| t != FORGOTTEN)
        && (id.is_some_and(|i| messages.contains(i)) || (author.is_some() && author == user))
}

/// The texts of every matching message object in a logged input.
fn collect(
    value: &serde_json::Value,
    messages: &BTreeSet<String>,
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
fn scrub(value: &mut serde_json::Value, messages: &BTreeSet<String>, user: Option<&str>) -> bool {
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

    #[test]
    fn matching_messages_lose_their_text_and_nothing_else_changes() {
        let mut value = serde_json::json!({"inbound": {"message": {
            "id": "m1", "author": "sam", "text": "I'm vegan", "attachments": [{"name": "x"}], "at": 5}}});
        let ids: BTreeSet<String> = ["m1".to_owned()].into();
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
        let mut value =
            serde_json::json!({"inbound": {"message": {"id": "m9", "author": "jo", "text": "hi"}}});
        assert!(!scrub(&mut value, &BTreeSet::new(), Some("sam")));
        assert!(scrub(&mut value, &BTreeSet::new(), Some("jo")));
        assert_eq!(value["inbound"]["message"]["text"], FORGOTTEN);
    }

    #[test]
    fn quotes_of_the_words_go_too_but_short_texts_only_as_messages() {
        let long = "I have been vegan since the spring of 2024";
        let message = serde_json::json!({"inbound": {"message": {"id": "m1", "author": "sam", "text": long}}});
        let ids: BTreeSet<String> = ["m1".to_owned()].into();
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
        let short = serde_json::json!({"inbound": {"message": {"id": "m2", "author": "sam", "text": "yes"}}});
        let mut none = Vec::new();
        collect(&short, &["m2".to_owned()].into(), None, &mut none);
        assert!(none.is_empty(), "three letters are not a quote to hunt for");
    }
}
