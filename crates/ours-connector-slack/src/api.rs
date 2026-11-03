//! The Slack Web API calls the connector makes, over slack-morphism's hyper
//! client with rustls on aws-lc-rs and the bundled Mozilla roots. A call
//! that is rate limited is retried once after the wait Slack asks for.

use serde::{Deserialize, Serialize};
use slack_morphism::api::{
    SlackApiAppsConnectionOpenRequest, SlackApiAuthTestResponse, SlackApiChatDeleteRequest,
    SlackApiConversationsInfoRequest, SlackApiConversationsMembersRequest,
    SlackApiConversationsOpenRequest, SlackApiPinsAddRequest, SlackApiReactionsAddRequest,
    SlackApiReactionsRemoveRequest, SlackApiUserGroupsUsersListRequest, SlackApiUsersInfoRequest,
};
use slack_morphism::errors::SlackClientError;
use slack_morphism::hyper_tokio::{SlackClientHyperConnector, SlackClientHyperHttpsConnector};
use slack_morphism::{
    ClientResult, SlackApiToken, SlackApiTokenValue, SlackChannelId, SlackChannelInfo, SlackClient,
    SlackCursorId, SlackHistoryMessage, SlackReactionName, SlackResponseMetadata, SlackTs,
    SlackUser, SlackUserGroupId, SlackUserId,
};
use std::future::Future;
use std::time::Duration;

/// The longest a rate-limited call waits before its one retry.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);

/// Page size for listing calls.
const LIST_PAGE: u16 = 200;

/// A guard against a cursor that never ends.
const MAX_LIST_PAGES: usize = 500;

/// Error codes that mean the token itself is no good.
const REJECTED: [&str; 6] = [
    "invalid_auth",
    "not_authed",
    "account_inactive",
    "token_revoked",
    "token_expired",
    "not_allowed_token_type",
];

/// A failed call: the Web API method and what Slack said.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{method}: {code}")]
pub struct ApiError {
    pub method: &'static str,
    pub code: String,
}

impl ApiError {
    pub fn new(method: &'static str, code: impl Into<String>) -> Self {
        ApiError {
            method,
            code: code.into(),
        }
    }

    fn from_client(method: &'static str, error: &SlackClientError) -> Self {
        let code = match error {
            SlackClientError::ApiError(e) => e.code.clone(),
            SlackClientError::RateLimitError(_) => "ratelimited".to_owned(),
            SlackClientError::HttpError(e) => format!("HTTP {}", e.status_code),
            SlackClientError::ProtocolError(e) => format!("unreadable reply: {}", e.json_error),
            other => other.to_string(),
        };
        ApiError::new(method, code)
    }

    /// Whether Slack refused the token rather than the request.
    pub fn rejects_token(&self) -> bool {
        REJECTED.contains(&self.code.as_str())
    }
}

/// One page of history, newest first for a channel and oldest first for a thread.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub messages: Vec<SlackHistoryMessage>,
    pub next: Option<String>,
}

/// The body of `chat.postMessage`. slack-morphism's request has no `mrkdwn`.
#[derive(Debug, Serialize)]
struct PostMessage<'a> {
    channel: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
    mrkdwn: bool,
    link_names: bool,
    unfurl_links: bool,
    unfurl_media: bool,
}

#[derive(Debug, Serialize)]
struct UpdateMessage<'a> {
    channel: &'a str,
    ts: &'a str,
    text: &'a str,
    link_names: bool,
}

#[derive(Debug, Deserialize)]
struct Posted {
    ts: SlackTs,
}

#[derive(Debug, Deserialize)]
struct Empty {}

#[derive(Debug, Deserialize)]
struct Groups {
    #[serde(default)]
    usergroups: Vec<Group>,
}

#[derive(Debug, Deserialize)]
struct Group {
    id: String,
    #[serde(default)]
    handle: String,
}

#[derive(Debug)]
pub struct Api {
    client: SlackClient<SlackClientHyperHttpsConnector>,
    bot: SlackApiToken,
    app: SlackApiToken,
}

fn token(value: &str) -> SlackApiToken {
    SlackApiToken::new(SlackApiTokenValue(value.to_owned()))
}

fn cursor_of(next: Option<&SlackCursorId>) -> Option<SlackCursorId> {
    next.filter(|c| !c.0.is_empty()).cloned()
}

impl Api {
    pub fn new(tls: rustls::ClientConfig, bot_token: &str, app_token: &str) -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_only()
            .enable_http2()
            .build();
        Api {
            client: SlackClient::new(SlackClientHyperConnector::from(https)),
            bot: token(bot_token),
            app: token(app_token),
        }
    }

    async fn call<T, F, Fut>(&self, method: &'static str, f: F) -> Result<T, ApiError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = ClientResult<T>>,
    {
        match f().await {
            Err(SlackClientError::RateLimitError(limit)) => {
                let wait = limit
                    .retry_after
                    .unwrap_or(Duration::from_secs(1))
                    .min(MAX_RETRY_WAIT);
                tracing::warn!(
                    method,
                    wait_secs = wait.as_secs(),
                    "rate limited; retrying once"
                );
                tokio::time::sleep(wait).await;
                f().await.map_err(|e| ApiError::from_client(method, &e))
            }
            other => other.map_err(|e| ApiError::from_client(method, &e)),
        }
    }

    pub async fn auth_test(&self) -> Result<SlackApiAuthTestResponse, ApiError> {
        self.call("auth.test", || async {
            self.client.open_session(&self.bot).auth_test().await
        })
        .await
    }

    /// A fresh Socket Mode URL, asked for with the app-level token.
    pub async fn socket_url(&self) -> Result<String, ApiError> {
        let opened = self
            .call("apps.connections.open", || async {
                self.client
                    .open_session(&self.app)
                    .apps_connections_open(&SlackApiAppsConnectionOpenRequest::new())
                    .await
            })
            .await?;
        Ok(opened.url.0.to_string())
    }

    pub async fn users(&self) -> Result<Vec<SlackUser>, ApiError> {
        let limit = LIST_PAGE.to_string();
        let mut users = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = vec![
                ("limit", Some(limit.as_str())),
                ("cursor", cursor.as_deref()),
            ];
            let page: RawList = self
                .call("users.list", || async {
                    self.client
                        .open_session(&self.bot)
                        .http_session_api
                        .http_get("users.list", &params, None)
                        .await
                })
                .await?;
            cursor = page.next();
            users.extend(each(page.members, "user"));
            if cursor.is_none() {
                break;
            }
        }
        Ok(users)
    }

    pub async fn user(&self, id: &SlackUserId) -> Result<SlackUser, ApiError> {
        let request = SlackApiUsersInfoRequest::new(id.clone());
        let info = self
            .call("users.info", || async {
                self.client
                    .open_session(&self.bot)
                    .users_info(&request)
                    .await
            })
            .await?;
        Ok(info.user)
    }

    /// Every channel, private channel and group DM the bot is in.
    pub async fn conversations(&self) -> Result<Vec<SlackChannelInfo>, ApiError> {
        let limit = LIST_PAGE.to_string();
        let mut channels = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let params = vec![
                ("limit", Some(limit.as_str())),
                ("exclude_archived", Some("true")),
                ("types", Some("public_channel,private_channel,mpim")),
                ("cursor", cursor.as_deref()),
            ];
            let page: RawList = self
                .call("users.conversations", || async {
                    self.client
                        .open_session(&self.bot)
                        .http_session_api
                        .http_get("users.conversations", &params, None)
                        .await
                })
                .await?;
            cursor = page.next();
            channels.extend(each(page.channels, "channel"));
            if cursor.is_none() {
                break;
            }
        }
        Ok(channels)
    }

    pub async fn conversation(&self, id: &SlackChannelId) -> Result<SlackChannelInfo, ApiError> {
        let request = SlackApiConversationsInfoRequest::new(id.clone());
        let info = self
            .call("conversations.info", || async {
                self.client
                    .open_session(&self.bot)
                    .conversations_info(&request)
                    .await
            })
            .await?;
        Ok(info.channel)
    }

    pub async fn members(&self, id: &SlackChannelId) -> Result<Vec<SlackUserId>, ApiError> {
        let mut members = Vec::new();
        let mut cursor = None;
        for _ in 0..MAX_LIST_PAGES {
            let request = SlackApiConversationsMembersRequest::new()
                .with_channel(id.clone())
                .with_limit(LIST_PAGE)
                .opt_cursor(cursor.clone());
            let page = self
                .call("conversations.members", || async {
                    self.client
                        .open_session(&self.bot)
                        .conversations_members(&request)
                        .await
                })
                .await?;
            members.extend(page.members);
            cursor = cursor_of(
                page.response_metadata
                    .as_ref()
                    .and_then(|m| m.next_cursor.as_ref()),
            );
            if cursor.is_none() {
                break;
            }
        }
        Ok(members)
    }

    /// The id of the user group with this handle, or this id. The list is
    /// read leniently because group records vary between plans.
    pub async fn usergroup(&self, wanted: &str) -> Result<Option<SlackUserGroupId>, ApiError> {
        let wanted = wanted.trim().trim_start_matches('@');
        let params = vec![("include_disabled", Some("false"))];
        let groups: Groups = self
            .call("usergroups.list", || async {
                self.client
                    .open_session(&self.bot)
                    .http_session_api
                    .http_get("usergroups.list", &params, None)
                    .await
            })
            .await?;
        Ok(groups
            .usergroups
            .into_iter()
            .find(|g| g.id == wanted || g.handle.eq_ignore_ascii_case(wanted))
            .map(|g| SlackUserGroupId(g.id)))
    }

    pub async fn usergroup_users(
        &self,
        id: &SlackUserGroupId,
    ) -> Result<Vec<SlackUserId>, ApiError> {
        let request = SlackApiUserGroupsUsersListRequest::new(id.clone());
        let list = self
            .call("usergroups.users.list", || async {
                self.client
                    .open_session(&self.bot)
                    .usergroups_users_list(&request)
                    .await
            })
            .await?;
        Ok(list.users)
    }

    /// Post plain text: no markup, no name linking, no previews.
    pub async fn post(
        &self,
        channel: &SlackChannelId,
        text: &str,
        thread: Option<&SlackTs>,
    ) -> Result<SlackTs, ApiError> {
        let body = PostMessage {
            channel: &channel.0,
            text,
            thread_ts: thread.map(|t| t.0.as_str()),
            mrkdwn: false,
            link_names: false,
            unfurl_links: false,
            unfurl_media: false,
        };
        let posted: Posted = self
            .call("chat.postMessage", || async {
                self.client
                    .open_session(&self.bot)
                    .http_session_api
                    .http_post("chat.postMessage", &body, None)
                    .await
            })
            .await?;
        Ok(posted.ts)
    }

    pub async fn update(
        &self,
        channel: &SlackChannelId,
        ts: &SlackTs,
        text: &str,
    ) -> Result<(), ApiError> {
        let body = UpdateMessage {
            channel: &channel.0,
            ts: &ts.0,
            text,
            link_names: false,
        };
        let _: Empty = self
            .call("chat.update", || async {
                self.client
                    .open_session(&self.bot)
                    .http_session_api
                    .http_post("chat.update", &body, None)
                    .await
            })
            .await?;
        Ok(())
    }

    pub async fn delete(&self, channel: &SlackChannelId, ts: &SlackTs) -> Result<(), ApiError> {
        let request = SlackApiChatDeleteRequest::new(channel.clone(), ts.clone());
        self.call("chat.delete", || async {
            self.client
                .open_session(&self.bot)
                .chat_delete(&request)
                .await
        })
        .await
        .map(|_| ())
    }

    /// Add a reaction. One already there counts as done.
    pub async fn react(
        &self,
        channel: &SlackChannelId,
        ts: &SlackTs,
        name: &str,
    ) -> Result<(), ApiError> {
        let request = SlackApiReactionsAddRequest::new(
            channel.clone(),
            SlackReactionName(name.to_owned()),
            ts.clone(),
        );
        let result = self
            .call("reactions.add", || async {
                self.client
                    .open_session(&self.bot)
                    .reactions_add(&request)
                    .await
            })
            .await;
        idempotent(result.map(|_| ()), "already_reacted")
    }

    /// Remove the agent's reaction. One already gone counts as done.
    pub async fn unreact(
        &self,
        channel: &SlackChannelId,
        ts: &SlackTs,
        name: &str,
    ) -> Result<(), ApiError> {
        let request = SlackApiReactionsRemoveRequest::new(SlackReactionName(name.to_owned()))
            .with_channel(channel.clone())
            .with_timestamp(ts.clone());
        let result = self
            .call("reactions.remove", || async {
                self.client
                    .open_session(&self.bot)
                    .reactions_remove(&request)
                    .await
            })
            .await;
        idempotent(result.map(|_| ()), "no_reaction")
    }

    /// Pin a message. One already pinned counts as done.
    pub async fn pin(&self, channel: &SlackChannelId, ts: &SlackTs) -> Result<(), ApiError> {
        let request = SlackApiPinsAddRequest::new(channel.clone(), ts.clone());
        let result = self
            .call("pins.add", || async {
                self.client.open_session(&self.bot).pins_add(&request).await
            })
            .await;
        idempotent(result.map(|_| ()), "already_pinned")
    }

    pub async fn open_dm(&self, user: &SlackUserId) -> Result<SlackChannelId, ApiError> {
        let request = SlackApiConversationsOpenRequest::new().with_users(vec![user.clone()]);
        let opened = self
            .call("conversations.open", || async {
                self.client
                    .open_session(&self.bot)
                    .conversations_open(&request)
                    .await
            })
            .await?;
        Ok(opened.channel.id)
    }

    pub async fn history_page(
        &self,
        channel: &SlackChannelId,
        latest: Option<&SlackTs>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<Page, ApiError> {
        let limit = limit.to_string();
        let params = vec![
            ("channel", Some(channel.0.as_str())),
            ("limit", Some(limit.as_str())),
            ("latest", latest.map(|t| t.0.as_str())),
            ("cursor", cursor),
        ];
        let page: RawPage = self
            .call("conversations.history", || async {
                self.client
                    .open_session(&self.bot)
                    .http_session_api
                    .http_get("conversations.history", &params, None)
                    .await
            })
            .await?;
        Ok(page.into_page())
    }

    pub async fn replies_page(
        &self,
        channel: &SlackChannelId,
        thread: &SlackTs,
        latest: Option<&SlackTs>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<Page, ApiError> {
        let limit = limit.to_string();
        let params = vec![
            ("channel", Some(channel.0.as_str())),
            ("ts", Some(thread.0.as_str())),
            ("limit", Some(limit.as_str())),
            ("latest", latest.map(|t| t.0.as_str())),
            ("cursor", cursor),
        ];
        let page: RawPage = self
            .call("conversations.replies", || async {
                self.client
                    .open_session(&self.bot)
                    .http_session_api
                    .http_get("conversations.replies", &params, None)
                    .await
            })
            .await?;
        Ok(page.into_page())
    }
}

/// A listing with each entry left as JSON, read one by one with `each`.
#[derive(Debug, Deserialize)]
struct RawList {
    #[serde(default)]
    members: Vec<serde_json::Value>,
    #[serde(default)]
    channels: Vec<serde_json::Value>,
    response_metadata: Option<SlackResponseMetadata>,
}

impl RawList {
    fn next(&self) -> Option<String> {
        cursor_of(
            self.response_metadata
                .as_ref()
                .and_then(|m| m.next_cursor.as_ref()),
        )
        .map(|c| c.0)
    }
}

/// Every entry that fits `T`, skipping the ones that do not.
fn each<T: serde::de::DeserializeOwned>(values: Vec<serde_json::Value>, what: &str) -> Vec<T> {
    values
        .into_iter()
        .filter_map(|value| {
            serde_json::from_value(value)
                .map_err(|error| tracing::warn!(what, %error, "skipped an unreadable entry"))
                .ok()
        })
        .collect()
}

/// A history reply with each message left as JSON, so one message that does
/// not fit slack-morphism's model is skipped instead of failing the page.
#[derive(Debug, Deserialize)]
struct RawPage {
    #[serde(default)]
    messages: Vec<serde_json::Value>,
    has_more: Option<bool>,
    response_metadata: Option<SlackResponseMetadata>,
}

impl RawPage {
    fn into_page(self) -> Page {
        let next = (self.has_more != Some(false))
            .then(|| {
                cursor_of(
                    self.response_metadata
                        .as_ref()
                        .and_then(|m| m.next_cursor.as_ref()),
                )
            })
            .flatten()
            .map(|c| c.0);
        let messages = self
            .messages
            .into_iter()
            .filter_map(|mut message| {
                crate::event::without_decorations(&mut message);
                serde_json::from_value(message)
                    .map_err(
                        |error| tracing::debug!(%error, "skipped an unreadable history message"),
                    )
                    .ok()
            })
            .collect();
        Page { messages, next }
    }
}

/// Treat one error code as success, for calls that are safe to repeat.
fn idempotent(result: Result<(), ApiError>, done: &str) -> Result<(), ApiError> {
    match result {
        Err(e) if e.code == done => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slack_morphism::errors::{SlackClientApiError, SlackRateLimitError};

    #[test]
    fn errors_name_the_method_and_slacks_code() {
        let api = SlackClientError::ApiError(
            SlackClientApiError::new("not_in_channel".to_owned())
                .with_http_response_body("{\"ok\":false}".to_owned()),
        );
        let mapped = ApiError::from_client("chat.postMessage", &api);
        assert_eq!(mapped.to_string(), "chat.postMessage: not_in_channel");
        assert!(!mapped.rejects_token());

        let limited = SlackClientError::RateLimitError(SlackRateLimitError::new());
        assert_eq!(
            ApiError::from_client("reactions.add", &limited).to_string(),
            "reactions.add: ratelimited"
        );
    }

    #[test]
    fn token_failures_are_told_apart_from_request_failures() {
        for code in REJECTED {
            assert!(ApiError::new("auth.test", code).rejects_token(), "{code}");
        }
        assert!(!ApiError::new("auth.test", "ratelimited").rejects_token());
        assert!(!ApiError::new("users.info", "user_not_found").rejects_token());
    }

    #[test]
    fn repeatable_calls_ignore_their_already_done_code() {
        assert_eq!(
            idempotent(
                Err(ApiError::new("reactions.add", "already_reacted")),
                "already_reacted"
            ),
            Ok(())
        );
        assert!(
            idempotent(
                Err(ApiError::new("reactions.add", "no_item")),
                "already_reacted"
            )
            .is_err()
        );
        assert_eq!(idempotent(Ok(()), "already_reacted"), Ok(()));
    }

    #[test]
    fn a_post_is_plain_text_with_no_previews() {
        let body = PostMessage {
            channel: "C1",
            text: "*not bold*",
            thread_ts: Some("100.000001"),
            mrkdwn: false,
            link_names: false,
            unfurl_links: false,
            unfurl_media: false,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["mrkdwn"], false);
        assert_eq!(json["link_names"], false);
        assert_eq!(json["unfurl_links"], false);
        assert_eq!(json["thread_ts"], "100.000001");
        assert!(json.get("blocks").is_none());
        let top = PostMessage {
            thread_ts: None,
            ..body
        };
        assert!(
            serde_json::to_value(&top)
                .unwrap()
                .get("thread_ts")
                .is_none()
        );
    }

    #[test]
    fn one_odd_message_does_not_sink_a_history_page() {
        let raw: RawPage = serde_json::from_value(serde_json::json!({
            "ok": true,
            "messages": [
                { "type": "message", "user": "U1", "text": "kept", "ts": "2.000000",
                  "attachments": [{ "id": "not-a-number" }] },
                { "type": "message", "user": "U1", "text": "no ts" },
                { "type": "message", "user": "U2", "text": "also kept", "ts": "1.000000" }
            ],
            "has_more": true,
            "response_metadata": { "next_cursor": "bmV4dA==" }
        }))
        .unwrap();
        let page = raw.into_page();
        let texts: Vec<_> = page
            .messages
            .iter()
            .map(|m| m.content.text.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(texts, ["kept", "also kept"]);
        assert_eq!(page.next.as_deref(), Some("bmV4dA=="));
    }

    #[test]
    fn the_last_page_has_no_next_cursor() {
        for reply in [
            serde_json::json!({ "messages": [], "has_more": false,
                "response_metadata": { "next_cursor": "stale" } }),
            serde_json::json!({ "messages": [], "response_metadata": { "next_cursor": "" } }),
            serde_json::json!({ "messages": [] }),
        ] {
            let raw: RawPage = serde_json::from_value(reply.clone()).unwrap();
            assert_eq!(raw.into_page().next, None, "{reply}");
        }
    }

    #[test]
    fn one_odd_user_or_channel_does_not_sink_a_listing() {
        let raw: RawList = serde_json::from_value(serde_json::json!({
            "ok": true,
            "members": [{ "id": "U1", "name": "sam" }, { "name": "no id" }],
            "channels": [{ "id": "C1", "name": "launch", "created": 1 }, { "id": "C2" }],
            "response_metadata": { "next_cursor": "" }
        }))
        .unwrap();
        assert_eq!(raw.next(), None);
        let users: Vec<SlackUser> = each(raw.members, "user");
        let channels: Vec<SlackChannelInfo> = each(raw.channels, "channel");
        assert_eq!(users.len(), 1);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id.0, "C1");
    }

    #[test]
    fn empty_cursors_end_paging() {
        assert_eq!(cursor_of(Some(&SlackCursorId(String::new()))), None);
        assert_eq!(
            cursor_of(Some(&SlackCursorId("abc".to_owned()))),
            Some(SlackCursorId("abc".to_owned()))
        );
        assert_eq!(cursor_of(None), None);
    }
}
