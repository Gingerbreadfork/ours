//! One actor per group. It owns the group's state, appends every input to
//! the journal before stepping, carries out the effects, and feeds what
//! comes back in as new inputs. Steps for a group never overlap.

use crate::runtime::admin::AdminHub;
use crate::runtime::journal::{Journal, JournalError, SNAPSHOT_EVERY};
use crate::runtime::model;
use crate::runtime::router::Router;
use crate::runtime::timers::{Fired, TimerWheel};
use ours_admin::{AdminEvent, GroupSummary};
use ours_core::{Command, Effect, GroupState, Input, render_charter, render_decisions, step};
use ours_mail::Mailer;
use ours_model::{Classifier, Extractor, LargeModel, TimeResolver};
use ours_types::{
    GroupId, InboundEvent, Instant, Kind, Outbound, OutboundAction, Platform, PlatformMessageId,
    PlatformRoomId, PlatformUserId,
};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How often the page's copy of the state is refreshed during a burst.
const STATE_PUBLISH_EVERY: Duration = Duration::from_secs(1);
use thiserror::Error;
use tokio::sync::{Semaphore, mpsc, oneshot, watch};

#[derive(Debug, Error)]
pub enum GroupError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("journal task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Core(#[from] ours_core::CoreError),
}

/// A wipe request from the admin page; answered once the group is fresh.
pub type WipeRequest = oneshot::Sender<Result<(), String>>;

/// How a run ended: the process is shutting down, or the host asked for
/// the group to be wiped and the queues come back for the fresh runner.
pub enum RunEnd {
    Shutdown,
    Wiped {
        inputs: mpsc::Receiver<Input>,
        wipes: mpsc::Receiver<WipeRequest>,
        done: WipeRequest,
    },
}

impl std::fmt::Debug for RunEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RunEnd::Shutdown => "Shutdown",
            RunEnd::Wiped { .. } => "Wiped",
        })
    }
}

/// What the configuration says the group should be. Applied as logged
/// commands whenever the rebuilt state disagrees.
#[derive(Debug, Clone)]
pub struct Desired {
    pub name: String,
    pub kind: Kind,
    /// Enforced only when the host set it; otherwise the group picks a name.
    pub agent_name: Option<String>,
    pub timezone: String,
    pub leads: Vec<String>,
    pub hosts: Vec<String>,
    pub rooms: Vec<(Platform, PlatformRoomId)>,
    pub model_label: String,
    pub dial: Option<u8>,
    pub voice: Option<String>,
    pub currency: String,
    pub traits: Vec<String>,
    pub bits: Vec<String>,
    pub never_jokes_about: Vec<String>,
    /// A starter persona by name; applied while the group still has the plain voice.
    pub persona: Option<String>,
    /// Legal hold at startup; a lead may lift it by sentence.
    pub legal_hold: bool,
    /// The feed base, `<url>/ics/<group>/<secret>`, or None when feeds are off.
    pub feed_base: Option<String>,
}

pub struct GroupRunner {
    id: GroupId,
    desired: Desired,
    state: GroupState,
    seq: u64,
    since_snapshot: u64,
    journal: Journal<Input, GroupState>,
    wheel: TimerWheel,
    fired: mpsc::Receiver<Fired>,
    inputs: mpsc::Receiver<Input>,
    wipes: mpsc::Receiver<WipeRequest>,
    self_tx: mpsc::Sender<Input>,
    outbound: HashMap<Platform, mpsc::Sender<Outbound>>,
    model: Arc<dyn LargeModel>,
    classifier: Arc<dyn Classifier>,
    extractor: Arc<dyn Extractor>,
    resolver: Arc<dyn TimeResolver>,
    mailer: Option<Arc<dyn Mailer>>,
    strict_schema: bool,
    chat: model::ChatSettings,
    hub: AdminHub,
    router: Arc<Router>,
    charter_path: PathBuf,
    last_charter: String,
    decisions_path: PathBuf,
    last_decisions: String,
    fresh: bool,
    /// Bounds concurrent extractions; a backfill can ask for hundreds at once.
    extract_slots: Arc<Semaphore>,
    /// What the calendars were last rendered from, so a message that
    /// changes nothing dated does not re-render a feed per member.
    calendar_mark: u64,
    last_state_publish: Option<tokio::time::Instant>,
}

impl std::fmt::Debug for GroupRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupRunner")
            .field("id", &self.id)
            .field("seq", &self.seq)
            .finish()
    }
}

/// Everything a runner needs from the outside, gathered by the run loop.
pub struct Wiring {
    pub journal: Journal<Input, GroupState>,
    pub inputs: mpsc::Receiver<Input>,
    /// Wipe requests from the admin page.
    pub wipes: mpsc::Receiver<WipeRequest>,
    pub self_tx: mpsc::Sender<Input>,
    pub outbound: HashMap<Platform, mpsc::Sender<Outbound>>,
    pub model: Arc<dyn LargeModel>,
    pub classifier: Arc<dyn Classifier>,
    pub extractor: Arc<dyn Extractor>,
    pub resolver: Arc<dyn TimeResolver>,
    /// None when the host has no [mail] section; outside actions then fail honestly.
    pub mailer: Option<Arc<dyn Mailer>>,
    pub strict_schema: bool,
    /// Temperature and reply budget for chat, as the host set them.
    pub chat: model::ChatSettings,
    pub hub: AdminHub,
    pub router: Arc<Router>,
    /// Where the group's charter.md is written whenever it changes.
    pub charter_path: PathBuf,
    /// Where the group's decisions.md is written whenever it changes.
    pub decisions_path: PathBuf,
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring")
            .field("strict_schema", &self.strict_schema)
            .finish()
    }
}

impl GroupRunner {
    /// Rebuilds the state from the snapshot and the log, then applies the
    /// configuration. Runs the replay on a blocking thread.
    pub async fn recover(
        id: GroupId,
        desired: Desired,
        wiring: Wiring,
    ) -> Result<Self, GroupError> {
        let journal = wiring.journal.clone();
        let (state, seq) = {
            let id = id.clone();
            let kind = desired.kind;
            tokio::task::spawn_blocking(move || -> Result<(GroupState, u64), JournalError> {
                let recovered = journal.recover()?;
                let mut state = recovered.state.unwrap_or_else(|| GroupState::new(id, kind));
                let seq = journal.replay(recovered.seq, |_, at, input| {
                    let _ = step(&mut state, input, at);
                    Ok(())
                })?;
                Ok((state, seq))
            })
            .await??
        };
        tracing::info!(group = %id, seq, rooms = state.rooms.len(), "state rebuilt");

        let (fired_tx, fired) = mpsc::channel(64);
        let wheel = TimerWheel::new();
        for (timer_id, timer) in &state.timers {
            wheel.set(timer_id.clone(), timer.at);
        }
        tokio::spawn(wheel.clone().run(fired_tx));

        let mut runner = GroupRunner {
            id,
            desired,
            state,
            seq,
            since_snapshot: 0,
            journal: wiring.journal,
            wheel,
            fired,
            inputs: wiring.inputs,
            wipes: wiring.wipes,
            self_tx: wiring.self_tx,
            outbound: wiring.outbound,
            model: wiring.model,
            classifier: wiring.classifier,
            extractor: wiring.extractor,
            resolver: wiring.resolver,
            mailer: wiring.mailer,
            strict_schema: wiring.strict_schema,
            chat: wiring.chat,
            hub: wiring.hub,
            router: wiring.router,
            charter_path: wiring.charter_path,
            last_charter: String::new(),
            decisions_path: wiring.decisions_path,
            last_decisions: String::new(),
            fresh: seq == 0,
            extract_slots: Arc::new(Semaphore::new(EXTRACT_CONCURRENCY)),
            calendar_mark: 0,
            last_state_publish: None,
        };
        for command in runner.reconcile() {
            runner.handle(Input::Command(command)).await?;
        }
        runner.publish_summary();
        runner.publish_charter();
        runner.publish_decisions();
        Ok(runner)
    }

    /// The commands that bring the state in line with the configuration.
    /// Kind, dial and leads are initial values only, so what a lead changes
    /// by sentence survives a restart; voice has no sentence, so the config
    /// wins; the rest is enforced.
    fn reconcile(&self) -> Vec<Command> {
        let mut commands = Vec::new();
        if self.fresh && self.state.kind != self.desired.kind {
            commands.push(Command::SetKind {
                kind: self.desired.kind,
            });
        }
        if let Some(name) = &self.desired.agent_name
            && (&self.state.agent_name != name || !self.state.name_from_host)
        {
            commands.push(Command::SetAgentName {
                name: name.clone(),
                by_host: true,
            });
        }
        if self.state.timezone != self.desired.timezone {
            commands.push(Command::SetTimezone {
                zone: self.desired.timezone.clone(),
            });
        }
        if self.state.currency != self.desired.currency {
            commands.push(Command::SetCurrency {
                code: self.desired.currency.clone(),
            });
        }
        let feed_base = self.desired.feed_base.clone();
        if self.state.feed_base != feed_base {
            commands.push(Command::SetFeedBase {
                url: feed_base.unwrap_or_default(),
            });
        }
        if self.state.model_label != self.desired.model_label {
            commands.push(Command::SetModelLabel {
                label: self.desired.model_label.clone(),
            });
        }
        // The config dial applies until someone changes it by sentence, which
        // shows as the dial no longer being the kind's default.
        if let Some(dial) = self.desired.dial
            && (self.fresh
                || self.state.profile.dial == ours_core::Profile::for_kind(self.state.kind).dial)
            && self.state.profile.dial != dial
        {
            let mut profile = self.state.profile.clone();
            profile.dial = dial.min(3);
            commands.push(Command::SetProfile { profile });
        }
        // A named starter owns the voice; otherwise the config's own lines do.
        if let Some(name) = &self.desired.persona {
            if (self.fresh || self.state.persona_name.is_none())
                && self.state.persona_name.as_deref() != Some(name.as_str())
            {
                commands.push(Command::UsePersona { name: name.clone() });
            }
        } else {
            let mut persona = self.state.persona.clone();
            if let Some(voice) = &self.desired.voice {
                persona.voice = voice.clone();
            }
            persona.traits = self.desired.traits.clone();
            persona.never_jokes_about = self.desired.never_jokes_about.clone();
            if self.fresh || persona.bits.is_empty() {
                persona.bits = self.desired.bits.clone();
            }
            if persona != self.state.persona {
                commands.push(Command::SetPersona { persona });
            }
        }
        if self.desired.legal_hold && !self.state.legal_hold {
            commands.push(Command::SetLegalHold { on: true });
        }
        let leads: BTreeSet<_> = self
            .desired
            .leads
            .iter()
            .map(|s| s.as_str().into())
            .collect();
        if self.fresh && !leads.is_empty() && self.state.leads != leads {
            commands.push(Command::SetLeads {
                users: leads.into_iter().collect(),
            });
        }
        let hosts: BTreeSet<_> = self
            .desired
            .hosts
            .iter()
            .map(|s| s.as_str().into())
            .collect();
        if self.state.hosts != hosts {
            commands.push(Command::SetHosts {
                users: hosts.into_iter().collect(),
            });
        }
        commands
    }

    /// Runs until the shutdown flag turns true, then snapshots.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> Result<RunEnd, GroupError> {
        loop {
            tokio::select! {
                Some(input) = self.inputs.recv() => self.handle(input).await?,
                Some(fired) = self.fired.recv() => self.handle(Input::TimerFired { id: fired.id }).await?,
                Some(done) = self.wipes.recv() => {
                    tracing::warn!(group = %self.id, seq = self.seq, "wipe requested; stopping the runner");
                    return Ok(RunEnd::Wiped { inputs: self.inputs, wipes: self.wipes, done });
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        self.publish_state();
        self.snapshot().await?;
        tracing::info!(group = %self.id, seq = self.seq, "stopped");
        Ok(RunEnd::Shutdown)
    }

    async fn handle(&mut self, input: Input) -> Result<(), GroupError> {
        // Millisecond time, exactly what the journal stores, so a replay
        // folds the same instants the live run did.
        let now = Instant::from_millis(jiff::Timestamp::now().as_millisecond())
            .unwrap_or_else(|| Instant::from(jiff::Timestamp::now()));
        let label = input.label();
        let seq = {
            let journal = self.journal.clone();
            let logged = input.clone();
            tokio::task::spawn_blocking(move || journal.append(&logged, now)).await??
        };
        self.seq = seq;
        self.since_snapshot += 1;
        let line = self.describe(&input);
        let detail = detail_of(&input);
        let effects = step(&mut self.state, input, now);
        self.hub.publish(AdminEvent {
            group: self.id.clone(),
            at: now,
            kind: label.to_owned(),
            line,
            detail,
        });
        for effect in effects {
            self.perform(effect, now).await;
        }
        self.publish_charter();
        self.publish_decisions();
        self.publish_calendars(now);
        self.publish_actions();
        // The page's copy of the state: once a second in a burst, and always
        // when the queue is drained so the last change is never left behind.
        let due = self
            .last_state_publish
            .is_none_or(|t| t.elapsed() >= STATE_PUBLISH_EVERY);
        if due || self.inputs.is_empty() {
            self.publish_state();
        }
        self.publish_summary();
        if self.since_snapshot >= SNAPSHOT_EVERY {
            self.snapshot().await?;
        }
        Ok(())
    }

    fn publish_state(&mut self) {
        if let Ok(value) = serde_json::to_value(&self.state) {
            self.hub.set_state(&self.id, value);
        }
        self.last_state_publish = Some(tokio::time::Instant::now());
    }

    async fn perform(&mut self, effect: Effect, now: Instant) {
        match effect {
            Effect::Send(outbound) => {
                let text = describe_action(&outbound.action);
                let Some(platform) = self.platform_for(&outbound.action) else {
                    tracing::warn!(group = %self.id, action = %outbound.id, "no platform for action; dropped");
                    return;
                };
                let Some(queue) = self.outbound.get(&platform) else {
                    tracing::warn!(group = %self.id, ?platform, "no connector for platform; dropped");
                    return;
                };
                self.router
                    .expect_result(outbound.id.clone(), self.id.clone());
                if queue.send(outbound).await.is_err() {
                    tracing::warn!(group = %self.id, ?platform, "connector queue is closed");
                    return;
                }
                self.hub.publish(AdminEvent {
                    group: self.id.clone(),
                    at: now,
                    kind: "send".to_owned(),
                    line: text,
                    detail: None,
                });
            }
            Effect::SetTimer { id, at, .. } => self.wheel.set(id, at),
            Effect::CancelTimer { id } => self.wheel.cancel(&id),
            Effect::Classify(request) => {
                let classifier = Arc::clone(&self.classifier);
                let group = self.id.clone();
                let tx = self.self_tx.clone();
                tokio::spawn(async move {
                    let (room, message, rules) = (
                        request.room.clone(),
                        request.message.clone(),
                        request.rules.clone(),
                    );
                    let score = match tokio::time::timeout(
                        CLASSIFY_TIMEOUT,
                        classifier.classify(&group, request),
                    )
                    .await
                    {
                        Ok(Ok(score)) => score,
                        Ok(Err(error)) => {
                            tracing::warn!(%group, %error, "classifier failed; using the rules score");
                            rules
                        }
                        Err(_) => {
                            tracing::warn!(%group, "classifier timed out; using the rules score");
                            rules
                        }
                    };
                    if tx
                        .send(Input::Scored {
                            room,
                            message,
                            score,
                        })
                        .await
                        .is_err()
                    {
                        tracing::warn!(%group, "group queue closed before the score arrived");
                    }
                });
            }
            Effect::Outside { action, outside } => {
                let summary = match &outside {
                    ours_types::OutsideAction::SendMail(m) => {
                        format!("mail to {}: {}", m.to, shorten(&m.subject, 80))
                    }
                    ours_types::OutsideAction::CancelMail { mail, .. } => {
                        format!("cancellation to {}", mail.to)
                    }
                };
                self.hub.publish(AdminEvent {
                    group: self.id.clone(),
                    at: now,
                    kind: "outside".to_owned(),
                    line: format!("sending {summary}"),
                    detail: Some(serde_json::json!({ "outside": outside })),
                });
                let mailer = self.mailer.clone();
                let group = self.id.clone();
                let tx = self.self_tx.clone();
                tokio::spawn(async move {
                    let mail = match &outside {
                        ours_types::OutsideAction::SendMail(m) => m.clone(),
                        ours_types::OutsideAction::CancelMail { mail, .. } => mail.clone(),
                    };
                    let result = match mailer {
                        Some(mailer) => {
                            match tokio::time::timeout(OUTSIDE_TIMEOUT, mailer.send(&action, &mail))
                                .await
                            {
                                Ok(Ok(reference)) => (true, Some(reference), None),
                                Ok(Err(error)) => (false, None, Some(error.to_string())),
                                Err(_) => (
                                    false,
                                    None,
                                    Some("the mail server did not answer in time".to_owned()),
                                ),
                            }
                        }
                        None => (
                            false,
                            None,
                            Some("this host has no mail account configured".to_owned()),
                        ),
                    };
                    let (ok, reference, error) = result;
                    let at = Instant::from(jiff::Timestamp::now());
                    let input = Input::OutsideResult(ours_types::OutsideResult {
                        action,
                        ok,
                        reference,
                        error,
                        at,
                    });
                    if tx.send(input).await.is_err() {
                        tracing::warn!(%group, "group queue closed before the outside result arrived");
                    }
                });
            }
            Effect::ResolveTime(request) => {
                let resolver = Arc::clone(&self.resolver);
                let group = self.id.clone();
                let tx = self.self_tx.clone();
                tokio::spawn(async move {
                    let (room, message, text) = (
                        request.room.clone(),
                        request.message.clone(),
                        request.text.clone(),
                    );
                    let resolution = match tokio::time::timeout(
                        RESOLVE_TIMEOUT,
                        resolver.resolve(&group, request),
                    )
                    .await
                    {
                        Ok(Ok(resolution)) => ours_types::TimeResolution {
                            phrase: Some(text),
                            ..resolution
                        },
                        Ok(Err(error)) => {
                            tracing::warn!(%group, %error, "time resolver failed; no time taken");
                            ours_types::TimeResolution {
                                room,
                                message,
                                phrase: Some(text),
                                meaning: ours_types::TimeMeaning::None,
                                echo: None,
                            }
                        }
                        Err(_) => {
                            tracing::warn!(%group, "time resolver timed out; no time taken");
                            ours_types::TimeResolution {
                                room,
                                message,
                                phrase: Some(text),
                                meaning: ours_types::TimeMeaning::None,
                                echo: None,
                            }
                        }
                    };
                    if tx.send(Input::TimeResolved(resolution)).await.is_err() {
                        tracing::warn!(%group, "group queue closed before the time arrived");
                    }
                });
            }
            Effect::Extract(request) => {
                let extractor = Arc::clone(&self.extractor);
                let slots = Arc::clone(&self.extract_slots);
                let group = self.id.clone();
                let tx = self.self_tx.clone();
                tokio::spawn(async move {
                    let (room, message) = (request.room.clone(), request.message.clone());
                    let _slot = slots.acquire().await;
                    let extraction = match tokio::time::timeout(
                        EXTRACT_TIMEOUT,
                        extractor.extract(&group, request),
                    )
                    .await
                    {
                        Ok(Ok(extraction)) => extraction,
                        Ok(Err(error)) => {
                            tracing::warn!(%group, %error, "extractor failed; nothing remembered");
                            ours_types::Extraction::default()
                        }
                        Err(_) => {
                            tracing::warn!(%group, "extractor timed out; nothing remembered");
                            ours_types::Extraction::default()
                        }
                    };
                    if tx
                        .send(Input::Extracted {
                            room,
                            message,
                            extraction,
                        })
                        .await
                        .is_err()
                    {
                        tracing::warn!(%group, "group queue closed before the extraction arrived");
                    }
                });
            }
            Effect::AskModel(request) => {
                let request_detail = serde_json::json!({ "request": {
                    "system": request.system,
                    "messages": request.messages.iter().map(|m| serde_json::json!({ "role": format!("{:?}", m.role).to_lowercase(), "content": m.content })).collect::<Vec<_>>(),
                }});
                let model = Arc::clone(&self.model);
                let group = self.id.clone();
                let tx = self.self_tx.clone();
                let request_id = request.id.clone();
                let strict = self.strict_schema;
                let chat = self.chat;
                tokio::spawn(async move {
                    let reply = model::ask(model.as_ref(), &group, request, strict, chat).await;
                    if tx
                        .send(Input::ModelReply {
                            request: request_id,
                            reply,
                        })
                        .await
                        .is_err()
                    {
                        tracing::warn!(%group, "group queue closed before the model answered");
                    }
                });
                self.hub.publish(AdminEvent {
                    group: self.id.clone(),
                    at: now,
                    kind: "model".to_owned(),
                    line: "asked the model".to_owned(),
                    detail: Some(request_detail),
                });
            }
            Effect::Redact { messages, user } => self.redact(messages, user, now).await,
            Effect::Notice(line) => {
                self.hub.publish(AdminEvent {
                    group: self.id.clone(),
                    at: now,
                    kind: "notice".to_owned(),
                    line,
                    detail: None,
                });
            }
        }
    }

    /// The platform an action goes to: the room's, or for a DM the one
    /// platform the user is known on, or the only connector there is.
    fn platform_for(&self, action: &OutboundAction) -> Option<Platform> {
        let room = match action {
            OutboundAction::SendMessage { room, .. }
            | OutboundAction::EditMessage { room, .. }
            | OutboundAction::DeleteMessage { room, .. }
            | OutboundAction::React { room, .. }
            | OutboundAction::Unreact { room, .. }
            | OutboundAction::Pin { room, .. }
            | OutboundAction::CreatePoll { room, .. }
            | OutboundAction::EndPoll { room, .. }
            | OutboundAction::Typing { room }
            | OutboundAction::FetchHistory { room, .. } => Some(room),
            OutboundAction::SendDm { .. } => None,
        };
        if let Some(room) = room {
            if let Some(platform) = self
                .state
                .rooms
                .get(room)
                .and_then(|r| r.info.as_ref())
                .map(|i| i.platform)
            {
                return Some(platform);
            }
            if let Some((platform, _)) = self.desired.rooms.iter().find(|(_, id)| id == room) {
                return Some(*platform);
            }
        }
        if let OutboundAction::SendDm { user, .. } = action {
            let on = self
                .state
                .rooms
                .values()
                .filter(|r| r.members.contains_key(user))
                .filter_map(|r| r.info.as_ref().map(|i| i.platform))
                .next();
            if on.is_some() {
                return on;
            }
        }
        (self.outbound.len() == 1).then(|| *self.outbound.keys().next().unwrap_or(&Platform::Sim))
    }

    /// Scrubs forgotten words out of the journal and writes a clean snapshot.
    async fn redact(
        &mut self,
        messages: Vec<PlatformMessageId>,
        user: Option<PlatformUserId>,
        now: Instant,
    ) {
        let ids: BTreeSet<String> = messages.iter().map(|m| m.as_str().to_owned()).collect();
        let journal = self.journal.clone();
        let who = user.as_ref().map(|u| u.as_str().to_owned());
        let scrubbed = tokio::task::spawn_blocking(move || {
            crate::runtime::redact::redact_journal(&journal, &ids, who.as_deref())
        })
        .await;
        let line = match scrubbed {
            Ok(Ok(count)) => {
                if let Err(error) = self.snapshot().await {
                    tracing::error!(group = %self.id, %error, "snapshot after redaction failed");
                }
                format!("{count} journal records redacted")
            }
            Ok(Err(error)) => format!("redaction failed: {error}"),
            Err(error) => format!("redaction failed: {error}"),
        };
        tracing::info!(group = %self.id, %line);
        self.hub.publish(AdminEvent {
            group: self.id.clone(),
            at: now,
            kind: "redact".to_owned(),
            line,
            detail: None,
        });
    }

    async fn snapshot(&mut self) -> Result<(), GroupError> {
        if self.seq == 0 {
            return Ok(());
        }
        let journal = self.journal.clone();
        let state = self.state.clone();
        let seq = self.seq;
        tokio::task::spawn_blocking(move || journal.snapshot(seq, &state)).await??;
        self.since_snapshot = 0;
        tracing::debug!(group = %self.id, seq, "snapshot saved");
        Ok(())
    }

    /// Writes charter.md and tells the page when the rendered charter changes.
    fn publish_charter(&mut self) {
        let charter = render_charter(&self.state);
        if charter == self.last_charter {
            return;
        }
        if let Err(error) = std::fs::write(&self.charter_path, &charter) {
            tracing::warn!(group = %self.id, path = %self.charter_path.display(), %error, "could not write the charter");
        }
        self.hub.set_charter(&self.id, charter.clone());
        self.hub.publish(AdminEvent {
            group: self.id.clone(),
            at: Instant::from(jiff::Timestamp::now()),
            kind: "charter".to_owned(),
            line: "charter updated".to_owned(),
            detail: None,
        });
        self.last_charter = charter;
    }

    /// The action records for the admin page, newest first, with names and
    /// numeric tiers as the page expects.
    fn publish_actions(&self) {
        let records: Vec<serde_json::Value> = self
            .state
            .actions
            .iter()
            .rev()
            .filter_map(|record| serde_json::to_value(record).ok())
            .map(|mut value| {
                for (key, name_key) in [
                    ("requested_by", "requested_by_name"),
                    ("approved_by", "approved_by_name"),
                ] {
                    if let Some(user) = value.get(key).and_then(|v| v.as_str()).map(str::to_owned) {
                        value[name_key] =
                            serde_json::Value::String(self.state.name_of(&user.as_str().into()));
                    }
                }
                let tier = match value.get("tier").and_then(|t| t.as_str()) {
                    Some("free") => 0,
                    Some("windowed") => 1,
                    Some("irreversible") => 2,
                    _ => 0,
                };
                value["tier"] = serde_json::Value::from(tier);
                value
            })
            .collect();
        self.hub.set_actions(&self.id, records);
    }

    /// Hands the admin server the calendar feeds: the group's and one per
    /// member the rooms know, so the ICS routes serve current text.
    fn publish_calendars(&mut self, now: Instant) {
        let mark = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_vec(&self.state.calendar)
                .unwrap_or_default()
                .hash(&mut hasher);
            serde_json::to_vec(&self.state.commitments)
                .unwrap_or_default()
                .hash(&mut hasher);
            for room in self.state.rooms.values() {
                for member in room.members.keys() {
                    member.as_str().hash(&mut hasher);
                }
            }
            self.state.feed_base.hash(&mut hasher);
            hasher.finish()
        };
        if mark == self.calendar_mark {
            return;
        }
        self.calendar_mark = mark;
        let group_ics = ours_core::render_ics(&self.state, None, now);
        let members: HashMap<String, String> = self
            .state
            .rooms
            .values()
            .flat_map(|r| r.members.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|user| {
                (
                    user.to_string(),
                    ours_core::render_ics(&self.state, Some(user), now),
                )
            })
            .collect();
        self.hub.set_calendars(&self.id, group_ics, members);
    }

    /// Writes decisions.md and tells the page when the rendered log changes.
    fn publish_decisions(&mut self) {
        let log = render_decisions(&self.state);
        if log == self.last_decisions {
            return;
        }
        if let Err(error) = std::fs::write(&self.decisions_path, &log) {
            tracing::warn!(group = %self.id, path = %self.decisions_path.display(), %error, "could not write the decision log");
        }
        self.hub.set_decision_log(&self.id, log.clone());
        self.last_decisions = log;
    }

    fn publish_summary(&self) {
        let members: BTreeSet<_> = self
            .state
            .rooms
            .values()
            .flat_map(|r| r.members.keys())
            .collect();
        let platforms: BTreeSet<String> = self
            .state
            .rooms
            .values()
            .filter_map(|r| r.info.as_ref())
            .map(|i| format!("{:?}", i.platform).to_lowercase())
            .collect();
        self.hub.set_summary(GroupSummary {
            id: self.id.clone(),
            name: self.desired.name.clone(),
            kind: format!("{:?}", self.state.kind).to_lowercase(),
            platforms: platforms.into_iter().collect(),
            rooms: self.state.rooms.len(),
            members: members.len(),
        });
    }

    /// One line for the admin feed, using the names the state knows.
    fn describe(&self, input: &Input) -> String {
        match input {
            Input::Inbound(InboundEvent::Message(m))
            | Input::Inbound(InboundEvent::MessageEdited(m)) => {
                let who = self.state.display_name(&m.room, &m.author);
                let room = self
                    .state
                    .rooms
                    .get(&m.room)
                    .map(|r| r.name(&m.room))
                    .unwrap_or_else(|| m.room.to_string());
                format!("{who} in {room}: {}", shorten(&m.text, 160))
            }
            Input::Inbound(InboundEvent::ReactionAdded { user, emoji, .. }) => {
                format!("{} reacted {emoji}", self.state.name_of(user))
            }
            Input::Inbound(InboundEvent::RoomSync { room, members }) => {
                format!(
                    "room {} synced with {} members",
                    room.name.clone().unwrap_or_else(|| room.id.to_string()),
                    members.len()
                )
            }
            Input::Inbound(InboundEvent::Ready {
                me_name, platform, ..
            }) => format!("connected to {platform:?} as {me_name}"),
            Input::Inbound(InboundEvent::ActionResult { ok, error, .. }) => {
                if *ok {
                    "action done".to_owned()
                } else {
                    format!("action failed: {}", error.as_deref().unwrap_or("unknown"))
                }
            }
            Input::Inbound(InboundEvent::Disconnected { reason, .. }) => {
                format!("disconnected: {reason}")
            }
            Input::ModelReply { reply, .. } => match reply {
                Ok(plan) => format!(
                    "model replied: {}",
                    shorten(plan.message.as_deref().unwrap_or("(no message)"), 160)
                ),
                Err(e) => format!("model failed: {}", shorten(e, 160)),
            },
            Input::TimerFired { id } => format!("timer {id} fired"),
            Input::OutsideResult(result) => {
                if result.ok {
                    "sent outside".to_owned()
                } else {
                    format!(
                        "outside action failed: {}",
                        result.error.as_deref().unwrap_or("unknown")
                    )
                }
            }
            Input::Mail(mail) => {
                format!("mail from {}: {}", mail.from, shorten(&mail.subject, 100))
            }
            Input::TimeResolved(resolution) => format!(
                "time resolved: {} -> {}",
                resolution.phrase.as_deref().unwrap_or("?"),
                resolution
                    .echo
                    .as_deref()
                    .unwrap_or(match resolution.meaning {
                        ours_types::TimeMeaning::None => "nothing",
                        ours_types::TimeMeaning::Ambiguous { .. } => "ambiguous",
                        _ => "a time",
                    })
            ),
            Input::Extracted { extraction, .. } => {
                let mut parts = Vec::new();
                for d in &extraction.facts {
                    parts.push(format!("fact: {} {} {}", d.subject, d.predicate, d.object));
                }
                for c in &extraction.commitments {
                    parts.push(format!("promise: {} to {}", c.who, c.what));
                }
                for l in &extraction.ledger {
                    parts.push(format!("paid: {} {:.2} for {}", l.payer, l.amount, l.what));
                }
                if parts.is_empty() {
                    "nothing to remember".to_owned()
                } else {
                    shorten(&parts.join("; "), 200)
                }
            }
            Input::Scored { score, .. } => format!(
                "scored {:.2} {:?} by {}",
                score.max(),
                score.kind,
                match &score.source {
                    ours_types::ScoreSource::Rules => "rules".to_owned(),
                    ours_types::ScoreSource::SmallModel { model } => model.clone(),
                }
            ),
            Input::Command(command) => {
                format!("command: {}", shorten(&format!("{command:?}"), 160))
            }
            Input::Inbound(other) => shorten(&format!("{other:?}"), 160),
        }
    }
}

const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(20);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);
const OUTSIDE_TIMEOUT: Duration = Duration::from_secs(90);
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(60);
const EXTRACT_CONCURRENCY: usize = 4;

/// What the admin page may show under an input's line.
fn detail_of(input: &Input) -> Option<serde_json::Value> {
    match input {
        Input::Scored { score, .. } => Some(serde_json::json!({ "score": score })),
        Input::ModelReply {
            reply: Ok(plan), ..
        } => Some(serde_json::json!({ "plan": plan })),
        Input::Extracted { extraction, .. }
            if !(extraction.facts.is_empty()
                && extraction.commitments.is_empty()
                && extraction.ledger.is_empty()) =>
        {
            Some(serde_json::json!({ "extraction": extraction }))
        }
        _ => None,
    }
}

fn describe_action(action: &OutboundAction) -> String {
    match action {
        OutboundAction::SendMessage { text, .. } => format!("said: {}", shorten(text, 200)),
        OutboundAction::SendDm { text, .. } => format!("sent a DM: {}", shorten(text, 200)),
        OutboundAction::EditMessage { text, .. } => format!("edited: {}", shorten(text, 200)),
        OutboundAction::DeleteMessage { .. } => "deleted a message".to_owned(),
        OutboundAction::React { emoji, .. } => format!("reacted {emoji}"),
        OutboundAction::Unreact { emoji, .. } => format!("removed reaction {emoji}"),
        OutboundAction::Pin { .. } => "pinned a message".to_owned(),
        OutboundAction::CreatePoll {
            question, options, ..
        } => format!(
            "opened a poll: {} ({} options)",
            shorten(question, 120),
            options.len()
        ),
        OutboundAction::EndPoll { .. } => "ended a poll".to_owned(),
        OutboundAction::Typing { .. } => "typing".to_owned(),
        OutboundAction::FetchHistory { limit, .. } => {
            format!("fetched {limit} messages of history")
        }
    }
}

fn shorten(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let cut: String = one_line.chars().take(max).collect();
        format!("{cut}...")
    }
}
