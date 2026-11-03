//! Telegram for Ours, through the Bot API with long polling, so a host
//! needs no public URL. Updates feed the engine's event model and the
//! engine's actions go out as Bot API calls. Mapping lives in `map`,
//! `text`, `reaction` and `poll` as pure functions; `state` holds the chat
//! cache and decides which events to emit; `actions` does the calls.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod actions;
mod api;
pub mod map;
pub mod poll;
pub mod reaction;
mod split;
mod state;
#[cfg(test)]
mod testutil;
pub mod text;
mod updates;

pub use api::DEFAULT_API_BASE;

use actions::{Actions, lock};
use api::{Api, ApiError};
use async_trait::async_trait;
use frankenstein::types::User;
use frankenstein::updates::UpdateContent;
use ours_connector::Connector;
use ours_types::{
    Capabilities, InboundEvent, Instant, Outbound, OutboundAction, Platform, PlatformRole,
};
use state::{ChatInfo, State, Want};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument;
use updates::{Next, Pacing};

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
        history: false,
        timezones: false,
        external_flag: false,
        typing: true,
        reaction_set: Some(reaction::ALLOWED.iter().map(|s| (*s).to_owned()).collect()),
    }
}

#[derive(Clone)]
pub struct TelegramConfig {
    /// The bot token from @BotFather, `123456789:AA...`.
    pub token: String,
    /// The name the agent answers to at the start of a message.
    pub agent_name: String,
}

impl std::fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("token", &"<redacted>")
            .field("agent_name", &self.agent_name)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("the Telegram bot token is empty")]
    EmptyToken,
    #[error("the Telegram bot token should look like 123456789:AA... as @BotFather gives it")]
    MalformedToken,
}

/// A bot token: the bot's id, a colon, and a secret of letters, digits,
/// `-` and `_`.
fn is_token(token: &str) -> bool {
    let Some((id, secret)) = token.split_once(':') else {
        return false;
    };
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_digit())
        && secret.len() >= 30
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug)]
pub struct TelegramConnector {
    config: TelegramConfig,
    api_base: String,
    chats: Vec<i64>,
}

impl TelegramConnector {
    pub fn new(mut config: TelegramConfig) -> Result<Self, ConfigError> {
        config.token = config.token.trim().to_owned();
        if config.token.is_empty() {
            return Err(ConfigError::EmptyToken);
        }
        if !is_token(&config.token) {
            return Err(ConfigError::MalformedToken);
        }
        Ok(TelegramConnector {
            config,
            api_base: DEFAULT_API_BASE.to_owned(),
            chats: Vec::new(),
        })
    }

    /// Use another Bot API server, such as a local `telegram-bot-api`.
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into().trim_end_matches('/').to_owned();
        self
    }

    /// Groups to sync at start, by room id. Telegram cannot list a bot's
    /// chats, so without this a group is synced when it next has an update.
    pub fn with_chats<S: AsRef<str>>(mut self, rooms: impl IntoIterator<Item = S>) -> Self {
        for room in rooms {
            match map::parse_room(&room.as_ref().into()) {
                Ok(target) if target.chat < 0 && !self.chats.contains(&target.chat) => {
                    self.chats.push(target.chat);
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "not a Telegram group to sync"),
            }
        }
        self
    }
}

#[async_trait]
impl Connector for TelegramConnector {
    fn platform(&self) -> Platform {
        Platform::Telegram
    }

    fn capabilities(&self) -> Capabilities {
        capabilities()
    }

    async fn run(
        self: Box<Self>,
        inbound: mpsc::Sender<InboundEvent>,
        outbound: mpsc::Receiver<Outbound>,
    ) -> anyhow::Result<()> {
        let TelegramConnector {
            config,
            api_base,
            chats,
        } = *self;
        let api = Arc::new(Api::new(&api_base, &config.token)?);

        // Events wait in an unbounded queue so a slow engine never stalls polling.
        let (queue, queued) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward(queued, inbound.clone()));

        let me = match login(&api, &outbound, Pacing::LIVE).await {
            Login::Ready(me) => me,
            Login::Stopped => return Ok(()),
            Login::Refused(reason) => {
                drop(queue);
                return give_up(reason, forwarder, &inbound).await;
            }
        };
        check_settings(&me);
        let state = Arc::new(Mutex::new(State::new(config.agent_name.clone(), &me)));
        let _ = queue.send(InboundEvent::Ready {
            platform: Platform::Telegram,
            me: map::user_id(me.id),
            me_name: me.username.clone().unwrap_or_else(|| me.first_name.clone()),
            capabilities: capabilities(),
        });

        let mut worker = tokio::spawn(run_actions(
            Actions::new(Arc::clone(&api), Arc::clone(&state)),
            outbound,
            queue.clone(),
        ));
        let io = Io {
            api,
            state,
            queue,
            pacing: Pacing::LIVE,
        };
        let reason = tokio::select! {
            reason = async {
                if io.sync_chats(&chats).await {
                    io.poll().await
                } else {
                    None
                }
            } => reason,
            done = &mut worker => {
                if let Err(error) = done {
                    tracing::error!(%error, "action worker stopped");
                }
                return Ok(());
            }
        };
        worker.abort();
        let _ = worker.await;
        drop(io);
        match reason {
            Some(reason) => give_up(reason, forwarder, &inbound).await,
            None => Ok(()),
        }
    }
}

/// Warn about bot settings that keep the agent from hearing the group.
fn check_settings(me: &User) {
    let name = me.username.as_deref().unwrap_or(&me.first_name);
    tracing::info!(bot = name, id = me.id, "telegram connected");
    if me.can_read_all_group_messages == Some(false) {
        tracing::warn!(
            bot = name,
            "privacy mode is on, so in a group where the bot is not an administrator it \
             sees only commands, mentions and replies; turn it off with /setprivacy in \
             @BotFather, or make the bot an administrator"
        );
    }
    if me.can_join_groups == Some(false) {
        tracing::warn!(
            bot = name,
            "the bot cannot be added to groups; allow it with /setjoingroups in @BotFather"
        );
    }
}

enum Login {
    Ready(Box<User>),
    Refused(String),
    /// The engine went away while Telegram was unreachable.
    Stopped,
}

/// Learn who the bot is, retrying with backoff until Telegram answers or
/// refuses the token.
async fn login(api: &Api, outbound: &mpsc::Receiver<Outbound>, pacing: Pacing) -> Login {
    let mut failures = 0u32;
    loop {
        match api.me().await {
            Ok(me) => return Login::Ready(Box::new(me)),
            Err(error) => {
                if let Some(reason) = updates::token_failure(&error) {
                    return Login::Refused(reason);
                }
                tracing::warn!(%error, "getMe failed; retrying");
            }
        }
        if outbound.is_closed() {
            return Login::Stopped;
        }
        tokio::time::sleep(pacing.backoff(failures)).await;
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
    tracing::error!(%reason, "telegram connector is down");
    let _ = forwarder.await;
    let _ = inbound
        .send(InboundEvent::Disconnected {
            platform: Platform::Telegram,
            reason: reason.clone(),
        })
        .await;
    anyhow::bail!(reason)
}

/// The polling side: updates in, the fetches `missing` asks for, events
/// out. A method that returns a bool returns false once the engine has
/// stopped listening.
struct Io {
    api: Arc<Api>,
    state: Arc<Mutex<State>>,
    queue: mpsc::UnboundedSender<InboundEvent>,
    pacing: Pacing,
}

impl Io {
    fn emit(&self, events: Vec<InboundEvent>) -> bool {
        events
            .into_iter()
            .all(|event| self.queue.send(event).is_ok())
    }

    /// Long-poll until Telegram refuses the token, returning why, or the
    /// engine stops listening, returning `None`.
    async fn poll(&self) -> Option<String> {
        let mut offset = None;
        let mut failures = 0u32;
        loop {
            match self.api.updates(offset).await {
                Ok(batch) => {
                    failures = 0;
                    for raw in batch.into_iter().map(updates::decode) {
                        if let Some(id) = raw.id {
                            offset = updates::next_offset(offset, id);
                        }
                        let Some(update) = raw.content else {
                            continue;
                        };
                        let span = tracing::info_span!(
                            "telegram_update",
                            kind = updates::kind(&update),
                            id = raw.id
                        );
                        if !self.process(update).instrument(span).await {
                            return None;
                        }
                    }
                }
                Err(error) => match updates::after(&error) {
                    Next::GiveUp(reason) => return Some(reason),
                    Next::DropWebhook => {
                        tracing::warn!("a webhook is set for this bot; removing it to long-poll");
                        if let Err(error) = self.api.delete_webhook().await {
                            tracing::warn!(%error, "could not remove the webhook");
                        }
                        tokio::time::sleep(self.pacing.backoff(failures)).await;
                        failures = failures.saturating_add(1);
                    }
                    Next::Wait(wait) => {
                        tracing::warn!(%error, "getUpdates was rate limited");
                        tokio::time::sleep(wait).await;
                    }
                    Next::Backoff => {
                        tracing::warn!(%error, "getUpdates failed; retrying");
                        tokio::time::sleep(self.pacing.backoff(failures)).await;
                        failures = failures.saturating_add(1);
                    }
                },
            }
            if self.queue.is_closed() {
                return None;
            }
        }
    }

    /// Fetch what the update needs, then hand it to the state.
    async fn process(&self, update: UpdateContent) -> bool {
        let wants = lock(&self.state).missing(&update, now());
        for want in wants {
            if !self.fetch(want).await {
                return false;
            }
        }
        let events = lock(&self.state).handle(&update, now());
        self.emit(events)
    }

    /// Sync the groups the host named. A group the bot is not in is
    /// logged so a wrong id shows up at start.
    async fn sync_chats(&self, chats: &[i64]) -> bool {
        for chat in chats {
            let info = match self.api.chat(*chat).await {
                Ok(full) => ChatInfo::from(&full),
                Err(error) => {
                    tracing::warn!(
                        chat,
                        %error,
                        "could not read a configured group; is the bot in it and is the id current?"
                    );
                    continue;
                }
            };
            let admins = self.admins(*chat).await;
            let events = lock(&self.state).apply_chat(info, admins, now());
            if !self.emit(events) {
                return false;
            }
        }
        true
    }

    async fn fetch(&self, want: Want) -> bool {
        match want {
            Want::Chat(seen) => {
                let info = match self.api.chat(seen.id).await {
                    Ok(full) => ChatInfo::from(&full),
                    Err(
                        error @ ApiError::Refused {
                            code: 400 | 403, ..
                        },
                    ) => {
                        tracing::info!(chat = seen.id, %error, "the agent is not in this group");
                        return true;
                    }
                    Err(error) => {
                        tracing::warn!(chat = seen.id, %error, "could not read a group; using what the update said");
                        ChatInfo::from(&*seen)
                    }
                };
                let admins = self.admins(seen.id).await;
                tracing::info!(
                    chat = seen.id,
                    title = info.title.as_deref().unwrap_or(""),
                    "telegram group synced"
                );
                let events = lock(&self.state).apply_chat(info, admins, now());
                self.emit(events)
            }
            Want::Admins(chat) => {
                let admins = self.admins(chat).await;
                let events = lock(&self.state).apply_admins(chat, admins, now());
                self.emit(events)
            }
        }
    }

    async fn admins(&self, chat: i64) -> Option<Vec<(User, PlatformRole)>> {
        match self.api.admins(chat).await {
            Ok(list) => Some(list),
            Err(error) => {
                tracing::warn!(chat, %error, "could not read a group's administrators");
                None
            }
        }
    }
}

/// Run engine actions one at a time, in order, off the poll loop.
async fn run_actions(
    mut actions: Actions,
    mut outbound: mpsc::Receiver<Outbound>,
    queue: mpsc::UnboundedSender<InboundEvent>,
) {
    while let Some(Outbound { id, action }) = outbound.recv().await {
        let span = tracing::info_span!("telegram_action", action = action_name(&action));
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

/// The wall clock as an `Instant`, for events Telegram does not timestamp.
pub(crate) fn now() -> Instant {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Instant::from_millis(ms).unwrap_or(Instant::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::{Value, json};
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

    const QUICK: Pacing = Pacing {
        first: Duration::from_millis(1),
        most: Duration::from_millis(5),
    };

    fn config() -> TelegramConfig {
        TelegramConfig {
            token: TOKEN.to_owned(),
            agent_name: "Q".to_owned(),
        }
    }

    fn route(name: &str) -> String {
        format!("/bot{TOKEN}/{name}")
    }

    fn ok(result: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": result }))
    }

    fn refused(code: u16, description: &str) -> ResponseTemplate {
        ResponseTemplate::new(code)
            .set_body_json(json!({ "ok": false, "error_code": code, "description": description }))
    }

    fn io(server: &MockServer, queue: mpsc::UnboundedSender<InboundEvent>) -> Io {
        Io {
            api: Arc::new(Api::new(&server.uri(), TOKEN).unwrap()),
            state: Arc::new(Mutex::new(State::new("Q".to_owned(), &bot_user()))),
            queue,
            pacing: QUICK,
        }
    }

    fn dm_update(id: i64, text: &str) -> Value {
        json!({ "update_id": id, "message": { "message_id": 1, "date": 1_793_613_600,
            "chat": private_chat(SAM, "Sam"), "from": person(SAM), "text": text } })
    }

    #[test]
    fn capabilities_match_what_telegram_gives_bots() {
        let connector = TelegramConnector::new(config()).unwrap();
        assert_eq!(connector.platform(), Platform::Telegram);
        assert_eq!(
            connector.capabilities(),
            Capabilities {
                reactions: true,
                threads: true,
                polls: true,
                dms: true,
                edit: true,
                delete: true,
                pin: true,
                history: false,
                timezones: false,
                external_flag: false,
                typing: true,
                reaction_set: Some(reaction::ALLOWED.iter().map(|s| (*s).to_owned()).collect()),
            }
        );
    }

    #[test]
    fn the_token_must_look_like_one_from_botfather() {
        let check = |token: &str| {
            TelegramConnector::new(TelegramConfig {
                token: token.to_owned(),
                ..config()
            })
            .map(|_| ())
        };
        assert_eq!(check(TOKEN), Ok(()));
        assert_eq!(check(&format!("  {TOKEN}\n")), Ok(()));
        assert_eq!(check(""), Err(ConfigError::EmptyToken));
        assert_eq!(check("   "), Err(ConfigError::EmptyToken));
        for bad in [
            "ABC-DEF1234ghIkl-zyx57W2v1u123ew11",
            "123456",
            "123456:",
            "abc:ABC-DEF1234ghIkl-zyx57W2v1u123ew11",
            "123456:short",
            "123456:ABC DEF1234ghIkl zyx57W2v1u123ew11",
            "xoxb-123-456",
        ] {
            assert_eq!(check(bad), Err(ConfigError::MalformedToken), "{bad}");
        }
    }

    #[test]
    fn the_token_never_prints() {
        let connector = TelegramConnector::new(config())
            .unwrap()
            .with_api_base("http://localhost:8081/");
        assert_eq!(connector.api_base, "http://localhost:8081");
        for printed in [
            format!("{:?}", config()),
            format!("{connector:?}"),
            format!("{:?}", Api::new(DEFAULT_API_BASE, TOKEN).unwrap()),
        ] {
            assert!(!printed.contains("ABC-DEF"), "{printed}");
        }
    }

    #[tokio::test]
    async fn every_action_ends_in_a_result_and_failures_carry_the_reason() {
        let api = Arc::new(Api::new("http://127.0.0.1:9", TOKEN).unwrap());
        let state = Arc::new(Mutex::new(State::new("Q".to_owned(), &bot_user())));
        let (queue, mut results) = mpsc::unbounded_channel();
        let (send, outbound) = mpsc::channel(4);
        let worker = tokio::spawn(run_actions(Actions::new(api, state), outbound, queue));

        let history = Outbound::new(OutboundAction::FetchHistory {
            room: GROUP.to_string().into(),
            limit: 30,
            before: None,
        });
        let digit = Outbound::new(OutboundAction::React {
            room: GROUP.to_string().into(),
            message: "5".into(),
            emoji: "1️⃣".to_owned(),
        });
        let ids = [history.id.clone(), digit.id.clone()];
        send.send(history).await.unwrap();
        send.send(digit).await.unwrap();
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
        assert_eq!(
            seen,
            [
                (
                    ids[0].clone(),
                    false,
                    None,
                    Some("Telegram keeps no history for bots".to_owned())
                ),
                (
                    ids[1].clone(),
                    false,
                    None,
                    Some("Telegram has no digit reactions; use a poll".to_owned())
                ),
            ]
        );
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
                    id: "1".into(),
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
    async fn a_401_while_polling_stops_the_loop_and_names_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(refused(401, "Unauthorized"))
            .expect(1)
            .mount(&server)
            .await;
        let (queue, _events) = mpsc::unbounded_channel();
        let reason = io(&server, queue).poll().await.unwrap();
        assert!(
            reason.contains("bot token") && reason.contains("401"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn transient_failures_are_retried_and_the_offset_moves_on() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(ResponseTemplate::new(502).set_body_string("Bad Gateway"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(refused(500, "Internal Server Error"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(ok(json!([
                { "update_id": 70, "an_update_from_the_future": {} },
                dm_update(71, "nudge me, not them")
            ])))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .and(body_partial_json(json!({ "offset": 72 })))
            .respond_with(ok(json!([])))
            .mount(&server)
            .await;
        let (queue, mut events) = mpsc::unbounded_channel();
        let polling = tokio::spawn(async move { io(&server, queue).poll().await });

        let sync = events.recv().await.unwrap();
        assert!(matches!(sync, InboundEvent::RoomSync { .. }), "{sync:?}");
        let Some(InboundEvent::Message(message)) = events.recv().await else {
            panic!()
        };
        assert_eq!(message.text, "nudge me, not them");
        drop(events);
        assert_eq!(polling.await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_webhook_in_the_way_is_removed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(refused(
                409,
                "Conflict: can't use getUpdates method while webhook is active; use deleteWebhook to delete the webhook first",
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("deleteWebhook")))
            .and(body_partial_json(json!({ "drop_pending_updates": false })))
            .respond_with(ok(json!(true)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(refused(401, "Unauthorized"))
            .mount(&server)
            .await;
        let (queue, _events) = mpsc::unbounded_channel();
        assert!(io(&server, queue).poll().await.is_some());
    }

    #[tokio::test]
    async fn a_group_is_read_before_its_first_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getChat")))
            .respond_with(ok(json!({ "id": GROUP, "type": "supergroup", "title": "The Six",
                "accepted_gift_types": { "unlimited_gifts": false, "limited_gifts": false,
                    "unique_gifts": false, "premium_subscription": false, "gifts_from_channels": false } })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getChatAdministrators")))
            .respond_with(ok(json!([creator(SAM)])))
            .expect(1)
            .mount(&server)
            .await;
        let (queue, mut events) = mpsc::unbounded_channel();
        let io = io(&server, queue);
        let message = said(1, ALEX, "Alex", "hi all");
        let alive = io
            .process(
                serde_json::from_value::<frankenstein::updates::Update>(
                    json!({ "update_id": 1, "message": message["message"] }),
                )
                .unwrap()
                .content,
            )
            .await;
        assert!(alive);
        let Some(InboundEvent::RoomSync { room, members }) = events.recv().await else {
            panic!()
        };
        assert_eq!(room.name.as_deref(), Some("The Six"));
        let roles: Vec<_> = members
            .iter()
            .map(|m| (m.id.as_str().to_owned(), m.platform_role))
            .collect();
        assert_eq!(
            roles,
            [
                ("11".to_owned(), PlatformRole::Owner),
                ("900".to_owned(), PlatformRole::Regular)
            ]
        );
        assert!(matches!(
            events.recv().await,
            Some(InboundEvent::MemberJoined { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Some(InboundEvent::Message(_))
        ));
    }

    #[tokio::test]
    async fn configured_groups_are_synced_at_start_and_wrong_ids_skipped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getChat")))
            .and(body_partial_json(json!({ "chat_id": GROUP })))
            .respond_with(ok(json!({ "id": GROUP, "type": "supergroup", "title": "The Six",
                "accepted_gift_types": { "unlimited_gifts": false, "limited_gifts": false,
                    "unique_gifts": false, "premium_subscription": false, "gifts_from_channels": false } })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getChat")))
            .respond_with(refused(400, "Bad Request: chat not found"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getChatAdministrators")))
            .respond_with(ok(json!([creator(SAM), administrator(BOT)])))
            .mount(&server)
            .await;
        let connector = TelegramConnector::new(config()).unwrap().with_chats([
            GROUP.to_string(),
            format!("{GROUP}:12"),
            "-4455".to_owned(),
            "5551234".to_owned(),
            "general".to_owned(),
        ]);
        assert_eq!(connector.chats, [GROUP, -4455]);

        let (queue, mut events) = mpsc::unbounded_channel();
        let io = io(&server, queue);
        assert!(io.sync_chats(&connector.chats).await);
        drop(io);
        let Some(InboundEvent::RoomSync { room, members }) = events.recv().await else {
            panic!()
        };
        assert_eq!(room.id.as_str(), GROUP.to_string());
        let roles: Vec<_> = members.iter().map(|m| m.platform_role).collect();
        assert_eq!(roles, [PlatformRole::Owner, PlatformRole::Admin]);
        assert!(events.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_group_that_refuses_the_agent_is_not_made_up() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getChat")))
            .respond_with(refused(
                403,
                "Forbidden: bot was kicked from the supergroup chat",
            ))
            .mount(&server)
            .await;
        let (queue, mut events) = mpsc::unbounded_channel();
        let io = io(&server, queue);
        let update = update(said(1, ALEX, "Alex", "hi"));
        assert!(io.process(update).await);
        drop(io);
        assert!(events.recv().await.is_none());
    }

    #[tokio::test]
    async fn login_retries_until_telegram_answers_and_stops_on_a_refused_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getMe")))
            .respond_with(refused(500, "Internal Server Error"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getMe")))
            .respond_with(ok(bot_json()))
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();
        let (_send, outbound) = mpsc::channel::<Outbound>(1);
        let Login::Ready(me) = login(&api, &outbound, QUICK).await else {
            panic!()
        };
        assert_eq!(me.username.as_deref(), Some("ours_q_bot"));

        let refusing = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(refused(401, "Unauthorized"))
            .mount(&refusing)
            .await;
        let api = Api::new(&refusing.uri(), TOKEN).unwrap();
        assert!(matches!(
            login(&api, &outbound, QUICK).await,
            Login::Refused(reason) if reason.contains("@BotFather")
        ));
    }

    #[tokio::test]
    async fn a_revoked_token_ends_the_run_with_disconnected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getMe")))
            .respond_with(ok(bot_json()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(refused(401, "Unauthorized"))
            .mount(&server)
            .await;
        let connector = TelegramConnector::new(config())
            .unwrap()
            .with_api_base(server.uri());
        let (inbound, mut engine) = mpsc::channel(8);
        let (_send, outbound) = mpsc::channel(8);
        let error = Box::new(connector)
            .run(inbound, outbound)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("bot token"), "{error}");

        let ready = engine.recv().await.unwrap();
        assert_eq!(
            ready,
            InboundEvent::Ready {
                platform: Platform::Telegram,
                me: "900".into(),
                me_name: "ours_q_bot".to_owned(),
                capabilities: capabilities(),
            }
        );
        let Some(InboundEvent::Disconnected { platform, reason }) = engine.recv().await else {
            panic!()
        };
        assert_eq!(platform, Platform::Telegram);
        assert!(reason.contains("401"), "{reason}");
    }

    #[tokio::test]
    async fn a_refused_token_at_login_ends_the_run_with_disconnected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(refused(401, "Unauthorized"))
            .mount(&server)
            .await;
        let connector = TelegramConnector::new(config())
            .unwrap()
            .with_api_base(server.uri());
        let (inbound, mut engine) = mpsc::channel(8);
        let (_send, outbound) = mpsc::channel(8);
        assert!(Box::new(connector).run(inbound, outbound).await.is_err());
        assert!(matches!(
            engine.recv().await,
            Some(InboundEvent::Disconnected { .. })
        ));
    }

    #[tokio::test]
    async fn the_run_ends_quietly_when_the_engine_goes_away() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("getMe")))
            .respond_with(ok(bot_json()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("getUpdates")))
            .respond_with(ok(json!([])).set_delay(Duration::from_millis(20)))
            .mount(&server)
            .await;
        let connector = TelegramConnector::new(config())
            .unwrap()
            .with_api_base(server.uri());
        let (inbound, mut engine) = mpsc::channel(8);
        let (send, outbound) = mpsc::channel(8);
        let run = tokio::spawn(Box::new(connector).run(inbound, outbound));
        assert!(matches!(
            engine.recv().await,
            Some(InboundEvent::Ready { .. })
        ));
        drop(send);
        assert!(run.await.unwrap().is_ok());
    }
}
