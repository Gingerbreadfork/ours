//! Discord for Ours, through twilight: the gateway feeds the engine's
//! event model and the engine's actions go out over REST. Mapping lives in
//! `map` as pure functions; `state` holds the guild cache and decides which
//! events to emit; `actions` does the REST calls.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod actions;
mod invite;
pub mod map;
mod poll;
mod split;
mod state;
#[cfg(test)]
mod testutil;

pub use invite::{INVITE_PERMISSIONS, invite_url};

use actions::Actions;
use async_trait::async_trait;
use ours_connector::Connector;
use ours_types::{Capabilities, InboundEvent, Instant, Outbound, Platform};
use state::{Cache, DmRooms, Step};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;
use twilight_gateway::error::ReceiveMessageErrorType;
use twilight_gateway::{
    CloseFrame, Event, EventTypeFlags, Intents, MessageSender, Shard, ShardId, StreamExt as _,
};
use twilight_http::Client;
use twilight_model::channel::Message as TwMessage;
use twilight_model::gateway::payload::incoming::rate_limited::RateLimitMetadata;
use twilight_model::gateway::payload::outgoing::RequestGuildMembers;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker, UserMarker};

/// What the gateway session asks Discord for. Server Members and Message
/// Content are privileged and must be switched on in the developer portal.
pub const INTENTS: Intents = Intents::GUILDS
    .union(Intents::GUILD_MEMBERS)
    .union(Intents::GUILD_MESSAGES)
    .union(Intents::MESSAGE_CONTENT)
    .union(Intents::GUILD_MESSAGE_REACTIONS)
    .union(Intents::DIRECT_MESSAGES)
    .union(Intents::DIRECT_MESSAGE_REACTIONS)
    .union(Intents::GUILD_MESSAGE_POLLS)
    .union(Intents::DIRECT_MESSAGE_POLLS);

/// What this connector can do. `capabilities()` and the `Ready` event both
/// report it.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        reactions: true,
        threads: true,
        polls: true,
        dms: true,
        edit: true,
        delete: true,
        pin: true,
        history: true,
        timezones: false,
        external_flag: false,
        typing: true,
        reaction_set: None,
    }
}

/// The gateway events the connector reads; the rest are not even parsed.
const WANTED: EventTypeFlags = EventTypeFlags::READY
    .union(EventTypeFlags::GUILD_CREATE)
    .union(EventTypeFlags::GUILD_DELETE)
    .union(EventTypeFlags::GUILD_UPDATE)
    .union(EventTypeFlags::CHANNEL_CREATE)
    .union(EventTypeFlags::CHANNEL_UPDATE)
    .union(EventTypeFlags::CHANNEL_DELETE)
    .union(EventTypeFlags::THREAD_CREATE)
    .union(EventTypeFlags::THREAD_UPDATE)
    .union(EventTypeFlags::THREAD_DELETE)
    .union(EventTypeFlags::THREAD_LIST_SYNC)
    .union(EventTypeFlags::THREAD_MEMBERS_UPDATE)
    .union(EventTypeFlags::ROLE_CREATE)
    .union(EventTypeFlags::ROLE_UPDATE)
    .union(EventTypeFlags::ROLE_DELETE)
    .union(EventTypeFlags::MEMBER_ADD)
    .union(EventTypeFlags::MEMBER_REMOVE)
    .union(EventTypeFlags::MEMBER_UPDATE)
    .union(EventTypeFlags::MEMBER_CHUNK)
    .union(EventTypeFlags::MESSAGE_CREATE)
    .union(EventTypeFlags::MESSAGE_UPDATE)
    .union(EventTypeFlags::MESSAGE_DELETE)
    .union(EventTypeFlags::MESSAGE_DELETE_BULK)
    .union(EventTypeFlags::REACTION_ADD)
    .union(EventTypeFlags::REACTION_REMOVE)
    .union(EventTypeFlags::MESSAGE_POLL_VOTE_ADD)
    .union(EventTypeFlags::MESSAGE_POLL_VOTE_REMOVE)
    .union(EventTypeFlags::RATE_LIMITED);

/// How long to wait for the gateway to acknowledge a close we asked for.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct DiscordConfig {
    pub token: String,
    /// The name the agent answers to at the start of a message.
    pub agent_name: String,
    /// Guild ids to serve; `None` serves every guild the bot is in.
    pub guilds: Option<Vec<String>>,
}

impl std::fmt::Debug for DiscordConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordConfig")
            .field("token", &"<redacted>")
            .field("agent_name", &self.agent_name)
            .field("guilds", &self.guilds)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("the Discord token is empty")]
    EmptyToken,
    #[error("`{0}` in the guild allowlist is not a Discord guild id")]
    BadGuildId(String),
}

#[derive(Debug)]
pub struct DiscordConnector {
    config: DiscordConfig,
    allowed: Option<HashSet<Id<GuildMarker>>>,
}

impl DiscordConnector {
    pub fn new(config: DiscordConfig) -> Result<Self, ConfigError> {
        if config.token.trim().is_empty() {
            return Err(ConfigError::EmptyToken);
        }
        let allowed = config
            .guilds
            .as_ref()
            .map(|ids| {
                ids.iter()
                    .map(|id| map::parse_id(id).map_err(|_| ConfigError::BadGuildId(id.clone())))
                    .collect::<Result<HashSet<_>, _>>()
            })
            .transpose()?;
        Ok(DiscordConnector { config, allowed })
    }
}

/// Work that finished off the event loop and needs the cache again.
#[derive(Debug)]
enum Followup {
    EditedMessage(Box<TwMessage>),
    ThreadMembers {
        thread: Id<ChannelMarker>,
        users: Vec<Id<UserMarker>>,
    },
}

#[async_trait]
impl Connector for DiscordConnector {
    fn platform(&self) -> Platform {
        Platform::Discord
    }

    fn capabilities(&self) -> Capabilities {
        capabilities()
    }

    async fn run(
        self: Box<Self>,
        inbound: mpsc::Sender<InboundEvent>,
        outbound: mpsc::Receiver<Outbound>,
    ) -> anyhow::Result<()> {
        install_crypto_provider();
        let DiscordConnector { config, allowed } = *self;

        let http = Arc::new(Client::new(config.token.clone()));
        let mut shard = Shard::new(ShardId::ONE, config.token.clone(), INTENTS);
        let dms = DmRooms::default();
        let me = Arc::new(OnceLock::new());
        let mut cache = Cache::new(config.agent_name.clone(), allowed, dms.clone());
        let (follow_tx, mut follow_rx) = mpsc::unbounded_channel();

        // The gateway loop must keep polling the shard, so events wait in an
        // unbounded queue instead of blocking on a slow engine.
        let (queue, queued) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward(queued, inbound.clone()));
        let actions = Actions::new(
            Arc::clone(&http),
            config.agent_name.clone(),
            Arc::clone(&me),
            dms,
        );
        let mut worker = tokio::spawn(run_actions(actions, outbound, queue.clone()));
        let io = Io {
            http,
            sender: shard.sender(),
            queue,
            follow: follow_tx,
        };

        let mut last_close: Option<CloseFrame<'static>> = None;
        loop {
            let (steps, span) = tokio::select! {
                item = shard.next_event(WANTED) => match item {
                    Some(Ok(Event::GatewayClose(frame))) => {
                        tracing::warn!(?frame, "gateway connection closed");
                        last_close = frame;
                        continue;
                    }
                    Some(Ok(event)) => {
                        let span = tracing::info_span!(
                            "discord_event",
                            kind = event.kind().name().unwrap_or("unknown")
                        );
                        let steps = span.in_scope(|| cache.handle(&event, now()));
                        if let Some(id) = cache.me() {
                            let _ = me.set(id);
                        }
                        io.on_event(&event);
                        (steps, span)
                    }
                    Some(Err(error)) => {
                        let Some(update) = io.on_receive_error(&error) else {
                            continue;
                        };
                        let span = tracing::info_span!("discord_event", kind = "MESSAGE_UPDATE");
                        let steps = span.in_scope(|| cache.on_partial_update(update));
                        (steps, span)
                    }
                    None => {
                        let (code, text) = last_close
                            .as_ref()
                            .map_or((None, String::new()), |f| (Some(f.code), f.reason.to_string()));
                        worker.abort();
                        let _ = worker.await;
                        drop(io);
                        return give_up(map::close_reason(code, &text), forwarder, &inbound).await;
                    }
                },
                Some(followup) = follow_rx.recv() => {
                    let span = tracing::info_span!("discord_followup");
                    let steps = span.in_scope(|| match followup {
                        Followup::EditedMessage(message) => cache.handle_fetched_edit(&message, now()),
                        Followup::ThreadMembers { thread, users } => cache.apply_thread_members(thread, users),
                    });
                    (steps, span)
                },
                done = &mut worker => {
                    if let Err(error) = done {
                        tracing::error!(%error, "action worker stopped");
                    }
                    shard.close(CloseFrame::NORMAL);
                    drain_until_closed(&mut shard).await;
                    return Ok(());
                }
            };
            if !span.in_scope(|| io.run(steps)) {
                return Ok(());
            }
        }
    }
}

/// The network side of the cache's `Step`s, and the follow-up work they start.
struct Io {
    http: Arc<Client>,
    sender: MessageSender,
    queue: mpsc::UnboundedSender<InboundEvent>,
    follow: mpsc::UnboundedSender<Followup>,
}

impl Io {
    /// Carry out the steps in order. False once the engine has stopped listening.
    fn run(&self, steps: Vec<Step>) -> bool {
        for step in steps {
            match step {
                Step::Emit(event) => {
                    if self.queue.send(event).is_err() {
                        return false;
                    }
                }
                Step::RequestMembers(guild) => self.request_members(guild),
                Step::FetchThreadMembers(thread) => self.fetch_thread_members(thread),
                Step::FetchMessage(update) => self.fetch_message(update),
            }
        }
        true
    }

    fn request_members(&self, guild: Id<GuildMarker>) {
        let request = RequestGuildMembers::builder(guild)
            .nonce(guild.to_string())
            .query("", None);
        if let Err(error) = self.sender.command(&request) {
            tracing::warn!(%guild, %error, "could not ask for guild members");
        }
    }

    /// Ask again after the delay when the gateway rate limited a member request.
    fn on_event(&self, event: &Event) {
        let Event::RateLimited(limited) = event else {
            return;
        };
        if let RateLimitMetadata::RequestGuildMembers { guild_id, .. } = &limited.meta {
            let (guild, sender) = (*guild_id, self.sender.clone());
            let delay = Duration::from_secs_f32(limited.retry_after.max(0.0));
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let request = RequestGuildMembers::builder(guild)
                    .nonce(guild.to_string())
                    .query("", None);
                if let Err(error) = sender.command(&request) {
                    tracing::warn!(%guild, %error, "could not repeat the member request");
                }
            });
        }
    }

    /// A gateway message that could not be read. A content edit that arrived
    /// in pieces comes back for the cache to decide on; everything else is
    /// skipped, with a warning unless it is a message update.
    fn on_receive_error(
        &self,
        error: &twilight_gateway::error::ReceiveMessageError,
    ) -> Option<map::PartialUpdate> {
        let ReceiveMessageErrorType::Deserializing { event } = error.kind() else {
            tracing::warn!(%error, "gateway receive error");
            return None;
        };
        if let Some(update) = map::partial_update_target(event) {
            return Some(update);
        }
        match map::event_name(event) {
            Some(name) if name == "MESSAGE_UPDATE" => {
                tracing::debug!("skipped a message update with no new content");
            }
            name => tracing::warn!(
                event = name.as_deref().unwrap_or("unknown"),
                "skipped a gateway event that could not be read"
            ),
        }
        None
    }

    /// Fetch the whole message, carrying over the guild the update named.
    fn fetch_message(&self, update: map::PartialUpdate) {
        let (http, follow) = (Arc::clone(&self.http), self.follow.clone());
        let (channel, message) = (update.channel, update.message);
        tokio::spawn(async move {
            let fetched = match http.message(channel, message).await {
                Ok(response) => response.model().await.map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            };
            match fetched {
                Ok(mut message) => {
                    message.guild_id = message.guild_id.or(update.guild);
                    let _ = follow.send(Followup::EditedMessage(Box::new(message)));
                }
                Err(error) => {
                    tracing::warn!(%channel, %message, %error, "could not fetch edited message")
                }
            }
        });
    }

    fn fetch_thread_members(&self, thread: Id<ChannelMarker>) {
        let (http, follow) = (Arc::clone(&self.http), self.follow.clone());
        tokio::spawn(async move {
            let fetched = match http.thread_members(thread).await {
                Ok(response) => response.models().await.map_err(|e| e.to_string()),
                Err(error) => Err(error.to_string()),
            };
            match fetched {
                Ok(members) => {
                    let users = members.iter().filter_map(|m| m.user_id).collect();
                    let _ = follow.send(Followup::ThreadMembers { thread, users });
                }
                Err(error) => tracing::warn!(%thread, %error, "could not read thread members"),
            }
        });
    }
}

/// Run engine actions one at a time, in order, off the gateway loop so a
/// rate-limited send never stalls heartbeats.
async fn run_actions(
    mut actions: Actions,
    mut outbound: mpsc::Receiver<Outbound>,
    queue: mpsc::UnboundedSender<InboundEvent>,
) {
    while let Some(Outbound { id, action }) = outbound.recv().await {
        let span = tracing::info_span!("discord_action", action = ?action_name(&action));
        let outcome = actions.perform(action).instrument(span.clone()).await;
        let result = match outcome {
            Ok(outcome) => {
                for event in outcome.events {
                    if queue.send(event).is_err() {
                        return;
                    }
                }
                InboundEvent::ActionResult {
                    id,
                    ok: true,
                    message: outcome.message,
                    error: None,
                }
            }
            Err(error) => {
                span.in_scope(|| tracing::warn!(%error, "action failed"));
                InboundEvent::ActionResult {
                    id,
                    ok: false,
                    message: None,
                    error: Some(error.to_string()),
                }
            }
        };
        if queue.send(result).is_err() {
            return;
        }
    }
}

/// Tell the engine the connector is down for good, after everything queued
/// has reached it. Every other sender of the queue must be gone.
async fn give_up(
    reason: String,
    forwarder: JoinHandle<()>,
    inbound: &mpsc::Sender<InboundEvent>,
) -> anyhow::Result<()> {
    tracing::error!(%reason, "discord gateway is down");
    let _ = forwarder.await;
    let _ = inbound
        .send(InboundEvent::Disconnected {
            platform: Platform::Discord,
            reason: reason.clone(),
        })
        .await;
    anyhow::bail!(reason)
}

/// Pass queued events on to the engine, in order, until either side closes.
async fn forward(
    mut queued: mpsc::UnboundedReceiver<InboundEvent>,
    inbound: mpsc::Sender<InboundEvent>,
) {
    while let Some(event) = queued.recv().await {
        if inbound.send(event).await.is_err() {
            return;
        }
    }
}

fn action_name(action: &ours_types::OutboundAction) -> &'static str {
    use ours_types::OutboundAction as A;
    match action {
        A::SendMessage { .. } => "send_message",
        A::EditMessage { .. } => "edit_message",
        A::DeleteMessage { .. } => "delete_message",
        A::React { .. } => "react",
        A::Unreact { .. } => "unreact",
        A::SendDm { .. } => "send_dm",
        A::Pin { .. } => "pin",
        A::FetchHistory { .. } => "fetch_history",
        A::Typing { .. } => "typing",
        A::CreatePoll { .. } => "create_poll",
        A::EndPoll { .. } => "end_poll",
    }
}

/// Read the shard until it reports the close we asked for, within a short wait.
async fn drain_until_closed(shard: &mut Shard) {
    let drain = async {
        while let Some(item) = shard.next_event(EventTypeFlags::empty()).await {
            if matches!(item, Ok(Event::GatewayClose(_))) {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(CLOSE_WAIT, drain).await;
}

/// Twilight enables no TLS provider of its own. Install one unless the host
/// process already has.
fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}

/// The wall clock as an `Instant`, for events Discord does not timestamp.
pub(crate) fn now() -> Instant {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Instant::from_millis(ms).unwrap_or(Instant::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DiscordConfig {
        DiscordConfig {
            token: "secret-token".to_owned(),
            agent_name: "Q".to_owned(),
            guilds: None,
        }
    }

    #[test]
    fn capabilities_match_what_discord_gives_us() {
        let connector = DiscordConnector::new(config()).unwrap();
        let caps = connector.capabilities();
        assert_eq!(connector.platform(), Platform::Discord);
        assert!(
            caps.reactions
                && caps.threads
                && caps.dms
                && caps.edit
                && caps.delete
                && caps.pin
                && caps.history
                && caps.polls
        );
        assert!(!caps.timezones && !caps.external_flag);
    }

    #[test]
    fn the_guild_allowlist_must_be_ids() {
        let ok = DiscordConnector::new(DiscordConfig {
            guilds: Some(vec!["123".to_owned()]),
            ..config()
        });
        assert!(ok.is_ok());
        let bad = DiscordConnector::new(DiscordConfig {
            guilds: Some(vec!["my-server".to_owned()]),
            ..config()
        });
        assert_eq!(
            bad.unwrap_err(),
            ConfigError::BadGuildId("my-server".to_owned())
        );
        let empty = DiscordConnector::new(DiscordConfig {
            token: "  ".to_owned(),
            ..config()
        });
        assert_eq!(empty.unwrap_err(), ConfigError::EmptyToken);
    }

    #[test]
    fn the_token_never_prints() {
        assert!(!format!("{:?}", config()).contains("secret-token"));
        assert!(
            !format!("{:?}", DiscordConnector::new(config()).unwrap()).contains("secret-token")
        );
    }

    #[tokio::test]
    async fn a_bad_id_comes_back_as_a_failed_result() {
        let actions = Actions::new(
            Arc::new(Client::new("token".to_owned())),
            "Q".to_owned(),
            Arc::new(OnceLock::new()),
            DmRooms::default(),
        );
        let (queue, mut results) = mpsc::unbounded_channel();
        let (send, outbound) = mpsc::channel(4);
        let worker = tokio::spawn(run_actions(actions, outbound, queue));

        let action = Outbound::new(ours_types::OutboundAction::SendMessage {
            room: "general".into(),
            text: "hello".to_owned(),
            reply_to: None,
        });
        let id = action.id.clone();
        send.send(action).await.unwrap();
        drop(send);
        worker.await.unwrap();

        let result = results.recv().await.unwrap();
        let InboundEvent::ActionResult {
            id: got,
            ok,
            message,
            error,
        } = result
        else {
            panic!("{result:?}")
        };
        assert_eq!(got, id);
        assert!(!ok);
        assert_eq!(message, None);
        assert!(error.unwrap().contains("not a Discord id"));
        assert!(results.recv().await.is_none());
    }

    #[tokio::test]
    async fn queued_events_reach_the_engine_in_order() {
        let (queue, queued) = mpsc::unbounded_channel();
        let (inbound, mut engine) = mpsc::channel(1);
        let forwarder = tokio::spawn(forward(queued, inbound));
        for n in 0..5 {
            let room = ours_types::PlatformRoomId::new(n.to_string());
            queue
                .send(InboundEvent::MessageDeleted {
                    room,
                    id: "m".into(),
                })
                .unwrap();
        }
        drop(queue);
        let mut seen = Vec::new();
        while let Some(InboundEvent::MessageDeleted { room, .. }) = engine.recv().await {
            seen.push(room.to_string());
        }
        forwarder.await.unwrap();
        assert_eq!(seen, ["0", "1", "2", "3", "4"]);
    }

    #[tokio::test]
    async fn giving_up_waits_for_a_stalled_engine_then_says_why_last() {
        let (queue, queued) = mpsc::unbounded_channel();
        let (inbound, mut engine) = mpsc::channel(1);
        let forwarder = tokio::spawn(forward(queued, inbound.clone()));
        for n in 0..3 {
            queue
                .send(InboundEvent::MessageDeleted {
                    room: ours_types::PlatformRoomId::new(n.to_string()),
                    id: "m".into(),
                })
                .unwrap();
        }
        drop(queue);
        let stopping =
            tokio::spawn(async move { give_up("gone".to_owned(), forwarder, &inbound).await });

        // The engine reads nothing yet, so the queue cannot drain and nothing can finish.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(!stopping.is_finished());

        let mut seen = Vec::new();
        while let Some(event) = engine.recv().await {
            seen.push(event);
        }
        let error = stopping.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "gone");
        let rooms: Vec<_> = seen[..3]
            .iter()
            .map(|e| match e {
                InboundEvent::MessageDeleted { room, .. } => room.to_string(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(rooms, ["0", "1", "2"]);
        assert_eq!(
            seen[3..],
            [InboundEvent::Disconnected {
                platform: Platform::Discord,
                reason: "gone".to_owned()
            }]
        );
    }

    #[test]
    fn intents_cover_the_brief() {
        for intent in [
            Intents::GUILDS,
            Intents::GUILD_MEMBERS,
            Intents::GUILD_MESSAGES,
            Intents::MESSAGE_CONTENT,
            Intents::GUILD_MESSAGE_REACTIONS,
            Intents::DIRECT_MESSAGES,
            Intents::DIRECT_MESSAGE_REACTIONS,
            Intents::GUILD_MESSAGE_POLLS,
            Intents::DIRECT_MESSAGE_POLLS,
        ] {
            assert!(INTENTS.contains(intent));
        }
        assert!(!INTENTS.contains(Intents::GUILD_PRESENCES));
    }

    #[tokio::test]
    async fn an_eleven_option_poll_comes_back_as_a_failed_result() {
        let actions = Actions::new(
            Arc::new(Client::new("token".to_owned())),
            "Q".to_owned(),
            Arc::new(OnceLock::new()),
            DmRooms::default(),
        );
        let (queue, mut results) = mpsc::unbounded_channel();
        let (send, outbound) = mpsc::channel(4);
        let worker = tokio::spawn(run_actions(actions, outbound, queue));

        let action = Outbound::new(ours_types::OutboundAction::CreatePoll {
            room: "20".into(),
            question: "when?".to_owned(),
            options: (1..=11).map(|n| n.to_string()).collect(),
            multiple: false,
            hours: 24,
        });
        let id = action.id.clone();
        send.send(action).await.unwrap();
        drop(send);
        worker.await.unwrap();

        let result = results.recv().await.unwrap();
        let InboundEvent::ActionResult {
            id: got,
            ok,
            message,
            error,
        } = result
        else {
            panic!("{result:?}")
        };
        assert_eq!(got, id);
        assert!(!ok);
        assert_eq!(message, None);
        assert!(error.unwrap().contains("at most 10 options"));
    }
}
