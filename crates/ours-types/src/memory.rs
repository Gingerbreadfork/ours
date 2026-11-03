//! What a fact looks like when a model has just extracted it from a message
//! (DESIGN.md section 9), and what the extractor is asked. The engine turns
//! drafts into facts with scope, provenance, confidence and expiry.

use crate::ids::{PlatformMessageId, PlatformRoomId};
use crate::triage::ContextLine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What kind of thing a fact is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Person,
    Place,
    Event,
    Thing,
    #[default]
    Group,
}

/// A fact as extracted, before the engine decides scope and confidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FactDraft {
    /// Who or what it is about: a display name as written, a place, an
    /// event ("the cabin trip"), or "the group".
    pub subject: String,
    pub subject_kind: SubjectKind,
    /// A short lower-case relation such as "is vegetarian", "drives",
    /// "birthday", "address", "favourite bar", "went to".
    pub predicate: String,
    pub object: String,
    /// The speaker was talking about themselves.
    pub about_self: bool,
    /// Said outright rather than implied.
    pub stated: bool,
    /// Durable facts have no end; perishable ones carry a date.
    pub durable: bool,
    /// For perishable facts: the day it stops being true, as YYYY-MM-DD.
    pub expires: Option<String>,
    /// For a span such as "away 12 to 16": the first day, as YYYY-MM-DD.
    #[serde(default)]
    pub starts: Option<String>,
    /// The words that carry it, short, for recall and for "why".
    pub quote: Option<String>,
}

/// A promise as extracted: "I'll have the draft by Thursday", "I'll bring
/// dessert", "I'll book the pitch".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommitmentDraft {
    /// Who promised: the speaker's name, or another name when the speaker
    /// assigned it ("Sam will drive").
    pub who: String,
    /// What they will do, short, in the infinitive ("bring dessert").
    pub what: String,
    /// Whom it is for, when said: a person's name or "the group".
    pub for_whom: Option<String>,
    /// When it is due, as YYYY-MM-DD or YYYY-MM-DDTHH:MM, resolved from the
    /// message's date and zone; null when no time was given.
    pub due: Option<String>,
    /// The event it belongs to, when named ("the cabin trip", "Thursday's dinner").
    pub event: Option<String>,
    /// Said outright rather than implied.
    pub stated: bool,
    pub quote: Option<String>,
}

/// Money paid for the group, as extracted: "I got the wine, 38".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LedgerDraft {
    /// Who paid: the speaker's name unless they said otherwise.
    pub payer: String,
    pub amount: f64,
    /// ISO 4217 code when said or obvious, else null for the group's currency.
    pub currency: Option<String>,
    pub what: String,
    /// Named people it is for; empty means everyone at the linked event or
    /// everyone in the group.
    pub participants: Vec<String>,
    /// The event it belongs to, when named.
    pub event: Option<String>,
    pub quote: Option<String>,
}

/// Everything one extraction call found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct Extraction {
    #[serde(default)]
    pub facts: Vec<FactDraft>,
    #[serde(default)]
    pub commitments: Vec<CommitmentDraft>,
    #[serde(default)]
    pub ledger: Vec<LedgerDraft>,
}

/// What the extractor needs to pull facts out of one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtractRequest {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    pub agent_name: String,
    pub speaker: String,
    pub text: String,
    /// Said in a direct message with the agent.
    pub private: bool,
    /// The last few messages before this one, oldest first.
    pub context: Vec<ContextLine>,
    /// The day the message was sent, YYYY-MM-DD, so "next Friday" can be resolved.
    pub date: String,
    /// The group's IANA zone.
    pub timezone: String,
}

/// A link to the original message where the platform has one.
pub fn message_link(
    platform: crate::Platform,
    container: Option<&str>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) -> Option<String> {
    match platform {
        crate::Platform::Discord => Some(format!(
            "https://discord.com/channels/{}/{room}/{message}",
            container.unwrap_or("@me")
        )),
        crate::Platform::Slack => {
            let ts = message.as_str().replace('.', "");
            // A thread room is "<channel>:<thread ts>"; its link needs both.
            match room.as_str().split_once(':') {
                Some((channel, thread_ts)) => Some(format!(
                    "https://slack.com/archives/{channel}/p{ts}?thread_ts={thread_ts}&cid={channel}"
                )),
                None => Some(format!("https://slack.com/archives/{room}/p{ts}")),
            }
        }
        // A supergroup id is "-100" and an internal id; a topic room is "<chat>:<topic>".
        crate::Platform::Telegram => {
            let (chat, topic) = match room.as_str().split_once(':') {
                Some((chat, topic)) => (chat, Some(topic)),
                None => (room.as_str(), None),
            };
            let internal = chat.strip_prefix("-100")?;
            Some(match topic {
                Some(topic) => format!("https://t.me/c/{internal}/{topic}/{message}"),
                None => format!("https://t.me/c/{internal}/{message}"),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_follow_each_platform() {
        let room = PlatformRoomId::new("200");
        let message = PlatformMessageId::new("300");
        assert_eq!(
            message_link(crate::Platform::Discord, Some("100"), &room, &message).as_deref(),
            Some("https://discord.com/channels/100/200/300")
        );
        assert_eq!(
            message_link(
                crate::Platform::Slack,
                None,
                &PlatformRoomId::new("C1"),
                &PlatformMessageId::new("1700000000.000100")
            )
            .as_deref(),
            Some("https://slack.com/archives/C1/p1700000000000100")
        );
        assert_eq!(
            message_link(
                crate::Platform::Slack,
                None,
                &PlatformRoomId::new("C1:1700000000.000100"),
                &PlatformMessageId::new("1700000005.000200")
            )
            .as_deref(),
            Some(
                "https://slack.com/archives/C1/p1700000005000200?thread_ts=1700000000.000100&cid=C1"
            )
        );
        assert_eq!(
            message_link(
                crate::Platform::Telegram,
                None,
                &PlatformRoomId::new("-1001234567890"),
                &PlatformMessageId::new("42")
            )
            .as_deref(),
            Some("https://t.me/c/1234567890/42")
        );
        assert_eq!(
            message_link(
                crate::Platform::Telegram,
                None,
                &PlatformRoomId::new("-1001234567890:7"),
                &PlatformMessageId::new("42")
            )
            .as_deref(),
            Some("https://t.me/c/1234567890/7/42")
        );
        assert!(
            message_link(
                crate::Platform::Telegram,
                None,
                &PlatformRoomId::new("12345"),
                &message
            )
            .is_none()
        );
    }
}
