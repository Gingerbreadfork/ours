//! A group's state: everything `step` folds inputs into. It snapshots to
//! JSON and back, so a restart resumes from the last snapshot plus the log.

use crate::action::{ActionRecord, Recording};
use crate::authority::{ActionKind, ActionSet, ScopedLead};
use crate::backfill::Backfill;
use crate::budget::Budget;
use crate::commitment::{Commitment, CommitmentId};
use crate::decide::{PendingTime, TIME_TTL_SECONDS};
use crate::decision::{Decision, Entry};
use crate::error::CoreError;
use crate::extract::{EXTRACT_TTL_SECONDS, PendingExtract};
use crate::hold::Hold;
use crate::memory::{Fact, FactId};
use crate::observe::Observe;
use crate::patterns::Patterns;
use crate::persona::Persona;
use crate::pin::Pinned;
use crate::plan::PlannedAction;
use crate::profile::{Jobs, Profile};
use crate::proposal::{ACTIVE_SECONDS, CloseRule, DecisionId, Proposal, ProposalId};
use crate::recur::{Recurrence, RecurrenceId};
use crate::rights::MemoryChange;
use crate::settings::SettingChange;
use crate::setup::Setup;
use crate::tab::{LedgerId, LedgerLine, Settlement, SettlementId};
use crate::timers::Timer;
use crate::trace::{TRACES_KEPT, Trace};
use crate::zone::Zone;
use ours_types::{
    ActionId, Capabilities, GroupId, Instant, Kind, Member, Message, Platform, PlatformMessageId,
    PlatformRoomId, PlatformUserId, RequestId, Role, Room, RoomKind, Score, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Messages kept per room; the ring buffer is a view of the log.
pub const RING_CAPACITY: usize = 200;
/// Ids of the agent's own messages kept per room, for reply detection.
pub const OWN_MESSAGES_KEPT: usize = 500;
/// A deferral nobody approves within a day expires without a word.
pub const DEFERRAL_TTL_SECONDS: i64 = 24 * 60 * 60;
/// Sends the connector never answered are forgotten after this long.
pub const PENDING_ACTION_TTL_SECONDS: i64 = 60 * 60;
/// Scores kept per room, oldest dropped first.
pub const SCORES_KEPT: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GroupState {
    pub id: GroupId,
    /// What the agent answers to in this group.
    pub agent_name: String,
    /// Whether the host's config chose the name, so setup need not ask.
    #[serde(default)]
    pub name_from_host: bool,
    pub kind: Kind,
    pub profile: Profile,
    pub persona: Persona,
    /// The starter persona in use by name, or none for the plain default
    /// or one the host wrote.
    #[serde(default)]
    pub persona_name: Option<String>,
    /// The group's home zone, an IANA name.
    pub timezone: String,
    /// Which model answers for the agent and where it runs, for disclosure.
    #[serde(default)]
    pub model_label: String,
    /// The agent's own account on each connected platform.
    pub me: Vec<Me>,
    pub rooms: BTreeMap<PlatformRoomId, RoomState>,
    /// Named leads, bound to platform ids and never to display names.
    pub leads: BTreeSet<PlatformUserId>,
    pub hosts: BTreeSet<PlatformUserId>,
    pub scoped_leads: Vec<ScopedLead>,
    pub pending_requests: BTreeMap<RequestId, PendingRequest>,
    pub pending_actions: BTreeMap<ActionId, PendingAction>,
    /// Armed timers. After a restart the binary re-arms every one of these.
    pub timers: BTreeMap<TimerId, Timer>,
    /// Open deferrals, keyed by the agent's "Needs a lead's ✅" message.
    pub deferrals: BTreeMap<PlatformMessageId, Deferral>,
    pub budget: Budget,
    /// Source of the engine's own ids, so replay mints the same ones.
    pub next_seq: u64,
    /// Watching before speaking unprompted (DESIGN.md section 18). Stays set
    /// after the period ends, until a lead says go.
    #[serde(default)]
    pub observe: Option<Observe>,
    /// The observe period that ended last, kept for its counts.
    #[serde(default)]
    pub observed: Option<Observe>,
    /// The first conversation (DESIGN.md section 7).
    #[serde(default)]
    pub setup: Setup,
    /// Members who shushed the agent, with when their plans weigh fully again.
    #[serde(default)]
    pub shushed: BTreeMap<PlatformUserId, Instant>,
    /// Questions the agent raised a hand on, or would have, by message.
    #[serde(default)]
    pub hands: BTreeMap<PlatformMessageId, Hand>,
    /// DMs sent whose platform id has not come back yet.
    #[serde(default)]
    pub pending_dms: BTreeMap<ActionId, PendingDm>,
    /// The group's memory (DESIGN.md section 9). Retracted facts stay.
    #[serde(default)]
    pub facts: BTreeMap<FactId, Fact>,
    /// Messages the extractor is reading, oldest first.
    #[serde(default)]
    pub extractions: Vec<PendingExtract>,
    /// Rooms whose history is being read.
    #[serde(default)]
    pub backfills: BTreeMap<PlatformRoomId, Backfill>,
    /// Reply hours and room rhythm, for timing only.
    #[serde(default)]
    pub patterns: Patterns,
    /// Each member's zone, from the platform or their own words.
    #[serde(default)]
    pub zones: BTreeMap<PlatformUserId, Zone>,
    /// When each member last posted or reacted, for who counts as active.
    #[serde(default)]
    pub last_seen: BTreeMap<PlatformUserId, Instant>,
    /// Open and finished proposals (DESIGN.md section 10).
    #[serde(default)]
    pub proposals: BTreeMap<ProposalId, Proposal>,
    /// The decision records, never changed once written.
    #[serde(default)]
    pub decisions: BTreeMap<DecisionId, Decision>,
    /// Dated outcomes and who is coming (DESIGN.md section 12).
    #[serde(default)]
    pub calendar: Vec<Entry>,
    /// Native polls sent whose message id has not come back yet.
    #[serde(default)]
    pub pending_polls: BTreeMap<ActionId, ProposalId>,
    /// Time expressions waiting on the resolver.
    #[serde(default)]
    pub pending_times: Vec<PendingTime>,
    /// How new proposals close, when the group has chosen; else its kind's rule.
    #[serde(default)]
    pub decision_rule: Option<CloseRule>,
    /// Who said they would do what (DESIGN.md section 11).
    #[serde(default)]
    pub commitments: BTreeMap<CommitmentId, Commitment>,
    /// The tab, every line in order; voided lines stay.
    #[serde(default)]
    pub ledger: Vec<LedgerLine>,
    /// The tab's currency as an ISO 4217 code.
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Payment links members gave by DM, shown only to their debtors, by DM.
    #[serde(default)]
    pub payment_links: BTreeMap<PlatformUserId, String>,
    /// Transfers asked for by settle-up, until confirmed or disputed.
    #[serde(default)]
    pub settlements: BTreeMap<SettlementId, Settlement>,
    /// Each room's pinned state message.
    #[serde(default)]
    pub pins: BTreeMap<PlatformRoomId, Pinned>,
    /// Repeating events, commitments and reminders (DESIGN.md section 12).
    #[serde(default)]
    pub recurrences: Vec<Recurrence>,
    /// When each room's digest last went out, or when it started counting.
    #[serde(default)]
    pub digests: BTreeMap<PlatformRoomId, Instant>,
    /// The last day each room had its day-of summary, as 2026-11-12.
    #[serde(default)]
    pub day_of_sent: BTreeMap<PlatformRoomId, String>,
    /// Where the binary serves the calendar feeds, without ".ics".
    #[serde(default)]
    pub feed_base: Option<String>,
    /// Every outward thing the agent did, oldest first (DESIGN.md section 13).
    #[serde(default)]
    pub actions: VecDeque<ActionRecord>,
    /// Actions waiting out their hold window before they leave the group.
    #[serde(default)]
    pub holds: BTreeMap<ActionId, Hold>,
    /// Nothing expires and nothing is dropped while this is on (DESIGN.md
    /// section 9, "Retention").
    #[serde(default)]
    pub legal_hold: bool,
    /// When the legal hold was turned on.
    #[serde(default)]
    pub legal_hold_since: Option<Instant>,
    /// Message-IDs of the mail last handled, oldest first, so mail
    /// delivered twice is read once.
    #[serde(default)]
    pub mail_seen: VecDeque<String>,
    /// Members who had the join DM or did not need it, so a join reported
    /// again after a restart gets no second DM.
    #[serde(default)]
    pub welcomed: BTreeSet<PlatformUserId>,
}

/// The tab's currency until the host sets one.
pub const DEFAULT_CURRENCY: &str = "USD";
/// The agent's name in a room until the host or the first conversation picks another.
pub const DEFAULT_AGENT_NAME: &str = "Noa";

fn default_currency() -> String {
    DEFAULT_CURRENCY.to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Me {
    pub platform: Platform,
    pub id: PlatformUserId,
    pub name: String,
    /// What the platform's connector can do.
    #[serde(default)]
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RoomState {
    /// What the connector said about the room; unknown until a `RoomSync`.
    pub info: Option<Room>,
    pub members: BTreeMap<PlatformUserId, Member>,
    /// The last `RING_CAPACITY` messages, oldest first.
    pub messages: VecDeque<Message>,
    /// Ids of messages the agent sent here, oldest first.
    pub own_messages: VecDeque<PlatformMessageId>,
    /// Attention scores of recent messages, oldest first.
    #[serde(default)]
    pub scores: VecDeque<(PlatformMessageId, Score)>,
    /// Why the agent sent each of its recent messages here, oldest first.
    #[serde(default)]
    pub traces: VecDeque<(PlatformMessageId, Trace)>,
}

/// A model call in flight for one message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingRequest {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    pub speaker: PlatformUserId,
    pub role: Role,
    /// Fixed before the model is called and enforced on its plan.
    pub allowed: ActionSet,
    pub guard: TimerId,
    pub asked_at: Instant,
    #[serde(default)]
    pub answering: Answering,
    /// The message's score, for the reply's trace.
    #[serde(default)]
    pub score: Option<Score>,
    /// Facts offered to the model, which its cites must come from.
    #[serde(default)]
    pub recalled: Vec<FactId>,
}

/// Why the model is being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Answering {
    /// Someone addressed the agent.
    #[default]
    Addressed,
    /// A question nobody answered, after the agent's 🙋.
    RaisedHand,
    /// A question nobody answered while observing; the answer goes to leads.
    Observed,
    /// Banter nobody addressed to it, at dial 2 and 3: a reaction or a line.
    Chime,
}

/// A question the agent raised a hand on, waiting for a reaction or the
/// window to pass again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hand {
    pub room: PlatformRoomId,
    /// The timer that answers anyway.
    pub timer: TimerId,
    /// Raised only in a DM to leads, because the group is observing.
    pub observed: bool,
}

/// A DM the agent sent and whose platform id it is waiting to learn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingDm {
    pub user: PlatformUserId,
    pub text: String,
    pub at: Instant,
    pub trace: Option<Trace>,
    /// A 👍 or 👎 from `user` on it counts as observe feedback.
    pub feedback: bool,
    #[serde(default)]
    pub then: Option<DmFollowUp>,
    #[serde(default)]
    pub record: Recording,
}

/// What to do once a sent DM's id is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DmFollowUp {
    /// A due nudge: replies and reactions on it are about this commitment.
    Due { commitment: CommitmentId },
    /// The debtor's settle-up line, where ✅ marks it paid.
    SettleAsk { settlement: SettlementId },
    /// The creditor's line, where ✅ confirms it.
    SettleConfirm { settlement: SettlementId },
    /// A guest's request flagged to a lead, where ✅ runs it.
    Request { deferral: Deferral },
}

/// A message the agent sent and whose platform id it is waiting to learn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingAction {
    pub room: PlatformRoomId,
    pub text: String,
    pub reply_to: Option<PlatformMessageId>,
    pub at: Instant,
    pub then: Option<FollowUp>,
    #[serde(default)]
    pub trace: Option<Trace>,
    #[serde(default)]
    pub record: Recording,
}

/// What to do once a sent message's id is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FollowUp {
    /// React ✅ on the sent message and wait for a lead's ✅ on it.
    OpenDeferral { deferral: Deferral },
    /// The message is a proposal's ballot: keep its id and react each option.
    Ballot { proposal: ProposalId },
    /// The tab's line for new expenses: keep its id and react 🙅 and ✌️ on it.
    TabLine { lines: Vec<LedgerId> },
    /// The room's state message: keep its id and pin it.
    StateMessage { room: PlatformRoomId },
}

/// A request from someone who may not make it, waiting for a lead's ✅.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Deferral {
    pub room: PlatformRoomId,
    pub asked_by: PlatformUserId,
    /// The message that made the request.
    pub request: PlatformMessageId,
    pub request_text: String,
    pub change: DeferredChange,
    pub expires: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeferredChange {
    Leads {
        change: LeadChange,
    },
    /// Planned actions the asker's role did not permit.
    Actions {
        actions: Vec<PlannedAction>,
    },
    /// A setting changed by sentence.
    Setting {
        change: SettingChange,
    },
    /// A change to facts that are not the asker's own.
    Memory {
        change: MemoryChange,
    },
    /// Reading the room's history.
    Backfill,
    /// Taking back an action the asker may not undo themselves.
    Undo {
        action: ActionId,
    },
    /// A guest's instruction a lead takes up: its words run again with the
    /// approving lead as the speaker.
    Request {
        text: String,
        room: PlatformRoomId,
        from: PlatformUserId,
    },
}

impl DeferredChange {
    /// The permission a lead's ✅ must carry to approve this.
    pub fn needs(&self) -> ActionSet {
        match self {
            DeferredChange::Leads { .. } => ActionSet::of(&[ActionKind::ChangeLeads]),
            DeferredChange::Setting { .. } => ActionSet::of(&[ActionKind::ChangeSettings]),
            DeferredChange::Memory { .. } | DeferredChange::Backfill => {
                ActionSet::of(&[ActionKind::ChangeMemory])
            }
            DeferredChange::Undo { .. } => ActionSet::of(&[ActionKind::Undo]),
            DeferredChange::Request { .. } => ActionSet::of(&[]),
            DeferredChange::Actions { actions } => ActionSet::of(
                &actions
                    .iter()
                    .filter_map(PlannedAction::kind)
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

/// A change to who the leads are, with names already resolved to ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LeadChange {
    Add {
        users: Vec<PlatformUserId>,
    },
    Remove {
        users: Vec<PlatformUserId>,
    },
    /// Exactly these people, and nobody by platform role.
    Set {
        users: Vec<PlatformUserId>,
    },
    /// Every member is a lead.
    Everyone,
}

impl GroupState {
    pub fn new(id: GroupId, kind: Kind) -> GroupState {
        GroupState {
            id,
            agent_name: DEFAULT_AGENT_NAME.to_owned(),
            name_from_host: false,
            kind,
            profile: Profile::for_kind(kind),
            persona: Persona::default(),
            persona_name: None,
            timezone: "UTC".to_owned(),
            model_label: String::new(),
            me: Vec::new(),
            rooms: BTreeMap::new(),
            leads: BTreeSet::new(),
            hosts: BTreeSet::new(),
            scoped_leads: Vec::new(),
            pending_requests: BTreeMap::new(),
            pending_actions: BTreeMap::new(),
            timers: BTreeMap::new(),
            deferrals: BTreeMap::new(),
            budget: Budget::default(),
            next_seq: 0,
            observe: None,
            observed: None,
            setup: Setup::default(),
            shushed: BTreeMap::new(),
            hands: BTreeMap::new(),
            pending_dms: BTreeMap::new(),
            facts: BTreeMap::new(),
            extractions: Vec::new(),
            backfills: BTreeMap::new(),
            patterns: Patterns::default(),
            zones: BTreeMap::new(),
            last_seen: BTreeMap::new(),
            proposals: BTreeMap::new(),
            decisions: BTreeMap::new(),
            calendar: Vec::new(),
            pending_polls: BTreeMap::new(),
            pending_times: Vec::new(),
            decision_rule: None,
            commitments: BTreeMap::new(),
            ledger: Vec::new(),
            currency: default_currency(),
            payment_links: BTreeMap::new(),
            settlements: BTreeMap::new(),
            pins: BTreeMap::new(),
            recurrences: Vec::new(),
            digests: BTreeMap::new(),
            day_of_sent: BTreeMap::new(),
            feed_base: None,
            actions: VecDeque::new(),
            holds: BTreeMap::new(),
            legal_hold: false,
            legal_hold_since: None,
            mail_seen: VecDeque::new(),
            welcomed: BTreeSet::new(),
        }
    }

    /// The scheduled jobs: the profile's when a lead changed them, else the kind's.
    pub fn jobs(&self) -> Jobs {
        self.profile
            .jobs
            .unwrap_or_else(|| Jobs::for_kind(self.kind, self.profile.working_hours))
    }

    pub fn snapshot(&self) -> Result<String, CoreError> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn restore(json: &str) -> Result<GroupState, CoreError> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn is_me(&self, user: &PlatformUserId) -> bool {
        self.me.iter().any(|m| &m.id == user)
    }

    /// The agent's account in `room`: the one on the room's platform, or the
    /// only one when the room has not been synced.
    pub fn me_in(&self, room: &PlatformRoomId) -> Option<&Me> {
        match self.rooms.get(room).and_then(|r| r.info.as_ref()) {
            Some(info) => self.me.iter().find(|m| m.platform == info.platform),
            None => self.me.first(),
        }
    }

    pub fn room(&self, id: &PlatformRoomId) -> Option<&RoomState> {
        self.rooms.get(id)
    }

    pub(crate) fn room_mut(&mut self, id: &PlatformRoomId) -> &mut RoomState {
        self.rooms.entry(id.clone()).or_default()
    }

    /// A user's name as the group knows it: the room's view first, then any
    /// room, then the raw platform id.
    pub fn display_name(&self, room: &PlatformRoomId, user: &PlatformUserId) -> String {
        self.rooms
            .get(room)
            .and_then(|r| r.members.get(user))
            .or_else(|| self.rooms.values().find_map(|r| r.members.get(user)))
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| user.to_string())
    }

    /// A user's name from any room, or the raw platform id.
    pub fn name_of(&self, user: &PlatformUserId) -> String {
        self.rooms
            .values()
            .find_map(|r| r.members.get(user))
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| user.to_string())
    }

    pub(crate) fn is_bot(&self, room: &PlatformRoomId, user: &PlatformUserId) -> bool {
        self.rooms
            .get(room)
            .and_then(|r| r.members.get(user))
            .is_some_and(|m| m.is_bot)
    }

    /// Neither the agent nor a bot the room knows of.
    pub(crate) fn is_human(&self, room: &PlatformRoomId, user: &PlatformUserId) -> bool {
        !self.is_me(user) && !self.is_bot(room, user)
    }

    /// How a room is named in a line: "#general", or "a DM".
    pub fn room_label(&self, room: &PlatformRoomId) -> String {
        match self.rooms.get(room) {
            Some(r) if r.is_dm() => "a DM".to_owned(),
            Some(r) => format!("#{}", r.name(room)),
            None => format!("#{room}"),
        }
    }

    /// The DM room with `user`, once the connector has synced one.
    pub fn dm_room_of(&self, user: &PlatformUserId) -> Option<PlatformRoomId> {
        self.rooms
            .iter()
            .find(|(_, r)| r.is_dm() && r.members.contains_key(user))
            .map(|(id, _)| id.clone())
    }

    /// Whether the agent is watching rather than speaking unprompted.
    pub fn is_observing(&self) -> bool {
        self.observe.is_some()
    }

    fn next_id(&mut self, tag: &str) -> String {
        self.next_seq += 1;
        format!("{}-{}{}", self.id, tag, self.next_seq)
    }

    pub(crate) fn mint_action(&mut self) -> ActionId {
        ActionId::new(self.next_id("a"))
    }

    pub(crate) fn mint_request(&mut self) -> RequestId {
        RequestId::new(self.next_id("r"))
    }

    pub(crate) fn mint_timer(&mut self) -> TimerId {
        TimerId::new(self.next_id("t"))
    }

    pub(crate) fn mint_fact(&mut self) -> FactId {
        FactId::new(self.next_id("f"))
    }

    pub(crate) fn mint_proposal(&mut self) -> ProposalId {
        ProposalId::new(self.next_id("p"))
    }

    pub(crate) fn mint_decision(&mut self) -> DecisionId {
        DecisionId::new(self.next_id("d"))
    }

    pub(crate) fn mint_commitment(&mut self) -> CommitmentId {
        CommitmentId::new(self.next_id("c"))
    }

    pub(crate) fn mint_ledger(&mut self) -> LedgerId {
        LedgerId::new(self.next_id("l"))
    }

    pub(crate) fn mint_settlement(&mut self) -> SettlementId {
        SettlementId::new(self.next_id("s"))
    }

    pub(crate) fn mint_recurrence(&mut self) -> RecurrenceId {
        RecurrenceId::new(self.next_id("rc"))
    }

    /// Notes that `user` posted or reacted at `at`.
    pub(crate) fn saw(&mut self, user: &PlatformUserId, at: Instant) {
        if self.is_me(user) {
            return;
        }
        let seen = self.last_seen.entry(user.clone()).or_insert(at);
        *seen = (*seen).max(at);
    }

    /// Drops whatever has expired by `now`. Runs at the start of every step.
    pub(crate) fn prune(&mut self, now: Instant) {
        self.deferrals.retain(|_, d| d.expires > now);
        self.pending_actions
            .retain(|_, a| now.seconds_since(a.at) < PENDING_ACTION_TTL_SECONDS);
        self.pending_dms
            .retain(|_, d| now.seconds_since(d.at) < PENDING_ACTION_TTL_SECONDS);
        self.shushed.retain(|_, until| *until > now);
        self.scoped_leads
            .retain(|s| s.expires.is_none_or(|e| e > now));
        self.budget.prune(now);
        // A "remember that" has its own guard; background reads just lapse.
        self.extractions
            .retain(|p| p.guard.is_some() || now.seconds_since(p.asked_at) < EXTRACT_TTL_SECONDS);
        self.patterns.decay(now);
        self.last_seen
            .retain(|_, at| now.seconds_since(*at) < ACTIVE_SECONDS);
        self.pending_times
            .retain(|p| p.guard.is_some() || now.seconds_since(p.asked_at) < TIME_TTL_SECONDS);
        let proposals = &self.proposals;
        self.pending_polls
            .retain(|_, p| proposals.get(p).is_some_and(Proposal::is_open));
    }
}

impl RoomState {
    pub fn is_dm(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.kind == RoomKind::Dm)
    }

    pub fn name(&self, id: &PlatformRoomId) -> String {
        self.info
            .as_ref()
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn is_own(&self, id: &PlatformMessageId) -> bool {
        self.own_messages.contains(id)
    }

    pub(crate) fn remember_own(&mut self, id: PlatformMessageId) {
        if !self.is_own(&id) {
            self.own_messages.push_back(id);
            while self.own_messages.len() > OWN_MESSAGES_KEPT {
                self.own_messages.pop_front();
            }
        }
    }

    /// Appends a message, or replaces the one with the same id in place.
    pub(crate) fn push(&mut self, message: Message) {
        if let Some(slot) = self.messages.iter_mut().find(|m| m.id == message.id) {
            *slot = message;
            return;
        }
        self.messages.push_back(message);
        self.trim();
    }

    /// Replaces a message by id; an edit to a message no longer held is dropped.
    pub(crate) fn replace(&mut self, message: Message) -> bool {
        match self.messages.iter_mut().find(|m| m.id == message.id) {
            Some(slot) => {
                *slot = message;
                true
            }
            None => false,
        }
    }

    pub(crate) fn remove(&mut self, id: &PlatformMessageId) {
        self.messages.retain(|m| &m.id != id);
    }

    pub fn score_of(&self, id: &PlatformMessageId) -> Option<&Score> {
        self.scores.iter().find(|(m, _)| m == id).map(|(_, s)| s)
    }

    /// Stores or replaces a message's score, keeping the last `SCORES_KEPT`.
    pub(crate) fn set_score(&mut self, id: &PlatformMessageId, score: Score) {
        if let Some(slot) = self.scores.iter_mut().find(|(m, _)| m == id) {
            slot.1 = score;
            return;
        }
        self.scores.push_back((id.clone(), score));
        while self.scores.len() > SCORES_KEPT {
            self.scores.pop_front();
        }
    }

    pub fn trace_of(&self, id: &PlatformMessageId) -> Option<&Trace> {
        self.traces.iter().find(|(m, _)| m == id).map(|(_, t)| t)
    }

    /// The trace of the agent's most recent message here.
    pub fn last_trace(&self) -> Option<&Trace> {
        self.traces.back().map(|(_, t)| t)
    }

    pub(crate) fn push_trace(&mut self, id: PlatformMessageId, trace: Trace) {
        self.traces.push_back((id, trace));
        while self.traces.len() > TRACES_KEPT {
            self.traces.pop_front();
        }
    }

    /// Whether a human other than the asker has posted here since `question`.
    /// A question no longer held counts as answered.
    pub(crate) fn answered_since(
        &self,
        question: &PlatformMessageId,
        is_human: impl Fn(&PlatformUserId) -> bool,
    ) -> bool {
        let Some(at) = self.messages.iter().position(|m| &m.id == question) else {
            return true;
        };
        let asker = &self.messages[at].author;
        self.messages
            .iter()
            .skip(at + 1)
            .any(|m| &m.author != asker && is_human(&m.author))
    }

    /// Puts older history, oldest first, in front of what is held.
    pub(crate) fn prepend(&mut self, history: Vec<Message>) {
        for message in history.into_iter().rev() {
            if !self.messages.iter().any(|m| m.id == message.id) {
                self.messages.push_front(message);
            }
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.messages.len() > RING_CAPACITY {
            self.messages.pop_front();
        }
    }
}
