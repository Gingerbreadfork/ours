//! Everything that can happen to a group. The event log stores these; the
//! group's state is a fold of `step` over them.

use crate::persona::Persona;
use crate::plan::Plan;
use crate::profile::Profile;
use crate::proposal::CloseRule;
use crate::settings::SettingChange;
use ours_types::{
    Extraction, FactDraft, InboundEvent, InboundMail, Kind, OutsideResult, PlatformMessageId,
    PlatformRoomId, PlatformUserId, RequestId, Score, TimeResolution, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Input {
    Inbound(InboundEvent),
    TimerFired {
        id: TimerId,
    },
    ModelReply {
        request: RequestId,
        reply: Result<Plan, String>,
    },
    /// The answer to `Effect::Classify`: the small model's score, or the
    /// rules pass's own when the model failed.
    Scored {
        room: PlatformRoomId,
        message: PlatformMessageId,
        score: Score,
    },
    /// The answer to `Effect::Extract`: the facts, commitments and tab
    /// lines drafted from one message, none when the extractor failed.
    Extracted {
        room: PlatformRoomId,
        message: PlatformMessageId,
        /// Logs written before commitments carried only the facts, as `drafts`.
        #[serde(alias = "drafts", deserialize_with = "extraction_or_facts")]
        #[schemars(with = "Extraction")]
        extraction: Extraction,
    },
    /// The answer to `Effect::ResolveTime`, with `TimeMeaning::None` when
    /// the resolver failed or is off.
    TimeResolved(TimeResolution),
    /// The answer to `Effect::Outside`: whether it went, and the provider's
    /// reference when it did.
    OutsideResult(OutsideResult),
    /// Mail that reached the group's address. It is a guest's words: it is
    /// relayed and remembered, never obeyed.
    Mail(InboundMail),
    Command(Command),
}

impl Input {
    /// A short name for logs and spans.
    pub fn label(&self) -> &'static str {
        match self {
            Input::Inbound(event) => match event {
                InboundEvent::Ready { .. } => "ready",
                InboundEvent::RoomSync { .. } => "room_sync",
                InboundEvent::Message(_) => "message",
                InboundEvent::MessageEdited(_) => "message_edited",
                InboundEvent::MessageDeleted { .. } => "message_deleted",
                InboundEvent::ReactionAdded { .. } => "reaction_added",
                InboundEvent::ReactionRemoved { .. } => "reaction_removed",
                InboundEvent::MemberJoined { .. } => "member_joined",
                InboundEvent::MemberLeft { .. } => "member_left",
                InboundEvent::MemberUpdated { .. } => "member_updated",
                InboundEvent::ActionResult { .. } => "action_result",
                InboundEvent::History { .. } => "history",
                InboundEvent::PollVote { .. } => "poll_vote",
                InboundEvent::PollEnded { .. } => "poll_ended",
                InboundEvent::Disconnected { .. } => "disconnected",
            },
            Input::TimerFired { .. } => "timer_fired",
            Input::ModelReply { .. } => "model_reply",
            Input::Scored { .. } => "scored",
            Input::Extracted { .. } => "extracted",
            Input::TimeResolved(_) => "time_resolved",
            Input::OutsideResult(_) => "outside_result",
            Input::Mail(_) => "mail",
            Input::Command(_) => "command",
        }
    }
}

/// An extraction, or the list of fact drafts older logs hold.
fn extraction_or_facts<'de, D: Deserializer<'de>>(d: D) -> Result<Extraction, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Facts(Vec<FactDraft>),
        Found(Extraction),
    }
    Ok(match Either::deserialize(d)? {
        Either::Facts(facts) => Extraction {
            facts,
            ..Extraction::default()
        },
        Either::Found(extraction) => extraction,
    })
}

/// A configuration change from the admin page or the host's CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Change the kind and reset the profile to that kind's defaults. The
    /// agent's name, leads, hosts and observe state are kept.
    SetKind {
        kind: Kind,
    },
    /// Replace the named leads list.
    SetLeads {
        users: Vec<PlatformUserId>,
    },
    AddLead {
        user: PlatformUserId,
    },
    RemoveLead {
        user: PlatformUserId,
    },
    SetHosts {
        users: Vec<PlatformUserId>,
    },
    SetPersona {
        persona: Persona,
    },
    /// One of the starter personas by name, or "plain" for the default.
    UsePersona {
        name: String,
    },
    /// Turn expiry and retention off, or back on.
    SetLegalHold {
        on: bool,
    },
    SetAgentName {
        name: String,
        /// Set by the host's config, which skips the name question in setup.
        #[serde(default)]
        by_host: bool,
    },
    /// The group's home zone, an IANA name such as "Europe/Lisbon".
    SetTimezone {
        zone: String,
    },
    SetProfile {
        profile: Profile,
    },
    /// Which model answers and where it runs, as the group should be told.
    SetModelLabel {
        label: String,
    },
    /// Any change a lead can make by sentence, applied as if a lead said it.
    ChangeSetting {
        change: SettingChange,
    },
    /// How new proposals close; none to go back to the kind's rule.
    SetDecisionRule {
        rule: Option<CloseRule>,
    },
    /// A member's zone as an IANA name; none to forget it.
    SetMemberZone {
        user: PlatformUserId,
        zone: Option<String>,
    },
    /// The tab's currency, an ISO 4217 code such as "AUD".
    SetCurrency {
        code: String,
    },
    /// Where the binary serves the calendar feeds, without ".ics": the
    /// group's is `<url>.ics` and a member's `<url>/<user>.ics`. Empty turns
    /// the links off.
    SetFeedBase {
        url: String,
    },
}
