//! Ours under test: the real `step` driven through a scenario on a virtual
//! clock, with the stack answering the engine's requests the way the runner
//! does, and everything it did written down as a transcript.

use crate::bench::BenchError;
use crate::bench::canned::CannedModel;
use crate::bench::scenario::{DM, RoleHint, Scenario, ScenarioMember, Target, Turn, is_channel};
use crate::bench::transcript::{Entry, EntryKind, Transcript};
use crate::runtime::model::{self, ChatSettings};
use ours_core::{
    CommitmentId, DecisionId, Effect, FactId, GroupState, Input, Profile, ProposalId, Stage,
    is_addressed, step,
};
use ours_model::{
    Classifier, Extractor, LargeModel, NoExtractor, NoTimeResolver, RulesOnlyClassifier,
    TimeResolver,
};
use ours_types::{
    ActionId, Capabilities, Extraction, GroupId, InboundEvent, Instant, Member, Message,
    OutboundAction, OutsideAction, OutsideResult, Platform, PlatformMessageId, PlatformRole,
    PlatformRoomId, PlatformUserId, Room, RoomKind, TimeMeaning, TimeResolution, TimerId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;

/// The agent's platform id in every scenario room.
pub const AGENT_ID: &str = "ours-bot";
/// How the engine's one-line decline begins, as in "That's one for Priya."
const DECLINE_PREFIX: &str = ours_core::DECLINE_PREFIX;
const MODEL_TIMEOUT: Duration = Duration::from_secs(120);
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(30);
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(60);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(30);
/// Timer firings allowed in one stretch of the clock before the run gives up on it.
const MAX_FIRINGS: usize = 10_000;

/// Everything that answers the engine's requests during a run.
#[derive(Clone)]
pub struct OursStack {
    pub model: Arc<dyn LargeModel>,
    pub classifier: Arc<dyn Classifier>,
    pub extractor: Arc<dyn Extractor>,
    pub resolver: Arc<dyn TimeResolver>,
    pub chat: ChatSettings,
    /// Send the plan schema in OpenAI's strict form.
    pub strict: bool,
}

impl std::fmt::Debug for OursStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OursStack")
            .field("model", &self.model.name())
            .field("classifier", &self.classifier.name())
            .field("extractor", &self.extractor.name())
            .field("resolver", &self.resolver.name())
            .field("strict", &self.strict)
            .finish()
    }
}

impl OursStack {
    /// The rules classifier, no extractor and no time resolver, with `model`
    /// answering addressed messages.
    pub fn rules(model: Arc<dyn LargeModel>) -> OursStack {
        OursStack {
            model,
            classifier: Arc::new(RulesOnlyClassifier),
            extractor: Arc::new(NoExtractor),
            resolver: Arc::new(NoTimeResolver),
            chat: ChatSettings::default(),
            strict: false,
        }
    }

    /// The rules stack with a canned model that answers "ok" to everything,
    /// so the score is what the code decides alone.
    pub fn rules_only() -> OursStack {
        OursStack::rules(Arc::new(CannedModel::ok()))
    }

    /// The stack `ours run` would use for this configuration.
    pub fn from_config(config: &crate::config::Config) -> Result<OursStack, BenchError> {
        let setup = |e: anyhow::Error| BenchError::Setup(e.to_string());
        let (model, dialect, meter) = crate::run::build_model(config).map_err(setup)?;
        let (classifier, extractor, resolver) =
            crate::run::build_small_models(config, &model, dialect, &meter).map_err(setup)?;
        Ok(OursStack {
            model,
            classifier,
            extractor,
            resolver,
            chat: ChatSettings {
                temperature: config.model.chat_temperature,
                max_tokens: config.model.chat_max_tokens,
            },
            strict: dialect == ours_model::Dialect::OpenAi,
        })
    }

    /// The leaderboard's name for this stack.
    pub fn label(&self) -> String {
        if self.classifier.name() == "rules" && self.extractor.name() == "none" {
            if self.model.name() == crate::bench::canned::CANNED {
                "Ours, rules only".to_owned()
            } else {
                format!("Ours, rules triage, {}", self.model.name())
            }
        } else {
            format!("Ours, {}", self.model.name())
        }
    }
}

/// Runs the real engine through `scenario` and writes down what it did.
pub async fn run_ours(scenario: &Scenario, stack: &OursStack) -> Result<Transcript, BenchError> {
    scenario.validate()?;
    let mut driver = Driver::new(scenario, stack)?;
    driver.setup().await;
    for (i, turn) in scenario.turns.iter().enumerate() {
        let at = driver.start.plus_seconds(turn.t);
        driver.advance_to(at).await;
        let span = tracing::info_span!("bench_turn", scenario = %scenario.name, turn = i);
        driver.feed_turn(i, turn).instrument(span).await;
    }
    let end = driver.start.plus_seconds(scenario.end_seconds());
    driver.advance_to(end).await;
    Ok(Transcript {
        scenario: scenario.name.clone(),
        agent: stack.label(),
        entries: driver.entries.into_iter().flatten().collect(),
    })
}

/// What the run has already reported, so each new record is reported once.
#[derive(Default)]
struct Seen {
    facts: BTreeSet<FactId>,
    commitments: BTreeSet<CommitmentId>,
    proposals: BTreeSet<ProposalId>,
    decisions: BTreeSet<DecisionId>,
    deferrals: BTreeSet<PlatformMessageId>,
    actions: BTreeSet<ActionId>,
    ledger: usize,
    settings: String,
}

impl Seen {
    fn of(state: &GroupState) -> Seen {
        Seen {
            facts: state.facts.keys().cloned().collect(),
            commitments: state.commitments.keys().cloned().collect(),
            proposals: state.proposals.keys().cloned().collect(),
            decisions: state.decisions.keys().cloned().collect(),
            deferrals: state.deferrals.keys().cloned().collect(),
            actions: state.actions.iter().map(|a| a.id.clone()).collect(),
            ledger: state.ledger.len(),
            settings: settings_of(state),
        }
    }
}

/// Everything a lead can change by sentence, as one comparable string.
fn settings_of(state: &GroupState) -> String {
    serde_json::to_string(&(
        state.kind,
        &state.profile,
        &state.leads,
        &state.hosts,
        &state.scoped_leads,
        &state.agent_name,
        &state.persona,
        &state.timezone,
        &state.currency,
        &state.decision_rule,
    ))
    .unwrap_or_default()
}

struct Driver<'a> {
    scenario: &'a Scenario,
    stack: &'a OursStack,
    group: GroupId,
    state: GroupState,
    start: Instant,
    now: Instant,
    timers: BTreeMap<TimerId, Instant>,
    /// The turn whose window is open; nothing is written down before turn 0.
    window: Option<usize>,
    /// Whether what happens now answers an addressed turn.
    prompted: bool,
    delivered: u64,
    outside: u64,
    dm_rooms: BTreeSet<PlatformUserId>,
    threads: BTreeSet<PlatformRoomId>,
    own: BTreeSet<PlatformMessageId>,
    latest_own: BTreeMap<PlatformRoomId, PlatformMessageId>,
    turn_messages: Vec<Option<PlatformMessageId>>,
    /// History and every message posted, for history fetches.
    archive: Vec<Message>,
    entries: Vec<Option<Entry>>,
    /// Reactions written down, so one taken back in the same window cancels.
    reactions: Vec<(usize, PlatformMessageId, String, usize)>,
    seen: Seen,
}

impl<'a> Driver<'a> {
    fn new(scenario: &'a Scenario, stack: &'a OursStack) -> Result<Driver<'a>, BenchError> {
        let start = scenario.start_instant()?;
        let group = GroupId::new(scenario.name.clone());
        let mut state = GroupState::new(group.clone(), scenario.kind);
        state.setup.stage = Stage::Done;
        let archive = scenario
            .history
            .iter()
            .enumerate()
            .map(|(k, line)| Message {
                id: PlatformMessageId::new(format!("h{k}")),
                room: PlatformRoomId::new(line.room.clone()),
                author: PlatformUserId::new(line.from.clone()),
                text: line.text.clone(),
                at: start.plus_seconds(-line.ago),
                reply_to: None,
                mentions: Vec::new(),
                mentions_me: false,
                attachments: Vec::new(),
                edited: false,
            })
            .collect();
        Ok(Driver {
            scenario,
            stack,
            group,
            state,
            start,
            now: start,
            timers: BTreeMap::new(),
            window: None,
            prompted: false,
            delivered: 0,
            outside: 0,
            dm_rooms: BTreeSet::new(),
            threads: BTreeSet::new(),
            own: BTreeSet::new(),
            latest_own: BTreeMap::new(),
            turn_messages: vec![None; scenario.turns.len()],
            archive,
            entries: Vec::new(),
            reactions: Vec::new(),
            seen: Seen::default(),
        })
    }

    /// Connects, sets the profile with no observe period, and syncs the
    /// channels and the leads.
    async fn setup(&mut self) {
        let s = self.scenario;
        self.feed(Input::Inbound(InboundEvent::Ready {
            platform: Platform::Sim,
            me: PlatformUserId::new(AGENT_ID),
            me_name: s.agent_name.clone(),
            capabilities: Capabilities {
                reactions: true,
                dms: true,
                history: true,
                ..Capabilities::default()
            },
        }))
        .await;
        let profile = Profile {
            observe_seconds: 0,
            ..Profile::for_kind(s.kind)
        };
        self.command(ours_core::Command::SetProfile { profile })
            .await;
        if self.state.agent_name != s.agent_name || !self.state.name_from_host {
            let name = s.agent_name.clone();
            self.command(ours_core::Command::SetAgentName {
                name,
                by_host: true,
            })
            .await;
        }
        if let Some(zone) = &s.timezone {
            let zone = zone.clone();
            self.command(ours_core::Command::SetTimezone { zone }).await;
        }
        for channel in s.channels() {
            let members = s
                .members
                .iter()
                .filter(|m| s.in_channel(m, &channel))
                .map(member)
                .collect();
            self.sync(&channel, RoomKind::Channel, None, members).await;
        }
        for lead in s.members.iter().filter(|m| m.role == RoleHint::Lead) {
            let user = PlatformUserId::new(lead.id.clone());
            self.command(ours_core::Command::AddLead { user }).await;
        }
        self.seen = Seen::of(&self.state);
    }

    async fn command(&mut self, command: ours_core::Command) {
        self.feed(Input::Command(command)).await;
    }

    async fn sync(
        &mut self,
        id: &str,
        kind: RoomKind,
        parent: Option<&str>,
        mut members: Vec<Member>,
    ) {
        members.push(Member {
            is_bot: true,
            ..member_of(AGENT_ID, "Ours", PlatformRole::Regular)
        });
        let room = Room {
            platform: Platform::Sim,
            id: PlatformRoomId::new(id),
            name: Some(id.to_owned()),
            kind,
            parent: parent.map(PlatformRoomId::new),
            container: None,
        };
        self.feed(Input::Inbound(InboundEvent::RoomSync { room, members }))
            .await;
    }

    fn dm_sync(&mut self, user: &PlatformUserId) -> Input {
        self.dm_rooms.insert(user.clone());
        let name = self
            .scenario
            .member(user.as_str())
            .map_or_else(|| user.to_string(), |m| m.name.clone());
        Input::Inbound(InboundEvent::RoomSync {
            room: Room {
                platform: Platform::Sim,
                id: dm_room(user.as_str()),
                name: None,
                kind: RoomKind::Dm,
                parent: None,
                container: None,
            },
            members: vec![member_of(user.as_str(), &name, PlatformRole::Regular)],
        })
    }

    /// The room a turn happens in, synced first when it is a DM or a thread.
    async fn prepare_room(&mut self, turn: &Turn) -> PlatformRoomId {
        if turn.room == DM {
            let user = PlatformUserId::new(turn.from.clone());
            if !self.dm_rooms.contains(&user) && self.state.dm_room_of(&user).is_none() {
                let sync = self.dm_sync(&user);
                self.feed(sync).await;
            }
            return dm_room(&turn.from);
        }
        let id = PlatformRoomId::new(turn.room.clone());
        if !is_channel(&turn.room) && !self.threads.contains(&id) {
            self.threads.insert(id.clone());
            let s = self.scenario;
            let members = s
                .members
                .iter()
                .filter(|m| s.in_channel(m, crate::bench::scenario::GENERAL))
                .map(member)
                .collect();
            self.sync(
                &turn.room,
                RoomKind::Thread,
                Some(crate::bench::scenario::GENERAL),
                members,
            )
            .await;
        }
        id
    }

    async fn feed_turn(&mut self, i: usize, turn: &Turn) {
        self.window = Some(i);
        let room = self.prepare_room(turn).await;
        let user = PlatformUserId::new(turn.from.clone());
        if let Some(text) = &turn.text {
            let message = Message {
                id: PlatformMessageId::new(format!("m{i}")),
                room,
                author: user,
                text: text.clone(),
                at: self.now,
                reply_to: None,
                mentions: Vec::new(),
                mentions_me: false,
                attachments: Vec::new(),
                edited: false,
            };
            self.turn_messages[i] = Some(message.id.clone());
            self.archive.push(message.clone());
            self.prompted = is_addressed(&self.state, &message);
            self.feed(Input::Inbound(InboundEvent::Message(message)))
                .await;
        } else if let Some(reaction) = &turn.reaction {
            let target = match &reaction.to {
                Target::Turn(to) => self.turn_messages.get(*to).cloned().flatten(),
                Target::Named(_) => self.latest_own.get(&room).cloned(),
            };
            let Some(target) = target else {
                tracing::warn!(turn = i, "nothing to react to; the reaction is skipped");
                return;
            };
            self.prompted = self.own.contains(&target);
            self.feed(Input::Inbound(InboundEvent::ReactionAdded {
                room,
                message: target,
                user,
                emoji: reaction.emoji.clone(),
                at: self.now,
            }))
            .await;
        }
        self.prompted = false;
    }

    /// Moves the clock to `until`, firing each timer at its own instant.
    async fn advance_to(&mut self, until: Instant) {
        self.prompted = false;
        for _ in 0..MAX_FIRINGS {
            let due = self
                .timers
                .iter()
                .filter(|(_, at)| **at <= until)
                .min_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)))
                .map(|(id, at)| (id.clone(), *at));
            let Some((id, at)) = due else {
                self.now = self.now.max(until);
                return;
            };
            self.timers.remove(&id);
            self.now = self.now.max(at);
            self.feed(Input::TimerFired { id }).await;
        }
        tracing::warn!(scenario = %self.scenario.name, "timers kept firing; the clock jumps ahead");
        self.now = self.now.max(until);
    }

    /// Steps `input` and everything it causes until nothing is left to feed.
    async fn feed(&mut self, input: Input) {
        let mut queue = VecDeque::from([input]);
        while let Some(input) = queue.pop_front() {
            let effects = step(&mut self.state, input, self.now);
            for effect in effects {
                self.carry_out(effect, &mut queue).await;
            }
            self.notice_records();
        }
    }

    async fn carry_out(&mut self, effect: Effect, queue: &mut VecDeque<Input>) {
        match effect {
            Effect::Send(outbound) => {
                if let OutboundAction::SendDm { user, .. } = &outbound.action
                    && !self.dm_rooms.contains(user)
                    && self.state.dm_room_of(user).is_none()
                {
                    let sync = self.dm_sync(user);
                    queue.push_back(sync);
                }
                let produces = matches!(
                    outbound.action,
                    OutboundAction::SendMessage { .. }
                        | OutboundAction::SendDm { .. }
                        | OutboundAction::CreatePoll { .. }
                );
                let message = produces.then(|| {
                    self.delivered += 1;
                    PlatformMessageId::new(format!("a{}", self.delivered))
                });
                if let Some(id) = &message {
                    self.own.insert(id.clone());
                    if let Some(room) = room_of(&outbound.action) {
                        self.latest_own.insert(room, id.clone());
                    }
                }
                self.transcribe(&outbound.action);
                queue.push_back(Input::Inbound(InboundEvent::ActionResult {
                    id: outbound.id.clone(),
                    ok: true,
                    message,
                    error: None,
                }));
                if let OutboundAction::FetchHistory {
                    room,
                    limit,
                    before,
                } = &outbound.action
                {
                    let messages = self.page(room, *limit, before.as_ref());
                    queue.push_back(Input::Inbound(InboundEvent::History {
                        room: room.clone(),
                        messages,
                    }));
                }
            }
            Effect::SetTimer { id, at, .. } => {
                self.timers.insert(id, at);
            }
            Effect::CancelTimer { id } => {
                self.timers.remove(&id);
            }
            Effect::AskModel(request) => {
                let id = request.id.clone();
                let ask = model::ask(
                    self.stack.model.as_ref(),
                    &self.group,
                    request,
                    self.stack.strict,
                    self.stack.chat,
                );
                let reply = tokio::time::timeout(MODEL_TIMEOUT, ask)
                    .await
                    .unwrap_or_else(|_| Err("the model did not answer in time".to_owned()));
                queue.push_back(Input::ModelReply { request: id, reply });
            }
            Effect::Classify(request) => {
                let (room, message, rules) = (
                    request.room.clone(),
                    request.message.clone(),
                    request.rules.clone(),
                );
                let classify = self.stack.classifier.classify(&self.group, request);
                let score = match tokio::time::timeout(CLASSIFY_TIMEOUT, classify).await {
                    Ok(Ok(score)) => score,
                    Ok(Err(error)) => {
                        tracing::warn!(error = ?error, "classifier failed; using the rules score");
                        rules
                    }
                    Err(_) => {
                        tracing::warn!("classifier timed out; using the rules score");
                        rules
                    }
                };
                tracing::debug!(?score, "scored");
                queue.push_back(Input::Scored {
                    room,
                    message,
                    score,
                });
            }
            Effect::Extract(request) => {
                let (room, message) = (request.room.clone(), request.message.clone());
                let extract = self.stack.extractor.extract(&self.group, request);
                let extraction = match tokio::time::timeout(EXTRACT_TIMEOUT, extract).await {
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
                tracing::debug!(?extraction, "extracted");
                queue.push_back(Input::Extracted {
                    room,
                    message,
                    extraction,
                });
            }
            Effect::ResolveTime(request) => {
                let (room, message, text) = (
                    request.room.clone(),
                    request.message.clone(),
                    request.text.clone(),
                );
                let resolve = self.stack.resolver.resolve(&self.group, request);
                let resolution = match tokio::time::timeout(RESOLVE_TIMEOUT, resolve).await {
                    Ok(Ok(resolution)) => TimeResolution {
                        phrase: Some(text),
                        ..resolution
                    },
                    Ok(Err(_)) | Err(_) => TimeResolution {
                        room,
                        message,
                        phrase: Some(text),
                        meaning: TimeMeaning::None,
                        echo: None,
                    },
                };
                queue.push_back(Input::TimeResolved(resolution));
            }
            Effect::Outside { action, outside } => {
                self.outside += 1;
                if let OutsideAction::SendMail(mail) = &outside {
                    self.record(Entry {
                        text: Some(format!("{}: {}", mail.to, mail.subject)),
                        ..Entry::action(0, "send_mail")
                    });
                }
                queue.push_back(Input::OutsideResult(OutsideResult {
                    action,
                    ok: true,
                    reference: Some(format!("<bench{}@ours>", self.outside)),
                    error: None,
                    at: self.now,
                }));
            }
            Effect::Notice(line) => tracing::debug!(%line, "notice"),
            Effect::Redact { .. } => {}
        }
    }

    /// Writes down what members would see.
    fn transcribe(&mut self, action: &OutboundAction) {
        let unprompted = !self.prompted;
        match action {
            OutboundAction::SendMessage { room, text, .. } => {
                let entry = match self.dm_partner(room) {
                    Some(user) => Entry::dm(0, user, text.clone()),
                    None => Entry {
                        room: Some(room.to_string()),
                        ..Entry::message(0, text.clone())
                    },
                };
                self.record(entry.unprompted(unprompted));
                if text.starts_with(DECLINE_PREFIX) {
                    self.record(Entry {
                        text: Some(text.clone()),
                        ..Entry::action(0, "decline")
                    });
                }
            }
            OutboundAction::SendDm { user, text } => {
                self.record(Entry::dm(0, user.as_str(), text.clone()).unprompted(unprompted));
            }
            OutboundAction::CreatePoll {
                room,
                question,
                options,
                ..
            } => {
                let text = std::iter::once(question.clone())
                    .chain(options.iter().cloned())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.record(
                    Entry {
                        room: Some(room.to_string()),
                        ..Entry::message(0, text)
                    }
                    .unprompted(unprompted),
                );
            }
            OutboundAction::React {
                room,
                message,
                emoji,
            } if !self.own.contains(message) => {
                let entry = Entry {
                    room: Some(self.label(room)),
                    ..Entry::reaction(0, emoji.clone())
                };
                if let (Some(index), Some(window)) =
                    (self.record(entry.unprompted(unprompted)), self.window)
                {
                    self.reactions
                        .push((window, message.clone(), emoji.clone(), index));
                }
            }
            OutboundAction::Unreact { message, emoji, .. } => {
                let window = self.window;
                if let Some(pos) = self
                    .reactions
                    .iter()
                    .position(|(w, m, e, _)| Some(*w) == window && m == message && e == emoji)
                {
                    let (_, _, _, index) = self.reactions.remove(pos);
                    if let Some(slot) = self.entries.get_mut(index) {
                        *slot = None;
                    }
                }
            }
            _ => {}
        }
    }

    /// Reports records the engine made since the last step: facts,
    /// commitments, proposals, decisions, deferrals, tab lines, mail, undo
    /// and changed settings.
    fn notice_records(&mut self) {
        let mut found: Vec<(&'static str, String)> = Vec::new();
        let s = &self.state;
        for (id, fact) in &s.facts {
            if self.seen.facts.insert(id.clone()) && fact.retracted.is_none() {
                found.push(("store_fact", fact.line()));
            }
        }
        for (id, c) in &s.commitments {
            if self.seen.commitments.insert(id.clone()) {
                found.push(("store_commitment", c.what.clone()));
            }
        }
        for (id, p) in &s.proposals {
            if self.seen.proposals.insert(id.clone()) {
                found.push(("open_poll", p.title()));
            }
        }
        for (id, d) in &s.decisions {
            if self.seen.decisions.insert(id.clone()) {
                let winner = d.options.get(d.winner).map(|o| o.label.clone());
                found.push(("record_decision", winner.unwrap_or_default()));
            }
        }
        for (id, d) in &s.deferrals {
            if self.seen.deferrals.insert(id.clone()) {
                found.push(("defer_to_lead", d.request_text.clone()));
            }
        }
        for record in &s.actions {
            if self.seen.actions.insert(record.id.clone()) {
                // A mail that went at once is reported as send_mail when it goes,
                // and a refused one was never held.
                match record.class {
                    ours_core::ActionClass::Mail
                        if matches!(record.status, ours_core::ActionStatus::Held { .. }) =>
                    {
                        found.push(("hold_mail", record.summary.clone()))
                    }
                    ours_core::ActionClass::Undo => found.push(("undo", record.summary.clone())),
                    _ => {}
                }
            }
        }
        for line in s.ledger.iter().skip(self.seen.ledger) {
            found.push(("ledger_line", line.what.clone()));
        }
        self.seen.ledger = s.ledger.len();
        let settings = settings_of(s);
        if settings != self.seen.settings {
            self.seen.settings = settings;
            found.push(("change_setting", String::new()));
        }
        for (action, text) in found {
            self.record(Entry {
                text: (!text.is_empty()).then_some(text),
                ..Entry::action(0, action)
            });
        }
    }

    /// Adds an entry to the open window and returns its index.
    fn record(&mut self, mut entry: Entry) -> Option<usize> {
        let turn = self.window?;
        entry.turn = turn;
        entry.t = Some(self.now.seconds_since(self.start));
        if entry.kind == EntryKind::Action {
            entry.unprompted = None;
        }
        self.entries.push(Some(entry));
        Some(self.entries.len() - 1)
    }

    /// The member a DM room is with.
    fn dm_partner(&self, room: &PlatformRoomId) -> Option<String> {
        self.state
            .rooms
            .get(room)
            .filter(|r| r.is_dm())
            .and_then(|r| r.members.keys().find(|u| u.as_str() != AGENT_ID))
            .map(|u| u.to_string())
    }

    fn label(&self, room: &PlatformRoomId) -> String {
        if self.dm_partner(room).is_some() {
            DM.to_owned()
        } else {
            room.to_string()
        }
    }

    /// Up to `limit` messages in `room` before `before`, oldest first.
    fn page(
        &self,
        room: &PlatformRoomId,
        limit: u32,
        before: Option<&PlatformMessageId>,
    ) -> Vec<Message> {
        let cutoff = before
            .and_then(|b| self.archive.iter().find(|m| &m.id == b))
            .map(|m| m.at);
        let mut older: Vec<Message> = self
            .archive
            .iter()
            .filter(|m| &m.room == room && cutoff.is_none_or(|c| m.at < c))
            .cloned()
            .collect();
        older.sort_by_key(|m| m.at);
        let keep = usize::try_from(limit).unwrap_or(usize::MAX);
        older.split_off(older.len().saturating_sub(keep))
    }
}

fn dm_room(user: &str) -> PlatformRoomId {
    PlatformRoomId::new(format!("dm-{user}"))
}

/// The room an action lands in, with DMs in the member's DM room.
fn room_of(action: &OutboundAction) -> Option<PlatformRoomId> {
    match action {
        OutboundAction::SendMessage { room, .. } | OutboundAction::CreatePoll { room, .. } => {
            Some(room.clone())
        }
        OutboundAction::SendDm { user, .. } => Some(dm_room(user.as_str())),
        _ => None,
    }
}

fn member(m: &ScenarioMember) -> Member {
    let role = match m.role {
        RoleHint::Lead => PlatformRole::Admin,
        RoleHint::Member => PlatformRole::Regular,
        RoleHint::Guest => PlatformRole::External,
    };
    member_of(&m.id, &m.name, role)
}

fn member_of(id: &str, name: &str, role: PlatformRole) -> Member {
    Member {
        id: PlatformUserId::new(id),
        display_name: name.to_owned(),
        platform_role: role,
        is_bot: false,
        timezone: None,
    }
}
