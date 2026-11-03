//! Proposals from opening to close (DESIGN.md section 10). A direct ask or
//! a forming plan opens one; reactions, native polls and plain lines vote;
//! nudges go out at the halfway point and near the deadline; the close rule
//! decides. Every close leaves a record and, for a dated outcome, a
//! calendar entry.

use crate::action::{self, ActionClass, ActionRecord, Origin, Recording, Undo, quoted};
use crate::authority::resolve_role;
use crate::budget::{can_speak_unprompted, record_unprompted};
use crate::decision::{
    self, DecidedOption, Decision, Entry, Outcome, Said, duration, latest_decision,
    recall_decisions,
};
use crate::effect::{Effect, queue, react, send, send_as, send_dm};
use crate::leads::{decline_line, join_names};
use crate::memory::Place;
use crate::observe;
use crate::patterns::usual_hour;
use crate::proposal::{
    Ballot, CloseRule, DecisionId, Held, MAX_OPTIONS, NUMBERS, Named, Offered, Proposal,
    ProposalId, ProposalKind, ProposalOption, ProposalStatus, active_members, capitalise,
    emoji_option, named, named_labels, offered, question, reason_after,
};
use crate::reactions::{self, Meaning};
use crate::sentence::{DecisionSentence, RuleWord};
use crate::settings::addressed_trigger;
use crate::state::{FollowUp, GroupState, PendingAction};
use crate::time::{self, resolve_time_rules, wants_resolver};
use crate::timers::{self, NudgeAt, TimerPurpose};
use crate::trace::{Trace, quote, shorten};
use crate::triage;
use crate::zone::{self, zone_of};
use ours_types::{
    ActionId, Instant, Message, Outbound, OutboundAction, PlatformMessageId, PlatformRoomId,
    PlatformUserId, Role, RoomKind, Score, TimeMeaning, TimeRequest, TimeResolution, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tracing::debug;

const HOUR: i64 = 60 * 60;
/// How long a proposal stays open unless its event is sooner.
pub const DEADLINE_SECONDS: i64 = 24 * HOUR;
/// The shortest a proposal stays open.
pub const SHORTEST_SECONDS: i64 = 2 * HOUR;
/// How long a deadline sentence waits for the resolver.
pub const TIME_GUARD_SECONDS: i64 = 60;
/// Resolver requests nobody answered are dropped after this long.
pub const TIME_TTL_SECONDS: i64 = HOUR;
/// Proposals listed in one "where are we".
const TALLIES: usize = 3;
/// Lines in one "what's open".
const OPEN_LINES: usize = 6;

pub const NOTHING_OPEN: &str = "Nothing's open here.";
pub const NOTHING_OPEN_ANYWHERE: &str = "Nothing's open.";
pub const POLL_IN_A_ROOM: &str = "Ask me in the room the poll is for.";
pub const TWO_OPTIONS: &str = "Give me at least two options, like: poll thai or pizza.";
pub const UNREADABLE_TIME: &str = "I couldn't tell when that is; try something like friday 6pm.";

/// Why a decision's announcement is not undone.
const DECISION_STANDS: &str = "a decision stands; say re-vote to open it again";
const OPEN_RULE: &str = "a direct ask opens a proposal with a ballot";
const PLAN_RULE: &str =
    "a plan forming with two or more options opens a proposal, within the budget";
const VOTE_RULE: &str = "a line that names one option is a vote";
const NUDGE_RULE: &str = "one nudge at the halfway point and one near the deadline, never a third";
const LATE_RULE: &str = "a late can't-do after close adjusts the headcount and offers a re-vote";

/// A time expression waiting on the resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingTime {
    pub room: PlatformRoomId,
    pub message: PlatformMessageId,
    /// The words sent to the resolver.
    pub text: String,
    pub target: TimeTarget,
    pub asked_at: Instant,
    /// Armed when someone is waiting for a reply.
    #[serde(default)]
    pub guard: Option<TimerId>,
}

/// What a resolved time is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TimeTarget {
    /// The date of one of a proposal's options.
    Option { proposal: ProposalId, index: usize },
    /// A lead's new deadline, answered in reply to their message.
    Deadline {
        proposal: ProposalId,
        reply_to: PlatformMessageId,
    },
}

/// The rule new proposals close on: the group's own, else its kind's.
pub fn default_rule(state: &GroupState) -> CloseRule {
    state
        .decision_rule
        .clone()
        .unwrap_or_else(|| CloseRule::for_kind(state.kind))
}

/// What opens a proposal.
pub(crate) struct Opening {
    pub room: PlatformRoomId,
    pub thread_root: PlatformMessageId,
    pub reply_to: Option<PlatformMessageId>,
    pub topic: Option<String>,
    pub options: Vec<Offered>,
    pub kind: ProposalKind,
    pub organiser: Option<PlatformUserId>,
    pub rule: CloseRule,
    /// A ballot even when the rule is lead decides, because someone asked.
    pub ballot: bool,
    /// The message the options came from, for the resolver.
    pub source: PlatformMessageId,
    /// The zone the options were read in.
    pub zone: String,
    pub reopens: Option<DecisionId>,
    pub trace: Trace,
}

/// Opens a proposal, posts its ballot and arms its timers.
pub(crate) fn open(
    state: &mut GroupState,
    o: Opening,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> ProposalId {
    let id = state.mint_proposal();
    let labels: Vec<String> = o.options.iter().map(|x| x.label.clone()).collect();
    let options: Vec<ProposalOption> = o
        .options
        .iter()
        .map(|x| ProposalOption::new(x.label.clone(), x.when.clone(), x.echo.clone()))
        .collect();
    let question = match o.kind {
        ProposalKind::Choice => question(o.topic.as_deref(), &labels),
        ProposalKind::Attendance => format!(
            "Who's in for {}?",
            options
                .first()
                .map_or_else(|| "it".to_owned(), |x| attendance_phrase(state, x, now))
        ),
    };
    let deadline = deadline_for(state, &options, now);
    let ballot = if o.ballot || o.rule.needs_ballot() {
        ballot_for(state, &o.room, o.kind, options.len())
    } else {
        Ballot::None
    };
    let proposal = Proposal {
        id: id.clone(),
        room: o.room.clone(),
        thread_root: o.thread_root,
        question,
        topic: o.topic,
        options,
        opened_at: now,
        deadline,
        rule: o.rule,
        organiser: o.organiser.clone(),
        ballot,
        nudged: Default::default(),
        status: ProposalStatus::Open,
        kind: o.kind,
        abstained: Default::default(),
        remarks: Vec::new(),
        quotes: Default::default(),
        reacted: Default::default(),
        lapsed: 0,
        held: None,
        reopens: o.reopens,
    };
    let notice = format!(
        "Opened a proposal in {}: {} Closes {}, {}.",
        state.room_label(&o.room),
        proposal.question,
        zone::stamp(deadline, &state.timezone),
        proposal.rule.name()
    );
    let summary = match proposal.ballot {
        Ballot::None => format!("the proposal {}", quoted(&proposal.question, 80)),
        _ => format!("the poll {}", quoted(&proposal.question, 80)),
    };
    state.proposals.insert(id.clone(), proposal);
    let origin = Origin {
        requested_by: o.organiser.clone(),
        approved_by: None,
        instruction: o.reply_to.clone(),
    };
    let record = ActionRecord::new(
        state.mint_action(),
        ActionClass::Poll,
        o.room.clone(),
        now,
        summary,
        Undo::EndPoll {
            proposal: id.clone(),
        },
    )
    .from_origin(&origin);
    action::push(state, record);
    post_ballot(state, &id, o.reply_to, now, o.trace, fx);
    arm(state, &id, now, fx);
    for (index, x) in o.options.iter().enumerate() {
        if x.unresolved {
            let pending = PendingTime {
                room: o.room.clone(),
                message: o.source.clone(),
                text: x.label.clone(),
                target: TimeTarget::Option {
                    proposal: id.clone(),
                    index,
                },
                asked_at: now,
                guard: None,
            };
            ask_time(state, pending, &o.zone, false, fx);
        }
    }
    fx.push(Effect::Notice(notice));
    id
}

fn ballot_for(state: &GroupState, room: &PlatformRoomId, kind: ProposalKind, n: usize) -> Ballot {
    let polls = state.me_in(room).is_some_and(|m| m.capabilities.polls);
    match kind {
        ProposalKind::Attendance => Ballot::Reactions {
            message: None,
            emojis: vec![reactions::emoji_for(state, room, Meaning::In).to_owned()],
        },
        ProposalKind::Choice if polls && (2..=MAX_OPTIONS).contains(&n) => {
            Ballot::Native { message: None }
        }
        ProposalKind::Choice => Ballot::Reactions {
            message: None,
            emojis: NUMBERS
                .iter()
                .take(n.min(MAX_OPTIONS))
                .map(|e| (*e).to_owned())
                .collect(),
        },
    }
}

/// 24 hours, or half the time to the soonest option when that is sooner,
/// never under two hours; pulled back out of quiet hours.
fn deadline_for(state: &GroupState, options: &[ProposalOption], now: Instant) -> Instant {
    let mut seconds = DEADLINE_SECONDS;
    let soonest = options
        .iter()
        .filter_map(|o| o.when.as_ref())
        .filter_map(|w| time::event_start(w, &state.timezone))
        .min();
    if let Some(at) = soonest {
        seconds = seconds.min((at.seconds_since(now) / 2).max(SHORTEST_SECONDS));
    }
    avoid_quiet(state, now, now.plus_seconds(seconds))
}

/// Moves `at` back to where the group's quiet hours start, if it falls in
/// them and that still leaves two hours.
fn avoid_quiet(state: &GroupState, now: Instant, at: Instant) -> Instant {
    let quiet = state.profile.quiet_hours;
    let hour_of = |t: Instant| u8::try_from(zone::local(t, &state.timezone).hour()).unwrap_or(0);
    if !quiet.contains(hour_of(at)) {
        return at;
    }
    let local = zone::local(at, &state.timezone);
    let mut t = at.plus_seconds(-(i64::from(local.minute()) * 60 + i64::from(local.second())));
    for _ in 0..24 {
        if !quiet.contains(hour_of(t.plus_seconds(-1))) {
            break;
        }
        t = t.plus_seconds(-HOUR);
    }
    if t.seconds_since(now) >= SHORTEST_SECONDS {
        t
    } else {
        at
    }
}

fn post_ballot(
    state: &mut GroupState,
    pid: &ProposalId,
    reply_to: Option<PlatformMessageId>,
    now: Instant,
    trace: Trace,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get(pid) else {
        return;
    };
    let room = p.room.clone();
    match &p.ballot {
        Ballot::Native { .. } => {
            let hours = ((p.deadline.seconds_since(now) + HOUR - 1) / HOUR).clamp(1, 768);
            let options = p
                .options
                .iter()
                .map(|o| {
                    shorten(
                        &format!("{}{}", o.display(), away_mark(state, &p.room, o)),
                        55,
                    )
                })
                .collect();
            let action = OutboundAction::CreatePoll {
                room,
                question: shorten(&p.question, 300),
                options,
                multiple: false,
                hours: u32::try_from(hours).unwrap_or(24),
            };
            let id = state.mint_action();
            state.pending_polls.insert(id.clone(), pid.clone());
            fx.push(Effect::Send(Outbound { id, action }));
        }
        Ballot::Reactions { .. } => {
            let text = ballot_text(state, p, now);
            let pending = PendingAction {
                room,
                text,
                reply_to,
                at: now,
                then: Some(FollowUp::Ballot {
                    proposal: pid.clone(),
                }),
                trace: Some(trace),
                record: Recording::Unrecorded,
            };
            queue(state, fx, pending);
        }
        Ballot::None => {
            let text = marker_line(p);
            let recording = match poll_record(state, pid) {
                Some(action) => Recording::Line { action },
                None => Recording::Own,
            };
            send_as(state, fx, &room, text, reply_to, now, trace, recording);
        }
    }
}

/// The action record of a proposal's ballot.
fn poll_record(state: &GroupState, pid: &ProposalId) -> Option<ActionId> {
    action::latest_where(
        state,
        |r| matches!(&r.undo, Undo::EndPoll { proposal } if proposal == pid),
    )
}

/// The reaction ballot: the question, then one numbered line per option.
fn ballot_text(state: &GroupState, p: &Proposal, now: Instant) -> String {
    match p.kind {
        ProposalKind::Attendance => {
            let what = p
                .options
                .first()
                .map_or_else(|| "it".to_owned(), |o| attendance_phrase(state, o, now));
            let away = p
                .options
                .first()
                .map_or_else(String::new, |o| away_mark(state, &p.room, o));
            let yes = reactions::emoji_for(state, &p.room, Meaning::In);
            format!("🗳️ Who's in for {what}{away}? React {yes} if you are.")
        }
        ProposalKind::Choice => {
            let mut lines = vec![format!("🗳️ {}", p.question)];
            for (i, o) in p.options.iter().enumerate() {
                lines.push(format!(
                    "{} {}{}",
                    NUMBERS.get(i).unwrap_or(&""),
                    o.display(),
                    away_mark(state, &p.room, o)
                ));
            }
            lines.join("\n")
        }
    }
}

/// " (Alex away)" when the option's date falls in a room member's away span.
fn away_mark(state: &GroupState, room: &PlatformRoomId, o: &ProposalOption) -> String {
    let Some((start, end)) = o.when.as_ref().and_then(time::bounds) else {
        return String::new();
    };
    let Some(r) = state.rooms.get(room) else {
        return String::new();
    };
    let mut names: Vec<String> = r
        .members
        .values()
        .filter(|m| !m.is_bot && !state.is_me(&m.id))
        .filter(|m| crate::away::away_during(state, &m.id, start, end))
        .map(|m| m.display_name.clone())
        .collect();
    if names.is_empty() {
        return String::new();
    }
    names.sort();
    format!(" ({} away)", join_names(&names, "and"))
}

/// "Saturday the 14th", or "drinks on saturday, Saturday the 14th" when the
/// option is more than its date.
fn attendance_phrase(state: &GroupState, o: &ProposalOption, now: Instant) -> String {
    let lower = lower_first(&o.label);
    match &o.echo {
        Some(echo) if offered(&o.label, &state.timezone, now).is_some_and(|x| x.when.is_some()) => {
            echo.clone()
        }
        Some(echo) => format!("{lower}, {echo}"),
        None => lower,
    }
}

/// The first letter lower-cased, unless the word looks like an acronym.
fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if text.chars().nth(1).is_some_and(char::is_uppercase) {
        return text.to_owned();
    }
    first.to_lowercase().chain(chars).collect()
}

/// "📌 Open: Acme or Beacon. Lead decides; I'll keep the thread."
fn marker_line(p: &Proposal) -> String {
    let options =
        crate::proposal::or_list(p.options.iter().map(|o| o.display().to_owned()).collect());
    format!("📌 Open: {options}. Lead decides; I'll keep the thread.")
}

/// The ballot message's id came back: react with each option's emoji.
pub(crate) fn ballot_posted(
    state: &mut GroupState,
    pid: &ProposalId,
    message: &PlatformMessageId,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get_mut(pid) else {
        return;
    };
    let room = p.room.clone();
    let emojis = match &mut p.ballot {
        Ballot::Reactions {
            message: slot,
            emojis,
        } => {
            *slot = Some(message.clone());
            emojis.clone()
        }
        _ => return,
    };
    if let Some(record) = poll_record(state, pid) {
        action::attach(state, &record, message, false);
    }
    for emoji in emojis {
        react(state, fx, &room, message, &emoji);
    }
}

/// A native poll's result: keep its id, or fall back to a reaction ballot.
/// Returns whether the action was a poll.
pub(crate) fn poll_created(
    state: &mut GroupState,
    action: &ActionId,
    ok: bool,
    message: Option<PlatformMessageId>,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let Some(pid) = state.pending_polls.remove(action) else {
        return false;
    };
    let Some(p) = state.proposals.get_mut(&pid) else {
        return true;
    };
    match message.filter(|_| ok) {
        Some(m) => {
            p.ballot = Ballot::Native {
                message: Some(m.clone()),
            };
            let room = p.room.clone();
            if let Some(record) = poll_record(state, &pid) {
                action::attach(state, &record, &m, false);
            }
            state.room_mut(&room).remember_own(m);
        }
        None => {
            let n = p.options.len();
            p.ballot = Ballot::Reactions {
                message: None,
                emojis: NUMBERS.iter().take(n).map(|e| (*e).to_owned()).collect(),
            };
            let reply_to = Some(p.thread_root.clone());
            let room = p.room.clone();
            let label = state.room_label(&room);
            let trace = Trace::new(
                format!("a native poll in {label} could not be made"),
                "a reaction ballot stands in when a poll cannot be made",
                now,
            );
            post_ballot(state, &pid, reply_to, now, trace, fx);
            fx.push(Effect::Notice(
                "The native poll failed; posted a reaction ballot instead.".to_owned(),
            ));
        }
    }
    true
}

fn late_nudge(opened: Instant, deadline: Instant) -> Instant {
    let window = deadline.seconds_since(opened).max(0);
    deadline.plus_seconds(-(window / 8).max(30 * 60))
}

/// Arms the halfway and late nudges, when there is a ballot, and the close.
fn arm(state: &mut GroupState, pid: &ProposalId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(p) = state.proposals.get(pid) else {
        return;
    };
    let (opened, deadline) = (p.opened_at, p.deadline);
    let nudges = p.rule.needs_ballot() && p.ballot != Ballot::None && p.lapsed == 0;
    let half = opened.plus_seconds(deadline.seconds_since(opened).max(0) / 2);
    let late = late_nudge(opened, deadline);
    if nudges {
        if half > now {
            let purpose = TimerPurpose::Nudge {
                proposal: pid.clone(),
                which: NudgeAt::Half,
                user: None,
            };
            timers::arm(state, fx, half, purpose);
        }
        if late > now && late > half {
            let purpose = TimerPurpose::Nudge {
                proposal: pid.clone(),
                which: NudgeAt::Deadline,
                user: None,
            };
            timers::arm(state, fx, late, purpose);
        }
    }
    let purpose = TimerPurpose::Close {
        proposal: pid.clone(),
    };
    timers::arm(state, fx, deadline, purpose);
}

fn disarm(state: &mut GroupState, pid: &ProposalId, fx: &mut Vec<Effect>) {
    timers::cancel_where(state, fx, |t| match t {
        TimerPurpose::Nudge { proposal, .. } | TimerPurpose::Close { proposal } => proposal == pid,
        _ => false,
    });
}

/// Moves a vote. Returns whether anything changed.
fn cast(
    state: &mut GroupState,
    pid: &ProposalId,
    user: &PlatformUserId,
    option: Option<usize>,
) -> bool {
    let Some(p) = state.proposals.get_mut(pid).filter(|p| p.is_open()) else {
        return false;
    };
    if p.vote_of(user) == option {
        return false;
    }
    p.set_vote(user, option);
    true
}

/// Votes as the rule counts them: only the named deciders' for that rule.
fn counted(p: &Proposal) -> Vec<u32> {
    match &p.rule {
        CloseRule::NamedDeciders(set) => p
            .options
            .iter()
            .map(|o| u32::try_from(o.votes.intersection(set).count()).unwrap_or(0))
            .collect(),
        _ => p.counts(),
    }
}

/// Whether the outcome can no longer change under the rule.
fn decided_early(state: &GroupState, p: &Proposal, now: Instant) -> bool {
    let electorate: BTreeSet<PlatformUserId> = match &p.rule {
        CloseRule::NamedDeciders(set) => set.clone(),
        _ => {
            let mut all = active_members(state, &p.room, now);
            all.extend(p.voters());
            all
        }
    };
    let counts = counted(p);
    let mut sorted = counts.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    let top = sorted.first().copied().unwrap_or(0);
    let second = sorted.get(1).copied().unwrap_or(0);
    let remaining = electorate
        .iter()
        .filter(|u| p.vote_of(u).is_none() && !p.abstained.contains_key(*u))
        .count();
    let remaining = u32::try_from(remaining).unwrap_or(u32::MAX);
    let total = u32::try_from(electorate.len()).unwrap_or(u32::MAX);
    if top == 0 {
        return false;
    }
    if p.kind == ProposalKind::Attendance {
        return remaining == 0;
    }
    match &p.rule {
        CloseRule::MajorityOfActive => top * 2 > total,
        CloseRule::MajorityOfVoters | CloseRule::WhoeverIsComing => {
            remaining == 0 || top - second > remaining
        }
        CloseRule::Unanimous => remaining == 0 && counts.iter().filter(|c| **c > 0).count() == 1,
        CloseRule::NamedDeciders(_) => top * 2 > total || remaining == 0,
        CloseRule::LeadDecides => false,
    }
}

/// The option with the most votes. Ties go to the organiser's choice, then
/// to the earliest date, then to the first listed.
fn winner(state: &GroupState, p: &Proposal, counts: &[u32]) -> Option<usize> {
    let top = *counts.iter().max()?;
    if top == 0 {
        return None;
    }
    let tied: Vec<usize> = (0..counts.len()).filter(|i| counts[*i] == top).collect();
    if let [one] = tied.as_slice() {
        return Some(*one);
    }
    if let Some(choice) = p
        .organiser
        .as_ref()
        .and_then(|o| p.vote_of(o))
        .filter(|i| tied.contains(i))
    {
        return Some(choice);
    }
    let dated = tied
        .iter()
        .filter_map(|i| {
            p.options
                .get(*i)?
                .when
                .as_ref()
                .and_then(|w| time::event_start(w, &state.timezone))
                .map(|at| (at, *i))
        })
        .min();
    dated.map(|(_, i)| i).or_else(|| tied.first().copied())
}

/// Closes early when the rule says the outcome can no longer change.
fn settle(state: &mut GroupState, pid: &ProposalId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(p) = state.proposals.get(pid).filter(|p| p.is_open()) else {
        return;
    };
    if !decided_early(state, p, now) {
        return;
    }
    let Some(w) = winner(state, p, &counted(p)) else {
        return;
    };
    close(state, pid, w, Closer::Count, now, fx);
}

/// How a proposal came to close.
enum Closer {
    /// By its rule, on the votes held.
    Count,
    /// A native poll ended with these counts.
    Counts(Vec<u32>),
    /// A lead chose, in `message` when their words did.
    Decider {
        user: PlatformUserId,
        message: Option<PlatformMessageId>,
        reason: Option<String>,
    },
}

/// Closes a proposal on `winner`: the record, the calendar entry, and one
/// line, held while another proposal in the thread is still open.
fn close(
    state: &mut GroupState,
    pid: &ProposalId,
    winner: usize,
    closer: Closer,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get(pid).filter(|p| p.is_open()).cloned() else {
        return;
    };
    let did = state.mint_decision();
    if let Some(slot) = state.proposals.get_mut(pid) {
        slot.status = ProposalStatus::Closed(did.clone());
    }
    disarm(state, pid, fx);
    if !matches!(closer, Closer::Counts(_)) {
        end_poll(state, &p, fx);
    }
    let counts = match &closer {
        Closer::Counts(c) => c.clone(),
        _ => counted(&p),
    };
    let rule_text = match &closer {
        Closer::Decider { .. } => "a lead's choice decides a proposal".to_owned(),
        _ => format!("the votes settled it under {}", p.rule.name()),
    };
    // A lead who picks the outcome has changed the rule for this one.
    let rule = match &closer {
        Closer::Decider { .. } => CloseRule::LeadDecides,
        _ => p.rule.clone(),
    };
    if rule != p.rule {
        fx.push(Effect::Notice(format!(
            "A lead decided {} in place of {}.",
            p.title(),
            p.rule.name()
        )));
    }
    let (outcome, closed_by, reason) = match closer {
        Closer::Decider {
            user,
            message,
            reason,
        } => (Outcome::Decider { user }, message, reason),
        Closer::Count | Closer::Counts(_) => (Outcome::Count { counts }, None, None),
    };
    let lines = said(state, &p, closed_by.as_ref());
    let decision = Decision {
        id: did.clone(),
        proposal: pid.clone(),
        room: p.room.clone(),
        question: p.question.clone(),
        topic: p.topic.clone(),
        kind: p.kind,
        options: p
            .options
            .iter()
            .map(|o| DecidedOption {
                label: o.label.clone(),
                when: o.when.clone(),
                echo: o.echo.clone(),
                votes: o.votes.clone(),
            })
            .collect(),
        winner,
        rule,
        outcome,
        reason,
        said: lines,
        closed_by,
        thread_root: p.thread_root.clone(),
        opened_at: p.opened_at,
        closed_at: now,
        open_seconds: now.seconds_since(p.opened_at),
    };
    let mut entry_record = None;
    if let Some(chosen) = p.options.get(winner)
        && let Some(when) = chosen.when.clone().filter(|w| *w != TimeMeaning::None)
    {
        if let Some(old) = &p.reopens {
            state.calendar.retain(|e| &e.decision != old);
        }
        let attendees = match decision.outcome {
            Outcome::Count { .. } => chosen.votes.clone(),
            Outcome::Decider { .. } => BTreeSet::new(),
        };
        let entry = Entry {
            when,
            label: p.topic.clone().unwrap_or_else(|| chosen.label.clone()),
            decision: did.clone(),
            attendees,
            room: Some(p.room.clone()),
            echo: chosen.echo.clone(),
            out: BTreeSet::new(),
        };
        let echo = decision::entry_echo(state, &entry, now);
        let summary = if echo.is_empty() {
            format!("the calendar entry {}", entry.label)
        } else {
            format!("the calendar entry {}, {echo}", entry.label)
        };
        state.calendar.push(entry);
        let id = state.mint_action();
        let record = ActionRecord::new(
            id.clone(),
            ActionClass::CalendarEntry,
            p.room.clone(),
            now,
            summary,
            Undo::RemoveEntry {
                decision: did.clone(),
            },
        );
        action::push(state, record);
        entry_record = Some(id);
    }
    let line = outcome_line(state, &decision);
    let notice = format!(
        "Decided in {}: {} {}.",
        state.room_label(&p.room),
        decision.question,
        line.part
    );
    let trace = Trace::new(
        format!("{} in {}", p.question, state.room_label(&p.room)),
        rule_text,
        now,
    );
    crate::recur::on_closed(state, pid, decision.open_seconds);
    state.decisions.insert(did, decision);
    let recording = match entry_record {
        Some(action) => Recording::Line { action },
        None => Recording::Note {
            why: DECISION_STANDS.to_owned(),
        },
    };
    announce(state, pid, line, now, trace, recording, fx);
    fx.push(Effect::Notice(notice));
}

fn end_poll(state: &mut GroupState, p: &Proposal, fx: &mut Vec<Effect>) {
    if let Ballot::Native { message: Some(m) } = &p.ballot {
        let action = OutboundAction::EndPoll {
            room: p.room.clone(),
            message: m.clone(),
        };
        fx.push(Effect::Send(Outbound {
            id: state.mint_action(),
            action,
        }));
    }
}

/// Who said what, in the order it was said.
fn said(state: &GroupState, p: &Proposal, closed_by: Option<&PlatformMessageId>) -> Vec<Said> {
    let mut lines: Vec<(PlatformUserId, PlatformMessageId, Option<usize>)> = Vec::new();
    for (i, o) in p.options.iter().enumerate() {
        for (user, message) in &o.views {
            lines.push((user.clone(), message.clone(), Some(i)));
        }
    }
    let others: Vec<(PlatformUserId, PlatformMessageId)> = p
        .remarks
        .iter()
        .cloned()
        .chain(p.abstained.iter().map(|(u, m)| (u.clone(), m.clone())))
        .collect();
    for (user, message) in others {
        if !lines.iter().any(|(_, m, _)| *m == message) {
            lines.push((user, message, None));
        }
    }
    let ring = state.rooms.get(&p.room).map(|r| &r.messages);
    let position = |m: &PlatformMessageId| {
        ring.and_then(|r| r.iter().position(|x| &x.id == m))
            .unwrap_or(usize::MAX)
    };
    lines.sort_by(|a, b| position(&a.1).cmp(&position(&b.1)).then(a.1.cmp(&b.1)));
    if let Some(m) = closed_by
        && !lines.iter().any(|(_, x, _)| x == m)
        && let Some(author) = ring
            .and_then(|r| r.iter().find(|x| &x.id == m))
            .map(|x| x.author.clone())
    {
        lines.push((author, m.clone(), None));
    }
    lines
        .into_iter()
        .map(|(user, message, option)| Said {
            name: state.display_name(&p.room, &user),
            text: p
                .quotes
                .get(&message)
                .cloned()
                .or_else(|| {
                    ring.and_then(|r| r.iter().find(|x| x.id == message))
                        .map(|x| shorten(&x.text, 200))
                })
                .unwrap_or_default(),
            user,
            message,
            option,
        })
        .collect()
}

/// The words that announce an outcome.
fn outcome_line(state: &GroupState, d: &Decision) -> Held {
    let chosen = d.chosen();
    let label = chosen.map_or_else(String::new, |o| o.label.clone());
    let tail = chosen
        .and_then(|o| o.echo.clone())
        .map(|e| format!("Holding {e}."));
    match (&d.outcome, d.kind) {
        (Outcome::Count { .. }, ProposalKind::Attendance) => {
            let names: Vec<String> = chosen
                .map(|o| {
                    o.votes
                        .iter()
                        .map(|u| state.display_name(&d.room, u))
                        .collect()
                })
                .unwrap_or_default();
            let what = chosen.map_or("", DecidedOption::display);
            let verb = if names.len() == 1 { "is" } else { "are" };
            Held {
                full: format!("{what}: {} {verb} in.", join_names(&names, "and")),
                part: format!("{label}: {} in", names.len()),
                tail: None,
            }
        }
        (Outcome::Count { .. }, ProposalKind::Choice) => {
            let count = d.count_line().unwrap_or_default();
            Held {
                full: format!("{label} it is, {count}."),
                part: format!("{label}, {count}"),
                tail,
            }
        }
        (Outcome::Decider { user }, _) => {
            let name = state.display_name(&d.room, user);
            let why = d.reason.as_deref().map_or_else(String::new, reason_clause);
            Held {
                full: format!("Decided: {label}, by {name}{why}."),
                part: format!("{label}, by {name}"),
                tail,
            }
        }
    }
}

/// "the date matters more than 8%" reads as ", for the date"; anything else
/// is quoted.
fn reason_clause(reason: &str) -> String {
    let raw: Vec<&str> = reason.split_whitespace().collect();
    let at = raw.iter().position(|w| {
        let w = w
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        w == "matters" || w == "matter"
    });
    match at {
        Some(at @ 1..=3) => format!(", for {}", raw[..at].join(" ")),
        _ => format!(": \"{}\"", shorten(reason, 120)),
    }
}

fn same_thread(state: &GroupState, a: &Proposal, b: &Proposal) -> bool {
    let threaded = state
        .rooms
        .get(&a.room)
        .and_then(|r| r.info.as_ref())
        .is_some_and(|i| i.kind == RoomKind::Thread);
    a.room == b.room && (a.thread_root == b.thread_root || threaded)
}

/// One line for one outcome; several in one thread become one line.
fn combine(lines: &[Held]) -> String {
    match lines {
        [one] => match &one.tail {
            Some(tail) => format!("{} {tail}", one.full),
            None => one.full.clone(),
        },
        many => {
            let parts: Vec<&str> = many.iter().map(|h| h.part.as_str()).collect();
            let tails: Vec<&str> = many.iter().filter_map(|h| h.tail.as_deref()).collect();
            let mut text = format!("Decided: {}.", parts.join("; "));
            if !tails.is_empty() {
                text.push(' ');
                text.push_str(&tails.join(" "));
            }
            text
        }
    }
}

/// Posts an outcome, combined with any held in the same thread, or holds it
/// while another proposal there is still open.
fn announce(
    state: &mut GroupState,
    pid: &ProposalId,
    line: Held,
    now: Instant,
    trace: Trace,
    recording: Recording,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get(pid).cloned() else {
        return;
    };
    let open_sibling = state
        .proposals
        .values()
        .any(|q| q.id != *pid && q.is_open() && same_thread(state, &p, q));
    if open_sibling {
        if let Some(slot) = state.proposals.get_mut(pid) {
            slot.held = Some(line);
        }
        return;
    }
    let mut lines = take_held(state, &p);
    lines.push(line);
    let reply_to = p.ballot.message().cloned().or(Some(p.thread_root.clone()));
    send_as(
        state,
        fx,
        &p.room,
        combine(&lines),
        reply_to,
        now,
        trace,
        recording,
    );
}

/// Held lines from other proposals in `p`'s thread, oldest first.
fn take_held(state: &mut GroupState, p: &Proposal) -> Vec<Held> {
    let mut ids: Vec<(Instant, u64, ProposalId)> = state
        .proposals
        .values()
        .filter(|q| q.id != p.id && q.held.is_some() && same_thread(state, p, q))
        .map(|q| (q.opened_at, q.id.seq(), q.id.clone()))
        .collect();
    ids.sort();
    ids.into_iter()
        .filter_map(|(_, _, id)| state.proposals.get_mut(&id).and_then(|q| q.held.take()))
        .collect()
}

/// Posts any held lines in `pid`'s thread on their own.
fn flush_held(state: &mut GroupState, pid: &ProposalId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(p) = state.proposals.get(pid).cloned() else {
        return;
    };
    let lines = take_held(state, &p);
    if lines.is_empty() {
        return;
    }
    let trace = Trace::new(
        format!(
            "the other proposal in {} did not close",
            state.room_label(&p.room)
        ),
        "outcomes held for a combined line go out when the other one lapses",
        now,
    );
    let note = Recording::Note {
        why: DECISION_STANDS.to_owned(),
    };
    send_as(
        state,
        fx,
        &p.room,
        combine(&lines),
        Some(p.thread_root.clone()),
        now,
        trace,
        note,
    );
}

/// Ends an open proposal without a decision because its poll was undone.
pub(crate) fn withdraw(
    state: &mut GroupState,
    pid: &ProposalId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get_mut(pid).filter(|p| p.is_open()) else {
        return;
    };
    p.status = ProposalStatus::Expired;
    let p = p.clone();
    disarm(state, pid, fx);
    end_poll(state, &p, fx);
    flush_held(state, pid, now, fx);
    fx.push(Effect::Notice(format!(
        "Proposal withdrawn by undo in {}: {}",
        state.room_label(&p.room),
        p.question
    )));
}

/// Closes without a decision and says so in one line.
fn expire(
    state: &mut GroupState,
    pid: &ProposalId,
    line: String,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get_mut(pid).filter(|p| p.is_open()) else {
        return;
    };
    p.status = ProposalStatus::Expired;
    let p = p.clone();
    disarm(state, pid, fx);
    end_poll(state, &p, fx);
    let line = match crate::recur::on_lapsed(state, pid, fx) {
        Some(paused) => format!("{line} {paused}"),
        None => line,
    };
    let trace = Trace::new(
        format!("{} in {}", p.question, state.room_label(&p.room)),
        "a proposal nobody settles lapses after its deadlines with one line",
        now,
    );
    let reply_to = p.ballot.message().cloned().or(Some(p.thread_root.clone()));
    let note = Recording::Note {
        why: "it lapsed with no decision; say re-vote to open it again".to_owned(),
    };
    send_as(state, fx, &p.room, line, reply_to, now, trace, note);
    flush_held(state, pid, now, fx);
    fx.push(Effect::Notice(format!(
        "Proposal lapsed in {}: {}",
        state.room_label(&p.room),
        p.question
    )));
}

/// Another deadline as long as the first, without nudges.
fn extend(state: &mut GroupState, pid: &ProposalId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(p) = state.proposals.get(pid) else {
        return;
    };
    let window = p.deadline.seconds_since(p.opened_at).max(SHORTEST_SECONDS);
    let deadline = avoid_quiet(state, now, now.plus_seconds(window));
    if let Some(p) = state.proposals.get_mut(pid) {
        p.lapsed += 1;
        p.deadline = deadline;
    }
    arm(state, pid, now, fx);
}

/// The deadline passed.
pub(crate) fn on_close_timer(
    state: &mut GroupState,
    pid: &ProposalId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get(pid).filter(|p| p.is_open()).cloned() else {
        return;
    };
    let title = p.title();
    if p.rule == CloseRule::LeadDecides {
        let heard = p
            .options
            .iter()
            .any(|o| !o.views.is_empty() || !o.votes.is_empty());
        match (heard, p.lapsed) {
            (true, 0) => {
                let text = tally(state, &p, now);
                let trace = Trace::new(
                    format!("the deadline for {title} passed"),
                    "under lead decides the tally goes to the thread at the deadline",
                    now,
                );
                send(
                    state,
                    fx,
                    &p.room,
                    text,
                    Some(p.thread_root.clone()),
                    now,
                    trace,
                );
                extend(state, pid, now, fx);
                flush_held(state, pid, now, fx);
            }
            (false, 0) => {
                extend(state, pid, now, fx);
                flush_held(state, pid, now, fx);
            }
            (true, _) => {
                let line = format!(
                    "No lead has decided {title}, so I've let it go. Say re-vote to reopen it."
                );
                expire(state, pid, line, now, fx);
            }
            (false, _) => {
                let line = format!("Nobody weighed in on {title}, so I've let it go.");
                expire(state, pid, line, now, fx);
            }
        }
        return;
    }
    let counts = counted(&p);
    let split = counts.iter().filter(|c| **c > 0).count() > 1;
    match winner(state, &p, &counts) {
        Some(_) if p.rule == CloseRule::Unanimous && split => {
            let line = format!(
                "No unanimous answer on {title}: {}. Say re-vote to try again.",
                counts_text(&p)
            );
            expire(state, pid, line, now, fx);
        }
        Some(w) => close(state, pid, w, Closer::Count, now, fx),
        None if p.lapsed == 0 => {
            extend(state, pid, now, fx);
            flush_held(state, pid, now, fx);
        }
        None => {
            let line = format!("No votes on {title}, so I've let it go.");
            expire(state, pid, line, now, fx);
        }
    }
}

/// "Thursday 3, Friday 1".
fn counts_text(p: &Proposal) -> String {
    p.options
        .iter()
        .map(|o| format!("{} {}", o.label, o.votes.len()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A nudge timer fired: for the proposal, send or schedule one DM per
/// member who has not voted; for one member, send theirs.
pub(crate) fn on_nudge(
    state: &mut GroupState,
    pid: &ProposalId,
    which: NudgeAt,
    user: Option<PlatformUserId>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(p) = state.proposals.get(pid).filter(|p| p.is_open()).cloned() else {
        return;
    };
    if !p.rule.needs_ballot() || p.ballot == Ballot::None {
        return;
    }
    let end = match which {
        NudgeAt::Half => late_nudge(p.opened_at, p.deadline),
        NudgeAt::Deadline => p.deadline.plus_seconds(-5 * 60),
    };
    let targets = targets(state, &p, now);
    match user {
        Some(user) => {
            if targets.contains(&user) {
                nudge(state, &p, &user, which, now, fx);
            }
        }
        None => {
            for user in targets {
                match nudge_time(state, &user, now, end) {
                    Some(at) if at <= now => nudge(state, &p, &user, which, now, fx),
                    Some(at) => {
                        let purpose = TimerPurpose::Nudge {
                            proposal: pid.clone(),
                            which,
                            user: Some(user),
                        };
                        timers::arm(state, fx, at, purpose);
                    }
                    None => debug!(%user, "no good hour for a nudge in this window"),
                }
            }
        }
    }
}

/// Active members who have not voted or abstained and have had fewer than
/// two nudges; only the deciders under named deciders.
fn targets(state: &GroupState, p: &Proposal, now: Instant) -> Vec<PlatformUserId> {
    let electorate = match &p.rule {
        CloseRule::NamedDeciders(set) => set.clone(),
        _ => active_members(state, &p.room, now),
    };
    electorate
        .into_iter()
        .filter(|u| {
            p.vote_of(u).is_none()
                && !p.abstained.contains_key(u)
                && p.nudged.get(u).copied().unwrap_or(0) < 2
                && !state.is_me(u)
        })
        .collect()
}

/// When to nudge `user` between `now` and `end`: at their usual hour when
/// that falls inside, never in their quiet hours. None when there is no
/// good moment before `end`.
fn nudge_time(
    state: &GroupState,
    user: &PlatformUserId,
    now: Instant,
    end: Instant,
) -> Option<Instant> {
    let group = state.timezone.as_str();
    let mut at = now;
    if let Some(hour) = usual_hour(state, user) {
        let hour = i8::try_from(hour).ok()?;
        let local = zone::local(now, group);
        if local.hour() != hour {
            let tz = zone::or_utc(group);
            let today = local.date().at(hour, 0, 0, 0).to_zoned(tz.clone()).ok()?;
            let candidate = if Instant(today.timestamp()) < now {
                local
                    .date()
                    .tomorrow()
                    .ok()?
                    .at(hour, 0, 0, 0)
                    .to_zoned(tz)
                    .ok()?
            } else {
                today
            };
            let candidate = Instant(candidate.timestamp());
            if candidate < end {
                at = candidate;
            }
        }
    }
    let quiet = state.profile.quiet_hours;
    let theirs = zone_of(state, user);
    let local = zone::local(at, &theirs);
    if quiet.contains(u8::try_from(local.hour()).unwrap_or(0)) {
        let tz = zone::or_utc(&theirs);
        let wake_hour = i8::try_from(quiet.end).ok()?;
        let mut wake = local
            .date()
            .at(wake_hour, 0, 0, 0)
            .to_zoned(tz.clone())
            .ok()?;
        if Instant(wake.timestamp()) <= at {
            wake = local
                .date()
                .tomorrow()
                .ok()?
                .at(wake_hour, 0, 0, 0)
                .to_zoned(tz)
                .ok()?;
        }
        let wake = Instant(wake.timestamp());
        if wake >= end {
            return None;
        }
        at = wake;
    }
    Some(at)
}

fn nudge(
    state: &mut GroupState,
    p: &Proposal,
    user: &PlatformUserId,
    which: NudgeAt,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let text = nudge_text(state, p, which, now);
    let name = state.name_of(user);
    let trace = Trace::new(
        format!(
            "{} in {} has no vote from {name} yet",
            p.title(),
            state.room_label(&p.room)
        ),
        NUDGE_RULE,
        now,
    );
    send_dm(state, fx, user, text, now, trace, false);
    if let Some(slot) = state.proposals.get_mut(&p.id) {
        *slot.nudged.entry(user.clone()).or_insert(0) += 1;
    }
}

/// "Dinner in #dinner is open: 1 Thursday the 12th, 2 Friday the 13th.
/// Reply with a number or react on the poll."
fn nudge_text(state: &GroupState, p: &Proposal, which: NudgeAt, now: Instant) -> String {
    let room = state.room_label(&p.room);
    let title = p.title();
    let how = match p.ballot {
        Ballot::Native { .. } => "vote in the poll",
        _ => "react on the poll",
    };
    let when = match which {
        NudgeAt::Half => "is open".to_owned(),
        NudgeAt::Deadline => format!("closes {}", within(p.deadline.seconds_since(now))),
    };
    match p.kind {
        ProposalKind::Attendance => {
            let what = p
                .options
                .first()
                .map_or_else(|| "it".to_owned(), |o| attendance_phrase(state, o, now));
            let yes = reactions::emoji_for(state, &p.room, Meaning::In);
            format!("Who's in for {what}, in {room}, {when}. Reply yes or react {yes} on the poll.")
        }
        ProposalKind::Choice => {
            let options: Vec<String> = p
                .options
                .iter()
                .enumerate()
                .map(|(i, o)| format!("{} {}", i + 1, o.display()))
                .collect();
            format!(
                "{title} in {room} {when}: {}. Reply with a number or {how}.",
                options.join(", ")
            )
        }
    }
}

/// "in 3 hours", "in 40 minutes".
fn within(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_owned();
    }
    format!("in {}", duration(seconds))
}

/// Asks the resolver what `pending.text` means in `zone_name`, with a
/// guard when someone waits on the answer.
fn ask_time(
    state: &mut GroupState,
    mut pending: PendingTime,
    zone_name: &str,
    guarded: bool,
    fx: &mut Vec<Effect>,
) {
    let now = pending.asked_at;
    let local = zone::local(now, zone_name);
    fx.push(Effect::ResolveTime(TimeRequest {
        room: pending.room.clone(),
        message: pending.message.clone(),
        text: pending.text.clone(),
        zone: zone_name.to_owned(),
        now,
        local_now: local.strftime("%Y-%m-%d %H:%M %A").to_string(),
    }));
    if guarded {
        let purpose = TimerPurpose::TimeGuard {
            room: pending.room.clone(),
            message: pending.message.clone(),
        };
        let at = now.plus_seconds(TIME_GUARD_SECONDS);
        pending.guard = Some(timers::arm(state, fx, at, purpose));
    }
    state.pending_times.push(pending);
}

/// The resolver's answer.
pub(crate) fn on_time_resolved(
    state: &mut GroupState,
    res: TimeResolution,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let matches_phrase = |p: &PendingTime| {
        res.phrase
            .as_deref()
            .is_some_and(|ph| ph.eq_ignore_ascii_case(&p.text))
    };
    let at = state
        .pending_times
        .iter()
        .position(|p| p.room == res.room && p.message == res.message && matches_phrase(p))
        .or_else(|| {
            state
                .pending_times
                .iter()
                .position(|p| p.room == res.room && p.message == res.message)
        });
    let Some(at) = at else {
        debug!(message = %res.message, "a time nobody is waiting for");
        return;
    };
    let pending = state.pending_times.remove(at);
    if let Some(guard) = &pending.guard {
        timers::cancel(state, fx, guard);
    }
    let echo = res
        .echo
        .clone()
        .or_else(|| time::echo(&res.meaning, &state.timezone, now));
    match pending.target {
        TimeTarget::Option { proposal, index } => {
            if res.meaning == TimeMeaning::None {
                return;
            }
            if let Some(o) = state
                .proposals
                .get_mut(&proposal)
                .and_then(|p| p.options.get_mut(index))
            {
                o.when = Some(res.meaning);
                o.echo = echo.clone();
                fx.push(Effect::Notice(format!(
                    "Read \"{}\" as {}.",
                    pending.text,
                    echo.unwrap_or_default()
                )));
            }
        }
        TimeTarget::Deadline { proposal, reply_to } => {
            let room = pending.room.clone();
            let text = match deadline_from(&res.meaning, &state.timezone) {
                Some(at) => set_deadline(state, &proposal, at, now, fx),
                None => UNREADABLE_TIME.to_owned(),
            };
            let trace = Trace::new(
                crate::step::trigger_for(state, &room, &reply_to),
                "a lead's deadline, read by the resolver",
                now,
            );
            send(state, fx, &room, text, Some(reply_to.clone()), now, trace);
            reactions::remove(state, fx, &room, &reply_to, Meaning::Eyes);
        }
    }
}

/// The resolver did not answer a deadline in time.
pub(crate) fn on_time_guard(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(at) = state
        .pending_times
        .iter()
        .position(|p| &p.room == room && &p.message == message && p.guard.is_some())
    else {
        return;
    };
    let pending = state.pending_times.remove(at);
    if let TimeTarget::Deadline { reply_to, .. } = pending.target {
        let trace = Trace::new(
            crate::step::trigger_for(state, room, &reply_to),
            "a time the resolver did not read in time is owned up to",
            now,
        );
        send(
            state,
            fx,
            room,
            UNREADABLE_TIME,
            Some(reply_to.clone()),
            now,
            trace,
        );
        reactions::remove(state, fx, room, &reply_to, Meaning::Eyes);
    }
}

/// The instant a deadline phrase means: its time, or that day's evening.
fn deadline_from(meaning: &TimeMeaning, zone_name: &str) -> Option<Instant> {
    match meaning {
        TimeMeaning::Ambiguous { .. } | TimeMeaning::None => None,
        other => time::event_start(other, zone_name),
    }
}

/// Moves a proposal's deadline and says so.
fn set_deadline(
    state: &mut GroupState,
    pid: &ProposalId,
    at: Instant,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    if at.seconds_since(now) < 10 * 60 {
        return "That's too soon; give me a later time.".to_owned();
    }
    let Some(p) = state.proposals.get_mut(pid).filter(|p| p.is_open()) else {
        return "That one has closed already.".to_owned();
    };
    p.deadline = at;
    p.lapsed = 0;
    let title = p.title();
    disarm(state, pid, fx);
    arm(state, pid, now, fx);
    let meaning = TimeMeaning::Instant { at, has_time: true };
    let echo = time::echo(&meaning, &state.timezone, now).unwrap_or_default();
    match time::relative(at, &state.timezone, now) {
        Some(rel) => format!("{title} now closes {echo}, {rel}."),
        None => format!("{title} now closes {echo}."),
    }
}

/// Open proposals in `room`, oldest first.
fn open_in(state: &GroupState, room: &PlatformRoomId) -> Vec<ProposalId> {
    let mut ids: Vec<(Instant, u64, ProposalId)> = state
        .proposals
        .values()
        .filter(|p| p.is_open() && &p.room == room)
        .map(|p| (p.opened_at, p.id.seq(), p.id.clone()))
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, _, id)| id).collect()
}

fn is_question(text: &str) -> bool {
    text.trim_end().ends_with('?')
}

/// A line in a room nobody addressed to the agent: views, votes, a lead's
/// choice, or a late can't-do.
pub(crate) fn heard(state: &mut GroupState, message: &Message, now: Instant, fx: &mut Vec<Effect>) {
    if state.rooms.get(&message.room).is_some_and(|r| r.is_dm()) {
        return;
    }
    let role = resolve_role(state, &message.room, &message.author);
    let mut claimed = false;
    for pid in open_in(state, &message.room) {
        claimed |= consider(state, &pid, message, &message.text, false, now, fx);
    }
    if !claimed {
        late_cant(state, message, &message.text, role, false, now, fx);
    }
}

/// What one line does to one open proposal. With `forced`, a line that
/// names an option counts as picking it. Returns whether it was about it.
fn consider(
    state: &mut GroupState,
    pid: &ProposalId,
    message: &Message,
    text: &str,
    forced: bool,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let role = resolve_role(state, &message.room, &message.author);
    let Some(p) = state.proposals.get_mut(pid).filter(|p| p.is_open()) else {
        return false;
    };
    let user = &message.author;
    let question = is_question(text);
    match named(text, p) {
        Named::One { index, clear } => {
            p.add_view(index, user, &message.id, &message.text);
            if question {
                return true;
            }
            let clear = clear || forced;
            let lead_decides = p.rule == CloseRule::LeadDecides;
            let ballot = p.ballot != Ballot::None;
            let kind = p.kind;
            if lead_decides && role >= Role::Lead && clear {
                let label = p.options[index].label.clone();
                let reason = reason_after(text, &label);
                let closer = Closer::Decider {
                    user: user.clone(),
                    message: Some(message.id.clone()),
                    reason,
                };
                close(state, pid, index, closer, now, fx);
            } else if role >= Role::Member && clear && (ballot || !lead_decides) {
                if cast(state, pid, user, Some(index)) {
                    let yes = reactions::emoji_for(state, &message.room, Meaning::In);
                    let emoji = if kind == ProposalKind::Attendance {
                        yes
                    } else {
                        NUMBERS.get(index).copied().unwrap_or(yes)
                    };
                    react(state, fx, &message.room, &message.id, emoji);
                }
                settle(state, pid, now, fx);
            }
            true
        }
        Named::Abstain => {
            if question || role < Role::Member {
                return false;
            }
            p.set_vote(user, None);
            p.abstained.insert(user.clone(), message.id.clone());
            p.add_remark(user, &message.id, &message.text);
            settle(state, pid, now, fx);
            true
        }
        Named::Against { index } => {
            if p.vote_of(user) == Some(index) {
                p.set_vote(user, None);
            }
            p.add_remark(user, &message.id, &message.text);
            true
        }
        Named::Several => {
            p.add_remark(user, &message.id, &message.text);
            true
        }
        Named::Nothing => false,
    }
}

/// The classifier says a line is about an open decision: count it.
pub(crate) fn open_decision(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    for pid in open_in(state, room) {
        consider(state, &pid, message, &message.text, true, now, fx);
    }
}

/// "I can't do Thursday" about a decided, dated outcome: the headcount
/// drops and a re-vote is offered. Nothing reopens.
fn late_cant(
    state: &mut GroupState,
    message: &Message,
    text: &str,
    role: Role,
    addressed: bool,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    if role < Role::Member {
        return false;
    }
    let mut recent: Vec<&Decision> = state
        .decisions
        .values()
        .filter(|d| d.room == message.room)
        .filter(|d| {
            d.chosen()
                .and_then(|o| o.when.as_ref())
                .and_then(time::bounds)
                .is_some_and(|(_, end)| end >= now)
        })
        .collect();
    recent.sort_by_key(|d| std::cmp::Reverse(d.closed_at));
    let Some(d) = recent.into_iter().find(|d| {
        let labels: Vec<&str> = d.options.iter().map(|o| o.label.as_str()).collect();
        named_labels(text, &labels, d.kind) == Named::Against { index: d.winner }
    }) else {
        return false;
    };
    let did = d.id.clone();
    let display = d
        .chosen()
        .map_or_else(String::new, |o| o.display().to_owned());
    let title = d.title();
    let Some(entry) = state.calendar.iter_mut().find(|e| e.decision == did) else {
        return false;
    };
    entry.attendees.remove(&message.author);
    entry.out.insert(message.author.clone());
    let left = i64::try_from(entry.attendees.len()).unwrap_or(0);
    let line = format!(
        "Noted. {} still can. Keep {display}, or re-vote?",
        capitalise(&time::number_word(left))
    );
    let name = state.display_name(&message.room, &message.author);
    fx.push(Effect::Notice(format!(
        "{name} can't make {title} on {display}; {left} still can."
    )));
    let speak = addressed || (!state.is_observing() && can_speak_unprompted(state, now));
    if speak {
        let trace = Trace::new(addressed_trigger(state, message), LATE_RULE, now);
        send(
            state,
            fx,
            &message.room,
            line,
            Some(message.id.clone()),
            now,
            trace,
        );
        if !addressed {
            record_unprompted(state, now);
        }
    }
    true
}

/// An addressed line that is not a ruled sentence: a vote, a lead's
/// choice, an abstention or a late can't-do, in the room or by DM.
/// Returns whether it was handled.
pub(crate) fn addressed(
    state: &mut GroupState,
    message: &Message,
    body: &str,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    if is_question(body) {
        return false;
    }
    if state.rooms.get(&message.room).is_some_and(|r| r.is_dm()) {
        return dm_vote(state, message, body, now, fx);
    }
    let mut reply = None;
    for pid in open_in(state, &message.room) {
        let Some(p) = state.proposals.get(&pid) else {
            continue;
        };
        let lead_decides = p.rule == CloseRule::LeadDecides;
        match named(body, p) {
            Named::One { index, .. } => {
                let display = p.options[index].display().to_owned();
                if lead_decides && role < Role::Lead && p.ballot == Ballot::None {
                    if let Some(p) = state.proposals.get_mut(&pid) {
                        p.add_view(index, &message.author, &message.id, body);
                    }
                    reply = Some(format!("Noted: {display}, for a lead to decide."));
                    continue;
                }
                if role < Role::Member {
                    if let Some(p) = state.proposals.get_mut(&pid) {
                        p.add_view(index, &message.author, &message.id, body);
                    }
                    reply = Some(format!("Noted: {display}. Only members vote."));
                    continue;
                }
                consider(state, &pid, message, body, true, now, fx);
                if state.proposals.get(&pid).is_some_and(Proposal::is_open) {
                    reply = Some(format!("Counted: {display}."));
                } else {
                    reply = None;
                    break;
                }
            }
            Named::Abstain => {
                consider(state, &pid, message, body, true, now, fx);
                reply = Some("Noted: no preference.".to_owned());
            }
            Named::Against { index } => {
                let display = p.options[index].display().to_owned();
                consider(state, &pid, message, body, true, now, fx);
                reply = Some(format!("Noted: not {display}."));
            }
            Named::Several | Named::Nothing => {}
        }
    }
    if let Some(text) = reply {
        let trace = Trace::new(addressed_trigger(state, message), VOTE_RULE, now);
        send(
            state,
            fx,
            &message.room,
            text,
            Some(message.id.clone()),
            now,
            trace,
        );
        return true;
    }
    late_cant(state, message, body, role, true, now, fx)
}

/// A vote by DM, for the proposal the member was nudged about or the one
/// open in their rooms.
fn dm_vote(
    state: &mut GroupState,
    message: &Message,
    body: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let user = &message.author;
    let mut candidates: Vec<(u8, Instant, ProposalId)> = state
        .proposals
        .values()
        .filter(|p| p.is_open() && p.ballot != Ballot::None)
        .filter(|p| {
            state
                .rooms
                .get(&p.room)
                .is_some_and(|r| !r.is_dm() && r.members.contains_key(user))
        })
        .map(|p| {
            (
                p.nudged.get(user).copied().unwrap_or(0),
                p.opened_at,
                p.id.clone(),
            )
        })
        .collect();
    candidates.sort_by(|a, b| b.cmp(a));
    for (_, _, pid) in candidates {
        let Some(p) = state.proposals.get(&pid) else {
            continue;
        };
        let room = p.room.clone();
        if resolve_role(state, &room, user) < Role::Member {
            continue;
        }
        let title = p.title();
        let text = match named(body, p) {
            Named::One { index, .. } => {
                let display = p.options[index].display().to_owned();
                if let Some(p) = state.proposals.get_mut(&pid) {
                    p.add_view(index, user, &message.id, body);
                }
                cast(state, &pid, user, Some(index));
                format!(
                    "Counted: {display}, for {title} in {}.",
                    state.room_label(&room)
                )
            }
            Named::Abstain => {
                if let Some(p) = state.proposals.get_mut(&pid) {
                    p.set_vote(user, None);
                    p.abstained.insert(user.clone(), message.id.clone());
                }
                format!("Noted: no preference on {title}.")
            }
            _ => continue,
        };
        let trace = Trace::new(addressed_trigger(state, message), "votes by DM count", now);
        send(
            state,
            fx,
            &message.room,
            text,
            Some(message.id.clone()),
            now,
            trace,
        );
        settle(state, &pid, now, fx);
        return true;
    }
    false
}

/// A reaction or a poll vote on a ballot, put on or taken off.
pub(crate) struct Mark<'a> {
    pub room: &'a PlatformRoomId,
    pub message: &'a PlatformMessageId,
    pub user: &'a PlatformUserId,
    pub added: bool,
}

/// A reaction on a ballot. Returns whether the message was a ballot.
pub(crate) fn on_reaction(
    state: &mut GroupState,
    mark: Mark<'_>,
    emoji: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let Mark {
        room,
        message,
        user,
        added,
    } = mark;
    let found = state.proposals.values().find(|p| {
        &p.room == room
            && matches!(&p.ballot, Ballot::Reactions { message: Some(m), .. } if m == message)
    });
    let Some(p) = found else {
        return false;
    };
    let pid = p.id.clone();
    if !p.is_open() || !state.is_human(room, user) {
        return true;
    }
    let index = if p.kind == ProposalKind::Attendance {
        reactions::means(state, room, emoji, Meaning::In).then_some(0)
    } else {
        emoji_option(emoji, p.kind)
    };
    let Some(index) = index.filter(|i| *i < p.options.len()) else {
        return true;
    };
    if resolve_role(state, room, user) < Role::Member {
        return true;
    }
    let Some(p) = state.proposals.get_mut(&pid) else {
        return true;
    };
    let current = p.vote_of(user);
    let stack = p.reacted.entry(user.clone()).or_default();
    stack.retain(|i| *i != index);
    let vote = if added {
        stack.push(index);
        Some(index)
    } else if current == Some(index) {
        stack.last().copied()
    } else {
        current
    };
    if p.reacted.get(user).is_some_and(Vec::is_empty) {
        p.reacted.remove(user);
    }
    if cast(state, &pid, user, vote) {
        settle(state, &pid, now, fx);
    }
    true
}

/// A vote on a native poll, or its removal.
pub(crate) fn on_poll_vote(
    state: &mut GroupState,
    mark: Mark<'_>,
    option: u32,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Mark {
        room,
        message,
        user,
        added,
    } = mark;
    let Some(pid) = native(state, room, message) else {
        debug!(%message, "a vote on a poll that is not open");
        return;
    };
    if !state.is_human(room, user) || resolve_role(state, room, user) < Role::Member {
        return;
    }
    let Ok(index) = usize::try_from(option) else {
        return;
    };
    let Some(p) = state.proposals.get(&pid) else {
        return;
    };
    if index >= p.options.len() {
        return;
    }
    let vote = if added {
        Some(index)
    } else if p.vote_of(user) == Some(index) {
        None
    } else {
        p.vote_of(user)
    };
    if cast(state, &pid, user, vote) {
        settle(state, &pid, now, fx);
    }
}

/// A native poll ended on the platform: close on its counts if still open.
pub(crate) fn on_poll_ended(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    counts: Vec<u32>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(pid) = native(state, room, message) else {
        return;
    };
    let Some(p) = state.proposals.get(&pid).cloned() else {
        return;
    };
    let mut counts = counts;
    counts.resize(p.options.len(), 0);
    match winner(state, &p, &counts) {
        Some(w) if p.rule != CloseRule::LeadDecides => {
            close(state, &pid, w, Closer::Counts(counts), now, fx);
        }
        Some(_) => {}
        None => {
            let line = format!("No votes on {}, so I've let it go.", p.title());
            expire(state, &pid, line, now, fx);
        }
    }
}

fn native(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) -> Option<ProposalId> {
    state
        .proposals
        .values()
        .find(|p| {
            p.is_open()
                && &p.room == room
                && matches!(&p.ballot, Ballot::Native { message: Some(m) } if m == message)
        })
        .map(|p| p.id.clone())
}

/// A deleted message takes its views and quotes out of open proposals.
pub(crate) fn on_deleted(state: &mut GroupState, room: &PlatformRoomId, id: &PlatformMessageId) {
    for p in state.proposals.values_mut() {
        if !p.is_open() || &p.room != room {
            continue;
        }
        for o in &mut p.options {
            o.views.retain(|(_, m)| m != id);
        }
        p.remarks.retain(|(_, m)| m != id);
        p.abstained.retain(|_, m| m != id);
        p.quotes.remove(id);
    }
}

/// A forming plan with options on offer: open a ballot, within the budget,
/// or tell the leads it would have while observing. Returns whether the
/// plan was taken care of.
pub(crate) fn plan_forming(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &Message,
    score: &Score,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    if state.rooms.get(room).is_some_and(|r| r.is_dm()) {
        return false;
    }
    let Some(offer) = from_recent(state, room, message, now) else {
        return false;
    };
    // The question that starts a plan is not a plan yet; wait for a reply.
    if offer.root == message.id {
        return false;
    }
    // The same plan, or an open proposal over the same options, is taken care of.
    let taken = state.proposals.values().any(|p| {
        &p.room == room
            && (p.thread_root == offer.root
                || (p.is_open()
                    && p.options.iter().any(|o| {
                        offer
                            .options
                            .iter()
                            .any(|x| x.label.eq_ignore_ascii_case(&o.label))
                    })))
    });
    if taken {
        return true;
    }
    let labels: Vec<String> = offer.options.iter().map(|o| o.label.clone()).collect();
    let asked = question(offer.topic.as_deref(), &labels);
    let label = state.room_label(room);
    if !can_speak_unprompted(state, now) {
        fx.push(Effect::Notice(format!(
            "Would have opened a poll in {label}, {asked} No budget left or the wrong hour."
        )));
        return true;
    }
    let trigger = format!("a plan forming in {label}: {}", quote(&message.text, 80));
    let trace = Trace::new(trigger, PLAN_RULE, now).scored(Some(score.clone()));
    if state.is_observing() {
        let text = format!(
            "In {label} I would have opened a poll: {asked}\n{}",
            reactions::feedback_line(state, room)
        );
        observe::report(state, fx, &text, trace);
        record_unprompted(state, now);
        return true;
    }
    let zone = zone_of(state, &offer.source_author);
    let pid = open(
        state,
        Opening {
            room: room.clone(),
            thread_root: offer.root.clone(),
            reply_to: Some(offer.root.clone()),
            topic: offer.topic,
            options: offer.options,
            kind: ProposalKind::Choice,
            organiser: Some(offer.organiser),
            rule: default_rule(state),
            ballot: false,
            source: offer.source,
            zone,
            reopens: None,
            trace,
        },
        now,
        fx,
    );
    record_unprompted(state, now);
    // The lines that led here are kept as what people said.
    for line in offer.lines {
        if let Some(p) = state.proposals.get_mut(&pid) {
            match named(&line.text, p) {
                Named::One { index, .. } => p.add_view(index, &line.author, &line.id, &line.text),
                Named::Several | Named::Against { .. } => {
                    p.add_remark(&line.author, &line.id, &line.text);
                }
                Named::Abstain | Named::Nothing => {}
            }
        }
    }
    true
}

/// Options offered in the room's last ten minutes.
struct Offer {
    options: Vec<Offered>,
    topic: Option<String>,
    root: PlatformMessageId,
    organiser: PlatformUserId,
    source: PlatformMessageId,
    source_author: PlatformUserId,
    lines: Vec<Message>,
}

/// Alternatives in one recent line, newest first, or else two or more
/// distinct days named across the recent lines.
fn from_recent(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &Message,
    now: Instant,
) -> Option<Offer> {
    let since = message.at.plus_seconds(-triage::PLAN_WINDOW_SECONDS);
    let r = state.rooms.get(room)?;
    let end = r
        .messages
        .iter()
        .position(|m| m.id == message.id)
        .map_or(r.messages.len(), |i| i + 1);
    let lines: Vec<Message> = r
        .messages
        .range(..end)
        .filter(|m| m.at >= since && state.is_human(room, &m.author) && !r.is_own(&m.id))
        .cloned()
        .collect();
    let mut found = None;
    for line in lines.iter().rev() {
        let zone = zone_of(state, &line.author);
        if let Some((options, topic)) = crate::proposal::alternatives(&line.text, &zone, now) {
            found = Some((line.clone(), options, topic));
            break;
        }
    }
    let (source, options, topic) = match found {
        Some(f) => f,
        None => {
            let mut days: Vec<(jiff::civil::Date, Offered, Message)> = Vec::new();
            for line in &lines {
                let zone = zone_of(state, &line.author);
                let Some(r) = resolve_time_rules(&line.text, &zone, now) else {
                    continue;
                };
                let TimeMeaning::Instant { at, .. } = r.meaning else {
                    continue;
                };
                let date = zone::local(at, &state.timezone).date();
                if days.iter().any(|(d, _, _)| *d == date) {
                    continue;
                }
                let label = if at.seconds_since(now) < 7 * 24 * HOUR {
                    time::weekday_name(date.weekday()).to_owned()
                } else {
                    capitalise(&r.echo)
                };
                let o = Offered {
                    label,
                    when: Some(r.meaning),
                    echo: Some(r.echo),
                    unresolved: false,
                };
                days.push((date, o, line.clone()));
            }
            if days.len() < 2 {
                return None;
            }
            days.truncate(4);
            let source = days.last().map(|(_, _, m)| m.clone())?;
            (source, days.into_iter().map(|(_, o, _)| o).collect(), None)
        }
    };
    let plan = triage::plan_root(state, room, message).cloned();
    let topic = topic.or_else(|| plan.as_ref().and_then(|p| triage::plan_topic(&p.text)));
    let (root, organiser) = match &plan {
        Some(p) => (p.id.clone(), p.author.clone()),
        None => (source.id.clone(), source.author.clone()),
    };
    Some(Offer {
        options,
        topic,
        root,
        organiser,
        source: source.id.clone(),
        source_author: source.author.clone(),
        lines,
    })
}

/// Ruled sentences about decisions. Returns None to hand the message to
/// the model, else whether the reply waits on the resolver.
pub(crate) fn handle(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    sentence: DecisionSentence,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> Option<bool> {
    let room = message.room.clone();
    let in_dm = state.rooms.get(&room).is_some_and(|r| r.is_dm());
    let trigger = addressed_trigger(state, message);
    let reply = |state: &mut GroupState, fx: &mut Vec<Effect>, text: String, rule: &str| {
        let trace = Trace::new(trigger.clone(), rule, now);
        send(state, fx, &room, text, Some(message.id.clone()), now, trace);
    };
    let place = Place::of(state, &room);
    match sentence {
        DecisionSentence::Recall { query } => {
            let found = if query.trim().is_empty() {
                latest_decision(state, &place)
            } else {
                recall_decisions(state, &place, &query, 1)
                    .into_iter()
                    .next()
            };
            let text = decision::answer(state, found?, now);
            reply(state, fx, text, "a decision is answered from its record");
        }
        DecisionSentence::WhatsOpen => {
            let text = open_list(state, &place, now);
            reply(state, fx, text, "\"what's open\" lists open proposals");
        }
        DecisionSentence::WhereAreWe => {
            let open = open_in(state, &room);
            let text = if open.is_empty() {
                NOTHING_OPEN.to_owned()
            } else {
                open.iter()
                    .take(TALLIES)
                    .filter_map(|pid| state.proposals.get(pid))
                    .map(|p| tally(state, p, now))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            reply(state, fx, text, "\"where are we\" reads the tally");
        }
        _ if role < Role::Member => {
            let text = decline_line(state, &room);
            reply(state, fx, text, "guests are answered, never obeyed");
        }
        _ if in_dm => reply(
            state,
            fx,
            POLL_IN_A_ROOM.to_owned(),
            "proposals live in rooms",
        ),
        DecisionSentence::Poll { topic, options } => {
            let zone = zone_of(state, &message.author);
            let mut offers: Vec<Offered> = Vec::new();
            for o in options.iter().filter_map(|o| offered(o, &zone, now)) {
                if !offers
                    .iter()
                    .any(|x| x.label.eq_ignore_ascii_case(&o.label))
                {
                    offers.push(o);
                }
            }
            if offers.len() < 2 {
                reply(
                    state,
                    fx,
                    TWO_OPTIONS.to_owned(),
                    "a poll needs two options",
                );
                return Some(false);
            }
            offers.truncate(MAX_OPTIONS);
            let trace = Trace::new(trigger.clone(), OPEN_RULE, now);
            open(
                state,
                Opening {
                    room: room.clone(),
                    thread_root: thread_root(state, message),
                    reply_to: Some(message.id.clone()),
                    topic: topic.map(|t| capitalise(&t)),
                    options: offers,
                    kind: ProposalKind::Choice,
                    organiser: Some(message.author.clone()),
                    rule: default_rule(state),
                    ballot: true,
                    source: message.id.clone(),
                    zone,
                    reopens: None,
                    trace,
                },
                now,
                fx,
            );
        }
        DecisionSentence::WhoIsIn { when } => {
            let zone = zone_of(state, &message.author);
            let mut o = offered(&when, &zone, now)?;
            if o.when.is_none()
                && let Some(r) = resolve_time_rules(&when, &zone, now)
            {
                o.when = Some(r.meaning);
                o.echo = Some(r.echo);
                o.unresolved = false;
            }
            let trace = Trace::new(trigger.clone(), OPEN_RULE, now);
            open(
                state,
                Opening {
                    room: room.clone(),
                    thread_root: thread_root(state, message),
                    reply_to: Some(message.id.clone()),
                    topic: None,
                    options: vec![o],
                    kind: ProposalKind::Attendance,
                    organiser: Some(message.author.clone()),
                    rule: CloseRule::WhoeverIsComing,
                    ballot: true,
                    source: message.id.clone(),
                    zone,
                    reopens: None,
                    trace,
                },
                now,
                fx,
            );
        }
        DecisionSentence::Revote => revote(state, message, role, now, fx),
        _ if role < Role::Lead => {
            let text = decline_line(state, &room);
            reply(
                state,
                fx,
                text,
                "only a lead decides or changes how a proposal closes",
            );
        }
        DecisionSentence::GoWith { choice, reason } => {
            let open = open_in(state, &room);
            let Some(pid) = open.last().cloned() else {
                reply(state, fx, NOTHING_OPEN.to_owned(), "nothing to decide");
                return Some(false);
            };
            let hit = open.iter().rev().find_map(|pid| {
                let p = state.proposals.get(pid)?;
                match named(&choice, p) {
                    Named::One { index, .. } => Some((pid.clone(), index)),
                    _ => None,
                }
            });
            let Some((pid, index)) = hit else {
                let options = state
                    .proposals
                    .get(&pid)
                    .map(Proposal::options_or)
                    .unwrap_or_default();
                let text = format!(
                    "{} isn't one of the options: {options}.",
                    capitalise(&choice)
                );
                reply(state, fx, text, "a lead picks one of the options");
                return Some(false);
            };
            if let Some(p) = state.proposals.get_mut(&pid) {
                p.add_view(index, &message.author, &message.id, &message.text);
            }
            let closer = Closer::Decider {
                user: message.author.clone(),
                message: Some(message.id.clone()),
                reason,
            };
            close(state, &pid, index, closer, now, fx);
        }
        DecisionSentence::Rule { rule, this_one } => {
            let rule = match rule {
                RuleWord::LeadDecides => CloseRule::LeadDecides,
                RuleWord::MajorityOfActive => CloseRule::MajorityOfActive,
                RuleWord::MajorityOfVoters => CloseRule::MajorityOfVoters,
                RuleWord::Unanimous => CloseRule::Unanimous,
                RuleWord::WhoeverIsComing => CloseRule::WhoeverIsComing,
            };
            let text = match open_in(state, &room).last().cloned() {
                Some(pid) => set_rule(state, &pid, rule, message, now, fx),
                None if this_one => NOTHING_OPEN.to_owned(),
                None => {
                    let by = state.display_name(&room, &message.author);
                    let line = format!("New proposals close on {}.", rule.name());
                    fx.push(Effect::Notice(format!(
                        "Decision rule: {line} Changed by {by}."
                    )));
                    state.decision_rule = Some(rule);
                    line
                }
            };
            if !text.is_empty() {
                reply(
                    state,
                    fx,
                    text,
                    "a lead's change to the close rule applies at once",
                );
            }
        }
        DecisionSentence::Deadline { when } => {
            let Some(pid) = open_in(state, &room).last().cloned() else {
                reply(
                    state,
                    fx,
                    NOTHING_OPEN.to_owned(),
                    "nothing to set a deadline on",
                );
                return Some(false);
            };
            let zone = zone_of(state, &message.author);
            match resolve_time_rules(&when, &zone, now) {
                Some(r) => {
                    let text = match deadline_from(&r.meaning, &zone) {
                        Some(at) => set_deadline(state, &pid, at, now, fx),
                        None => UNREADABLE_TIME.to_owned(),
                    };
                    reply(state, fx, text, "a lead's deadline applies at once");
                }
                None if wants_resolver(&when) => {
                    let pending = PendingTime {
                        room: room.clone(),
                        message: message.id.clone(),
                        text: when.clone(),
                        target: TimeTarget::Deadline {
                            proposal: pid,
                            reply_to: message.id.clone(),
                        },
                        asked_at: now,
                        guard: None,
                    };
                    ask_time(state, pending, &zone, true, fx);
                    return Some(true);
                }
                None => reply(
                    state,
                    fx,
                    UNREADABLE_TIME.to_owned(),
                    "a deadline needs a time",
                ),
            }
        }
    }
    Some(false)
}

/// The message a proposal hangs off: the one the ask replied to, else the ask.
fn thread_root(state: &GroupState, message: &Message) -> PlatformMessageId {
    message
        .reply_to
        .clone()
        .filter(|parent| {
            state
                .rooms
                .get(&message.room)
                .is_some_and(|r| r.messages.iter().any(|m| &m.id == parent))
        })
        .unwrap_or_else(|| message.id.clone())
}

/// Changes how an open proposal closes, posting a ballot when the new rule
/// needs one, and returns the line that says so.
fn set_rule(
    state: &mut GroupState,
    pid: &ProposalId,
    rule: CloseRule,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    let Some(p) = state.proposals.get_mut(pid) else {
        return String::new();
    };
    let old = std::mem::replace(&mut p.rule, rule.clone());
    let title = p.title();
    let needs = rule.needs_ballot() && p.ballot == Ballot::None;
    let (room, kind, n) = (p.room.clone(), p.kind, p.options.len());
    if needs {
        let ballot = ballot_for(state, &room, kind, n);
        if let Some(p) = state.proposals.get_mut(pid) {
            p.ballot = ballot;
        }
        let trace = Trace::new(
            addressed_trigger(state, message),
            "a lead put it to a vote",
            now,
        );
        post_ballot(state, pid, Some(message.id.clone()), now, trace, fx);
    }
    disarm(state, pid, fx);
    arm(state, pid, now, fx);
    let by = state.display_name(&room, &message.author);
    fx.push(Effect::Notice(format!(
        "Rule for {title} changed from {} to {} by {by}.",
        old.name(),
        rule.name()
    )));
    settle(state, pid, now, fx);
    if needs {
        return String::new();
    }
    match rule {
        CloseRule::LeadDecides => format!("A lead decides {title} now."),
        other => format!("{title} closes on {} now.", other.name()),
    }
}

/// "re-vote": a fresh copy of the room's last closed proposal. Under lead
/// decides only a lead may ask.
fn revote(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = message.room.clone();
    let trigger = addressed_trigger(state, message);
    let say = |state: &mut GroupState, fx: &mut Vec<Effect>, text: String| {
        let trace = Trace::new(trigger.clone(), "\"re-vote\" reopens a copy", now);
        send(state, fx, &room, text, Some(message.id.clone()), now, trace);
    };
    if let Some(pid) = open_in(state, &room).last()
        && let Some(p) = state.proposals.get(pid)
    {
        let text = format!("{} is still open.", p.title());
        say(state, fx, text);
        return;
    }
    let last = state
        .proposals
        .values()
        .filter(|p| p.room == room && !p.is_open())
        .max_by_key(|p| (p.opened_at, p.id.seq()))
        .cloned();
    let Some(old) = last else {
        say(state, fx, "Nothing here to vote on again.".to_owned());
        return;
    };
    if old.rule == CloseRule::LeadDecides && role < Role::Lead {
        let text = decline_line(state, &room);
        say(state, fx, text);
        return;
    }
    let reopens = match &old.status {
        ProposalStatus::Closed(did) => Some(did.clone()),
        _ => None,
    };
    let options = old
        .options
        .iter()
        .map(|o| Offered {
            label: o.label.clone(),
            when: o.when.clone(),
            echo: o.echo.clone(),
            unresolved: false,
        })
        .collect();
    let zone = zone_of(state, &message.author);
    let trace = Trace::new(trigger.clone(), "\"re-vote\" reopens a copy", now);
    open(
        state,
        Opening {
            room: room.clone(),
            thread_root: message.id.clone(),
            reply_to: Some(message.id.clone()),
            topic: old.topic.clone(),
            options,
            kind: old.kind,
            organiser: Some(message.author.clone()),
            rule: old.rule.clone(),
            ballot: true,
            source: message.id.clone(),
            zone,
            reopens,
            trace,
        },
        now,
        fx,
    );
}

/// Two lines for one open proposal: who chose what, and how long it has
/// been open and how it closes.
fn tally(state: &GroupState, p: &Proposal, now: Instant) -> String {
    let open_for = duration(now.seconds_since(p.opened_at));
    let title = p.title();
    let names = |users: &mut dyn Iterator<Item = &PlatformUserId>| -> Vec<String> {
        let mut seen = BTreeSet::new();
        users
            .filter(|u| seen.insert((*u).clone()))
            .map(|u| state.display_name(&p.room, u))
            .collect()
    };
    if p.rule == CloseRule::LeadDecides && p.ballot == Ballot::None {
        let parts: Vec<String> = p
            .options
            .iter()
            .map(|o| {
                let who = names(&mut o.views.iter().map(|(u, _)| u));
                if who.is_empty() {
                    format!("{}: nobody yet", o.label)
                } else {
                    format!("{} ({})", o.label, who.join(", "))
                }
            })
            .collect();
        return format!(
            "{title}: {}.\nOpen {open_for}; a lead decides.",
            parts.join(", ")
        );
    }
    let closes = within(p.deadline.seconds_since(now));
    if p.kind == ProposalKind::Attendance {
        let voters = p.voters();
        let who = names(&mut voters.iter());
        let what = p
            .options
            .first()
            .map_or_else(String::new, |o| attendance_phrase(state, o, now));
        let line = if who.is_empty() {
            format!("Who's in for {what}: nobody yet.")
        } else {
            format!("Who's in for {what}: {}.", join_names(&who, "and"))
        };
        return format!("{line}\nOpen {open_for}; closes {closes}.");
    }
    let hint = match &p.rule {
        CloseRule::MajorityOfActive => {
            let mut all = active_members(state, &p.room, now);
            all.extend(p.voters());
            let need = all.len() / 2 + 1;
            format!(", or once one has {need} of {}", all.len())
        }
        CloseRule::LeadDecides => ", then a lead decides".to_owned(),
        _ => String::new(),
    };
    format!(
        "{title}: {}.\nOpen {open_for}; closes {closes}{hint}.",
        counts_text(p)
    )
}

/// "what's open": open proposals the place may see, one line each.
fn open_list(state: &GroupState, place: &Place, now: Instant) -> String {
    let mut open: Vec<&Proposal> = state
        .proposals
        .values()
        .filter(|p| p.is_open() && decision::visible(state, &p.room, place))
        .collect();
    open.sort_by_key(|p| (p.opened_at, p.id.seq()));
    if open.is_empty() {
        return NOTHING_OPEN_ANYWHERE.to_owned();
    }
    open.iter()
        .take(OPEN_LINES)
        .map(|p| {
            format!(
                "{} in {}: {}, open {}.",
                p.title(),
                state.room_label(&p.room),
                standing(p),
                duration(now.seconds_since(p.opened_at))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// "2 votes", "1 view, lead decides", "3 in".
pub(crate) fn standing(p: &Proposal) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if p.rule == CloseRule::LeadDecides && p.ballot == Ballot::None {
        let views: usize = p.options.iter().map(|o| o.views.len()).sum();
        return format!("{}, lead decides", plural(views, "view", "views"));
    }
    let votes = p.voters().len();
    match p.kind {
        ProposalKind::Attendance => format!("{votes} in"),
        ProposalKind::Choice => plural(votes, "vote", "votes"),
    }
}

/// What the classifier is told is open in `room`: "Dinner: Thursday or
/// Friday (2 votes)".
pub(crate) fn open_items(state: &GroupState, room: &PlatformRoomId) -> Vec<String> {
    open_in(state, room)
        .iter()
        .filter_map(|pid| state.proposals.get(pid))
        .map(|p| match p.kind {
            ProposalKind::Attendance => {
                format!("{} ({})", p.question.trim_end_matches('?'), standing(p))
            }
            ProposalKind::Choice => match &p.topic {
                Some(topic) => format!("{topic}: {} ({})", p.options_or(), standing(p)),
                None => format!("{} ({})", p.options_or(), standing(p)),
            },
        })
        .collect()
}

/// "Open: Dinner: Thursday or Friday, 2 votes, open 3 hours." for the
/// system prompt, as the place may see.
pub(crate) fn open_line(state: &GroupState, place: &Place, now: Instant) -> Option<String> {
    let parts: Vec<String> = state
        .proposals
        .values()
        .filter(|p| p.is_open() && decision::visible(state, &p.room, place))
        .map(|p| {
            format!(
                "{} in {}, {}, open {}",
                p.question.trim_end_matches('?'),
                state.room_label(&p.room),
                standing(p),
                duration(now.seconds_since(p.opened_at))
            )
        })
        .collect();
    (!parts.is_empty()).then(|| format!("Open: {}.", parts.join("; ")))
}

/// The rules pass's open-decision signal: 0.9 when a line plainly picks an
/// option of an open proposal in its room, 0.6 when it only names one.
pub(crate) fn rules_signal(state: &GroupState, room: &PlatformRoomId, text: &str) -> f32 {
    let mut best: f32 = 0.0;
    for p in state
        .proposals
        .values()
        .filter(|p| p.is_open() && &p.room == room)
    {
        let signal = match named(text, p) {
            Named::One { clear: true, .. } | Named::Abstain => 0.9,
            Named::One { .. } | Named::Against { .. } | Named::Several => 0.6,
            Named::Nothing => 0.0,
        };
        best = best.max(signal);
    }
    if is_question(text) {
        best.min(0.6)
    } else {
        best
    }
}
