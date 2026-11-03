//! The Bot API client: frankenstein's types and calls over the workspace's
//! reqwest. Long polls and actions use separate clients so a slow send
//! never shares a timeout with a 30 second poll. Every failure becomes an
//! `ApiError`; a flood-control 429 on an action is retried once.

use crate::map;
use frankenstein::AsyncTelegramApi;
use frankenstein::client_reqwest::Bot;
use frankenstein::methods::{
    DeleteMessageParams, DeleteWebhookParams, EditMessageTextParams, GetChatAdministratorsParams,
    GetChatParams, GetUpdatesParams, PinChatMessageParams, SendChatActionParams, SendMessageParams,
    SendPollParams, SetMessageReactionParams, StopPollParams,
};
use frankenstein::response::MethodResponse;
use frankenstein::types::{AllowedUpdate, ChatFullInfo, Message as TgMessage, Poll, User};
use ours_types::PlatformRole;
use serde_json::Value;
use std::future::Future;
use std::time::Duration;

/// The public Bot API server.
pub const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// How long Telegram holds a `getUpdates` call open when there is nothing new.
pub const POLL_SECONDS: u32 = 30;

/// The updates the connector reads. `chat_member` and `message_reaction`
/// are only sent when named here and only to a bot that is an administrator.
pub const ALLOWED_UPDATES: [AllowedUpdate; 7] = [
    AllowedUpdate::Message,
    AllowedUpdate::EditedMessage,
    AllowedUpdate::MessageReaction,
    AllowedUpdate::Poll,
    AllowedUpdate::PollAnswer,
    AllowedUpdate::ChatMember,
    AllowedUpdate::MyChatMember,
];

/// The longest flood-control wait an action sits out before its one retry.
const MAX_RETRY_WAIT: u16 = 60;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error("Telegram refused the bot token ({0})")]
    Unauthorized(String),
    #[error("{description}")]
    Refused {
        code: u64,
        description: String,
        retry_after: Option<u16>,
        migrate_to: Option<i64>,
    },
    #[error("could not reach Telegram: {0}")]
    Http(String),
    #[error("could not read Telegram's answer: {0}")]
    Decode(String),
}

impl ApiError {
    pub fn code(&self) -> Option<u64> {
        match self {
            ApiError::Unauthorized(_) => Some(401),
            ApiError::Refused { code, .. } => Some(*code),
            ApiError::Http(_) | ApiError::Decode(_) => None,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            ApiError::Unauthorized(d) | ApiError::Http(d) | ApiError::Decode(d) => d,
            ApiError::Refused { description, .. } => description,
        }
    }
}

impl From<frankenstein::Error> for ApiError {
    fn from(error: frankenstein::Error) -> Self {
        match error {
            frankenstein::Error::Api(response) if response.error_code == 401 => {
                ApiError::Unauthorized(response.description)
            }
            frankenstein::Error::Api(response) => ApiError::Refused {
                code: response.error_code,
                description: response.description,
                retry_after: response.parameters.and_then(|p| p.retry_after),
                migrate_to: response.parameters.and_then(|p| p.migrate_to_chat_id),
            },
            frankenstein::Error::HttpReqwest(error) => ApiError::Http(error.to_string()),
            frankenstein::Error::JsonDecode { source, .. }
            | frankenstein::Error::JsonEncode { source, .. } => {
                ApiError::Decode(source.to_string())
            }
            other => ApiError::Http(other.to_string()),
        }
    }
}

pub struct Api {
    poller: Bot,
    bot: Bot,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Api { .. }")
    }
}

fn client(timeout: Duration) -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .map_err(|e| ApiError::Http(e.to_string()))
}

/// Run a call, and once more after the wait Telegram names if it answers 429.
async fn once_more<T, F, Fut>(call: F) -> Result<T, ApiError>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<MethodResponse<T>, frankenstein::Error>>,
{
    match call().await.map_err(ApiError::from) {
        Err(ApiError::Refused {
            code: 429,
            retry_after: Some(wait),
            ..
        }) if wait <= MAX_RETRY_WAIT => {
            tracing::info!(wait, "Telegram asked the agent to slow down; retrying once");
            tokio::time::sleep(Duration::from_secs(u64::from(wait))).await;
            call().await.map(|r| r.result).map_err(ApiError::from)
        }
        other => other.map(|r| r.result),
    }
}

impl Api {
    /// A client for the Bot API at `base`, such as `https://api.telegram.org`.
    pub fn new(base: &str, token: &str) -> Result<Self, ApiError> {
        let url = format!("{}/bot{token}", base.trim_end_matches('/'));
        let poll_timeout = Duration::from_secs(u64::from(POLL_SECONDS) + 15);
        Ok(Api {
            poller: Bot::builder()
                .api_url(url.clone())
                .client(client(poll_timeout)?)
                .build(),
            bot: Bot::builder()
                .api_url(url)
                .client(client(Duration::from_secs(30))?)
                .build(),
        })
    }

    pub async fn me(&self) -> Result<User, ApiError> {
        Ok(self.bot.get_me().await?.result)
    }

    /// The next batch of updates as raw JSON, so one update this client
    /// cannot read never holds up the rest.
    pub async fn updates(&self, offset: Option<i64>) -> Result<Vec<Value>, ApiError> {
        let params = GetUpdatesParams::builder()
            .maybe_offset(offset)
            .timeout(POLL_SECONDS)
            .allowed_updates(ALLOWED_UPDATES.to_vec())
            .build();
        let response: MethodResponse<Vec<Value>> =
            self.poller.request("getUpdates", Some(&params)).await?;
        Ok(response.result)
    }

    /// Remove a webhook, which blocks long polling. Pending updates are kept.
    pub async fn delete_webhook(&self) -> Result<(), ApiError> {
        let params = DeleteWebhookParams::builder()
            .drop_pending_updates(false)
            .build();
        self.bot.delete_webhook(&params).await?;
        Ok(())
    }

    pub async fn chat(&self, chat: i64) -> Result<ChatFullInfo, ApiError> {
        let params = GetChatParams::builder().chat_id(chat).build();
        once_more(|| self.bot.get_chat(&params)).await
    }

    /// A group's administrators, each read on its own.
    pub async fn admins(&self, chat: i64) -> Result<Vec<(User, PlatformRole)>, ApiError> {
        let params = GetChatAdministratorsParams::builder().chat_id(chat).build();
        let raw: Vec<Value> = once_more(|| {
            self.bot
                .request::<_, MethodResponse<Vec<Value>>>("getChatAdministrators", Some(&params))
        })
        .await?;
        Ok(raw.iter().filter_map(map::admin_entry).collect())
    }

    pub async fn send_message(&self, params: &SendMessageParams) -> Result<TgMessage, ApiError> {
        once_more(|| self.bot.send_message(params)).await
    }

    pub async fn edit_text(&self, params: &EditMessageTextParams) -> Result<(), ApiError> {
        once_more(|| self.bot.edit_message_text(params)).await?;
        Ok(())
    }

    pub async fn delete_message(&self, params: &DeleteMessageParams) -> Result<(), ApiError> {
        once_more(|| self.bot.delete_message(params)).await?;
        Ok(())
    }

    pub async fn set_reaction(&self, params: &SetMessageReactionParams) -> Result<(), ApiError> {
        once_more(|| self.bot.set_message_reaction(params)).await?;
        Ok(())
    }

    pub async fn send_poll(&self, params: &SendPollParams) -> Result<TgMessage, ApiError> {
        once_more(|| self.bot.send_poll(params)).await
    }

    pub async fn stop_poll(&self, params: &StopPollParams) -> Result<Poll, ApiError> {
        once_more(|| self.bot.stop_poll(params)).await
    }

    pub async fn pin(&self, params: &PinChatMessageParams) -> Result<(), ApiError> {
        once_more(|| self.bot.pin_chat_message(params)).await?;
        Ok(())
    }

    pub async fn chat_action(&self, params: &SendChatActionParams) -> Result<(), ApiError> {
        once_more(|| self.bot.send_chat_action(params)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

    fn sent(id: i32) -> Value {
        json!({ "ok": true, "result": { "message_id": id, "date": 1,
            "chat": { "id": -100, "type": "supergroup", "title": "x" }, "text": "hi" } })
    }

    fn message(chat: i64) -> SendMessageParams {
        SendMessageParams::builder()
            .chat_id(chat)
            .text("hi")
            .build()
    }

    #[tokio::test]
    async fn a_429_is_retried_once_after_the_wait() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 0",
                "parameters": { "retry_after": 0 } })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(200).set_body_json(sent(77)))
            .expect(1)
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();
        let message = api.send_message(&message(-100)).await.unwrap();
        assert_eq!(message.message_id, 77);
    }

    #[tokio::test]
    async fn a_second_429_is_a_failure_with_telegrams_words() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 0",
                "parameters": { "retry_after": 0 } })))
            .expect(2)
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();
        let error = api.send_message(&message(-100)).await.unwrap_err();
        assert_eq!(error.code(), Some(429));
        assert_eq!(error.to_string(), "Too Many Requests: retry after 0");
    }

    #[tokio::test]
    async fn a_long_flood_wait_is_not_sat_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "ok": false, "error_code": 429, "description": "Too Many Requests: retry after 3600",
                "parameters": { "retry_after": 3600 } })))
            .expect(1)
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();
        let error = api.send_message(&message(-100)).await.unwrap_err();
        assert_eq!(error.code(), Some(429));
    }

    #[tokio::test]
    async fn errors_are_classified_without_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/getMe")))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "ok": false, "error_code": 401, "description": "Unauthorized" })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/sendMessage")))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "ok": false, "error_code": 400,
                "description": "Bad Request: group chat was upgraded to a supergroup chat",
                "parameters": { "migrate_to_chat_id": -1_009_999 } })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/getChat")))
            .respond_with(ResponseTemplate::new(502).set_body_string("<html>bad gateway</html>"))
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();

        let unauthorized = api.me().await.unwrap_err();
        assert_eq!(
            unauthorized,
            ApiError::Unauthorized("Unauthorized".to_owned())
        );
        assert_eq!(
            unauthorized.to_string(),
            "Telegram refused the bot token (Unauthorized)"
        );

        let migrated = api.send_message(&message(-5)).await.unwrap_err();
        assert!(matches!(
            migrated,
            ApiError::Refused {
                code: 400,
                migrate_to: Some(-1_009_999),
                ..
            }
        ));

        let gateway = api.chat(-5).await.unwrap_err();
        assert!(matches!(gateway, ApiError::Decode(_)), "{gateway:?}");

        let unreachable = Api::new("http://127.0.0.1:9", TOKEN).unwrap();
        let down = unreachable.me().await.unwrap_err();
        assert!(matches!(down, ApiError::Http(_)), "{down:?}");
        for error in [unauthorized, migrated, gateway, down] {
            assert!(!error.to_string().contains("ABC-DEF"), "{error}");
        }
    }

    #[tokio::test]
    async fn updates_ask_for_what_the_connector_reads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/getUpdates")))
            .and(body_partial_json(json!({
                "offset": 8, "timeout": 30,
                "allowed_updates": ["message", "edited_message", "message_reaction", "poll",
                    "poll_answer", "chat_member", "my_chat_member"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true, "result": [{ "update_id": 8, "something_new": {} }] })))
            .expect(1)
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();
        let batch = api.updates(Some(8)).await.unwrap();
        assert_eq!(batch, [json!({ "update_id": 8, "something_new": {} })]);
    }

    #[tokio::test]
    async fn an_administrator_entry_the_client_cannot_read_is_still_counted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/bot{TOKEN}/getChatAdministrators")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": [
                { "status": "creator", "is_anonymous": false,
                    "user": { "id": 11, "is_bot": false, "first_name": "Sam" } },
                { "status": "administrator", "user": { "id": 12, "is_bot": false, "first_name": "Jo" } }
            ] })))
            .mount(&server)
            .await;
        let api = Api::new(&server.uri(), TOKEN).unwrap();
        let admins = api.admins(-100).await.unwrap();
        let roles: Vec<_> = admins.iter().map(|(u, r)| (u.id, *r)).collect();
        assert_eq!(
            roles,
            [(11, PlatformRole::Owner), (12, PlatformRole::Admin)]
        );
    }
}
