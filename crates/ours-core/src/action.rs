//! The action record and the undo catalogue (DESIGN.md section 13). Every
//! outward thing the agent does leaves a record: what, when, on whose
//! instruction, approved by whom, its tier, how it is taken back and what
//! became of it.

use crate::commitment::CommitmentId;
use crate::memory::FactId;
use crate::profile::Outbound;
use crate::proposal::{DecisionId, ProposalId};
use crate::state::{GroupState, PendingAction, PendingDm};
use crate::tab::LedgerId;
use crate::trace::shorten;
use ours_types::{
    ActionId, Instant, Message, OutsideAction, PlatformMessageId, PlatformRoomId, PlatformUserId,
    Tier,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Records kept, oldest dropped first.
pub const ACTIONS_KEPT: usize = 500;

/// What kind of thing an action did.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    RoomMessage,
    Reaction,
    Poll,
    Pin,
    Dm,
    Fact,
    Commitment,
    LedgerLine,
    CalendarEntry,
    Mail,
    /// Taking another action back.
    Undo,
}

impl ActionClass {
    pub const ALL: [ActionClass; 11] = [
        ActionClass::RoomMessage,
        ActionClass::Reaction,
        ActionClass::Poll,
        ActionClass::Pin,
        ActionClass::Dm,
        ActionClass::Fact,
        ActionClass::Commitment,
        ActionClass::LedgerLine,
        ActionClass::CalendarEntry,
        ActionClass::Mail,
        ActionClass::Undo,
    ];

    /// How a person names it: "undo the email".
    pub fn noun(self) -> &'static str {
        match self {
            ActionClass::RoomMessage => "message",
            ActionClass::Reaction => "reaction",
            ActionClass::Poll => "poll",
            ActionClass::Pin => "pinned message",
            ActionClass::Dm => "DM",
            ActionClass::Fact => "fact",
            ActionClass::Commitment => "commitment",
            ActionClass::LedgerLine => "tab line",
            ActionClass::CalendarEntry => "calendar entry",
            ActionClass::Mail => "email",
            ActionClass::Undo => "undo",
        }
    }
}

/// Where an action stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Done,
    /// Waiting out its hold window, or for the one person who must say yes.
    Held {
        until: Instant,
    },
    /// Handed to the integration; the result has not come back.
    Sending {
        since: Instant,
    },
    Sent {
        at: Instant,
        reference: Option<String>,
    },
    Undone {
        at: Instant,
        by: Option<PlatformUserId>,
    },
    Failed {
        error: String,
    },
    Declined,
}

/// How an action is taken back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Undo {
    Delete {
        room: PlatformRoomId,
        message: PlatformMessageId,
    },
    Unreact {
        room: PlatformRoomId,
        message: PlatformMessageId,
        emoji: String,
    },
    RetractFacts {
        ids: Vec<FactId>,
    },
    DropCommitment {
        id: CommitmentId,
    },
    VoidLine {
        id: LedgerId,
    },
    /// End the poll and close its proposal as expired.
    EndPoll {
        proposal: ProposalId,
    },
    RemoveEntry {
        decision: DecisionId,
    },
    /// Still held: cancelled outright.
    CancelHold,
    /// Already sent: a compensating mail to the same address.
    CancelMail {
        to: String,
    },
    None {
        why: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionRecord {
    pub id: ActionId,
    pub class: ActionClass,
    pub tier: Tier,
    pub room: PlatformRoomId,
    pub requested_by: Option<PlatformUserId>,
    /// The lead whose ✅ carried out someone else's request.
    pub approved_by: Option<PlatformUserId>,
    /// The message that asked for it.
    pub instruction: Option<PlatformMessageId>,
    pub at: Instant,
    pub status: ActionStatus,
    /// A noun phrase: "the email to bookings@baitong.com: Table for five".
    pub summary: String,
    pub undo: Undo,
    /// What came of it, such as the venue's reply.
    pub outcome: Option<String>,
    /// The agent's messages about it, the one that announced it first.
    #[serde(default)]
    pub lines: Vec<PlatformMessageId>,
    /// The room saw it, so a bare "undo" may mean it.
    #[serde(default)]
    pub visible: bool,
    /// What it sends, or sent, outside the group.
    #[serde(default)]
    pub outside: Option<OutsideAction>,
    /// The provider's reference once it was sent, kept after an undo so
    /// replies still match.
    #[serde(default)]
    pub reference: Option<String>,
    /// For an undo, the record it took back.
    #[serde(default)]
    pub undoes: Option<ActionId>,
}

impl ActionRecord {
    /// A record done now, on nobody's instruction, at the class's tier.
    pub fn new(
        id: ActionId,
        class: ActionClass,
        room: PlatformRoomId,
        at: Instant,
        summary: impl Into<String>,
        undo: Undo,
    ) -> ActionRecord {
        ActionRecord {
            id,
            class,
            tier: tier_of(&class),
            room,
            requested_by: None,
            approved_by: None,
            instruction: None,
            at,
            status: ActionStatus::Done,
            summary: summary.into(),
            undo,
            outcome: None,
            lines: Vec::new(),
            visible: true,
            outside: None,
            reference: None,
            undoes: None,
        }
    }

    pub fn from_origin(mut self, origin: &Origin) -> ActionRecord {
        self.requested_by = origin.requested_by.clone();
        self.approved_by = origin.approved_by.clone();
        self.instruction = origin.instruction.clone();
        self
    }

    pub fn hidden(mut self) -> ActionRecord {
        self.visible = false;
        self
    }

    pub fn is_undone(&self) -> bool {
        matches!(self.status, ActionStatus::Undone { .. })
    }

    /// Whether it is about one of the agent's messages.
    pub fn concerns_message(&self, message: &PlatformMessageId) -> bool {
        self.lines.contains(message)
            || matches!(&self.undo, Undo::Delete { message: m, .. } if m == message)
    }
}

/// Who asked for an action, who approved it and in which message.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Origin {
    pub requested_by: Option<PlatformUserId>,
    pub approved_by: Option<PlatformUserId>,
    pub instruction: Option<PlatformMessageId>,
}

impl Origin {
    pub fn asked(user: &PlatformUserId, message: &PlatformMessageId) -> Origin {
        Origin {
            requested_by: Some(user.clone()),
            approved_by: None,
            instruction: Some(message.clone()),
        }
    }
}

/// How a message the agent sends enters the record.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Recording {
    /// A record of its own, undone by deleting it; who asked comes from
    /// the message it replies to.
    #[default]
    Own,
    /// A record of its own for this origin.
    For { origin: Origin },
    /// One of the lines about an existing record.
    Line { action: ActionId },
    /// It reports a change that undo cannot take back; the record says why.
    Note { why: String },
    /// Talk about the agent's own records, kept out of them.
    Unrecorded,
}

/// One row of the undo catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CatalogueEntry {
    pub class: ActionClass,
    pub tier: Tier,
    pub how_undone: &'static str,
}

/// Every class of action, its tier and how it is taken back.
pub fn catalogue() -> Vec<CatalogueEntry> {
    ActionClass::ALL
        .iter()
        .map(|class| CatalogueEntry {
            class: *class,
            tier: tier_of(class),
            how_undone: how_undone(*class),
        })
        .collect()
}

fn how_undone(class: ActionClass) -> &'static str {
    match class {
        ActionClass::RoomMessage => "deleted",
        ActionClass::Reaction => "the reaction is taken off",
        ActionClass::Poll => "the poll is ended and its proposal closed as expired",
        ActionClass::Pin => "not undone: the pinned message follows the group's state",
        ActionClass::Dm => "deleted",
        ActionClass::Fact => "retracted with reason undo; the record of it stays",
        ActionClass::Commitment => "dropped, and its 📌 taken off",
        ActionClass::LedgerLine => "voided; the balances recount without it",
        ActionClass::CalendarEntry => "taken off the calendar and the feeds",
        ActionClass::Mail => {
            "cancelled outright while it is held; once sent, a cancellation goes to the same address"
        }
        ActionClass::Undo => "not undone; ask for the action again",
    }
}

/// The consequence tier of a class: free inside the group, windowed for
/// anything that leaves it. Nothing is irreversible yet.
pub fn tier_of(class: &ActionClass) -> Tier {
    match class {
        ActionClass::Mail => Tier::Windowed,
        ActionClass::RoomMessage
        | ActionClass::Reaction
        | ActionClass::Poll
        | ActionClass::Pin
        | ActionClass::Dm
        | ActionClass::Fact
        | ActionClass::Commitment
        | ActionClass::LedgerLine
        | ActionClass::CalendarEntry
        | ActionClass::Undo => Tier::Free,
    }
}

pub fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Free => "free",
        Tier::Windowed => "windowed",
        Tier::Irreversible => "irreversible",
    }
}

/// Whether the agent may send something outside the group that no lead
/// asked for: only where the profile says do, and never while observing.
pub fn may_leave_unprompted(state: &GroupState) -> bool {
    state.profile.unprompted_outbound == Outbound::Do && !state.is_observing()
}

/// Appends a record, dropping the oldest past `ACTIONS_KEPT`.
pub(crate) fn push(state: &mut GroupState, record: ActionRecord) {
    state.actions.push_back(record);
    while state.actions.len() > ACTIONS_KEPT {
        state.actions.pop_front();
    }
}

pub fn find<'a>(state: &'a GroupState, id: &ActionId) -> Option<&'a ActionRecord> {
    state.actions.iter().rev().find(|r| &r.id == id)
}

pub(crate) fn find_mut<'a>(
    state: &'a mut GroupState,
    id: &ActionId,
) -> Option<&'a mut ActionRecord> {
    state.actions.iter_mut().rev().find(|r| &r.id == id)
}

/// The newest record that `matches` accepts.
pub(crate) fn latest_where(
    state: &GroupState,
    matches: impl Fn(&ActionRecord) -> bool,
) -> Option<ActionId> {
    state
        .actions
        .iter()
        .rev()
        .find(|r| matches(r))
        .map(|r| r.id.clone())
}

/// The record about one of the agent's messages in `room`.
pub fn about_message<'a>(
    state: &'a GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) -> Option<&'a ActionRecord> {
    state
        .actions
        .iter()
        .rev()
        .find(|r| &r.room == room && r.concerns_message(message))
}

/// The line "why" adds for an action that is more than a message.
pub fn why_line(state: &GroupState, record: &ActionRecord) -> Option<String> {
    if matches!(record.class, ActionClass::RoomMessage | ActionClass::Dm) {
        return None;
    }
    let undo = match &record.status {
        ActionStatus::Undone { by, .. } => match by {
            Some(user) => format!("undone by {}", state.name_of(user)),
            None => "undone".to_owned(),
        },
        ActionStatus::Failed { .. } | ActionStatus::Declined => "nothing to undo".to_owned(),
        ActionStatus::Held { until } => format!(
            "say undo before {} and it never goes",
            crate::zone::local(*until, &state.timezone).strftime("%H:%M")
        ),
        _ => match &record.undo {
            Undo::None { why } => format!("none, {why}"),
            Undo::CancelMail { to } => format!("say undo and I send {to} a cancellation"),
            _ => format!("say undo: {}", how_undone(record.class)),
        },
    };
    Some(format!("Tier: {}. Undo: {undo}.", tier_name(record.tier)))
}

/// `text` quoted for a summary, without its closing punctuation.
pub(crate) fn quoted(text: &str, max: usize) -> String {
    let short = shorten(text, max);
    format!("\"{}\"", short.trim_end_matches(['.', '!', '?']))
}

/// Who asked for a message that replies to `reply_to`, when it is a
/// person's message the room holds.
fn origin_of(
    state: &GroupState,
    room: &PlatformRoomId,
    reply_to: Option<&PlatformMessageId>,
) -> Origin {
    let asked = reply_to.and_then(|id| {
        state
            .rooms
            .get(room)?
            .messages
            .iter()
            .find(|m: &&Message| &m.id == id && state.is_human(room, &m.author))
    });
    match asked {
        Some(m) => Origin::asked(&m.author, &m.id),
        None => Origin::default(),
    }
}

/// Adds `message` to the lines of record `action`, and to its hold.
pub(crate) fn attach(
    state: &mut GroupState,
    action: &ActionId,
    message: &PlatformMessageId,
    dm: bool,
) {
    if let Some(record) = find_mut(state, action) {
        if !record.lines.contains(message) {
            record.lines.push(message.clone());
        }
        record.visible = true;
    }
    if let Some(hold) = state.holds.get_mut(action) {
        if dm && hold.ask.is_some() {
            hold.asked.get_or_insert_with(|| message.clone());
        } else if !dm {
            hold.notice.get_or_insert_with(|| message.clone());
        }
    }
}

/// A room message's id came back: record it as its `Recording` says.
pub(crate) fn on_message_sent(
    state: &mut GroupState,
    id: &ActionId,
    pending: &PendingAction,
    message: &PlatformMessageId,
    now: Instant,
) {
    let room = &pending.room;
    let origin = match &pending.record {
        Recording::Line { action } => {
            attach(state, action, message, false);
            return;
        }
        Recording::Unrecorded => return,
        Recording::For { origin } => origin.clone(),
        Recording::Own | Recording::Note { .. } => {
            origin_of(state, room, pending.reply_to.as_ref())
        }
    };
    let undo = match &pending.record {
        Recording::Note { why } => Undo::None { why: why.clone() },
        _ => Undo::Delete {
            room: room.clone(),
            message: message.clone(),
        },
    };
    let summary = format!("my message {}", quoted(&pending.text, 60));
    let mut record = ActionRecord::new(
        id.clone(),
        ActionClass::RoomMessage,
        room.clone(),
        now,
        summary,
        undo,
    )
    .from_origin(&origin);
    record.lines.push(message.clone());
    push(state, record);
}

/// A DM's id came back: record it in the DM room.
pub(crate) fn on_dm_sent(
    state: &mut GroupState,
    id: &ActionId,
    pending: &PendingDm,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
) {
    let origin = match &pending.record {
        Recording::Line { .. } | Recording::Unrecorded => return,
        Recording::For { origin } => origin.clone(),
        Recording::Own | Recording::Note { .. } => Origin::default(),
    };
    let undo = match &pending.record {
        Recording::Note { why } => Undo::None { why: why.clone() },
        _ => Undo::Delete {
            room: room.clone(),
            message: message.clone(),
        },
    };
    let summary = format!(
        "my DM to {} {}",
        state.name_of(&pending.user),
        quoted(&pending.text, 60)
    );
    let mut record = ActionRecord::new(
        id.clone(),
        ActionClass::Dm,
        room.clone(),
        now,
        summary,
        undo,
    )
    .from_origin(&origin);
    record.lines.push(message.clone());
    push(state, record);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_types::{GroupId, Kind};

    #[test]
    fn the_catalogue_covers_every_class() {
        // A new class fails to compile here until it is placed in `ALL`.
        let index = |c: ActionClass| match c {
            ActionClass::RoomMessage => 0,
            ActionClass::Reaction => 1,
            ActionClass::Poll => 2,
            ActionClass::Pin => 3,
            ActionClass::Dm => 4,
            ActionClass::Fact => 5,
            ActionClass::Commitment => 6,
            ActionClass::LedgerLine => 7,
            ActionClass::CalendarEntry => 8,
            ActionClass::Mail => 9,
            ActionClass::Undo => 10,
        };
        let mut seen: Vec<usize> = ActionClass::ALL.iter().map(|c| index(*c)).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..ActionClass::ALL.len()).collect::<Vec<_>>());

        let entries = catalogue();
        for class in ActionClass::ALL {
            let entry = entries.iter().find(|e| e.class == class).unwrap();
            assert!(!entry.how_undone.is_empty(), "{class:?}");
            assert_eq!(entry.tier, tier_of(&class));
        }
        let json = serde_json::to_value(&entries).unwrap();
        assert_eq!(json[9]["class"], "mail");
        assert_eq!(json[9]["tier"], "windowed");
    }

    #[test]
    fn only_what_leaves_the_group_is_windowed() {
        for class in ActionClass::ALL {
            let expected = if class == ActionClass::Mail {
                Tier::Windowed
            } else {
                Tier::Free
            };
            assert_eq!(tier_of(&class), expected, "{class:?}");
        }
    }

    #[test]
    fn unprompted_outbound_follows_the_profile() {
        let cases = [
            (Kind::Friends, true),
            (Kind::Household, true),
            (Kind::Community, false),
            (Kind::Team, false),
            (Kind::Project, false),
            (Kind::Committee, false),
        ];
        for (kind, expected) in cases {
            let state = GroupState::new(GroupId::new("g"), kind);
            assert_eq!(may_leave_unprompted(&state), expected, "{kind:?}");
        }
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        state.profile.unprompted_outbound = Outbound::Never;
        assert!(!may_leave_unprompted(&state));
        state.profile.unprompted_outbound = Outbound::Do;
        state.observe = Some(crate::observe::Observe {
            until: Instant::UNIX_EPOCH,
            started: Instant::UNIX_EPOCH,
            would_have: 0,
            up: 0,
            down: 0,
            targets: Default::default(),
        });
        assert!(!may_leave_unprompted(&state));
    }

    #[test]
    fn records_serialise_the_way_the_admin_page_reads_them() {
        let mut record = ActionRecord::new(
            ActionId::new("a1"),
            ActionClass::Mail,
            "r".into(),
            Instant::UNIX_EPOCH,
            "the email to x@y.com",
            Undo::CancelMail {
                to: "x@y.com".into(),
            },
        );
        record.status = ActionStatus::Held {
            until: Instant::UNIX_EPOCH,
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["class"], "mail");
        assert_eq!(json["tier"], "windowed");
        assert!(json["status"]["held"]["until"].is_string());
        assert_eq!(json["undo"]["cancel_mail"]["to"], "x@y.com");
        let back: ActionRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, record);
        let done = serde_json::to_value(ActionStatus::Done).unwrap();
        assert_eq!(done, "done");
    }
}
