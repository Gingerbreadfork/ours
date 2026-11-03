//! The two folds a replay runs side by side: the logged run, a pure re-fold
//! of the journal, and the replayed run, whose requests the oracle answers.
//! Ids the two mint for the same thing are paired, so the log's later
//! answers, timers and action results reach the right twin.

use super::describe::{observe, spent_at, summarise};
use super::fold::minted_number;
use super::matcher::Matcher;
use super::{Divergence, DivergenceKind, SlowInput, Tally, Timing};
use crate::runtime::model::{self, ChatSettings};
use ours_core::{Answering, Effect, GroupState, Input, ModelRequest, Plan, TimerPurpose, step};
use ours_model::{Classifier, Extractor, LargeModel, TimeResolver};
use ours_types::{
    ActionId, ClassifyRequest, ExtractRequest, Extraction, GroupId, InboundEvent, Instant,
    Outbound, OutboundAction, OutsideResult, PlatformMessageId, PlatformRoomId, RequestId, Score,
    TimeMeaning, TimeRequest, TimeResolution, TimerId,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

/// The runner's own limits on the small stages.
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(20);
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(60);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Follow-up steps one logged input may cause on the replayed side.
const MAX_FOLLOW_UPS: usize = 1000;
const SLOWEST_KEPT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Classify,
    Extract,
    Resolve,
}

type StageKey = (Stage, PlatformRoomId, PlatformMessageId);

#[derive(Debug)]
enum StageAnswer {
    /// Use the logged answer when it comes.
    Logged,
    Score(Score),
    Extraction(Extraction),
    Time(TimeResolution),
}

/// The live stages that answer the replayed run.
#[derive(Clone)]
pub(crate) struct Live {
    pub model: Arc<dyn LargeModel>,
    pub classifier: Arc<dyn Classifier>,
    pub extractor: Arc<dyn Extractor>,
    pub resolver: Arc<dyn TimeResolver>,
}

pub(crate) struct Run {
    group: GroupId,
    live: Option<Live>,
    chat: ChatSettings,
    strict: bool,
    reference: GroupState,
    candidate: GroupState,
    /// Matches the journal's answers to the reference's requests.
    matcher: Matcher,
    /// The first seq compared; earlier gaps belong to the warm-up.
    from: u64,
    pub inputs: u64,
    pub logged: Tally,
    pub replayed: Tally,
    pub divergences: Vec<Divergence>,
    pub model_calls: u32,
    pub stage_calls: u32,
    ref_requests: BTreeSet<RequestId>,
    ref_timers: BTreeSet<TimerId>,
    ref_actions: BTreeSet<ActionId>,
    ref_outside: BTreeSet<ActionId>,
    /// Reference model requests still pending, by what they are about.
    ref_open_requests: BTreeMap<RequestId, (PlatformRoomId, PlatformMessageId, Answering)>,
    /// Stage requests the reference made whose logged answer has not come.
    ref_open_stages: BTreeSet<StageKey>,
    requests: BTreeMap<RequestId, RequestId>,
    timers: BTreeMap<TimerId, TimerId>,
    actions: BTreeMap<ActionId, ActionId>,
    outside: BTreeMap<ActionId, ActionId>,
    /// Live answers waiting for the place of the logged answer they replace.
    replies: BTreeMap<RequestId, Result<Plan, String>>,
    stage_answers: BTreeMap<StageKey, StageAnswer>,
    /// Replayed timers with no twin in the reference, fired on the virtual clock.
    own_timers: BTreeSet<TimerId>,
    logged_us: u64,
    replayed_us: u64,
    live_us: u64,
    slowest: Vec<SlowInput>,
}

/// The reference effects of one step, waiting to be paired.
#[derive(Default)]
struct Twins {
    sends: Vec<(String, ActionId)>,
    timers: Vec<(String, TimerId)>,
    outside: VecDeque<ActionId>,
}

impl Twins {
    fn take_send(&mut self, key: &str) -> Option<ActionId> {
        let at = self.sends.iter().position(|(k, _)| k == key)?;
        Some(self.sends.remove(at).1)
    }

    fn take_timer(&mut self, key: &str) -> Option<TimerId> {
        let at = self.timers.iter().position(|(k, _)| k == key)?;
        Some(self.timers.remove(at).1)
    }
}

impl Run {
    pub(crate) fn new(
        group: GroupId,
        start: super::fold::Start,
        from: u64,
        live: Option<Live>,
        chat: ChatSettings,
        strict: bool,
    ) -> Run {
        let super::fold::Start { state, matcher, .. } = start;
        Run {
            group,
            live,
            chat,
            strict,
            matcher,
            from,
            candidate: state.clone(),
            reference: state,
            inputs: 0,
            logged: Tally::default(),
            replayed: Tally::default(),
            divergences: Vec::new(),
            model_calls: 0,
            stage_calls: 0,
            ref_requests: BTreeSet::new(),
            ref_timers: BTreeSet::new(),
            ref_actions: BTreeSet::new(),
            ref_outside: BTreeSet::new(),
            ref_open_requests: BTreeMap::new(),
            ref_open_stages: BTreeSet::new(),
            requests: BTreeMap::new(),
            timers: BTreeMap::new(),
            actions: BTreeMap::new(),
            outside: BTreeMap::new(),
            replies: BTreeMap::new(),
            stage_answers: BTreeMap::new(),
            own_timers: BTreeSet::new(),
            logged_us: 0,
            replayed_us: 0,
            live_us: 0,
            slowest: Vec::new(),
        }
    }

    pub(crate) fn timing(&self) -> Timing {
        Timing {
            logged_ms: self.logged_us / 1000,
            replayed_ms: self.replayed_us / 1000,
            live_ms: self.live_us / 1000,
            slowest: self.slowest.clone(),
        }
    }

    /// Folds one logged input on both sides and notes any difference.
    pub(crate) async fn input(&mut self, seq: u64, at: Instant, input: Input) {
        self.inputs += 1;
        let label = input.label();
        let raw = summarise(&self.reference, &input);
        let matched = self.matcher.translate(at, input, &self.reference);
        let summary = matched
            .input
            .as_ref()
            .map_or(raw, |input| summarise(&self.reference, input));
        if let Some(note) = matched.unmatched {
            self.divergences.push(Divergence {
                seq,
                at,
                kind: DivergenceKind::UnmatchedAnswer,
                input_label: label.to_owned(),
                summary: summary.clone(),
                logged: vec![note],
                replayed: vec!["this replay never asked for it".to_owned()],
            });
        }

        let clock = std::time::Instant::now();
        let mut logged = Vec::new();
        let mut ref_fx = Vec::new();
        if let Some(input) = &matched.input {
            let before = spent_at(&self.reference, at);
            ref_fx = step(&mut self.reference, input.clone(), at);
            let after = spent_at(&self.reference, at);
            observe(
                &self.reference,
                &ref_fx,
                after.0.saturating_sub(before.0),
                after.1.saturating_sub(before.1),
                &mut logged,
                &mut self.logged,
            );
            self.matcher.note(seq, at, &ref_fx, &self.reference);
            self.note_reference(&ref_fx);
        }
        let ref_us = micros(clock.elapsed());

        let clock = std::time::Instant::now();
        let mut replayed = Vec::new();
        self.fire_own_timers(at, &mut replayed).await;
        if let Some(input) = matched.input.and_then(|input| self.translate(input)) {
            let fx = self.candidate_step(input, at, &mut replayed);
            self.settle(fx, Some(&ref_fx), at, &mut replayed).await;
        }
        self.orphan_timers(&ref_fx);
        let rep_us = micros(clock.elapsed());

        self.logged_us += ref_us;
        self.replayed_us += rep_us;
        self.keep_if_slow(SlowInput {
            seq,
            input_label: label.to_owned(),
            logged_us: ref_us,
            replayed_us: rep_us,
        });

        if logged != replayed {
            tracing::debug!(seq, input = label, "divergence");
            self.divergences.push(Divergence {
                seq,
                at,
                kind: DivergenceKind::Content,
                input_label: label.to_owned(),
                summary,
                logged,
                replayed,
            });
        }
        self.unanswered(at);
    }

    /// Requests the reference made in the window that the log gave up on.
    fn unanswered(&mut self, at: Instant) {
        for gap in self.matcher.expire(at) {
            if gap.seq < self.from {
                continue;
            }
            self.divergences.push(Divergence {
                seq: gap.seq,
                at: gap.at,
                kind: DivergenceKind::Unanswered,
                input_label: gap.label.to_owned(),
                summary: gap.what.clone(),
                logged: Vec::new(),
                replayed: vec![format!("{}; the log never answered", gap.what)],
            });
        }
    }

    /// The divergences in seq order.
    pub(crate) fn divergences(&mut self) -> Vec<Divergence> {
        let mut all = std::mem::take(&mut self.divergences);
        all.sort_by_key(|d| d.seq);
        all
    }

    fn keep_if_slow(&mut self, entry: SlowInput) {
        let slower = self
            .slowest
            .last()
            .is_none_or(|s| s.replayed_us < entry.replayed_us);
        if self.slowest.len() < SLOWEST_KEPT || slower {
            self.slowest.push(entry);
            self.slowest
                .sort_by(|a, b| b.replayed_us.cmp(&a.replayed_us).then(a.seq.cmp(&b.seq)));
            self.slowest.truncate(SLOWEST_KEPT);
        }
    }

    fn note_reference(&mut self, fx: &[Effect]) {
        for effect in fx {
            match effect {
                Effect::Send(out) => {
                    self.ref_actions.insert(out.id.clone());
                }
                Effect::SetTimer { id, .. } => {
                    self.ref_timers.insert(id.clone());
                }
                Effect::AskModel(request) => {
                    self.ref_requests.insert(request.id.clone());
                    if let Some(p) = self.reference.pending_requests.get(&request.id) {
                        self.ref_open_requests.insert(
                            request.id.clone(),
                            (p.room.clone(), p.message.clone(), p.answering),
                        );
                    }
                }
                Effect::Outside { action, .. } => {
                    self.ref_outside.insert(action.clone());
                }
                Effect::Classify(r) => {
                    self.ref_open_stages.insert((
                        Stage::Classify,
                        r.room.clone(),
                        r.message.clone(),
                    ));
                }
                Effect::Extract(r) => {
                    self.ref_open_stages.insert((
                        Stage::Extract,
                        r.room.clone(),
                        r.message.clone(),
                    ));
                }
                Effect::ResolveTime(r) => {
                    self.ref_open_stages.insert((
                        Stage::Resolve,
                        r.room.clone(),
                        r.message.clone(),
                    ));
                }
                Effect::CancelTimer { .. } | Effect::Notice(_) | Effect::Redact { .. } => {}
            }
        }
        let pending = &self.reference.pending_requests;
        self.ref_open_requests
            .retain(|id, _| pending.contains_key(id));
    }

    /// The logged input as the replayed run should see it: answers swapped
    /// for the oracle's, ids swapped for the twins', or nothing when the
    /// replayed run never made what it answers.
    fn translate(&mut self, input: Input) -> Option<Input> {
        match input {
            Input::ModelReply { request, reply } => {
                self.ref_open_requests.remove(&request);
                if let Some(twin) = self.requests.remove(&request) {
                    let reply = self.replies.remove(&request).unwrap_or(reply);
                    return Some(Input::ModelReply {
                        request: twin,
                        reply,
                    });
                }
                (!self.ref_requests.contains(&request))
                    .then_some(Input::ModelReply { request, reply })
            }
            Input::Scored {
                room,
                message,
                score,
            } => {
                let key = (Stage::Classify, room.clone(), message.clone());
                let score = match self.stage_answer(key)? {
                    Some(StageAnswer::Score(live)) => live,
                    _ => score,
                };
                Some(Input::Scored {
                    room,
                    message,
                    score,
                })
            }
            Input::Extracted {
                room,
                message,
                extraction,
            } => {
                let key = (Stage::Extract, room.clone(), message.clone());
                let extraction = match self.stage_answer(key)? {
                    Some(StageAnswer::Extraction(live)) => live,
                    _ => extraction,
                };
                Some(Input::Extracted {
                    room,
                    message,
                    extraction,
                })
            }
            Input::TimeResolved(resolution) => {
                let key = (
                    Stage::Resolve,
                    resolution.room.clone(),
                    resolution.message.clone(),
                );
                Some(Input::TimeResolved(match self.stage_answer(key)? {
                    Some(StageAnswer::Time(live)) => live,
                    _ => resolution,
                }))
            }
            Input::TimerFired { id } => {
                if let Some(twin) = self.timers.remove(&id) {
                    return Some(Input::TimerFired { id: twin });
                }
                (!self.ref_timers.contains(&id)).then_some(Input::TimerFired { id })
            }
            Input::Inbound(InboundEvent::ActionResult {
                id,
                ok,
                message,
                error,
            }) => {
                let id = match self.actions.remove(&id) {
                    Some(twin) => twin,
                    None if self.ref_actions.contains(&id) => return None,
                    None => id,
                };
                Some(Input::Inbound(InboundEvent::ActionResult {
                    id,
                    ok,
                    message,
                    error,
                }))
            }
            Input::OutsideResult(result) => {
                let action = match self.outside.remove(&result.action) {
                    Some(twin) => twin,
                    None if self.ref_outside.contains(&result.action) => return None,
                    None => result.action.clone(),
                };
                Some(Input::OutsideResult(OutsideResult { action, ..result }))
            }
            other => Some(other),
        }
    }

    /// What the replayed run gets for a logged stage answer: `None` to drop
    /// it, `Some(None)` to pass the logged one through, or the live answer.
    fn stage_answer(&mut self, key: StageKey) -> Option<Option<StageAnswer>> {
        let asked = self.ref_open_stages.remove(&key);
        match self.stage_answers.remove(&key) {
            Some(answer) => Some(Some(answer)),
            None if asked => None,
            None => Some(None),
        }
    }

    fn candidate_step(
        &mut self,
        input: Input,
        now: Instant,
        lines: &mut Vec<String>,
    ) -> Vec<Effect> {
        let before = spent_at(&self.candidate, now);
        let fx = step(&mut self.candidate, input, now);
        let after = spent_at(&self.candidate, now);
        observe(
            &self.candidate,
            &fx,
            after.0.saturating_sub(before.0),
            after.1.saturating_sub(before.1),
            lines,
            &mut self.replayed,
        );
        fx
    }

    /// Answers the replayed run's effects, then folds whatever those
    /// answers cause, until nothing is left.
    async fn settle(
        &mut self,
        first: Vec<Effect>,
        twin: Option<&[Effect]>,
        now: Instant,
        lines: &mut Vec<String>,
    ) {
        let mut queue = VecDeque::new();
        self.answer(&first, twin, now, &mut queue).await;
        let mut steps = 0;
        while let Some(input) = queue.pop_front() {
            steps += 1;
            if steps > MAX_FOLLOW_UPS {
                tracing::warn!(group = %self.group, "replayed run kept answering itself; stopped");
                break;
            }
            let fx = self.candidate_step(input, now, lines);
            self.answer(&fx, None, now, &mut queue).await;
        }
    }

    async fn answer(
        &mut self,
        fx: &[Effect],
        twin: Option<&[Effect]>,
        now: Instant,
        queue: &mut VecDeque<Input>,
    ) {
        let mut twins = self.twins(twin);
        let live = self.live.clone();
        for effect in fx {
            match effect {
                Effect::Send(out) => match twins.take_send(&send_key(&out.action)) {
                    Some(reference) => {
                        self.actions.insert(reference, out.id.clone());
                    }
                    None => queue.push_back(went_out(out)),
                },
                Effect::SetTimer { id, purpose, .. } => {
                    match twins.take_timer(&self.timer_key(purpose)) {
                        Some(reference) => {
                            self.timers.insert(reference, id.clone());
                        }
                        None => {
                            self.own_timers.insert(id.clone());
                        }
                    }
                }
                Effect::CancelTimer { id } => {
                    self.own_timers.remove(id);
                }
                Effect::AskModel(request) => {
                    let paired = self.pair_request(&request.id);
                    if let Some(live) = &live {
                        let reply = self.ask(live, request.clone()).await;
                        match paired {
                            Some(reference) => {
                                self.replies.insert(reference, reply);
                            }
                            None => queue.push_back(Input::ModelReply {
                                request: request.id.clone(),
                                reply,
                            }),
                        }
                    }
                }
                Effect::Classify(request) => {
                    let key = (
                        Stage::Classify,
                        request.room.clone(),
                        request.message.clone(),
                    );
                    let paired = self.pair_stage(&key, live.is_none());
                    if let Some(live) = &live {
                        let score = self.classify(live, request.clone()).await;
                        if paired {
                            self.stage_answers.insert(key, StageAnswer::Score(score));
                        } else {
                            queue.push_back(Input::Scored {
                                room: request.room.clone(),
                                message: request.message.clone(),
                                score,
                            });
                        }
                    }
                }
                Effect::Extract(request) => {
                    let key = (
                        Stage::Extract,
                        request.room.clone(),
                        request.message.clone(),
                    );
                    let paired = self.pair_stage(&key, live.is_none());
                    if let Some(live) = &live {
                        let extraction = self.extract(live, request.clone()).await;
                        if paired {
                            self.stage_answers
                                .insert(key, StageAnswer::Extraction(extraction));
                        } else {
                            queue.push_back(Input::Extracted {
                                room: request.room.clone(),
                                message: request.message.clone(),
                                extraction,
                            });
                        }
                    }
                }
                Effect::ResolveTime(request) => {
                    let key = (
                        Stage::Resolve,
                        request.room.clone(),
                        request.message.clone(),
                    );
                    let paired = self.pair_stage(&key, live.is_none());
                    if let Some(live) = &live {
                        let resolution = self.resolve(live, request.clone()).await;
                        if paired {
                            self.stage_answers
                                .insert(key, StageAnswer::Time(resolution));
                        } else {
                            queue.push_back(Input::TimeResolved(resolution));
                        }
                    }
                }
                Effect::Outside { action, .. } => match twins.outside.pop_front() {
                    Some(reference) => {
                        self.outside.insert(reference, action.clone());
                    }
                    None => queue.push_back(Input::OutsideResult(OutsideResult {
                        action: action.clone(),
                        ok: true,
                        reference: Some(format!("<replay-{action}>")),
                        error: None,
                        at: now,
                    })),
                },
                Effect::Notice(_) | Effect::Redact { .. } => {}
            }
        }
    }

    fn twins(&self, twin: Option<&[Effect]>) -> Twins {
        let mut twins = Twins::default();
        for effect in twin.unwrap_or_default() {
            match effect {
                Effect::Send(out) => twins.sends.push((send_key(&out.action), out.id.clone())),
                Effect::SetTimer { id, purpose, .. } => {
                    twins.timers.push((self.timer_key(purpose), id.clone()));
                }
                Effect::Outside { action, .. } => twins.outside.push_back(action.clone()),
                _ => {}
            }
        }
        twins
    }

    /// Pairs a replayed model request with a pending reference request about
    /// the same message for the same reason, if there is one.
    fn pair_request(&mut self, id: &RequestId) -> Option<RequestId> {
        let about = self
            .candidate
            .pending_requests
            .get(id)
            .map(|p| (p.room.clone(), p.message.clone(), p.answering))?;
        let reference = self
            .ref_open_requests
            .iter()
            .find(|(_, a)| **a == about)
            .map(|(r, _)| r.clone())?;
        self.ref_open_requests.remove(&reference);
        self.requests.insert(reference.clone(), id.clone());
        Some(reference)
    }

    /// Whether the reference asked the same and still waits on the log's
    /// answer. With the logged oracle that answer is what the replay uses.
    fn pair_stage(&mut self, key: &StageKey, logged: bool) -> bool {
        let paired = self.ref_open_stages.contains(key) && !self.stage_answers.contains_key(key);
        if paired && logged {
            self.stage_answers.insert(key.clone(), StageAnswer::Logged);
        }
        paired
    }

    /// A timer's purpose with engine ids blanked, so twins with different
    /// ids still match.
    fn timer_key(&self, purpose: &TimerPurpose) -> String {
        let mut value = serde_json::to_value(purpose).unwrap_or_default();
        blank_ids(&self.group, &mut value);
        value.to_string()
    }

    /// A reference timer cancelled while its twin stays armed: the twin now
    /// runs on its own clock.
    fn orphan_timers(&mut self, ref_fx: &[Effect]) {
        for effect in ref_fx {
            if let Effect::CancelTimer { id } = effect
                && let Some(twin) = self.timers.remove(id)
                && self.candidate.timers.contains_key(&twin)
            {
                self.own_timers.insert(twin);
            }
        }
    }

    /// Fires the replayed run's own timers that fall due by `at`, in order.
    async fn fire_own_timers(&mut self, at: Instant, lines: &mut Vec<String>) {
        loop {
            let armed = &self.candidate.timers;
            self.own_timers.retain(|id| armed.contains_key(id));
            let due = self
                .own_timers
                .iter()
                .filter_map(|id| armed.get(id).map(|t| (t.at, id.clone())))
                .filter(|(when, _)| *when <= at)
                .min();
            let Some((when, id)) = due else {
                return;
            };
            self.own_timers.remove(&id);
            let fx = self.candidate_step(Input::TimerFired { id }, when, lines);
            self.settle(fx, None, when, lines).await;
        }
    }

    async fn ask(&mut self, live: &Live, request: ModelRequest) -> Result<Plan, String> {
        self.model_calls += 1;
        let clock = std::time::Instant::now();
        let reply = model::ask(
            live.model.as_ref(),
            &self.group,
            request,
            self.strict,
            self.chat,
        )
        .await;
        self.live_us += micros(clock.elapsed());
        reply
    }

    async fn classify(&mut self, live: &Live, request: ClassifyRequest) -> Score {
        self.stage_calls += 1;
        let clock = std::time::Instant::now();
        let rules = request.rules.clone();
        let call = live.classifier.classify(&self.group, request);
        let score = match tokio::time::timeout(CLASSIFY_TIMEOUT, call).await {
            Ok(Ok(score)) => score,
            Ok(Err(error)) => {
                tracing::warn!(%error, "classifier failed; using the rules score");
                rules
            }
            Err(_) => {
                tracing::warn!("classifier timed out; using the rules score");
                rules
            }
        };
        self.live_us += micros(clock.elapsed());
        score
    }

    async fn extract(&mut self, live: &Live, request: ExtractRequest) -> Extraction {
        self.stage_calls += 1;
        let clock = std::time::Instant::now();
        let call = live.extractor.extract(&self.group, request);
        let extraction = match tokio::time::timeout(EXTRACT_TIMEOUT, call).await {
            Ok(Ok(extraction)) => extraction,
            Ok(Err(error)) => {
                tracing::warn!(%error, "extractor failed; nothing remembered");
                Extraction::default()
            }
            Err(_) => {
                tracing::warn!("extractor timed out; nothing remembered");
                Extraction::default()
            }
        };
        self.live_us += micros(clock.elapsed());
        extraction
    }

    async fn resolve(&mut self, live: &Live, request: TimeRequest) -> TimeResolution {
        self.stage_calls += 1;
        let clock = std::time::Instant::now();
        let (room, message, text) = (
            request.room.clone(),
            request.message.clone(),
            request.text.clone(),
        );
        let call = live.resolver.resolve(&self.group, request);
        let resolution = match tokio::time::timeout(RESOLVE_TIMEOUT, call).await {
            Ok(Ok(resolution)) => TimeResolution {
                phrase: Some(text),
                ..resolution
            },
            outcome => {
                if let Ok(Err(error)) = outcome {
                    tracing::warn!(%error, "time resolver failed; no time taken");
                } else {
                    tracing::warn!("time resolver timed out; no time taken");
                }
                TimeResolution {
                    room,
                    message,
                    phrase: Some(text),
                    meaning: TimeMeaning::None,
                    echo: None,
                }
            }
        };
        self.live_us += micros(clock.elapsed());
        resolution
    }
}

/// The connector's answer to a send nobody logged, as if it went out.
fn went_out(out: &Outbound) -> Input {
    let produces = matches!(
        out.action,
        OutboundAction::SendMessage { .. }
            | OutboundAction::SendDm { .. }
            | OutboundAction::CreatePoll { .. }
    );
    Input::Inbound(InboundEvent::ActionResult {
        id: out.id.clone(),
        ok: true,
        message: produces.then(|| PlatformMessageId::new(format!("replay-{}", out.id))),
        error: None,
    })
}

/// What two sends must share to be twins: their kind and where they go.
fn send_key(action: &OutboundAction) -> String {
    match action {
        OutboundAction::SendMessage { room, .. } => format!("say {room}"),
        OutboundAction::EditMessage { room, id, .. } => format!("edit {room} {id}"),
        OutboundAction::DeleteMessage { room, id } => format!("delete {room} {id}"),
        OutboundAction::React { room, message, .. } => format!("react {room} {message}"),
        OutboundAction::Unreact { room, message, .. } => format!("unreact {room} {message}"),
        OutboundAction::SendDm { user, .. } => format!("dm {user}"),
        OutboundAction::Pin { room, message } => format!("pin {room} {message}"),
        OutboundAction::CreatePoll { room, .. } => format!("poll {room}"),
        OutboundAction::EndPoll { room, message } => format!("end {room} {message}"),
        OutboundAction::Typing { room } => format!("typing {room}"),
        OutboundAction::FetchHistory { room, .. } => format!("fetch {room}"),
    }
}

fn blank_ids(group: &GroupId, value: &mut Value) {
    match value {
        Value::String(text) if minted_number(group, text).is_some() => *text = "*".to_owned(),
        Value::Array(items) => items.iter_mut().for_each(|v| blank_ids(group, v)),
        Value::Object(map) => map.values_mut().for_each(|v| blank_ids(group, v)),
        _ => {}
    }
}

fn micros(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}
