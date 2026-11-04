//! Carrying out `OutboundAction`s over the Bot API. A failure comes back as
//! an error for the caller to report, never a panic. Text goes out plain:
//! no parse mode, no link previews, nothing but words.

use crate::api::{Api, ApiError};
use crate::map::{self, MapError, Target};
use crate::poll::{self, PollError};
use crate::reaction::{self, ReactionError};
use crate::split::{MESSAGE_LIMIT, split_message, truncate_edit};
use crate::state::State;
use frankenstein::methods::{
    DeleteMessageParams, EditMessageTextParams, PinChatMessageParams, SendChatActionParams,
    SendMessageParams, SetMessageReactionParams, StopPollParams,
};
use frankenstein::types::{
    ChatAction, LinkPreviewOptions, Message as TgMessage, ReactionType, ReactionTypeEmoji,
    ReplyParameters,
};
use ours_types::{InboundEvent, OutboundAction, PlatformMessageId};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// The agent's own reactions remembered, so an unreact takes back only
/// what it set.
const REACTIONS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error(transparent)]
    Id(#[from] MapError),
    #[error(transparent)]
    Poll(#[from] PollError),
    #[error(transparent)]
    Reaction(#[from] ReactionError),
    #[error("Telegram keeps no history for bots")]
    NoHistory,
    #[error("nothing to send: the message is empty")]
    Empty,
    #[error("they have not started a chat with the bot")]
    NotStarted,
    #[error("they have blocked the bot")]
    Blocked,
    #[error("the group is now a supergroup with chat id {0}; put that id under the group's rooms")]
    Migrated(i64),
    #[error("{0}")]
    Api(String),
}

impl From<ApiError> for ActionError {
    fn from(error: ApiError) -> Self {
        match error {
            ApiError::Refused {
                migrate_to: Some(chat),
                ..
            } => ActionError::Migrated(chat),
            other => ActionError::Api(other.to_string()),
        }
    }
}

/// What a finished action produced.
#[derive(Debug, Default)]
pub struct Outcome {
    pub message: Option<PlatformMessageId>,
    /// Events to deliver before the action's own result.
    pub events: Vec<InboundEvent>,
}

/// The state, even if a thread panicked while holding it.
pub fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// The one reaction the agent has on each message it reacted to.
#[derive(Debug, Default)]
struct Mine {
    set: HashMap<(i64, i32), &'static str>,
    order: VecDeque<(i64, i32)>,
}

impl Mine {
    fn set(&mut self, key: (i64, i32), emoji: &'static str) {
        if self.set.insert(key, emoji).is_none() {
            self.order.push_back(key);
            if self.order.len() > REACTIONS
                && let Some(oldest) = self.order.pop_front()
            {
                self.set.remove(&oldest);
            }
        }
    }

    fn get(&self, key: (i64, i32)) -> Option<&'static str> {
        self.set.get(&key).copied()
    }

    fn clear(&mut self, key: (i64, i32)) {
        if self.set.remove(&key).is_some() {
            self.order.retain(|k| *k != key);
        }
    }
}

#[derive(Debug)]
pub struct Actions {
    api: Arc<Api>,
    state: Arc<Mutex<State>>,
    mine: Mine,
}

impl Actions {
    pub fn new(api: Arc<Api>, state: Arc<Mutex<State>>) -> Self {
        Actions {
            api,
            state,
            mine: Mine::default(),
        }
    }

    pub async fn perform(&mut self, action: OutboundAction) -> Result<Outcome, ActionError> {
        match action {
            OutboundAction::SendMessage {
                room,
                text,
                reply_to,
            } => {
                let target = map::parse_room(&room)?;
                let reply = reply_to.as_ref().map(map::parse_message).transpose()?;
                let sent = self.send(target, &text, reply).await?;
                Ok(Outcome {
                    message: Some(map::message_id(sent.message_id)),
                    events: Vec::new(),
                })
            }
            OutboundAction::EditMessage { room, id, text } => {
                let target = map::parse_room(&room)?;
                let params = EditMessageTextParams::builder()
                    .chat_id(target.chat)
                    .message_id(map::parse_message(&id)?)
                    .text(truncate_edit(&text, MESSAGE_LIMIT))
                    .link_preview_options(LinkPreviewOptions::DISABLED)
                    .build();
                match self.api.edit_text(&params).await {
                    Err(error) if error.description().contains("message is not modified") => {
                        Ok(Outcome::default())
                    }
                    other => other
                        .map(|()| Outcome::default())
                        .map_err(ActionError::from),
                }
            }
            OutboundAction::DeleteMessage { room, id } => {
                let target = map::parse_room(&room)?;
                let params = DeleteMessageParams::builder()
                    .chat_id(target.chat)
                    .message_id(map::parse_message(&id)?)
                    .build();
                self.api.delete_message(&params).await?;
                Ok(Outcome::default())
            }
            OutboundAction::React {
                room,
                message,
                emoji,
            } => {
                let target = map::parse_room(&room)?;
                let message = map::parse_message(&message)?;
                let emoji = reaction::telegram_form(&emoji)?;
                self.set_reaction(target.chat, message, Some(emoji)).await?;
                self.mine.set((target.chat, message), emoji);
                Ok(Outcome::default())
            }
            OutboundAction::Unreact {
                room,
                message,
                emoji,
            } => {
                let target = map::parse_room(&room)?;
                let message = map::parse_message(&message)?;
                let key = (target.chat, message);
                // Only an emoji Telegram allows can have been set, and only
                // the agent's current one is taken back.
                let Ok(emoji) = reaction::telegram_form(&emoji) else {
                    return Ok(Outcome::default());
                };
                if self.mine.get(key).is_some_and(|current| current != emoji) {
                    return Ok(Outcome::default());
                }
                self.set_reaction(target.chat, message, None).await?;
                self.mine.clear(key);
                Ok(Outcome::default())
            }
            OutboundAction::SendDm { user, text } => {
                let user = map::parse_user(&user)?;
                let chat = i64::try_from(user).map_err(|_| MapError::BadUser(user.to_string()))?;
                let sent = match self.send(Target::chat(chat), &text, None).await {
                    Err(ActionError::Api(description)) if description.starts_with("Forbidden") => {
                        return Err(if description.contains("blocked") {
                            ActionError::Blocked
                        } else {
                            ActionError::NotStarted
                        });
                    }
                    other => other?,
                };
                let events = lock(&self.state).open_dm(&sent.chat);
                Ok(Outcome {
                    message: Some(map::message_id(sent.message_id)),
                    events,
                })
            }
            OutboundAction::Pin { room, message } => {
                let target = map::parse_room(&room)?;
                let params = PinChatMessageParams::builder()
                    .chat_id(target.chat)
                    .message_id(map::parse_message(&message)?)
                    .disable_notification(true)
                    .build();
                self.api.pin(&params).await?;
                Ok(Outcome::default())
            }
            OutboundAction::CreatePoll {
                room,
                question,
                options,
                multiple,
                hours,
            } => {
                let target = map::parse_room(&room)?;
                let params = poll::params(target, &question, &options, multiple, hours)?;
                let sent = self.api.send_poll(&params).await?;
                let mut state = lock(&self.state);
                state.record_sent(target, sent.message_id);
                if let Some(created) = &sent.poll {
                    state.record_poll(created.id.clone(), target, sent.message_id, options.len());
                }
                Ok(Outcome {
                    message: Some(map::message_id(sent.message_id)),
                    events: Vec::new(),
                })
            }
            OutboundAction::EndPoll { room, message } => {
                let target = map::parse_room(&room)?;
                let message = map::parse_message(&message)?;
                let params = StopPollParams::builder()
                    .chat_id(target.chat)
                    .message_id(message)
                    .build();
                let stopped = self.api.stop_poll(&params).await?;
                let events = lock(&self.state)
                    .close_poll(&stopped, target, message)
                    .into_iter()
                    .collect();
                Ok(Outcome {
                    message: None,
                    events,
                })
            }
            OutboundAction::Typing { room } => {
                let target = map::parse_room(&room)?;
                let params = SendChatActionParams::builder()
                    .chat_id(target.chat)
                    .maybe_message_thread_id(target.topic)
                    .action(ChatAction::Typing)
                    .build();
                self.api.chat_action(&params).await?;
                Ok(Outcome::default())
            }
            OutboundAction::FetchHistory { .. } => Err(ActionError::NoHistory),
        }
    }

    async fn set_reaction(
        &self,
        chat: i64,
        message: i32,
        emoji: Option<&str>,
    ) -> Result<(), ActionError> {
        let reaction = emoji
            .map(|emoji| {
                ReactionType::Emoji(ReactionTypeEmoji {
                    emoji: emoji.to_owned(),
                })
            })
            .into_iter()
            .collect();
        let params = SetMessageReactionParams::builder()
            .chat_id(chat)
            .message_id(message)
            .reaction(reaction)
            .build();
        self.api.set_reaction(&params).await?;
        Ok(())
    }

    /// Send text in as many pieces as it takes. The first piece carries the
    /// reply and is the message the engine is told about. Once it is out, a
    /// later piece that fails cuts the message short there, and the send
    /// still succeeds.
    async fn send(
        &self,
        target: Target,
        text: &str,
        reply: Option<i32>,
    ) -> Result<TgMessage, ActionError> {
        let pieces = split_message(text, MESSAGE_LIMIT);
        let total = pieces.len();
        let mut first = None;
        for (i, piece) in pieces.into_iter().enumerate() {
            let reply = reply.filter(|_| i == 0).map(|message_id| {
                ReplyParameters::builder()
                    .message_id(message_id)
                    .allow_sending_without_reply(true)
                    .build()
            });
            let params = SendMessageParams::builder()
                .chat_id(target.chat)
                .maybe_message_thread_id(target.topic)
                .text(piece)
                .link_preview_options(LinkPreviewOptions::DISABLED)
                .maybe_reply_parameters(reply)
                .build();
            let sent = match self.api.send_message(&params).await {
                Ok(sent) => sent,
                Err(error) => {
                    let Some(first) = first else {
                        return Err(error.into());
                    };
                    tracing::warn!(
                        %error,
                        sent = i,
                        of = total,
                        "a long message was cut short; the rest was not sent"
                    );
                    return Ok(first);
                }
            };
            lock(&self.state).record_sent(target, sent.message_id);
            first.get_or_insert(sent);
        }
        first.ok_or(ActionError::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const TOKEN: &str = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

    fn route(name: &str) -> String {
        format!("/bot{TOKEN}/{name}")
    }

    async fn actions(server: &MockServer) -> Actions {
        let api = Arc::new(Api::new(&server.uri(), TOKEN).unwrap());
        let state = State::new("Q".to_owned(), &bot_user());
        Actions::new(api, Arc::new(Mutex::new(state)))
    }

    fn offline() -> Actions {
        let api = Arc::new(Api::new("http://127.0.0.1:9", TOKEN).unwrap());
        let state = State::new("Q".to_owned(), &bot_user());
        Actions::new(api, Arc::new(Mutex::new(state)))
    }

    fn ok(result: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": result }))
    }

    fn refused(code: u16, description: &str) -> ResponseTemplate {
        ResponseTemplate::new(code)
            .set_body_json(json!({ "ok": false, "error_code": code, "description": description }))
    }

    fn sent_message(id: i32, chat: Value) -> Value {
        json!({ "message_id": id, "date": 1, "chat": chat, "text": "x" })
    }

    async fn bodies(server: &MockServer, name: &str) -> Vec<Value> {
        let wanted = route(name);
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r: &&Request| r.url.path() == wanted)
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn a_long_message_goes_in_pieces_and_the_first_carries_the_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("sendMessage")))
            .respond_with(ok(sent_message(101, group_chat(GROUP, true))))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("sendMessage")))
            .respond_with(ok(sent_message(102, group_chat(GROUP, true))))
            .mount(&server)
            .await;
        let mut actions = actions(&server).await;
        let text = format!("{}\n{}", "a".repeat(4000), "b".repeat(200));
        let outcome = actions
            .perform(OutboundAction::SendMessage {
                room: format!("{GROUP}:12").into(),
                text,
                reply_to: Some("90".into()),
            })
            .await
            .unwrap();
        assert_eq!(outcome.message.as_ref().map(|m| m.as_str()), Some("101"));

        let sent = bodies(&server, "sendMessage").await;
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[0],
            json!({
                "chat_id": GROUP, "message_thread_id": 12, "text": "a".repeat(4000),
                "link_preview_options": { "is_disabled": true },
                "reply_parameters": { "message_id": 90, "allow_sending_without_reply": true }
            })
        );
        assert_eq!(sent[1]["text"], "b".repeat(200));
        assert_eq!(sent[1]["message_thread_id"], 12);
        assert!(sent[1].get("reply_parameters").is_none());
        assert!(sent.iter().all(|b| b.get("parse_mode").is_none()));
    }

    #[tokio::test]
    async fn a_send_cut_short_after_its_first_piece_still_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("sendMessage")))
            .respond_with(ok(sent_message(101, group_chat(GROUP, true))))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("sendMessage")))
            .respond_with(refused(429, "Too Many Requests: retry after 5"))
            .mount(&server)
            .await;
        let mut actions = actions(&server).await;
        let text = ["a", "b", "c"].map(|c| c.repeat(4000)).join("\n");
        let outcome = actions
            .perform(OutboundAction::SendMessage {
                room: format!("{GROUP}:12").into(),
                text,
                reply_to: None,
            })
            .await
            .unwrap();
        assert_eq!(outcome.message.as_ref().map(|m| m.as_str()), Some("101"));
        let sent = bodies(&server, "sendMessage").await;
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1]["text"], "b".repeat(4000));
    }

    #[tokio::test]
    async fn a_dm_to_someone_who_never_started_the_bot_says_so() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("sendMessage")))
            .respond_with(refused(
                403,
                "Forbidden: bot can't initiate conversation with a user",
            ))
            .mount(&server)
            .await;
        let error = actions(&server)
            .await
            .perform(OutboundAction::SendDm {
                user: SAM.to_string().into(),
                text: "hi".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "they have not started a chat with the bot"
        );
    }

    #[tokio::test]
    async fn a_first_dm_announces_the_room() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("sendMessage")))
            .respond_with(ok(sent_message(5, private_chat(SAM, "Sam"))))
            .mount(&server)
            .await;
        let mut actions = actions(&server).await;
        let dm = OutboundAction::SendDm {
            user: SAM.to_string().into(),
            text: "Hi, I'm Q.".to_owned(),
        };
        let outcome = actions.perform(dm.clone()).await.unwrap();
        let [InboundEvent::RoomSync { room, members }] = outcome.events.as_slice() else {
            panic!("{:?}", outcome.events)
        };
        assert_eq!(room.id.as_str(), SAM.to_string());
        assert_eq!(members[0].display_name, "Sam");
        assert_eq!(outcome.message.as_ref().map(|m| m.as_str()), Some("5"));
        assert!(actions.perform(dm).await.unwrap().events.is_empty());
        assert_eq!(bodies(&server, "sendMessage").await[0]["chat_id"], SAM);
    }

    #[tokio::test]
    async fn a_poll_names_its_voters_and_its_votes_find_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("sendPoll")))
            .respond_with(ok(json!({ "message_id": 300, "date": 1, "chat": group_chat(GROUP, false),
                "poll": { "id": "P9", "question": "when?", "total_voter_count": 0, "is_closed": false,
                    "is_anonymous": false, "type": "regular",
                    "options": [{ "text": "Thu", "voter_count": 0 }, { "text": "Fri", "voter_count": 0 }] } })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(route("stopPoll")))
            .respond_with(ok(json!({ "id": "P9", "question": "when?", "total_voter_count": 3,
                "is_closed": true, "is_anonymous": false, "type": "regular",
                "options": [{ "text": "Thu", "voter_count": 2 }, { "text": "Fri", "voter_count": 1 }] })))
            .mount(&server)
            .await;
        let mut actions = actions(&server).await;
        let outcome = actions
            .perform(OutboundAction::CreatePoll {
                room: GROUP.to_string().into(),
                question: "when?".to_owned(),
                options: vec!["Thu".to_owned(), "Fri".to_owned()],
                multiple: false,
                hours: 24,
            })
            .await
            .unwrap();
        assert_eq!(outcome.message.as_ref().map(|m| m.as_str()), Some("300"));
        let body = &bodies(&server, "sendPoll").await[0];
        assert_eq!(body["is_anonymous"], false);
        assert_eq!(body["open_period"], 86_400);
        assert_eq!(body["allows_multiple_answers"], false);

        let vote = update(
            json!({ "poll_answer": { "poll_id": "P9", "user": user_json(SAM, "Sam"),
            "option_ids": [1], "option_persistent_ids": ["b"] } }),
        );
        let events = lock(&actions.state).handle(&vote, at());
        assert!(
            matches!(events.as_slice(), [InboundEvent::PollVote { message, option: 1, added: true, .. }]
                if message.as_str() == "300"),
            "{events:?}"
        );

        let ended = actions
            .perform(OutboundAction::EndPoll {
                room: GROUP.to_string().into(),
                message: "300".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            ended.events,
            [InboundEvent::PollEnded {
                room: GROUP.to_string().into(),
                message: "300".into(),
                counts: vec![2, 1]
            }]
        );
        let closed = update(
            json!({ "poll": { "id": "P9", "question": "when?", "total_voter_count": 3,
            "is_closed": true, "is_anonymous": false, "type": "regular",
            "options": [{ "text": "Thu", "voter_count": 2 }, { "text": "Fri", "voter_count": 1 }] } }),
        );
        assert!(lock(&actions.state).handle(&closed, at()).is_empty());
    }

    #[tokio::test]
    async fn an_unchanged_edit_is_not_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("editMessageText")))
            .respond_with(refused(
                400,
                "Bad Request: message is not modified: specified new message content and reply markup are exactly the same",
            ))
            .mount(&server)
            .await;
        let outcome = actions(&server)
            .await
            .perform(OutboundAction::EditMessage {
                room: GROUP.to_string().into(),
                id: "7".into(),
                text: "📌 Open".to_owned(),
            })
            .await;
        assert!(outcome.is_ok());
        let body = &bodies(&server, "editMessageText").await[0];
        assert_eq!(body["link_preview_options"]["is_disabled"], true);
    }

    #[tokio::test]
    async fn reactions_are_one_per_message_and_unreact_takes_back_only_its_own() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(route("setMessageReaction")))
            .respond_with(ok(json!(true)))
            .mount(&server)
            .await;
        let mut actions = actions(&server).await;
        let react = |emoji: &str| OutboundAction::React {
            room: GROUP.to_string().into(),
            message: "40".into(),
            emoji: emoji.to_owned(),
        };
        let unreact = |emoji: &str| OutboundAction::Unreact {
            room: GROUP.to_string().into(),
            message: "40".into(),
            emoji: emoji.to_owned(),
        };
        actions.perform(react("👀")).await.unwrap();
        actions.perform(unreact("👍")).await.unwrap();
        actions.perform(unreact("✅")).await.unwrap();
        actions.perform(unreact("👀")).await.unwrap();
        let sent = bodies(&server, "setMessageReaction").await;
        assert_eq!(
            sent,
            [
                json!({ "chat_id": GROUP, "message_id": 40,
                    "reaction": [{ "type": "emoji", "emoji": "👀" }] }),
                json!({ "chat_id": GROUP, "message_id": 40, "reaction": [] }),
            ]
        );
    }

    #[test]
    fn reactions_past_the_cap_forget_the_oldest() {
        let mut mine = Mine::default();
        let count = i32::try_from(REACTIONS).unwrap();
        for message in 1..=count {
            mine.set((GROUP, message), "👀");
        }
        mine.set((GROUP, 1), "👍");
        assert_eq!(mine.order.len(), REACTIONS);
        mine.set((GROUP, count + 1), "👀");
        assert_eq!(mine.get((GROUP, 1)), None);
        assert_eq!(mine.get((GROUP, 2)), Some("👀"));
        mine.clear((GROUP, 2));
        assert_eq!(mine.get((GROUP, 2)), None);
        assert_eq!(
            (mine.set.len(), mine.order.len()),
            (REACTIONS - 1, REACTIONS - 1)
        );
    }

    #[tokio::test]
    async fn what_telegram_cannot_do_fails_before_any_call() {
        let mut actions = offline();
        let cases = [
            (
                OutboundAction::React {
                    room: GROUP.to_string().into(),
                    message: "1".into(),
                    emoji: "2️⃣".to_owned(),
                },
                "Telegram has no digit reactions; use a poll",
            ),
            (
                OutboundAction::React {
                    room: GROUP.to_string().into(),
                    message: "1".into(),
                    emoji: "✅".to_owned(),
                },
                "Telegram does not allow ✅ as a reaction",
            ),
            (
                OutboundAction::FetchHistory {
                    room: GROUP.to_string().into(),
                    limit: 50,
                    before: None,
                },
                "Telegram keeps no history for bots",
            ),
            (
                OutboundAction::CreatePoll {
                    room: GROUP.to_string().into(),
                    question: "when?".to_owned(),
                    options: (1..=13).map(|n| n.to_string()).collect(),
                    multiple: false,
                    hours: 24,
                },
                "Telegram allows at most 12 poll options, got 13",
            ),
            (
                OutboundAction::SendMessage {
                    room: "general".into(),
                    text: "hi".to_owned(),
                    reply_to: None,
                },
                "`general` is not a Telegram room id",
            ),
            (
                OutboundAction::SendMessage {
                    room: GROUP.to_string().into(),
                    text: "  ".to_owned(),
                    reply_to: None,
                },
                "nothing to send: the message is empty",
            ),
        ];
        for (action, expected) in cases {
            let error = actions.perform(action).await.unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    #[tokio::test]
    async fn typing_and_pins_name_the_topic_and_stay_quiet() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ok(json!(true)))
            .mount(&server)
            .await;
        let mut actions = actions(&server).await;
        actions
            .perform(OutboundAction::Typing {
                room: format!("{GROUP}:12").into(),
            })
            .await
            .unwrap();
        actions
            .perform(OutboundAction::Pin {
                room: format!("{GROUP}:12").into(),
                message: "55".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            bodies(&server, "sendChatAction").await,
            [json!({ "chat_id": GROUP, "message_thread_id": 12, "action": "typing" })]
        );
        assert_eq!(
            bodies(&server, "pinChatMessage").await,
            [json!({ "chat_id": GROUP, "message_id": 55, "disable_notification": true })]
        );
    }

    #[tokio::test]
    async fn a_send_to_a_migrated_group_names_the_new_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "ok": false, "error_code": 400,
                "description": "Bad Request: group chat was upgraded to a supergroup chat",
                "parameters": { "migrate_to_chat_id": -1_009_999 } })))
            .mount(&server)
            .await;
        let error = actions(&server)
            .await
            .perform(OutboundAction::SendMessage {
                room: "-4455".into(),
                text: "hi".to_owned(),
                reply_to: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "the group is now a supergroup with chat id -1009999; put that id under the group's rooms"
        );
    }
}
