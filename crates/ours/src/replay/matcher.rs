//! Matches the journal's answers to what a fold asked for. A journal written
//! by another engine version names other ids for the same requests, so an
//! answer goes to the fold's id shifted by the offset learned so far, then
//! to the id as written, then to the oldest outstanding one of its kind.

use super::fold::minted_number;
use ours_core::{Effect, GroupState, Input};
use ours_types::{
    ActionId, GroupId, InboundEvent, Instant, OutboundAction, OutsideResult, PlatformMessageId,
    PlatformRoomId, RequestId, TimeResolution, TimerId,
};

/// How long an outstanding item waits for its answer before it is given up.
/// Each is well past the runner's own timeout for that kind of call.
const MODEL_SECONDS: i64 = 10 * 60;
const CLASSIFY_SECONDS: i64 = 5 * 60;
const EXTRACT_SECONDS: i64 = 30 * 60;
const RESOLVE_SECONDS: i64 = 5 * 60;
const SEND_SECONDS: i64 = 2 * 60;
const OUTSIDE_SECONDS: i64 = 10 * 60;
/// A logged timer may fire this much before the fold's copy is due.
const EARLY_SECONDS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Stage {
    Classify,
    Extract,
    Resolve,
}

impl Stage {
    fn expiry(self) -> i64 {
        match self {
            Stage::Classify => CLASSIFY_SECONDS,
            Stage::Extract => EXTRACT_SECONDS,
            Stage::Resolve => RESOLVE_SECONDS,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Stage::Classify => "classify",
            Stage::Extract => "extract",
            Stage::Resolve => "resolve_time",
        }
    }
}

type StageKey = (Stage, PlatformRoomId, PlatformMessageId);

#[derive(Debug)]
struct Asked<T> {
    id: T,
    seq: u64,
    at: Instant,
    /// Outstanding across a restart or from before the fold began, so never reported.
    excused: bool,
    what: String,
}

/// A request the journal never answered.
#[derive(Debug)]
pub(crate) struct Gap {
    pub seq: u64,
    pub at: Instant,
    pub label: &'static str,
    pub what: String,
}

/// A logged input in the fold's ids, or a note on why it has no match.
#[derive(Debug)]
pub(crate) struct Matched {
    pub input: Option<Input>,
    pub unmatched: Option<String>,
}

impl Matched {
    fn fed(input: Input) -> Matched {
        Matched {
            input: Some(input),
            unmatched: None,
        }
    }

    fn unmatched(note: String) -> Matched {
        Matched {
            input: None,
            unmatched: Some(note),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Matcher {
    group: GroupId,
    /// The fold's id counter minus the journal's, from the last match.
    delta: i64,
    requests: Vec<Asked<RequestId>>,
    stages: Vec<Asked<StageKey>>,
    /// Sends and whether they produce a message, which their result shows.
    sends: Vec<Asked<(ActionId, bool)>>,
    outside: Vec<Asked<ActionId>>,
    /// Stage answers without a match pass through until then: their requests
    /// were made before the snapshot the fold started from.
    lenient_until: Option<Instant>,
}

impl Matcher {
    pub(crate) fn new(group: GroupId) -> Matcher {
        Matcher {
            group,
            delta: 0,
            requests: Vec::new(),
            stages: Vec::new(),
            sends: Vec::new(),
            outside: Vec::new(),
            lenient_until: None,
        }
    }

    /// Takes what a snapshot's state is still waiting on as outstanding.
    pub(crate) fn seed(&mut self, state: &GroupState, seq: u64, at: Instant) {
        let mut requests: Vec<_> = state.pending_requests.iter().collect();
        requests.sort_by_key(|(id, p)| (p.asked_at, (*id).clone()));
        for (id, p) in requests {
            self.requests
                .push(excused(id.clone(), seq, p.asked_at, String::new()));
        }
        for p in &state.extractions {
            let key = (Stage::Extract, p.room.clone(), p.message.clone());
            self.stages
                .push(excused(key, seq, p.asked_at, String::new()));
        }
        for p in &state.pending_times {
            let key = (Stage::Resolve, p.room.clone(), p.message.clone());
            self.stages
                .push(excused(key, seq, p.asked_at, String::new()));
        }
        for (id, p) in &state.pending_actions {
            self.sends
                .push(excused((id.clone(), true), seq, p.at, String::new()));
        }
        for (id, p) in &state.pending_dms {
            self.sends
                .push(excused((id.clone(), true), seq, p.at, String::new()));
        }
        for id in state.pending_polls.keys() {
            self.sends
                .push(excused((id.clone(), true), seq, at, String::new()));
        }
        for record in &state.actions {
            if let ours_core::ActionStatus::Sending { since } = record.status {
                self.outside
                    .push(excused(record.id.clone(), seq, since, String::new()));
            }
        }
        self.sends.sort_by_key(|a| a.at);
        self.lenient_until = Some(at.plus_seconds(EXTRACT_SECONDS));
    }

    /// Notes what a step of the fold asked for or sent.
    pub(crate) fn note(&mut self, seq: u64, at: Instant, effects: &[Effect], state: &GroupState) {
        for effect in effects {
            match effect {
                Effect::AskModel(request) => {
                    let what = match state.pending_requests.get(&request.id) {
                        Some(p) => format!(
                            "asked the model ({}) about {} in #{}",
                            super::describe::answering_name(p.answering),
                            p.message,
                            p.room
                        ),
                        None => "asked the model".to_owned(),
                    };
                    self.requests.push(asked(request.id.clone(), seq, at, what));
                }
                Effect::Classify(r) => self.stage(Stage::Classify, &r.room, &r.message, seq, at),
                Effect::Extract(r) => self.stage(Stage::Extract, &r.room, &r.message, seq, at),
                Effect::ResolveTime(r) => self.stage(Stage::Resolve, &r.room, &r.message, seq, at),
                Effect::Send(out) => {
                    let key = (out.id.clone(), produces(&out.action));
                    self.sends.push(asked(key, seq, at, String::new()));
                }
                Effect::Outside { action, .. } => {
                    self.outside
                        .push(asked(action.clone(), seq, at, String::new()));
                }
                Effect::SetTimer { .. }
                | Effect::CancelTimer { .. }
                | Effect::Notice(_)
                | Effect::Redact { .. } => {}
            }
        }
    }

    fn stage(
        &mut self,
        stage: Stage,
        room: &PlatformRoomId,
        message: &PlatformMessageId,
        seq: u64,
        at: Instant,
    ) {
        let what = format!(
            "asked to {} {message} in #{room}",
            stage.label().replace('_', " ")
        );
        let key = (stage, room.clone(), message.clone());
        self.stages.push(asked(key, seq, at, what));
    }

    /// The logged input with its ids swapped for the fold's.
    pub(crate) fn translate(&mut self, at: Instant, input: Input, state: &GroupState) -> Matched {
        match input {
            Input::ModelReply { request, reply } => {
                let Some(i) =
                    self.pick(&self.requests, request.as_str(), |id| id.as_str(), |_| true)
                else {
                    return Matched::unmatched(format!("the log answers model request {request}"));
                };
                let asked = self.requests.remove(i);
                self.learn(request.as_str(), asked.id.as_str());
                Matched::fed(Input::ModelReply {
                    request: asked.id,
                    reply,
                })
            }
            Input::TimerFired { id } => match self.timer(&id, at, state) {
                Some(twin) => {
                    self.learn(id.as_str(), twin.as_str());
                    Matched::fed(Input::TimerFired { id: twin })
                }
                None => Matched::unmatched(format!("the log fires timer {id}")),
            },
            Input::Inbound(InboundEvent::ActionResult {
                id,
                ok,
                message,
                error,
            }) => {
                let produced = message.is_some();
                let fits = |a: &Asked<(ActionId, bool)>| !ok || a.id.1 == produced;
                let Some(i) = self.pick(&self.sends, id.as_str(), |(a, _)| a.as_str(), fits) else {
                    return Matched::unmatched(format!("the log reports action {id}"));
                };
                let twin = self.sends.remove(i).id.0;
                self.learn(id.as_str(), twin.as_str());
                Matched::fed(Input::Inbound(InboundEvent::ActionResult {
                    id: twin,
                    ok,
                    message,
                    error,
                }))
            }
            Input::OutsideResult(result) => {
                let logged = result.action.clone();
                let Some(i) = self.pick(&self.outside, logged.as_str(), |a| a.as_str(), |_| true)
                else {
                    return Matched::unmatched(format!("the log reports outside action {logged}"));
                };
                let twin = self.outside.remove(i).id;
                self.learn(logged.as_str(), twin.as_str());
                Matched::fed(Input::OutsideResult(OutsideResult {
                    action: twin,
                    ..result
                }))
            }
            Input::Scored {
                room,
                message,
                score,
            } => match self.take_stage(Stage::Classify, &room, &message, at) {
                Some(found) => {
                    let (room, message) = found.unwrap_or((room, message));
                    Matched::fed(Input::Scored {
                        room,
                        message,
                        score,
                    })
                }
                None => Matched::unmatched(format!("the log scores {message} in #{room}")),
            },
            Input::Extracted {
                room,
                message,
                extraction,
            } => match self.take_stage(Stage::Extract, &room, &message, at) {
                Some(found) => {
                    let (room, message) = found.unwrap_or((room, message));
                    Matched::fed(Input::Extracted {
                        room,
                        message,
                        extraction,
                    })
                }
                None => Matched::unmatched(format!("the log extracts from {message} in #{room}")),
            },
            Input::TimeResolved(resolution) => {
                match self.take_stage(Stage::Resolve, &resolution.room, &resolution.message, at) {
                    Some(found) => {
                        let (room, message) =
                            found.unwrap_or((resolution.room.clone(), resolution.message.clone()));
                        Matched::fed(Input::TimeResolved(TimeResolution {
                            room,
                            message,
                            ..resolution
                        }))
                    }
                    None => Matched::unmatched(format!(
                        "the log resolves a time in {} in #{}",
                        resolution.message, resolution.room
                    )),
                }
            }
            Input::Inbound(event @ InboundEvent::Ready { .. }) => {
                // A restart drops whatever was in flight; the log will not answer it.
                self.excuse();
                Matched::fed(Input::Inbound(event))
            }
            other => Matched::fed(other),
        }
    }

    /// Gives up on what has waited too long, returning the requests among
    /// them that should have had an answer.
    pub(crate) fn expire(&mut self, at: Instant) -> Vec<Gap> {
        let mut gaps = Vec::new();
        let late = |since: Instant, limit: i64| at.seconds_since(since) > limit;
        self.requests.retain(|a| {
            let keep = !late(a.at, MODEL_SECONDS);
            if !keep && !a.excused {
                gaps.push(Gap {
                    seq: a.seq,
                    at: a.at,
                    label: "ask_model",
                    what: a.what.clone(),
                });
            }
            keep
        });
        self.stages.retain(|a| {
            let keep = !late(a.at, a.id.0.expiry());
            if !keep && !a.excused {
                gaps.push(Gap {
                    seq: a.seq,
                    at: a.at,
                    label: a.id.0.label(),
                    what: a.what.clone(),
                });
            }
            keep
        });
        self.sends.retain(|a| !late(a.at, SEND_SECONDS));
        self.outside.retain(|a| !late(a.at, OUTSIDE_SECONDS));
        gaps
    }

    fn excuse(&mut self) {
        self.requests.iter_mut().for_each(|a| a.excused = true);
        self.stages.iter_mut().for_each(|a| a.excused = true);
    }

    /// Where the logged id lands in `list`: its id under the learned offset,
    /// the id as written, or the oldest that fits.
    fn pick<T>(
        &self,
        list: &[Asked<T>],
        logged: &str,
        id_of: impl Fn(&T) -> &str,
        fits: impl Fn(&Asked<T>) -> bool,
    ) -> Option<usize> {
        let find = |want: &str| list.iter().position(|a| fits(a) && id_of(&a.id) == want);
        self.aligned(logged)
            .and_then(|want| find(&want))
            .or_else(|| find(logged))
            .or_else(|| list.iter().position(&fits))
    }

    /// The fold's armed timer for a logged one: by id as for the others, else
    /// the one that fell due most recently, the lowest id among equals since
    /// the runner fires ties in id order.
    fn timer(&self, logged: &TimerId, at: Instant, state: &GroupState) -> Option<TimerId> {
        let armed = &state.timers;
        self.aligned(logged.as_str())
            .map(TimerId::new)
            .filter(|id| armed.contains_key(id))
            .or_else(|| armed.contains_key(logged).then(|| logged.clone()))
            .or_else(|| {
                armed
                    .iter()
                    .filter(|(_, t)| t.at <= at.plus_seconds(EARLY_SECONDS))
                    .max_by(|a, b| a.1.at.cmp(&b.1.at).then_with(|| b.0.cmp(a.0)))
                    .map(|(id, _)| id.clone())
            })
    }

    /// The outstanding stage request a logged answer is for: the same
    /// message, else the oldest in the same room. `Some(None)` passes the
    /// answer through unchanged; `None` means nothing matches.
    fn take_stage(
        &mut self,
        stage: Stage,
        room: &PlatformRoomId,
        message: &PlatformMessageId,
        at: Instant,
    ) -> Option<Option<(PlatformRoomId, PlatformMessageId)>> {
        let exact = self
            .stages
            .iter()
            .position(|a| a.id.0 == stage && &a.id.1 == room && &a.id.2 == message);
        let found = exact.or_else(|| {
            self.stages
                .iter()
                .position(|a| a.id.0 == stage && &a.id.1 == room)
        });
        match found {
            Some(i) => {
                let (_, room, message) = self.stages.remove(i).id;
                Some(Some((room, message)))
            }
            None if self.lenient_until.is_some_and(|until| at <= until) => Some(None),
            None => None,
        }
    }

    fn aligned(&self, logged: &str) -> Option<String> {
        let n = minted_number(&self.group, logged)?;
        let prefix = format!("{}-", self.group);
        let tag: String = logged
            .strip_prefix(&prefix)?
            .chars()
            .take_while(char::is_ascii_lowercase)
            .collect();
        let shifted = i64::try_from(n).ok()?.checked_add(self.delta)?;
        (shifted > 0).then(|| format!("{prefix}{tag}{shifted}"))
    }

    fn learn(&mut self, logged: &str, fold: &str) {
        let (Some(a), Some(b)) = (
            minted_number(&self.group, logged),
            minted_number(&self.group, fold),
        ) else {
            return;
        };
        if let (Ok(a), Ok(b)) = (i64::try_from(a), i64::try_from(b)) {
            self.delta = b - a;
        }
    }
}

fn asked<T>(id: T, seq: u64, at: Instant, what: String) -> Asked<T> {
    Asked {
        id,
        seq,
        at,
        excused: false,
        what,
    }
}

fn excused<T>(id: T, seq: u64, at: Instant, what: String) -> Asked<T> {
    Asked {
        excused: true,
        ..asked(id, seq, at, what)
    }
}

/// Whether a send's result names a message.
fn produces(action: &OutboundAction) -> bool {
    matches!(
        action,
        OutboundAction::SendMessage { .. }
            | OutboundAction::SendDm { .. }
            | OutboundAction::CreatePoll { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_core::{Timer, TimerPurpose};
    use ours_types::Kind;

    #[test]
    fn an_unmatched_timer_goes_to_the_first_of_those_due_together() {
        let group = GroupId::new("g");
        let mut state = GroupState::new(group.clone(), Kind::Friends);
        let at = Instant::from_millis(1_000_000).unwrap();
        for (id, due) in [("b", at), ("a", at), ("c", at.plus_seconds(-60))] {
            state.timers.insert(
                TimerId::new(id),
                Timer {
                    at: due,
                    purpose: TimerPurpose::Retention,
                },
            );
        }
        let mut matcher = Matcher::new(group);
        let fired = |matcher: &mut Matcher, state: &GroupState| {
            let input = Input::TimerFired {
                id: TimerId::new("from-the-log"),
            };
            match matcher.translate(at, input, state).input {
                Some(Input::TimerFired { id }) => id,
                other => panic!("expected a timer, got {other:?}"),
            }
        };
        assert_eq!(fired(&mut matcher, &state), TimerId::new("a"));
        state.timers.remove(&TimerId::new("a"));
        assert_eq!(fired(&mut matcher, &state), TimerId::new("b"));
    }
}
