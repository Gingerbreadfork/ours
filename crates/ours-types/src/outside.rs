//! Actions that leave the group (DESIGN.md section 13, tier 1 and above)
//! and what comes back from outside: results, and mail arriving at the
//! group's address, which is always a guest.

use crate::ids::ActionId;
use crate::instant::Instant;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An email the group sends. `in_reply_to` threads it under an earlier
/// message id so replies can be matched back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutgoingMail {
    pub to: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
}

/// What the binary carries out through an integration. Every variant has a
/// compensating variant or is its own compensation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutsideAction {
    SendMail(OutgoingMail),
    /// The compensation for a sent mail: a follow-up to the same address.
    CancelMail {
        original: ActionId,
        mail: OutgoingMail,
    },
}

/// The outcome of an outside action, fed back as an input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutsideResult {
    pub action: ActionId,
    pub ok: bool,
    /// The provider's reference: a message id for mail.
    pub reference: Option<String>,
    pub error: Option<String>,
    pub at: Instant,
}

/// Mail that reached the group's address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboundMail {
    pub from: String,
    pub from_name: Option<String>,
    pub subject: String,
    /// Plain text, quoted replies stripped where the parser could.
    pub body: String,
    pub at: Instant,
    pub message_id: Option<String>,
    /// The message ids this replies to, newest first, for matching against
    /// mail the group sent.
    pub in_reply_to: Vec<String>,
    /// DKIM and SPF both passed; otherwise the sender is less trusted.
    pub authenticated: bool,
}
