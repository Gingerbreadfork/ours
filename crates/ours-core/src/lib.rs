//! The Ours engine. A group's state and the pure `step` fold over its
//! inputs: no I/O, no clock reads, no randomness. Every outside fact arrives
//! as an `Input` and everything the engine wants done leaves as an `Effect`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod action;
pub mod addressing;
pub mod attention;
pub mod authority;
pub mod away;
pub mod backfill;
pub mod budget;
pub mod charter;
pub mod chime;
pub mod commitment;
pub mod decide;
pub mod decision;
pub mod digest;
pub mod effect;
pub mod error;
pub mod export;
pub mod extract;
pub mod hold;
pub mod ics;
pub mod input;
pub mod leads;
pub mod mail;
pub mod memory;
pub mod observe;
pub mod patterns;
pub mod persona;
pub mod pin;
pub mod plan;
pub mod profile;
pub mod proposal;
pub mod reactions;
pub mod recall;
pub mod recur;
pub mod retention;
pub mod rights;
pub mod schema;
pub mod sentence;
pub mod settings;
pub mod setup;
pub mod sim;
pub mod state;
pub mod step;
pub mod tab;
pub mod time;
pub mod timers;
pub mod trace;
pub mod triage;
pub mod undo;
pub mod zone;

pub use action::{
    ACTIONS_KEPT, ActionClass, ActionRecord, ActionStatus, CatalogueEntry, Origin, Recording, Undo,
    catalogue, may_leave_unprompted, tier_of,
};
pub use addressing::is_addressed;
pub use authority::{
    ActionKind, ActionSet, Disposition, FilteredPlan, Scope, ScopedLead, dispose, filter_actions,
    permitted, resolve_role,
};
pub use away::{away, is_availability};
pub use backfill::Backfill;
pub use budget::{
    Budget, BudgetCut, can_speak_unprompted, record_chime, record_unprompted, stop_today,
};
pub use charter::{render_charter, settings_summary};
pub use commitment::{Commitment, CommitmentId, CommitmentStatus, ForWhom};
pub use decide::{PendingTime, TimeTarget, default_rule};
pub use decision::{
    DecidedOption, Decision, Entry, Outcome, Said, recall_decisions, render_decisions, upcoming,
};
pub use digest::{day_line, digest};
pub use effect::{ChatTurn, Effect, ModelRequest, TurnRole};
pub use error::CoreError;
pub use export::{Export, ExportData, ExportFact, ExportMember, ExportRoom, render_export};
pub use extract::{ExtractWhy, PendingExtract};
pub use hold::{Hold, Route, route};
pub use ics::{member_feed_token, render_ics};
pub use input::{Command, Input};
pub use leads::DECLINE_PREFIX;
pub use mail::{address_known, addresses_in, cancellation, looks_like_address};
pub use memory::{Fact, FactId, FactScope, Place, Retraction, Source};
pub use observe::{Observe, observers};
pub use patterns::{Patterns, room_active, usual_hour};
pub use persona::{Persona, Starter, starter, starters, system_prompt};
pub use pin::Pinned;
pub use plan::{Plan, PlannedAction, plan_schema};
pub use profile::{ClockTime, Jobs, LeadsRule, Outbound, Profile, QuietHours, WorkingHours};
pub use proposal::{
    Ballot, CloseRule, DecisionId, Held, Proposal, ProposalId, ProposalKind, ProposalOption,
    ProposalStatus,
};
pub use reactions::{Meaning, emoji_for, meaning_of};
pub use recall::recall;
pub use recur::{RecurKind, Recurrence, RecurrenceId, next_after};
pub use retention::retention_days;
pub use rights::{FORGOTTEN_TRIGGER, MemoryChange, dump};
pub use sentence::UndoWhat;
pub use settings::SettingChange;
pub use setup::{Setup, Stage};
pub use state::{
    Answering, Deferral, DeferredChange, DmFollowUp, FollowUp, GroupState, Hand, LeadChange, Me,
    PendingAction, PendingDm, PendingRequest, RoomState,
};
pub use step::step;
pub use tab::{
    LedgerId, LedgerLine, LineKind, Settlement, SettlementId, SettlementStatus, Split, Transfer,
    balances, settle,
};
pub use time::{Resolved, resolve_time_rules};
pub use timers::{DueAt, NudgeAt, Timer, TimerPurpose};
pub use trace::Trace;
pub use triage::rules_score;
pub use zone::{Zone, zone_of};
