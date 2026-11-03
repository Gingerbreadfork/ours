//! Long polling: reading raw updates one at a time, and what to do when a
//! `getUpdates` call fails.

use crate::api::ApiError;
use frankenstein::updates::{Update, UpdateContent};
use serde_json::Value;
use std::time::Duration;

/// One update from a batch: its id, which moves the offset on even when
/// the content cannot be read, and the content when it can.
#[derive(Debug)]
pub struct Raw {
    pub id: Option<i64>,
    pub content: Option<UpdateContent>,
}

pub fn decode(value: Value) -> Raw {
    let id = value.get("update_id").and_then(Value::as_i64);
    let content = match serde_json::from_value::<Update>(value) {
        Ok(update) => Some(update.content),
        Err(error) => {
            tracing::warn!(update = ?id, %error, "skipped an update this client cannot read");
            None
        }
    };
    Raw { id, content }
}

/// The offset that confirms everything up to and including `id`.
pub fn next_offset(offset: Option<i64>, id: i64) -> Option<i64> {
    let next = id.saturating_add(1);
    Some(offset.map_or(next, |o| o.max(next)))
}

pub fn kind(update: &UpdateContent) -> &'static str {
    match update {
        UpdateContent::Message(_) => "message",
        UpdateContent::EditedMessage(_) => "edited_message",
        UpdateContent::MessageReaction(_) => "message_reaction",
        UpdateContent::Poll(_) => "poll",
        UpdateContent::PollAnswer(_) => "poll_answer",
        UpdateContent::ChatMember(_) => "chat_member",
        UpdateContent::MyChatMember(_) => "my_chat_member",
        _ => "other",
    }
}

/// Waits that double from `first` up to `most`.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    pub first: Duration,
    pub most: Duration,
}

impl Pacing {
    pub const LIVE: Pacing = Pacing {
        first: Duration::from_secs(1),
        most: Duration::from_secs(60),
    };

    pub fn backoff(self, failures: u32) -> Duration {
        let factor = 2u32.saturating_pow(failures.min(16));
        self.first.saturating_mul(factor).min(self.most)
    }
}

/// What the poll loop does after a failed call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// The token is no good; the connector stops with this reason.
    GiveUp(String),
    /// A webhook is set, which blocks long polling; remove it and go on.
    DropWebhook,
    /// Telegram named how long to wait.
    Wait(Duration),
    /// Anything else: wait longer each time and try again.
    Backoff,
}

/// Why the connector cannot go on with this token.
pub fn token_failure(error: &ApiError) -> Option<String> {
    match error.code() {
        Some(401) => Some(
            "Telegram refused the bot token (401 Unauthorized); it was revoked or mistyped, \
             so copy it again from @BotFather into the config"
                .to_owned(),
        ),
        Some(404) => Some(
            "Telegram does not know the bot token (404 Not Found); copy it again from \
             @BotFather into the config"
                .to_owned(),
        ),
        _ => None,
    }
}

pub fn after(error: &ApiError) -> Next {
    if let Some(reason) = token_failure(error) {
        return Next::GiveUp(reason);
    }
    match error {
        ApiError::Refused {
            code: 409,
            description,
            ..
        } if description.contains("webhook") => Next::DropWebhook,
        ApiError::Refused {
            code: 429,
            retry_after: Some(wait),
            ..
        } => Next::Wait(Duration::from_secs(u64::from(*wait).min(300))),
        _ => Next::Backoff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refused(code: u64, description: &str, retry_after: Option<u16>) -> ApiError {
        ApiError::Refused {
            code,
            description: description.to_owned(),
            retry_after,
            migrate_to: None,
        }
    }

    #[test]
    fn a_401_or_404_ends_the_connector_and_says_why() {
        let Next::GiveUp(reason) = after(&ApiError::Unauthorized("Unauthorized".to_owned())) else {
            panic!()
        };
        assert!(
            reason.contains("token") && reason.contains("@BotFather"),
            "{reason}"
        );
        assert!(matches!(
            after(&refused(404, "Not Found", None)),
            Next::GiveUp(_)
        ));
    }

    #[test]
    fn everything_else_is_retried() {
        assert_eq!(
            after(&refused(
                409,
                "Conflict: can't use getUpdates method while webhook is active; use deleteWebhook to delete the webhook first",
                None
            )),
            Next::DropWebhook
        );
        assert_eq!(
            after(&refused(
                409,
                "Conflict: terminated by other getUpdates request; make sure that only one bot instance is running",
                None
            )),
            Next::Backoff
        );
        assert_eq!(
            after(&refused(429, "Too Many Requests", Some(7))),
            Next::Wait(Duration::from_secs(7))
        );
        assert_eq!(after(&refused(502, "Bad Gateway", None)), Next::Backoff);
        assert_eq!(
            after(&ApiError::Http("timed out".to_owned())),
            Next::Backoff
        );
        assert_eq!(after(&ApiError::Decode("eof".to_owned())), Next::Backoff);
    }

    #[test]
    fn backoff_doubles_up_to_a_minute() {
        let waits: Vec<u64> = (0..9).map(|n| Pacing::LIVE.backoff(n).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(Pacing::LIVE.backoff(u32::MAX).as_secs(), 60);
    }

    #[test]
    fn an_unreadable_update_still_moves_the_offset() {
        let raw = decode(json!({ "update_id": 41, "message": { "message_id": "not a number" } }));
        assert_eq!(raw.id, Some(41));
        assert!(raw.content.is_none());
        let unknown = decode(json!({ "update_id": 42, "a_kind_from_the_future": {} }));
        assert_eq!(unknown.id, Some(42));
        assert!(unknown.content.is_none());
        assert_eq!(next_offset(None, 41), Some(42));
        assert_eq!(next_offset(Some(50), 41), Some(50));
    }

    #[test]
    fn a_readable_update_keeps_its_content() {
        let raw = decode(json!({ "update_id": 7, "message": {
            "message_id": 1, "date": 1, "chat": { "id": 5, "type": "private", "first_name": "Sam" },
            "from": { "id": 5, "is_bot": false, "first_name": "Sam" }, "text": "hi" } }));
        assert_eq!(raw.id, Some(7));
        assert_eq!(kind(&raw.content.unwrap()), "message");
    }
}
