//! Slack for Ours, through Socket Mode so a host needs no public URL.
//! Socket Mode events feed the engine's event model and the engine's
//! actions go out over the Web API. Mapping lives in `map`, `text` and
//! `emoji` as pure functions; `state` holds the workspace cache and decides
//! which events to emit; `actions` does the Web API calls.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod actions;
mod api;
pub mod emoji;
mod event;
pub mod map;
mod socket;
mod state;
#[cfg(test)]
mod testutil;
pub mod text;
mod tls;

use actions::{Actions, lock};
use api::Api;
use async_trait::async_trait;
use event::Event;
use ours_connector::Connector;
use ours_types::{Capabilities, InboundEvent, Instant, Outbound, OutboundAction, Platform};
use slack_morphism::api::SlackApiAuthTestResponse;
use slack_morphism::{SlackChannelId, SlackChannelInfo, SlackTs, SlackUserId};
use socket::SocketMessage;
use state::{State, Want};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;

/// What this connector can do. `capabilities()` and the `Ready` event both
/// report it.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        reactions: true,
        threads: true,
        polls: false,
        dms: true,
        edit: true,
        delete: true,
        pin: true,
        history: true,
        timezones: true,
        external_flag: true,
        typing: false,
        reaction_set: None,
    }
}

/// The Slack app manifest a host pastes into "Create app from manifest".
pub const MANIFEST: &str = include_str!("../../../manifests/slack.yaml");

#[derive(Clone)]
pub struct SlackConfig {
    /// The bot token, `xoxb-...`.
    pub bot_token: String,
    /// The app-level token, `xapp-...`, which opens Socket Mode.
    pub app_token: String,
    /// The name the agent answers to at the start of a message.
    pub agent_name: String,
    /// A user group handle, or its id, whose members count as admins for the
    /// leads rule.
    pub leads_usergroup: Option<String>,
}

impl std::fmt::Debug for SlackConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackConfig")
            .field("bot_token", &"<redacted>")
            .field("app_token", &"<redacted>")
            .field("agent_name", &self.agent_name)
            .field("leads_usergroup", &self.leads_usergroup)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("the Slack bot token is empty")]
    EmptyBotToken,
    #[error("the Slack bot token should start with xoxb-")]
    NotABotToken,
    #[error("the Slack app-level token is empty")]
    EmptyAppToken,
    #[error("the Slack app-level token should start with xapp-")]
    NotAnAppToken,
}

#[derive(Debug)]
pub struct SlackConnector {
    config: SlackConfig,
}

impl SlackConnector {
    pub fn new(mut config: SlackConfig) -> Result<Self, ConfigError> {
        config.bot_token = config.bot_token.trim().to_owned();
        config.app_token = config.app_token.trim().to_owned();
        if config.bot_token.is_empty() {
            return Err(ConfigError::EmptyBotToken);
        }
        if !config.bot_token.starts_with("xoxb-") {
            return Err(ConfigError::NotABotToken);
        }
        if config.app_token.is_empty() {
            return Err(ConfigError::EmptyAppToken);
        }
        if !config.app_token.starts_with("xapp-") {
            return Err(ConfigError::NotAnAppToken);
        }
        config.leads_usergroup = config
            .leads_usergroup
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty());
        Ok(SlackConnector { config })
    }
}

#[async_trait]
impl Connector for SlackConnector {
    fn platform(&self) -> Platform {
        Platform::Slack
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
        let SlackConnector { config } = *self;
        let tls = tls::client_config()?;
        let api = Arc::new(Api::new(tls.clone(), &config.bot_token, &config.app_token));

        // Events wait in an unbounded queue so a slow engine never stalls the socket.
        let (queue, queued) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward(queued, inbound.clone()));

        let auth = match authenticate(&api, &outbound).await {
            Login::Ready(auth) => auth,
            Login::Stopped => return Ok(()),
            Login::Refused(reason) => {
                drop(queue);
                return give_up(reason, forwarder, &inbound).await;
            }
        };
        let state = Arc::new(Mutex::new(State::new(
            config.agent_name.clone(),
            auth.user_id.clone(),
            auth.bot_id.clone(),
            auth.team_id.clone(),
        )));
        let io = Io {
            api: Arc::clone(&api),
            state: Arc::clone(&state),
            queue: queue.clone(),
        };
        io.emit(vec![InboundEvent::Ready {
            platform: Platform::Slack,
            me: map::user_id(&auth.user_id),
            me_name: auth
                .user
                .clone()
                .unwrap_or_else(|| config.agent_name.clone()),
            capabilities: capabilities(),
        }]);

        // The socket connects and actions run while the rooms sync; socket
        // events queue until the sync is done.
        let (socket_tx, mut socket_rx) = mpsc::unbounded_channel();
        let socket = tokio::spawn(socket::run(Arc::clone(&api), Arc::new(tls), socket_tx));
        let mut worker = tokio::spawn(run_actions(Actions::new(api, state), outbound, queue));
        let startup = async {
            io.load_users().await;
            if let Some(group) = &config.leads_usergroup {
                io.resolve_leads(group).await;
            }
            io.sync_rooms().await
        };
        if !startup.instrument(tracing::info_span!("slack_sync")).await {
            socket.abort();
            worker.abort();
            return Ok(());
        }

        let reason = loop {
            tokio::select! {
                message = socket_rx.recv() => match message {
                    Some(SocketMessage::Event(event)) if matches!(*event, Event::Uninstalled) => {
                        break "the Slack app was uninstalled or its tokens revoked; reinstall it and \
                               update both tokens in the config"
                            .to_owned();
                    }
                    Some(SocketMessage::Event(event)) => {
                        let span = tracing::info_span!("slack_event", kind = event.kind());
                        if !io.process(*event).instrument(span).await {
                            socket.abort();
                            worker.abort();
                            return Ok(());
                        }
                    }
                    Some(SocketMessage::Connected { resync }) => {
                        if resync && !io.sync_rooms().instrument(tracing::info_span!("slack_resync")).await {
                            socket.abort();
                            worker.abort();
                            return Ok(());
                        }
                    }
                    Some(SocketMessage::Fatal(reason)) => break reason,
                    None => break "the Socket Mode connection stopped".to_owned(),
                },
                done = &mut worker => {
                    if let Err(error) = done {
                        tracing::error!(%error, "action worker stopped");
                    }
                    socket.abort();
                    return Ok(());
                }
            }
        };
        socket.abort();
        worker.abort();
        let _ = worker.await;
        drop(io);
        give_up(reason, forwarder, &inbound).await
    }
}

enum Login {
    Ready(Box<SlackApiAuthTestResponse>),
    Refused(String),
    /// The engine went away while Slack was unreachable.
    Stopped,
}

/// Learn who the bot is, retrying with backoff until Slack answers or
/// refuses the token.
async fn authenticate(api: &Api, outbound: &mpsc::Receiver<Outbound>) -> Login {
    let mut failures = 0u32;
    loop {
        match api.auth_test().await {
            Ok(auth) => return Login::Ready(Box::new(auth)),
            Err(error) => {
                if let Some(reason) = socket::bot_token_failure(&error) {
                    return Login::Refused(reason);
                }
                tracing::warn!(%error, "auth.test failed; retrying");
            }
        }
        if outbound.is_closed() {
            return Login::Stopped;
        }
        tokio::time::sleep(socket::backoff(failures)).await;
        failures = failures.saturating_add(1);
    }
}

/// Tell the engine the connector is down for good, after everything queued
/// has reached it. Every other sender of the queue must be gone.
async fn give_up(
    reason: String,
    forwarder: JoinHandle<()>,
    inbound: &mpsc::Sender<InboundEvent>,
) -> anyhow::Result<()> {
    tracing::error!(%reason, "slack connector is down");
    let _ = forwarder.await;
    let _ = inbound
        .send(InboundEvent::Disconnected {
            platform: Platform::Slack,
            reason: reason.clone(),
        })
        .await;
    anyhow::bail!(reason)
}

/// The Web API side of the state: the fetches `missing` asks for and the
/// syncs that fill the cache. Every method returns false once the engine has
/// stopped listening.
struct Io {
    api: Arc<Api>,
    state: Arc<Mutex<State>>,
    queue: mpsc::UnboundedSender<InboundEvent>,
}

impl Io {
    fn emit(&self, events: Vec<InboundEvent>) -> bool {
        events
            .into_iter()
            .all(|event| self.queue.send(event).is_ok())
    }

    async fn load_users(&self) {
        match self.api.users().await {
            Ok(users) => {
                tracing::info!(count = users.len(), "slack users loaded");
                lock(&self.state).load_users(&users);
            }
            Err(error) => {
                tracing::warn!(%error, "could not list users; each is looked up when seen")
            }
        }
    }

    /// Find the leads user group and its members. A missing group is logged,
    /// not fatal: the leads rule still has admins and owners.
    async fn resolve_leads(&self, wanted: &str) -> bool {
        let group = match self.api.usergroup(wanted).await {
            Ok(Some(group)) => group,
            Ok(None) => {
                tracing::error!(usergroup = wanted, "the leads user group was not found");
                return true;
            }
            Err(error) => {
                tracing::error!(usergroup = wanted, %error, "could not read user groups");
                return true;
            }
        };
        match self.api.usergroup_users(&group).await {
            Ok(users) => {
                tracing::info!(
                    usergroup = wanted,
                    count = users.len(),
                    "leads user group loaded"
                );
                let events = lock(&self.state).set_leads(group, users);
                self.emit(events)
            }
            Err(error) => {
                tracing::error!(usergroup = wanted, %error, "could not read the leads user group");
                true
            }
        }
    }

    async fn fetch_users(&self, ids: Vec<SlackUserId>) -> bool {
        for id in ids {
            match self.api.user(&id).await {
                Ok(user) => {
                    let events = lock(&self.state).apply_user(&user);
                    if !self.emit(events) {
                        return false;
                    }
                }
                Err(error) => tracing::warn!(user = %id.0, %error, "could not look up a user"),
            }
        }
        true
    }

    /// Read one channel and record it, or leave it when the agent is no longer in it.
    async fn fetch_channel(&self, id: &SlackChannelId) -> bool {
        let info = match self.api.conversation(id).await {
            Ok(info) => info,
            Err(error) => {
                tracing::warn!(channel = %id.0, %error, "could not read a channel");
                return true;
            }
        };
        if info.flags.is_im == Some(true) {
            return true;
        }
        if info.flags.is_archived == Some(true) || info.flags.is_member == Some(false) {
            let events = lock(&self.state).drop_channel(id, now());
            return self.emit(events);
        }
        self.record_channel(&info).await
    }

    /// Learn which thread a message is in. When the lookup fails, a reaction
    /// to it lands in the channel.
    async fn fetch_thread(&self, channel: &SlackChannelId, ts: &SlackTs) {
        match self.api.thread_ts(channel, ts).await {
            Ok(thread) => lock(&self.state).record_message(channel, ts, thread.as_ref()),
            Err(error) => tracing::warn!(
                channel = %channel.0,
                ts = %ts.0,
                %error,
                "could not find a message's thread"
            ),
        }
    }

    async fn record_channel(&self, info: &SlackChannelInfo) -> bool {
        let members = match self.api.members(&info.id).await {
            Ok(members) => members,
            Err(error) => {
                tracing::warn!(channel = %info.id.0, %error, "could not read channel members");
                return true;
            }
        };
        let unknown = lock(&self.state).unknown_users(&members);
        if !self.fetch_users(unknown).await {
            return false;
        }
        let events = lock(&self.state).apply_channel(info, &members);
        self.emit(events)
    }

    /// Sync every conversation the agent is in and leave the ones it no
    /// longer is.
    async fn sync_rooms(&self) -> bool {
        let channels = match self.api.conversations().await {
            Ok(channels) => channels,
            Err(error) => {
                tracing::warn!(%error, "could not list the agent's channels");
                return true;
            }
        };
        let mut seen = HashSet::new();
        for info in channels.iter().filter(|c| {
            c.flags.is_member != Some(false)
                && c.flags.is_archived != Some(true)
                && c.flags.is_im != Some(true)
        }) {
            seen.insert(info.id.clone());
            if !self.record_channel(info).await {
                return false;
            }
        }
        tracing::info!(channels = seen.len(), "slack rooms synced");
        let events = lock(&self.state).retain_channels(&seen, now());
        self.emit(events)
    }

    /// Fetch what the event needs, then hand it to the state.
    async fn process(&self, event: Event) -> bool {
        let wants = lock(&self.state).missing(&event);
        for want in wants {
            let alive = match want {
                Want::User(id) => self.fetch_users(vec![id]).await,
                Want::Channel(id) => self.fetch_channel(&id).await,
                Want::Thread(channel, ts) => {
                    self.fetch_thread(&channel, &ts).await;
                    true
                }
            };
            if !alive {
                return false;
            }
        }
        let events = lock(&self.state).handle(&event, now());
        self.emit(events)
    }
}

/// Run engine actions one at a time, in order, off the socket loop so a
/// rate-limited call never delays an acknowledgement.
async fn run_actions(
    actions: Actions,
    mut outbound: mpsc::Receiver<Outbound>,
    queue: mpsc::UnboundedSender<InboundEvent>,
) {
    while let Some(Outbound { id, action }) = outbound.recv().await {
        let span = tracing::info_span!("slack_action", action = action_name(&action));
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

fn action_name(action: &OutboundAction) -> &'static str {
    match action {
        OutboundAction::SendMessage { .. } => "send_message",
        OutboundAction::EditMessage { .. } => "edit_message",
        OutboundAction::DeleteMessage { .. } => "delete_message",
        OutboundAction::React { .. } => "react",
        OutboundAction::Unreact { .. } => "unreact",
        OutboundAction::SendDm { .. } => "send_dm",
        OutboundAction::Pin { .. } => "pin",
        OutboundAction::FetchHistory { .. } => "fetch_history",
        OutboundAction::Typing { .. } => "typing",
        OutboundAction::CreatePoll { .. } => "create_poll",
        OutboundAction::EndPoll { .. } => "end_poll",
    }
}

/// Install aws-lc-rs as the process's rustls provider unless the host
/// process already has one.
fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}

/// The wall clock as an `Instant`, for events Slack does not timestamp.
pub(crate) fn now() -> Instant {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Instant::from_millis(ms).unwrap_or(Instant::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slack_morphism::SlackTeamId;

    fn config() -> SlackConfig {
        SlackConfig {
            bot_token: "xoxb-secret-bot".to_owned(),
            app_token: "xapp-secret-app".to_owned(),
            agent_name: "Q".to_owned(),
            leads_usergroup: Some(" @leads ".to_owned()),
        }
    }

    #[test]
    fn capabilities_match_what_slack_gives_us() {
        let connector = SlackConnector::new(config()).unwrap();
        assert_eq!(connector.platform(), Platform::Slack);
        assert_eq!(
            connector.capabilities(),
            Capabilities {
                reactions: true,
                threads: true,
                polls: false,
                dms: true,
                edit: true,
                delete: true,
                pin: true,
                history: true,
                timezones: true,
                external_flag: true,
                typing: false,
                reaction_set: None,
            }
        );
    }

    #[test]
    fn both_tokens_are_needed_and_of_the_right_kind() {
        let check = |bot: &str, app: &str| {
            SlackConnector::new(SlackConfig {
                bot_token: bot.to_owned(),
                app_token: app.to_owned(),
                ..config()
            })
            .map(|_| ())
        };
        assert_eq!(check("xoxb-1", "xapp-1"), Ok(()));
        assert_eq!(check(" ", "xapp-1"), Err(ConfigError::EmptyBotToken));
        assert_eq!(check("xoxb-1", ""), Err(ConfigError::EmptyAppToken));
        assert_eq!(check("xapp-1", "xoxb-1"), Err(ConfigError::NotABotToken));
        assert_eq!(check("xoxb-1", "xoxb-1"), Err(ConfigError::NotAnAppToken));
        assert_eq!(check("xoxp-1", "xapp-1"), Err(ConfigError::NotABotToken));
    }

    #[test]
    fn the_leads_group_is_trimmed_and_empty_means_none() {
        let connector = SlackConnector::new(config()).unwrap();
        assert_eq!(connector.config.leads_usergroup.as_deref(), Some("@leads"));
        let none = SlackConnector::new(SlackConfig {
            leads_usergroup: Some("  ".to_owned()),
            ..config()
        })
        .unwrap();
        assert_eq!(none.config.leads_usergroup, None);
    }

    #[test]
    fn the_tokens_never_print() {
        for printed in [
            format!("{:?}", config()),
            format!("{:?}", SlackConnector::new(config()).unwrap()),
        ] {
            assert!(!printed.contains("secret"), "{printed}");
        }
    }

    #[derive(serde::Deserialize)]
    struct Manifest {
        features: Features,
        oauth_config: OAuth,
        settings: Settings,
    }

    #[derive(serde::Deserialize)]
    struct Features {
        bot_user: BotUser,
        app_home: AppHome,
    }

    #[derive(serde::Deserialize)]
    struct BotUser {
        display_name: String,
    }

    #[derive(serde::Deserialize)]
    struct AppHome {
        messages_tab_enabled: bool,
        messages_tab_read_only_enabled: bool,
    }

    #[derive(serde::Deserialize)]
    struct OAuth {
        scopes: Scopes,
    }

    #[derive(serde::Deserialize)]
    struct Scopes {
        bot: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    struct Settings {
        socket_mode_enabled: bool,
        event_subscriptions: Events,
        interactivity: Interactivity,
    }

    #[derive(serde::Deserialize)]
    struct Events {
        bot_events: Vec<String>,
        request_url: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct Interactivity {
        is_enabled: bool,
    }

    #[test]
    fn the_manifest_is_socket_mode_with_the_scopes_and_events_used() {
        let manifest: Manifest = yaml_serde::from_str(MANIFEST).unwrap();
        assert!(manifest.settings.socket_mode_enabled);
        assert!(!manifest.settings.interactivity.is_enabled);
        assert_eq!(manifest.settings.event_subscriptions.request_url, None);
        assert_eq!(manifest.features.bot_user.display_name, "Noa");
        assert!(manifest.features.app_home.messages_tab_enabled);
        assert!(!manifest.features.app_home.messages_tab_read_only_enabled);

        let scopes = &manifest.oauth_config.scopes.bot;
        for scope in [
            "channels:history",
            "channels:read",
            "groups:history",
            "groups:read",
            "im:history",
            "im:read",
            "im:write",
            "mpim:history",
            "mpim:read",
            "mpim:write",
            "chat:write",
            "reactions:read",
            "reactions:write",
            "pins:write",
            "users:read",
            "usergroups:read",
            "files:read",
        ] {
            assert!(scopes.iter().any(|s| s == scope), "missing scope {scope}");
        }
        let events = &manifest.settings.event_subscriptions.bot_events;
        for event in [
            "message.channels",
            "message.groups",
            "message.im",
            "message.mpim",
            "reaction_added",
            "reaction_removed",
            "member_joined_channel",
            "member_left_channel",
            "channel_rename",
            "channel_archive",
            "channel_deleted",
            "channel_left",
            "group_left",
            "user_change",
            "team_join",
            "subteam_members_changed",
        ] {
            assert!(events.iter().any(|e| e == event), "missing event {event}");
        }
        assert!(MANIFEST.starts_with('#'));
        assert!(MANIFEST.contains("connections:write"));
    }

    fn offline_actions() -> Actions {
        let api = Arc::new(Api::new(
            tls::client_config().unwrap(),
            "xoxb-test",
            "xapp-test",
        ));
        let state = State::new(
            "Q".to_owned(),
            SlackUserId("UBOT".to_owned()),
            None,
            SlackTeamId("T1".to_owned()),
        );
        Actions::new(api, Arc::new(Mutex::new(state)))
    }

    #[tokio::test]
    async fn a_failed_action_comes_back_as_a_failed_result() {
        let (queue, mut results) = mpsc::unbounded_channel();
        let (send, outbound) = mpsc::channel(4);
        let worker = tokio::spawn(run_actions(offline_actions(), outbound, queue));

        let bad = Outbound::new(OutboundAction::SendMessage {
            room: "general".into(),
            text: "hello".to_owned(),
            reply_to: None,
        });
        let poll = Outbound::new(OutboundAction::CreatePoll {
            room: "C1".into(),
            question: "when?".to_owned(),
            options: vec!["Thu".to_owned(), "Fri".to_owned()],
            multiple: false,
            hours: 24,
        });
        let typing = Outbound::new(OutboundAction::Typing { room: "C1".into() });
        let ids = [bad.id.clone(), poll.id.clone(), typing.id.clone()];
        for action in [bad, poll, typing] {
            send.send(action).await.unwrap();
        }
        drop(send);
        worker.await.unwrap();

        let mut seen = Vec::new();
        while let Some(InboundEvent::ActionResult {
            id,
            ok,
            message,
            error,
        }) = results.recv().await
        {
            seen.push((id, ok, message, error));
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, ids[0]);
        assert!(!seen[0].1);
        assert!(
            seen[0]
                .3
                .as_deref()
                .unwrap()
                .contains("not a Slack room id")
        );
        assert_eq!(
            (seen[1].1, seen[1].3.as_deref()),
            (false, Some("Slack has no native polls; use reactions"))
        );
        assert_eq!((&seen[2].0, seen[2].1, &seen[2].2), (&ids[2], true, &None));
    }

    #[tokio::test]
    async fn queued_events_reach_the_engine_in_order() {
        let (queue, queued) = mpsc::unbounded_channel();
        let (inbound, mut engine) = mpsc::channel(1);
        let forwarder = tokio::spawn(forward(queued, inbound));
        for n in 0..5 {
            queue
                .send(InboundEvent::MessageDeleted {
                    room: ours_types::PlatformRoomId::new(n.to_string()),
                    id: "1.0".into(),
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
    async fn giving_up_flushes_the_queue_then_says_why() {
        let (queue, queued) = mpsc::unbounded_channel();
        let (inbound, mut engine) = mpsc::channel(8);
        let forwarder = tokio::spawn(forward(queued, inbound.clone()));
        queue
            .send(InboundEvent::MessageDeleted {
                room: "C1".into(),
                id: "1.0".into(),
            })
            .unwrap();
        drop(queue);
        let error = give_up("the token was rejected".to_owned(), forwarder, &inbound)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the token was rejected");
        assert!(matches!(
            engine.recv().await,
            Some(InboundEvent::MessageDeleted { .. })
        ));
        assert_eq!(
            engine.recv().await,
            Some(InboundEvent::Disconnected {
                platform: Platform::Slack,
                reason: "the token was rejected".to_owned()
            })
        );
    }
}
