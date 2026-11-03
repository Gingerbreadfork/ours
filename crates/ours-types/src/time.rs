//! Time expressions as a resolver is asked about them and as it answers
//! (DESIGN.md section 12). The engine resolves the easy forms by rules and
//! asks a model for the rest; either way the reply echoes the date it chose.

use crate::ids::{PlatformMessageId, PlatformRoomId};
use crate::instant::Instant;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the resolver needs for one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeRequest {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    pub text: String,
    /// The speaker's IANA zone when known, else the group's.
    pub zone: String,
    /// The instant the message was sent.
    pub now: Instant,
    /// The same instant as local civil time, "YYYY-MM-DD HH:MM Weekday".
    pub local_now: String,
}

/// What a time expression turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimeMeaning {
    /// One moment, with or without a time of day.
    Instant { at: Instant, has_time: bool },
    /// A range such as "next week" or "12 to 16".
    Span { start: Instant, end: Instant },
    /// A repeating schedule as an RFC 5545 RRULE.
    Recurrence { rrule: String, first: Instant },
    /// Two readings differ by more than a day and the difference matters.
    Ambiguous { candidates: Vec<Instant> },
    /// Nothing temporal in the text.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeResolution {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    /// The words that carried the time, as written.
    pub phrase: Option<String>,
    pub meaning: TimeMeaning,
    /// How to say it back, such as "Thursday the 14th" or "the week of the 9th".
    pub echo: Option<String>,
}
