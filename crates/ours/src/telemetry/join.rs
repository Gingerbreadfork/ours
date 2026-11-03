//! Joins each scored message to what the engine did about it and what
//! people did in the day after, while the journal is re-folded.

use super::{Acted, Outcome, Record, WINDOW_HOURS};
use ours_core::attention::SHUSH_SECONDS;
use ours_core::reactions::{Meaning, means};
use ours_core::{Answering, Effect, FollowUp, GroupState, Input, TimerPurpose, rules_score};
use ours_types::{
    ActionId, InboundEvent, Instant, MessageKind, OutboundAction, PlatformMessageId,
    PlatformRoomId, PlatformUserId, Score,
};
use std::collections::BTreeMap;

const WINDOW_SECONDS: i64 = WINDOW_HOURS as i64 * 60 * 60;

/// Reactions that are not approval.
const NEGATIVE: [&str; 14] = [
    "👎",
    "🤫",
    "🙄",
    "😒",
    "😠",
    "😡",
    "🙅",
    "❌",
    "🛑",
    "🤐",
    "😑",
    "-1",
    "thumbsdown",
    "shush",
];

#[derive(Debug)]
struct Entry {
    seq: u64,
    at: Instant,
    room: PlatformRoomId,
    message: PlatformMessageId,
    asker: Option<PlatformUserId>,
    question: bool,
    spoke: bool,
    actions: Vec<ActionId>,
    record: Record,
}

/// What the step about to run is about, read before it changes the state.
#[derive(Debug, Default)]
pub(crate) struct Before {
    about: Option<usize>,
    chime: bool,
    shush_mark: Option<Instant>,
}

#[derive(Debug)]
pub(crate) struct Join {
    host: String,
    group: String,
    since: u64,
    entries: Vec<Entry>,
    by_message: BTreeMap<(PlatformRoomId, PlatformMessageId), usize>,
    by_action: BTreeMap<ActionId, usize>,
    /// The agent's messages, by the entry that caused them.
    by_line: BTreeMap<PlatformMessageId, usize>,
}

impl Join {
    pub(crate) fn new(host: String, group: String, since: u64) -> Join {
        Join {
            host,
            group,
            since,
            entries: Vec::new(),
            by_message: BTreeMap::new(),
            by_action: BTreeMap::new(),
            by_line: BTreeMap::new(),
        }
    }

    pub(crate) fn before(
        &mut self,
        state: &GroupState,
        seq: u64,
        at: Instant,
        input: &Input,
    ) -> Before {
        let mut before = Before::default();
        match input {
            Input::Scored {
                room,
                message,
                score,
            } if seq > self.since => {
                before.about = Some(self.open(state, seq, at, room, message, score));
            }
            Input::TimerFired { id } => {
                before.about = match state.timers.get(id).map(|t| &t.purpose) {
                    Some(
                        TimerPurpose::RaiseHand { room, message }
                        | TimerPurpose::AnswerAnyway { room, message },
                    ) => self.find(room, message),
                    Some(TimerPurpose::ModelGuard { request }) => state
                        .pending_requests
                        .get(request)
                        .and_then(|p| self.find(&p.room, &p.message)),
                    _ => None,
                };
            }
            Input::ModelReply { request, .. } => {
                if let Some(p) = state.pending_requests.get(request) {
                    before.about = self.find(&p.room, &p.message);
                    before.chime = p.answering == Answering::Chime;
                }
            }
            Input::Inbound(InboundEvent::ReactionAdded {
                room,
                message,
                user,
                emoji,
                ..
            }) if human(state, room, user) => {
                let raised = state
                    .hands
                    .get(message)
                    .is_some_and(|h| &h.room == room && !h.observed);
                let entry = self
                    .by_line
                    .get(message)
                    .copied()
                    .or_else(|| raised.then(|| self.find(room, message)).flatten());
                if let Some(i) = entry {
                    self.reacted(state, i, emoji, at);
                }
            }
            Input::Inbound(InboundEvent::Message(m)) => {
                before.shush_mark = state.shushed.get(&m.author).copied();
                if human(state, &m.room, &m.author) {
                    self.heard(&m.room, &m.id, &m.author, at);
                }
            }
            _ => {}
        }
        before
    }

    pub(crate) fn after(
        &mut self,
        state: &GroupState,
        input: &Input,
        effects: &[Effect],
        before: Before,
        at: Instant,
    ) {
        if let Some(i) = before.about {
            self.acted(state, i, effects, before.chime);
        }
        match input {
            Input::Inbound(InboundEvent::ActionResult {
                id,
                ok: true,
                message: Some(line),
                ..
            }) => {
                if let Some(&i) = self.by_action.get(id) {
                    self.by_line.insert(line.clone(), i);
                }
            }
            Input::Inbound(InboundEvent::Message(m)) => {
                // A "shut up" is seen in the state: the speaker is shushed as of now.
                let mark = state.shushed.get(&m.author).copied();
                if mark.is_some()
                    && mark == Some(at.plus_seconds(SHUSH_SECONDS))
                    && mark != before.shush_mark
                {
                    let target = m
                        .reply_to
                        .as_ref()
                        .and_then(|r| self.by_line.get(r).copied())
                        .or_else(|| self.latest_acted(&m.room, at));
                    if let Some(i) = target.filter(|i| self.within(*i, at)) {
                        self.entries[i].record.outcome.shushed = true;
                    }
                }
            }
            _ => {}
        }
        let takes_back = effects.iter().any(|e| {
            matches!(
                e,
                Effect::Send(out) if matches!(
                    out.action,
                    OutboundAction::DeleteMessage { .. } | OutboundAction::Unreact { .. }
                )
            )
        });
        if takes_back {
            self.undone(state, at);
        }
    }

    /// The finished records up to the first whose window is still open at
    /// `now`, and the last seq they cover.
    pub(crate) fn finish(self, now: Instant, last_seq: u64) -> (Vec<Record>, u64) {
        let open = self
            .entries
            .iter()
            .find(|e| now.seconds_since(e.at) < WINDOW_SECONDS)
            .map(|e| e.seq);
        let covered = match open {
            Some(seq) => seq.saturating_sub(1).max(self.since),
            None => last_seq.max(self.since),
        };
        let records = self
            .entries
            .into_iter()
            .filter(|e| e.seq <= covered)
            .map(|e| e.record)
            .collect();
        (records, covered)
    }

    fn open(
        &mut self,
        state: &GroupState,
        seq: u64,
        at: Instant,
        room: &PlatformRoomId,
        message: &PlatformMessageId,
        score: &Score,
    ) -> usize {
        let held = state
            .rooms
            .get(room)
            .and_then(|r| r.messages.iter().find(|m| &m.id == message));
        // Before the classifier's answer lands, the stored score is the rules pass's.
        let rules = state
            .rooms
            .get(room)
            .and_then(|r| r.score_of(message))
            .cloned()
            .or_else(|| held.map(|m| rules_score(state, room, m)))
            .unwrap_or_default();
        let hour = at.0.to_zoned(jiff::tz::TimeZone::UTC).hour();
        let question = score.kind == MessageKind::Question || score.unanswered_question > 0.0;
        // Someone may have answered while the classifier was still reading.
        let answered = question
            && held.is_some_and(|asked| {
                state.rooms.get(room).is_some_and(|r| {
                    r.messages
                        .iter()
                        .skip_while(|m| m.id != asked.id)
                        .skip(1)
                        .any(|m| m.author != asked.author && human(state, room, &m.author))
                })
            });
        let record = Record {
            host: self.host.clone(),
            group: self.group.clone(),
            kind: state.kind,
            dial: state.profile.dial,
            at_hour_utc: u8::try_from(hour).unwrap_or(0),
            score: score.clone(),
            rules,
            acted: Acted::Nothing,
            outcome: Outcome {
                positive_reactions: 0,
                shushed: false,
                undone: false,
                answered_by_human: answered,
                window_hours: WINDOW_HOURS,
            },
        };
        let index = self.entries.len();
        self.entries.push(Entry {
            seq,
            at,
            room: room.clone(),
            message: message.clone(),
            asker: held.map(|m| m.author.clone()),
            question,
            spoke: false,
            actions: Vec::new(),
            record,
        });
        self.by_message
            .insert((room.clone(), message.clone()), index);
        index
    }

    fn find(&self, room: &PlatformRoomId, message: &PlatformMessageId) -> Option<usize> {
        self.by_message
            .get(&(room.clone(), message.clone()))
            .copied()
    }

    fn within(&self, i: usize, at: Instant) -> bool {
        at.seconds_since(self.entries[i].at) < WINDOW_SECONDS
    }

    /// What the agent did about entry `i` in this step.
    fn acted(&mut self, state: &GroupState, i: usize, effects: &[Effect], chime: bool) {
        for effect in effects {
            let Effect::Send(out) = effect else {
                continue;
            };
            let entry = &self.entries[i];
            let acted = match &out.action {
                OutboundAction::React { message, emoji, .. } if *message == entry.message => {
                    if means(state, &entry.room, emoji, Meaning::RaisedHand) {
                        Acted::RaisedHand
                    } else if chime {
                        Acted::Chimed
                    } else {
                        Acted::Reacted
                    }
                }
                OutboundAction::SendMessage { reply_to, .. } => {
                    let ballot = state
                        .pending_actions
                        .get(&out.id)
                        .is_some_and(|p| matches!(p.then, Some(FollowUp::Ballot { .. })));
                    if ballot {
                        Acted::OpenedPoll
                    } else if reply_to.as_ref() == Some(&entry.message) {
                        self.entries[i].spoke = true;
                        if chime { Acted::Chimed } else { Acted::Spoke }
                    } else {
                        continue;
                    }
                }
                OutboundAction::CreatePoll { .. } => Acted::OpenedPoll,
                _ => continue,
            };
            let entry = &mut self.entries[i];
            if acted.rank() > entry.record.acted.rank() {
                entry.record.acted = acted;
            }
            entry.actions.push(out.id.clone());
            self.by_action.insert(out.id.clone(), i);
        }
    }

    fn reacted(&mut self, state: &GroupState, i: usize, emoji: &str, at: Instant) {
        if !self.within(i, at) {
            return;
        }
        let room = self.entries[i].room.clone();
        let outcome = &mut self.entries[i].record.outcome;
        if means(state, &room, emoji, Meaning::Shush) {
            outcome.shushed = true;
        } else if !negative(emoji) {
            outcome.positive_reactions = outcome.positive_reactions.saturating_add(1);
        }
    }

    /// A person spoke in `room`: an open question someone else asked counts
    /// as answered by a human if the agent had not answered it first.
    fn heard(
        &mut self,
        room: &PlatformRoomId,
        message: &PlatformMessageId,
        author: &PlatformUserId,
        at: Instant,
    ) {
        for entry in self.entries.iter_mut().rev() {
            if at.seconds_since(entry.at) >= WINDOW_SECONDS {
                break;
            }
            if &entry.room == room
                && entry.question
                && !entry.spoke
                && &entry.message != message
                && entry.asker.as_ref().is_some_and(|a| a != author)
            {
                entry.record.outcome.answered_by_human = true;
            }
        }
    }

    fn latest_acted(&self, room: &PlatformRoomId, at: Instant) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .rev()
            .take_while(|(_, e)| at.seconds_since(e.at) < WINDOW_SECONDS)
            .find(|(_, e)| &e.room == room && e.record.acted != Acted::Nothing)
            .map(|(i, _)| i)
    }

    fn undone(&mut self, state: &GroupState, at: Instant) {
        for entry in self.entries.iter_mut().rev() {
            if at.seconds_since(entry.at) >= WINDOW_SECONDS {
                break;
            }
            let undone = entry
                .actions
                .iter()
                .any(|id| ours_core::action::find(state, id).is_some_and(|r| r.is_undone()));
            if undone {
                entry.record.outcome.undone = true;
            }
        }
    }
}

/// Neither the agent nor a bot the room knows of.
fn human(state: &GroupState, room: &PlatformRoomId, user: &PlatformUserId) -> bool {
    !state.is_me(user)
        && !state
            .rooms
            .get(room)
            .and_then(|r| r.members.get(user))
            .is_some_and(|m| m.is_bot)
}

fn negative(emoji: &str) -> bool {
    let e = emoji.trim_matches(':');
    NEGATIVE.iter().any(|n| e.starts_with(n))
}
