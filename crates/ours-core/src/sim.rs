//! A deterministic scenario harness. `Sim` owns a group's state and a
//! virtual clock, feeds inputs through `step`, answers sends and polls the
//! way a connector would, answers model, classifier, extractor and time
//! resolver calls, outside actions and history fetches from a script, and
//! fires timers as the clock passes them. Nothing here does I/O.

use crate::effect::{Effect, ModelRequest};
use crate::input::{Command, Input};
use crate::plan::{Plan, PlannedAction};
use crate::profile::Profile;
use crate::state::{DEFAULT_AGENT_NAME, GroupState};
use crate::step::step;
use crate::timers::TimerPurpose;
use ours_types::{
    ActionId, Capabilities, ClassifyRequest, CommitmentDraft, ExtractRequest, Extraction,
    FactDraft, GroupId, InboundEvent, InboundMail, Instant, Kind, LedgerDraft, Member, Message,
    MessageKind, Outbound, OutboundAction, OutsideAction, OutsideResult, Platform,
    PlatformMessageId, PlatformRole, PlatformRoomId, PlatformUserId, Room, RoomKind, Score,
    ScoreSource, SubjectKind, TimeMeaning, TimeRequest, TimeResolution, TimerId,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The harness's clock starts at 2026-11-02 18:00 UTC, a Monday.
pub const START: Instant = Instant(jiff::Timestamp::constant(1_793_642_400, 0));
/// The agent's platform id in every simulated room.
pub const ME: &str = "ours-bot";
/// The agent's name in every simulated group.
pub const SIM_AGENT_NAME: &str = "Q";

/// How the scripted model answers the next request.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelScript {
    Reply(Plan),
    Fail(String),
    /// Never answers, so the guard timer decides.
    Silent,
}

/// How the scripted classifier answers the next `Effect::Classify`. With
/// nothing scripted it answers with the rules score, as the binary does
/// when the small model fails.
#[derive(Debug, Clone, PartialEq)]
pub enum ClassifierScript {
    Rules,
    /// The rules score, with the question marked answerable from context.
    Answerable,
    Score(Score),
    /// Never answers; the scenario feeds `Input::Scored` itself.
    Silent,
}

/// How the scripted extractor answers the next `Effect::Extract`. With
/// nothing scripted it finds nothing, as the binary answers when the
/// extractor fails.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtractorScript {
    Drafts(Vec<FactDraft>),
    /// Facts, commitments and tab lines.
    Found(Extraction),
    /// Never answers; the scenario feeds `Input::Extracted` itself.
    Silent,
}

/// How the scripted resolver answers the next `Effect::ResolveTime`. With
/// nothing scripted it answers `TimeMeaning::None`, as the binary does when
/// the resolver fails.
#[derive(Debug, Clone, PartialEq)]
pub enum TimeScript {
    Resolve {
        meaning: TimeMeaning,
        echo: Option<String>,
    },
    /// Never answers; the scenario feeds `Input::TimeResolved` itself.
    Silent,
}

/// How the scripted integration answers the next `Effect::Outside`. With
/// nothing scripted it succeeds with a reference of its own.
#[derive(Debug, Clone, PartialEq)]
pub enum OutsideScript {
    Ok {
        reference: String,
    },
    Fail(String),
    /// Never answers; the scenario feeds `Input::OutsideResult` itself.
    Silent,
}

/// An outbound action and the message id the simulated platform gave it.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    pub outbound: Outbound,
    pub message: Option<PlatformMessageId>,
    pub at: Instant,
}

/// A message the harness posted and everything that followed from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Posted {
    pub id: PlatformMessageId,
    pub effects: Vec<Effect>,
}

#[derive(Debug)]
pub struct Sim {
    pub state: GroupState,
    pub now: Instant,
    /// Every input fed, with its instant, in order: the event log.
    pub log: Vec<(Instant, Input)>,
    pub effects: Vec<Effect>,
    pub requests: Vec<ModelRequest>,
    pub classified: Vec<ClassifyRequest>,
    pub extracted: Vec<ExtractRequest>,
    pub resolving: Vec<TimeRequest>,
    pub sent: Vec<Sent>,
    /// Outside actions carried out, in order.
    pub outside: Vec<(ActionId, OutsideAction)>,
    script: VecDeque<ModelScript>,
    integration: VecDeque<OutsideScript>,
    resolver: VecDeque<TimeScript>,
    classifier: VecDeque<ClassifierScript>,
    extractor: VecDeque<ExtractorScript>,
    pages: VecDeque<Vec<Message>>,
    timers: BTreeMap<TimerId, Instant>,
    dm_rooms: BTreeSet<PlatformUserId>,
    /// People the platform will not DM until they write to the agent first.
    closed_dms: BTreeSet<PlatformUserId>,
    posted: u64,
    delivered: u64,
    history: u64,
}

impl Sim {
    /// A group of `kind` with the agent connected to the sim platform and
    /// observe mode off, so scenarios see it speak. With nothing scripted
    /// the model stays silent.
    pub fn new(kind: Kind) -> Sim {
        let mut sim = Sim::observing(kind);
        let profile = Profile {
            observe_seconds: 0,
            ..Profile::for_kind(kind)
        };
        sim.feed(Input::Command(Command::SetProfile { profile }));
        sim
    }

    /// A group of `kind` with the profile as the kind sets it, so the first
    /// room sync starts observe mode.
    pub fn observing(kind: Kind) -> Sim {
        Sim::with_state(GroupState::new(GroupId::new("group"), kind))
    }

    /// A harness around `state`. A state still carrying the shipped default
    /// name is first renamed to Q, the simulator's name.
    pub fn with_state(state: GroupState) -> Sim {
        let rename = state.agent_name == DEFAULT_AGENT_NAME && !state.name_from_host;
        let mut sim = Sim {
            state,
            now: START,
            log: Vec::new(),
            effects: Vec::new(),
            requests: Vec::new(),
            classified: Vec::new(),
            extracted: Vec::new(),
            resolving: Vec::new(),
            sent: Vec::new(),
            outside: Vec::new(),
            script: VecDeque::new(),
            integration: VecDeque::new(),
            resolver: VecDeque::new(),
            classifier: VecDeque::new(),
            extractor: VecDeque::new(),
            pages: VecDeque::new(),
            timers: BTreeMap::new(),
            dm_rooms: BTreeSet::new(),
            closed_dms: BTreeSet::new(),
            posted: 0,
            delivered: 0,
            history: 0,
        };
        if rename {
            sim.feed(Input::Command(Command::SetAgentName {
                name: SIM_AGENT_NAME.to_owned(),
                by_host: false,
            }));
        }
        sim.feed(Input::Inbound(InboundEvent::Ready {
            platform: Platform::Sim,
            me: PlatformUserId::new(ME),
            me_name: SIM_AGENT_NAME.to_owned(),
            capabilities: Default::default(),
        }));
        sim
    }

    /// Names the agent as the host's config does, which skips the name question.
    pub fn named_by_host(&mut self, name: &str) -> Vec<Effect> {
        self.feed(Input::Command(Command::SetAgentName {
            name: name.to_owned(),
            by_host: true,
        }))
    }

    /// Syncs a channel with these members, plus the agent itself.
    pub fn room(
        &mut self,
        id: &str,
        name: &str,
        members: &[(&str, &str, PlatformRole)],
    ) -> Vec<Effect> {
        let room = Room {
            platform: Platform::Sim,
            id: PlatformRoomId::new(id),
            name: Some(name.to_owned()),
            kind: RoomKind::Channel,
            parent: None,
            container: None,
        };
        let mut list: Vec<Member> = members
            .iter()
            .map(|(id, name, role)| member(id, name, *role))
            .collect();
        list.push(Member {
            is_bot: true,
            ..member(ME, "Ours", PlatformRole::Regular)
        });
        self.feed(Input::Inbound(InboundEvent::RoomSync {
            room,
            members: list,
        }))
    }

    /// Syncs a Discord channel in the server `container`, so its messages
    /// have links. The agent's account is the same on both platforms.
    pub fn discord_room(
        &mut self,
        id: &str,
        name: &str,
        container: &str,
        members: &[(&str, &str, PlatformRole)],
    ) -> Vec<Effect> {
        if !self
            .state
            .me
            .iter()
            .any(|m| m.platform == Platform::Discord)
        {
            self.feed(Input::Inbound(InboundEvent::Ready {
                platform: Platform::Discord,
                me: PlatformUserId::new(ME),
                me_name: SIM_AGENT_NAME.to_owned(),
                capabilities: Default::default(),
            }));
        }
        let room = Room {
            platform: Platform::Discord,
            id: PlatformRoomId::new(id),
            name: Some(name.to_owned()),
            kind: RoomKind::Channel,
            parent: None,
            container: Some(container.to_owned()),
        };
        let mut list: Vec<Member> = members
            .iter()
            .map(|(id, name, role)| member(id, name, *role))
            .collect();
        list.push(Member {
            is_bot: true,
            ..member(ME, "Ours", PlatformRole::Regular)
        });
        self.feed(Input::Inbound(InboundEvent::RoomSync {
            room,
            members: list,
        }))
    }

    /// Reconnects the sim platform with native polls, so ballots become
    /// `CreatePoll` and votes arrive as `PollVote`.
    pub fn native_polls(&mut self) -> Vec<Effect> {
        self.feed(Input::Inbound(InboundEvent::Ready {
            platform: Platform::Sim,
            me: PlatformUserId::new(ME),
            me_name: SIM_AGENT_NAME.to_owned(),
            capabilities: Capabilities {
                reactions: true,
                polls: true,
                ..Capabilities::default()
            },
        }))
    }

    /// Syncs a thread hanging off `parent`, with these members and the agent.
    pub fn thread_room(
        &mut self,
        id: &str,
        name: &str,
        parent: &str,
        members: &[(&str, &str, PlatformRole)],
    ) -> Vec<Effect> {
        let room = Room {
            platform: Platform::Sim,
            id: PlatformRoomId::new(id),
            name: Some(name.to_owned()),
            kind: RoomKind::Thread,
            parent: Some(PlatformRoomId::new(parent)),
            container: None,
        };
        let mut list: Vec<Member> = members
            .iter()
            .map(|(id, name, role)| member(id, name, *role))
            .collect();
        list.push(Member {
            is_bot: true,
            ..member(ME, "Ours", PlatformRole::Regular)
        });
        self.feed(Input::Inbound(InboundEvent::RoomSync {
            room,
            members: list,
        }))
    }

    /// Queues the resolver's answer to the next time it is asked about.
    pub fn resolves(&mut self, answer: TimeScript) {
        self.resolver.push_back(answer);
    }

    /// Queues the extractor's answer to the next message it reads.
    pub fn extract(&mut self, answer: ExtractorScript) {
        self.extractor.push_back(answer);
    }

    /// Queues these drafts as the extractor's answer to the next message.
    pub fn extracts(&mut self, drafts: Vec<FactDraft>) {
        self.extract(ExtractorScript::Drafts(drafts));
    }

    /// Queues these commitments as the extractor's answer to the next message.
    pub fn promises(&mut self, commitments: Vec<CommitmentDraft>) {
        self.extract(ExtractorScript::Found(Extraction {
            commitments,
            ..Extraction::default()
        }));
    }

    /// Queues these tab lines as the extractor's answer to the next message.
    pub fn pays(&mut self, ledger: Vec<LedgerDraft>) {
        self.extract(ExtractorScript::Found(Extraction {
            ledger,
            ..Extraction::default()
        }));
    }

    /// Reconnects the sim platform with these capabilities, as a connector
    /// that can edit and pin does.
    pub fn capabilities(&mut self, capabilities: Capabilities) -> Vec<Effect> {
        self.feed(Input::Inbound(InboundEvent::Ready {
            platform: Platform::Sim,
            me: PlatformUserId::new(ME),
            me_name: SIM_AGENT_NAME.to_owned(),
            capabilities,
        }))
    }

    /// Queues a page of history, oldest first, as the answer to the next
    /// history fetch. With nothing queued a fetch is never answered.
    pub fn history_page(&mut self, messages: Vec<Message>) {
        self.pages.push_back(messages);
    }

    /// A message from `user` in `room` sent `seconds_ago`, for history
    /// pages. Its id is "h" and a count.
    pub fn old_message(&mut self, room: &str, user: &str, text: &str, seconds_ago: i64) -> Message {
        self.history += 1;
        Message {
            id: PlatformMessageId::new(format!("h{}", self.history)),
            room: PlatformRoomId::new(room),
            author: PlatformUserId::new(user),
            text: text.to_owned(),
            at: self.now.plus_seconds(-seconds_ago),
            reply_to: None,
            mentions: Vec::new(),
            mentions_me: false,
            attachments: Vec::new(),
            edited: false,
        }
    }

    /// Queues the integration's answer to the next outside action.
    pub fn integration(&mut self, answer: OutsideScript) {
        self.integration.push_back(answer);
    }

    /// Mail arriving at the group's address now.
    pub fn mail(&mut self, mail: InboundMail) -> Vec<Effect> {
        self.feed(Input::Mail(mail))
    }

    /// Queues the model's answer to the next request.
    pub fn script(&mut self, answer: ModelScript) {
        self.script.push_back(answer);
    }

    /// Queues a plain reply as the model's answer to the next request.
    pub fn model_says(&mut self, text: &str) {
        self.script(ModelScript::Reply(plan(text)));
    }

    /// Queues the classifier's answer to the next message it scores.
    pub fn classify(&mut self, answer: ClassifierScript) {
        self.classifier.push_back(answer);
    }

    /// The DM room with `user`, synced the way a connector syncs one.
    pub fn dm_room(&mut self, user: &str) -> PlatformRoomId {
        let id = PlatformRoomId::new(format!("dm-{user}"));
        let user = PlatformUserId::new(user);
        if self.state.dm_room_of(&user).is_none() {
            let input = self.dm_sync(&user);
            self.feed(input);
        }
        id
    }

    /// A DM from `user` to the agent. It opens the chat, so DMs to them
    /// get through from then on.
    pub fn dm(&mut self, user: &str, text: &str) -> Posted {
        self.closed_dms.remove(&PlatformUserId::new(user));
        let room = self.dm_room(user);
        self.say(room.as_str(), user, text)
    }

    /// Refuses DMs to `user` until they DM the agent, as Telegram does for
    /// anyone who has not opened the bot.
    pub fn refuse_dms_to(&mut self, user: &str) {
        self.closed_dms.insert(PlatformUserId::new(user));
    }

    fn dm_sync(&mut self, user: &PlatformUserId) -> Input {
        self.dm_rooms.insert(user.clone());
        let name = self.state.name_of(user);
        Input::Inbound(InboundEvent::RoomSync {
            room: Room {
                platform: Platform::Sim,
                id: PlatformRoomId::new(format!("dm-{user}")),
                name: None,
                kind: RoomKind::Dm,
                parent: None,
                container: None,
            },
            members: vec![member(user.as_str(), &name, PlatformRole::Regular)],
        })
    }

    /// A message from `user` in `room` at the current instant, not yet posted.
    pub fn message(&mut self, room: &str, user: &str, text: &str) -> Message {
        self.posted += 1;
        Message {
            id: PlatformMessageId::new(format!("m{}", self.posted)),
            room: PlatformRoomId::new(room),
            author: PlatformUserId::new(user),
            text: text.to_owned(),
            at: self.now,
            reply_to: None,
            mentions: Vec::new(),
            mentions_me: false,
            attachments: Vec::new(),
            edited: false,
        }
    }

    pub fn say(&mut self, room: &str, user: &str, text: &str) -> Posted {
        let message = self.message(room, user, text);
        self.post(message)
    }

    pub fn reply(&mut self, room: &str, user: &str, text: &str, to: &PlatformMessageId) -> Posted {
        let message = Message {
            reply_to: Some(to.clone()),
            ..self.message(room, user, text)
        };
        self.post(message)
    }

    pub fn post(&mut self, message: Message) -> Posted {
        let id = message.id.clone();
        let effects = self.feed(Input::Inbound(InboundEvent::Message(message)));
        Posted { id, effects }
    }

    pub fn react(
        &mut self,
        room: &str,
        message: &PlatformMessageId,
        user: &str,
        emoji: &str,
    ) -> Vec<Effect> {
        self.feed(Input::Inbound(InboundEvent::ReactionAdded {
            room: PlatformRoomId::new(room),
            message: message.clone(),
            user: PlatformUserId::new(user),
            emoji: emoji.to_owned(),
            at: self.now,
        }))
    }

    pub fn unreact(
        &mut self,
        room: &str,
        message: &PlatformMessageId,
        user: &str,
        emoji: &str,
    ) -> Vec<Effect> {
        self.feed(Input::Inbound(InboundEvent::ReactionRemoved {
            room: PlatformRoomId::new(room),
            message: message.clone(),
            user: PlatformUserId::new(user),
            emoji: emoji.to_owned(),
            at: self.now,
        }))
    }

    /// A vote on a native poll, zero-based, or its removal.
    pub fn poll_vote(
        &mut self,
        room: &str,
        poll: &PlatformMessageId,
        user: &str,
        option: u32,
        added: bool,
    ) -> Vec<Effect> {
        self.feed(Input::Inbound(InboundEvent::PollVote {
            room: PlatformRoomId::new(room),
            message: poll.clone(),
            user: PlatformUserId::new(user),
            option,
            added,
            at: self.now,
        }))
    }

    pub fn poll_ended(
        &mut self,
        room: &str,
        poll: &PlatformMessageId,
        counts: &[u32],
    ) -> Vec<Effect> {
        self.feed(Input::Inbound(InboundEvent::PollEnded {
            room: PlatformRoomId::new(room),
            message: poll.clone(),
            counts: counts.to_vec(),
        }))
    }

    /// Steps `input` and everything it causes: action results for sends,
    /// scripted model answers, until nothing is left to feed.
    pub fn feed(&mut self, input: Input) -> Vec<Effect> {
        let mut queue = VecDeque::from([input]);
        let mut all = Vec::new();
        while let Some(input) = queue.pop_front() {
            self.log.push((self.now, input.clone()));
            let effects = step(&mut self.state, input, self.now);
            for effect in &effects {
                self.carry_out(effect, &mut queue);
            }
            self.effects.extend(effects.iter().cloned());
            all.extend(effects);
        }
        all
    }

    fn carry_out(&mut self, effect: &Effect, queue: &mut VecDeque<Input>) {
        match effect {
            Effect::Send(outbound) => {
                if let OutboundAction::SendDm { user, .. } = &outbound.action
                    && self.closed_dms.contains(user)
                {
                    self.sent.push(Sent {
                        outbound: outbound.clone(),
                        message: None,
                        at: self.now,
                    });
                    queue.push_back(Input::Inbound(InboundEvent::ActionResult {
                        id: outbound.id.clone(),
                        ok: false,
                        message: None,
                        error: Some(
                            "Forbidden: bot can't initiate conversation with a user".to_owned(),
                        ),
                    }));
                    return;
                }
                // A connector syncs the DM room before reporting the send.
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
                    let prefix = match outbound.action {
                        OutboundAction::CreatePoll { .. } => "poll",
                        _ => "sent",
                    };
                    PlatformMessageId::new(format!("{prefix}{}", self.delivered))
                });
                self.sent.push(Sent {
                    outbound: outbound.clone(),
                    message: message.clone(),
                    at: self.now,
                });
                queue.push_back(Input::Inbound(InboundEvent::ActionResult {
                    id: outbound.id.clone(),
                    ok: true,
                    message,
                    error: None,
                }));
                if let OutboundAction::FetchHistory { room, .. } = &outbound.action
                    && let Some(messages) = self.pages.pop_front()
                {
                    queue.push_back(Input::Inbound(InboundEvent::History {
                        room: room.clone(),
                        messages,
                    }));
                }
            }
            Effect::SetTimer { id, at, .. } => {
                self.timers.insert(id.clone(), *at);
            }
            Effect::CancelTimer { id } => {
                self.timers.remove(id);
            }
            Effect::AskModel(request) => {
                self.requests.push(request.clone());
                let reply = match self.script.pop_front().unwrap_or(ModelScript::Silent) {
                    ModelScript::Reply(plan) => Some(Ok(plan)),
                    ModelScript::Fail(error) => Some(Err(error)),
                    ModelScript::Silent => None,
                };
                if let Some(reply) = reply {
                    queue.push_back(Input::ModelReply {
                        request: request.id.clone(),
                        reply,
                    });
                }
            }
            Effect::Classify(request) => {
                self.classified.push(request.clone());
                let score = match self
                    .classifier
                    .pop_front()
                    .unwrap_or(ClassifierScript::Rules)
                {
                    ClassifierScript::Rules => Some(request.rules.clone()),
                    ClassifierScript::Answerable => Some(Score {
                        answerable_from_context: true,
                        source: ScoreSource::SmallModel {
                            model: "sim".to_owned(),
                        },
                        ..request.rules.clone()
                    }),
                    ClassifierScript::Score(score) => Some(score),
                    ClassifierScript::Silent => None,
                };
                if let Some(score) = score {
                    queue.push_back(Input::Scored {
                        room: request.room.clone(),
                        message: request.message.clone(),
                        score,
                    });
                }
            }
            Effect::Extract(request) => {
                self.extracted.push(request.clone());
                let extraction = match self
                    .extractor
                    .pop_front()
                    .unwrap_or(ExtractorScript::Drafts(Vec::new()))
                {
                    ExtractorScript::Drafts(facts) => Some(Extraction {
                        facts,
                        ..Extraction::default()
                    }),
                    ExtractorScript::Found(extraction) => Some(extraction),
                    ExtractorScript::Silent => None,
                };
                if let Some(extraction) = extraction {
                    queue.push_back(Input::Extracted {
                        room: request.room.clone(),
                        message: request.message.clone(),
                        extraction,
                    });
                }
            }
            Effect::ResolveTime(request) => {
                self.resolving.push(request.clone());
                let answer = self.resolver.pop_front().unwrap_or(TimeScript::Resolve {
                    meaning: TimeMeaning::None,
                    echo: None,
                });
                if let TimeScript::Resolve { meaning, echo } = answer {
                    queue.push_back(Input::TimeResolved(TimeResolution {
                        room: request.room.clone(),
                        message: request.message.clone(),
                        phrase: Some(request.text.clone()),
                        meaning,
                        echo,
                    }));
                }
            }
            Effect::Outside { action, outside } => {
                self.outside.push((action.clone(), outside.clone()));
                let n = self.outside.len();
                let answer = self
                    .integration
                    .pop_front()
                    .unwrap_or_else(|| OutsideScript::Ok {
                        reference: format!("<ref{n}@sim>"),
                    });
                let result = match answer {
                    OutsideScript::Ok { reference } => Some(OutsideResult {
                        action: action.clone(),
                        ok: true,
                        reference: Some(reference),
                        error: None,
                        at: self.now,
                    }),
                    OutsideScript::Fail(error) => Some(OutsideResult {
                        action: action.clone(),
                        ok: false,
                        reference: None,
                        error: Some(error),
                        at: self.now,
                    }),
                    OutsideScript::Silent => None,
                };
                if let Some(result) = result {
                    queue.push_back(Input::OutsideResult(result));
                }
            }
            Effect::Notice(_) | Effect::Redact { .. } => {}
        }
    }

    /// Moves the clock forward, firing each timer at its own instant.
    pub fn advance(&mut self, seconds: i64) -> Vec<Effect> {
        let until = self.now.plus_seconds(seconds);
        let mut all = Vec::new();
        loop {
            let due = self
                .timers
                .iter()
                .filter(|(_, at)| **at <= until)
                .min_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)))
                .map(|(id, at)| (id.clone(), *at));
            let Some((id, at)) = due else {
                break;
            };
            self.timers.remove(&id);
            self.now = self.now.max(at);
            all.extend(self.feed(Input::TimerFired { id }));
        }
        self.now = until;
        all
    }

    /// Moves the clock to `at`, firing timers on the way.
    pub fn advance_to(&mut self, at: Instant) -> Vec<Effect> {
        let seconds = at.seconds_since(self.now).max(0);
        self.advance(seconds)
    }

    /// The group's home zone, as an IANA name.
    pub fn zone(&mut self, zone: &str) -> Vec<Effect> {
        self.feed(Input::Command(Command::SetTimezone {
            zone: zone.to_owned(),
        }))
    }

    /// The platform id of the most recent native poll.
    pub fn last_poll(&self) -> Option<PlatformMessageId> {
        self.sent
            .iter()
            .rev()
            .find_map(|s| match s.outbound.action {
                OutboundAction::CreatePoll { .. } => s.message.clone(),
                _ => None,
            })
    }

    /// Timers armed and not yet fired or cancelled.
    pub fn armed_timers(&self) -> usize {
        self.timers.len()
    }

    /// Armed timers other than the nightly retention sweep, which stays
    /// armed while the group holds anything that will one day go.
    pub fn timers_besides_retention(&self) -> usize {
        self.timers
            .keys()
            .filter(|id| {
                self.state
                    .timers
                    .get(*id)
                    .is_none_or(|t| t.purpose != TimerPurpose::Retention)
            })
            .count()
    }

    /// The platform id of the agent's most recent message.
    pub fn last_sent_message(&self) -> Option<PlatformMessageId> {
        self.sent.iter().rev().find_map(|s| s.message.clone())
    }

    /// The platform id of the agent's most recent DM to `user`.
    pub fn last_dm_to(&self, user: &str) -> Option<PlatformMessageId> {
        self.sent
            .iter()
            .rev()
            .find_map(|s| match &s.outbound.action {
                OutboundAction::SendDm { user: to, .. } if to.as_str() == user => s.message.clone(),
                _ => None,
            })
    }
}

/// An instant from RFC 3339 text such as "2026-11-09T19:00:00+11:00", or
/// the Unix epoch when the text is not one.
pub fn instant(text: &str) -> Instant {
    text.parse::<jiff::Timestamp>()
        .map_or(Instant::UNIX_EPOCH, Instant)
}

/// A score from the small model that sees a plan forming.
pub fn plan_score() -> Score {
    Score {
        plan_forming: 0.9,
        source: ScoreSource::SmallModel {
            model: "sim".to_owned(),
        },
        ..Score::default()
    }
}

/// A score from the small model that hears only banter at `level`.
pub fn banter(level: f32) -> Score {
    Score {
        banter: level,
        source: ScoreSource::SmallModel {
            model: "sim".to_owned(),
        },
        ..Score::default()
    }
}

/// A plan that only reacts with `emoji`.
pub fn reacts(emoji: &str) -> Plan {
    Plan {
        message: None,
        actions: vec![PlannedAction::React {
            emoji: emoji.to_owned(),
        }],
        cites: Vec::new(),
    }
}

/// A score from the small model for a question it can answer from the room.
pub fn answerable_question() -> Score {
    Score {
        unanswered_question: 0.6,
        kind: MessageKind::Question,
        answerable_from_context: true,
        source: ScoreSource::SmallModel {
            model: "sim".to_owned(),
        },
        ..Score::default()
    }
}

pub fn member(id: &str, name: &str, role: PlatformRole) -> Member {
    Member {
        id: PlatformUserId::new(id),
        display_name: name.to_owned(),
        platform_role: role,
        is_bot: false,
        timezone: None,
    }
}

/// A plan that only replies with `text`.
pub fn plan(text: &str) -> Plan {
    Plan {
        message: Some(text.to_owned()),
        ..Plan::default()
    }
}

/// A plan that emails `to`, with no reply of its own.
pub fn mail_plan(to: &str, subject: &str, body: &str) -> Plan {
    Plan {
        message: None,
        actions: vec![PlannedAction::Mail {
            to: to.to_owned(),
            subject: subject.to_owned(),
            body: body.to_owned(),
        }],
        cites: Vec::new(),
    }
}

/// Mail from `from` to the group's address, replying to `in_reply_to`.
pub fn inbound_mail(
    from: &str,
    name: Option<&str>,
    subject: &str,
    body: &str,
    in_reply_to: &[&str],
    at: Instant,
) -> InboundMail {
    InboundMail {
        from: from.to_owned(),
        from_name: name.map(str::to_owned),
        subject: subject.to_owned(),
        body: body.to_owned(),
        at,
        message_id: Some(format!(
            "<{}-{}@mail>",
            from.replace('@', "."),
            at.as_millis()
        )),
        in_reply_to: in_reply_to.iter().map(|s| (*s).to_owned()).collect(),
        authenticated: true,
    }
}

/// Outside actions asked for.
pub fn outside(effects: &[Effect]) -> Vec<(&ActionId, &OutsideAction)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Outside { action, outside } => Some((action, outside)),
            _ => None,
        })
        .collect()
}

/// Messages deleted.
pub fn deleted(effects: &[Effect]) -> Vec<&PlatformMessageId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::DeleteMessage { id, .. },
                ..
            }) => Some(id),
            _ => None,
        })
        .collect()
}

/// A plan that replies with `text` and cites these message ids.
pub fn citing(text: &str, cites: &[&str]) -> Plan {
    Plan {
        message: Some(text.to_owned()),
        cites: cites.iter().map(|c| (*c).to_owned()).collect(),
        ..Plan::default()
    }
}

/// A durable fact about someone else, stated outright.
pub fn draft(subject: &str, predicate: &str, object: &str) -> FactDraft {
    FactDraft {
        subject: subject.to_owned(),
        subject_kind: SubjectKind::Person,
        predicate: predicate.to_owned(),
        object: object.to_owned(),
        about_self: false,
        stated: true,
        durable: true,
        expires: None,
        starts: None,
        quote: None,
    }
}

/// An extraction that found only these facts.
pub fn facts(drafts: Vec<FactDraft>) -> Extraction {
    Extraction {
        facts: drafts,
        ..Extraction::default()
    }
}

/// A commitment stated outright, due on `due` as YYYY-MM-DD when given.
pub fn promise(who: &str, what: &str, due: Option<&str>) -> CommitmentDraft {
    CommitmentDraft {
        who: who.to_owned(),
        what: what.to_owned(),
        for_whom: None,
        due: due.map(str::to_owned),
        event: None,
        stated: true,
        quote: None,
    }
}

/// Money `payer` paid for `what`, split as the room's defaults say.
pub fn paid(payer: &str, amount: f64, what: &str) -> LedgerDraft {
    LedgerDraft {
        payer: payer.to_owned(),
        amount,
        currency: None,
        what: what.to_owned(),
        participants: Vec::new(),
        event: None,
        quote: None,
    }
}

/// The speaker away from `starts` to `expires`, both YYYY-MM-DD.
pub fn away_draft(subject: &str, starts: &str, expires: &str) -> FactDraft {
    FactDraft {
        about_self: true,
        durable: false,
        starts: Some(starts.to_owned()),
        expires: Some(expires.to_owned()),
        ..draft(subject, "is", "away")
    }
}

/// A durable fact the speaker stated about themselves, in these words.
pub fn self_draft(subject: &str, predicate: &str, object: &str, quote: &str) -> FactDraft {
    FactDraft {
        about_self: true,
        quote: Some(quote.to_owned()),
        ..draft(subject, predicate, object)
    }
}

/// Texts of the messages sent, with what each replies to.
pub fn messages(effects: &[Effect]) -> Vec<(&str, Option<&PlatformMessageId>)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::SendMessage { text, reply_to, .. },
                ..
            }) => Some((text.as_str(), reply_to.as_ref())),
            _ => None,
        })
        .collect()
}

/// Texts of the messages sent.
pub fn texts(effects: &[Effect]) -> Vec<&str> {
    messages(effects)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// Reactions added, as (message, emoji).
pub fn reactions(effects: &[Effect]) -> Vec<(&PlatformMessageId, &str)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::React { message, emoji, .. },
                ..
            }) => Some((message, emoji.as_str())),
            _ => None,
        })
        .collect()
}

/// Rooms shown that the agent is writing, in order.
pub fn typing(effects: &[Effect]) -> Vec<&PlatformRoomId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::Typing { room },
                ..
            }) => Some(room),
            _ => None,
        })
        .collect()
}

/// Reactions removed, as (message, emoji).
pub fn unreactions(effects: &[Effect]) -> Vec<(&PlatformMessageId, &str)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::Unreact { message, emoji, .. },
                ..
            }) => Some((message, emoji.as_str())),
            _ => None,
        })
        .collect()
}

pub fn model_requests(effects: &[Effect]) -> Vec<&ModelRequest> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::AskModel(request) => Some(request),
            _ => None,
        })
        .collect()
}

pub fn notices(effects: &[Effect]) -> Vec<&str> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Notice(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Journal redactions asked for, as (messages, user).
pub fn redactions(effects: &[Effect]) -> Vec<(&[PlatformMessageId], Option<&PlatformUserId>)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Redact { messages, user } => Some((messages.as_slice(), user.as_ref())),
            _ => None,
        })
        .collect()
}

/// Whether any effect sends anything to a room.
pub fn any_send(effects: &[Effect]) -> bool {
    effects.iter().any(|e| matches!(e, Effect::Send(_)))
}

/// DMs sent, as (user, text).
pub fn dms(effects: &[Effect]) -> Vec<(&PlatformUserId, &str)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::SendDm { user, text },
                ..
            }) => Some((user, text.as_str())),
            _ => None,
        })
        .collect()
}

pub fn extract_requests(effects: &[Effect]) -> Vec<&ExtractRequest> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Extract(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// History fetches, as (room, limit, before).
pub fn fetches(effects: &[Effect]) -> Vec<(&PlatformRoomId, u32, Option<&PlatformMessageId>)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action:
                    OutboundAction::FetchHistory {
                        room,
                        limit,
                        before,
                    },
                ..
            }) => Some((room, *limit, before.as_ref())),
            _ => None,
        })
        .collect()
}

/// Native polls created, as (question, options, hours).
pub fn polls(effects: &[Effect]) -> Vec<(&str, &[String], u32)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action:
                    OutboundAction::CreatePoll {
                        question,
                        options,
                        hours,
                        ..
                    },
                ..
            }) => Some((question.as_str(), options.as_slice(), *hours)),
            _ => None,
        })
        .collect()
}

/// Messages edited, as (message, text).
pub fn edits(effects: &[Effect]) -> Vec<(&PlatformMessageId, &str)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::EditMessage { id, text, .. },
                ..
            }) => Some((id, text.as_str())),
            _ => None,
        })
        .collect()
}

/// Messages pinned.
pub fn pinned(effects: &[Effect]) -> Vec<&PlatformMessageId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::Pin { message, .. },
                ..
            }) => Some(message),
            _ => None,
        })
        .collect()
}

/// Native polls ended early, by message.
pub fn ended_polls(effects: &[Effect]) -> Vec<&PlatformMessageId> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(Outbound {
                action: OutboundAction::EndPoll { message, .. },
                ..
            }) => Some(message),
            _ => None,
        })
        .collect()
}

pub fn time_requests(effects: &[Effect]) -> Vec<&TimeRequest> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::ResolveTime(request) => Some(request),
            _ => None,
        })
        .collect()
}

pub fn classify_requests(effects: &[Effect]) -> Vec<&ClassifyRequest> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Classify(request) => Some(request),
            _ => None,
        })
        .collect()
}
