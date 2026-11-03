//! Carrying out `OutboundAction`s over Discord's REST API. Rate limits are
//! queued by twilight-http; a failure comes back as an error for the caller
//! to report, never a panic.
//!
//! History is read a page at a time. Discord returns at most 100 messages a
//! call, newest first, so a larger request loops backwards from the oldest id
//! of the last page.

use crate::map::{self, EmojiRef, MapError};
use crate::poll::{self, PollError};
use crate::split::{MESSAGE_LIMIT, split_message, truncate_edit};
use crate::state::{DmRooms, dm_sync};
use ours_types::{InboundEvent, OutboundAction, PlatformMessageId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use twilight_http::Client;
use twilight_http::request::channel::reaction::RequestReactionType;
use twilight_model::channel::Message as TwMessage;
use twilight_model::channel::message::AllowedMentions;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker, UserMarker};

/// The most messages Discord returns in one call.
const PAGE: u32 = 100;

/// The most one history request may read, however much the caller asks for.
const MAX_HISTORY: u32 = 1000;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error(transparent)]
    Id(#[from] MapError),
    #[error(transparent)]
    Poll(#[from] PollError),
    #[error("{0}")]
    Request(String),
}

fn request<E: std::fmt::Display>(error: E) -> ActionError {
    ActionError::Request(error.to_string())
}

/// What a finished action produced.
#[derive(Debug, Default)]
pub struct Outcome {
    pub message: Option<PlatformMessageId>,
    /// Events to deliver before the action's own result.
    pub events: Vec<InboundEvent>,
}

#[derive(Debug)]
pub struct Actions {
    http: Arc<Client>,
    agent_name: String,
    me: Arc<OnceLock<Id<UserMarker>>>,
    dms: DmRooms,
    dm_channels: HashMap<Id<UserMarker>, Id<ChannelMarker>>,
}

impl Actions {
    pub fn new(
        http: Arc<Client>,
        agent_name: String,
        me: Arc<OnceLock<Id<UserMarker>>>,
        dms: DmRooms,
    ) -> Self {
        Actions {
            http,
            agent_name,
            me,
            dms,
            dm_channels: HashMap::new(),
        }
    }

    pub async fn perform(&mut self, action: OutboundAction) -> Result<Outcome, ActionError> {
        match action {
            OutboundAction::SendMessage {
                room,
                text,
                reply_to,
            } => {
                let channel = map::parse_id(room.as_str())?;
                let reply = reply_to
                    .as_ref()
                    .map(|id| map::parse_id(id.as_str()))
                    .transpose()?;
                self.send(channel, &text, reply).await
            }
            OutboundAction::EditMessage { room, id, text } => {
                let channel = map::parse_id(room.as_str())?;
                let message = map::parse_id(id.as_str())?;
                let mentions = AllowedMentions::default();
                let text = truncate_edit(&text, MESSAGE_LIMIT);
                self.http
                    .update_message(channel, message)
                    .content(Some(&text))
                    .allowed_mentions(Some(&mentions))
                    .await
                    .map_err(request)?;
                Ok(Outcome::default())
            }
            OutboundAction::DeleteMessage { room, id } => {
                let channel = map::parse_id(room.as_str())?;
                let message = map::parse_id(id.as_str())?;
                self.http
                    .delete_message(channel, message)
                    .await
                    .map_err(request)?;
                Ok(Outcome::default())
            }
            OutboundAction::React {
                room,
                message,
                emoji,
            } => {
                let channel = map::parse_id(room.as_str())?;
                let message = map::parse_id(message.as_str())?;
                let emoji = map::parse_emoji(&emoji);
                let kind = reaction_type(&emoji);
                self.http
                    .create_reaction(channel, message, &kind)
                    .await
                    .map_err(request)?;
                Ok(Outcome::default())
            }
            OutboundAction::Unreact {
                room,
                message,
                emoji,
            } => {
                let channel = map::parse_id(room.as_str())?;
                let message = map::parse_id(message.as_str())?;
                let emoji = map::parse_emoji(&emoji);
                let kind = reaction_type(&emoji);
                self.http
                    .delete_current_user_reaction(channel, message, &kind)
                    .await
                    .map_err(request)?;
                Ok(Outcome::default())
            }
            OutboundAction::SendDm { user, text } => {
                self.send_dm(map::parse_id(user.as_str())?, &text).await
            }
            OutboundAction::Pin { room, message } => {
                let channel = map::parse_id(room.as_str())?;
                let message = map::parse_id(message.as_str())?;
                self.http
                    .create_pin(channel, message)
                    .await
                    .map_err(request)?;
                Ok(Outcome::default())
            }
            OutboundAction::CreatePoll {
                room,
                question,
                options,
                multiple,
                hours,
            } => {
                let body = poll::create_body(&question, &options, multiple, hours)?;
                let channel = map::parse_id(room.as_str())?;
                self.create_poll(channel, &body).await
            }
            OutboundAction::EndPoll { room, message } => {
                let channel = map::parse_id(room.as_str())?;
                let message = map::parse_id(message.as_str())?;
                self.end_poll(channel, message).await
            }
            OutboundAction::Typing { room } => {
                let channel = map::parse_id(room.as_str())?;
                self.http
                    .create_typing_trigger(channel)
                    .await
                    .map_err(request)?;
                Ok(Outcome::default())
            }
            OutboundAction::FetchHistory {
                room,
                limit,
                before,
            } => {
                let channel = map::parse_id(room.as_str())?;
                let before = before
                    .as_ref()
                    .map(|id| map::parse_id(id.as_str()))
                    .transpose()?;
                self.history(channel, limit, before).await
            }
        }
    }

    async fn send(
        &self,
        channel: Id<ChannelMarker>,
        text: &str,
        reply: Option<Id<twilight_model::id::marker::MessageMarker>>,
    ) -> Result<Outcome, ActionError> {
        let mut first = None;
        for (i, piece) in split_message(text, MESSAGE_LIMIT).iter().enumerate() {
            let reply = reply.filter(|_| i == 0);
            let mentions = AllowedMentions {
                replied_user: reply.is_some(),
                ..AllowedMentions::default()
            };
            let mut create = self
                .http
                .create_message(channel)
                .content(piece)
                .allowed_mentions(Some(&mentions));
            if let Some(reply) = reply {
                create = create.reply(reply).fail_if_not_exists(false);
            }
            let sent = create
                .await
                .map_err(request)?
                .model()
                .await
                .map_err(request)?;
            first.get_or_insert(sent.id);
        }
        Ok(Outcome {
            message: first.map(map::message_id),
            events: Vec::new(),
        })
    }

    /// Post a message made only of a poll. The body is sent as raw JSON
    /// because twilight's `Poll` cannot carry a duration.
    async fn create_poll(
        &self,
        channel: Id<ChannelMarker>,
        body: &serde_json::Value,
    ) -> Result<Outcome, ActionError> {
        let bytes = serde_json::to_vec(body).map_err(request)?;
        let sent = self
            .http
            .create_message(channel)
            .payload_json(&bytes)
            .await
            .map_err(request)?
            .model()
            .await
            .map_err(request)?;
        Ok(Outcome {
            message: Some(map::message_id(sent.id)),
            events: Vec::new(),
        })
    }

    /// End a poll now. The close arrives through the gateway as a message
    /// update, so no event is made here.
    async fn end_poll(
        &self,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
    ) -> Result<Outcome, ActionError> {
        let ended = self
            .http
            .end_poll(channel, message)
            .await
            .map_err(request)?
            .model()
            .await
            .map_err(request)?;
        if ended.poll.is_none() {
            return Err(ActionError::Request("that message has no poll".to_owned()));
        }
        Ok(Outcome::default())
    }

    async fn send_dm(&mut self, user: Id<UserMarker>, text: &str) -> Result<Outcome, ActionError> {
        let mut events = Vec::new();
        let channel = match self.dm_channels.get(&user) {
            Some(channel) => *channel,
            None => {
                let created = self
                    .http
                    .create_private_channel(user)
                    .await
                    .map_err(request)?
                    .model()
                    .await
                    .map_err(request)?;
                self.dm_channels.insert(user, created.id);
                if self.dms.insert(created.id)
                    && let Some(recipient) = created
                        .recipients
                        .as_ref()
                        .and_then(|r| r.iter().find(|u| u.id == user))
                {
                    events.push(dm_sync(created.id, recipient));
                }
                created.id
            }
        };
        let mut outcome = self.send(channel, text, None).await?;
        outcome.events = events;
        Ok(outcome)
    }

    async fn history(
        &self,
        channel: Id<ChannelMarker>,
        limit: u32,
        before: Option<Id<MessageMarker>>,
    ) -> Result<Outcome, ActionError> {
        let limit = clamp_limit(limit);
        let fetched = fetch_pages(&*self.http, channel, before, limit).await?;
        let messages = map::history(
            &fetched,
            self.me.get().copied(),
            &self.agent_name,
            crate::now(),
            limit as usize,
        );
        Ok(Outcome {
            message: None,
            events: vec![InboundEvent::History {
                room: map::room_id(channel),
                messages,
            }],
        })
    }
}

/// One call to Discord's channel messages route.
trait MessagePages {
    /// Up to `limit` messages older than `before`, or the newest when it is
    /// unset, in Discord's order: newest first.
    async fn page(
        &self,
        channel: Id<ChannelMarker>,
        before: Option<Id<MessageMarker>>,
        limit: u16,
    ) -> Result<Vec<TwMessage>, ActionError>;
}

impl MessagePages for Client {
    async fn page(
        &self,
        channel: Id<ChannelMarker>,
        before: Option<Id<MessageMarker>>,
        limit: u16,
    ) -> Result<Vec<TwMessage>, ActionError> {
        let builder = self.channel_messages(channel).limit(limit);
        let response = match before {
            Some(before) => builder.before(before).await,
            None => builder.await,
        };
        response.map_err(request)?.models().await.map_err(request)
    }
}

/// Cap a history request at `MAX_HISTORY`.
fn clamp_limit(limit: u32) -> u32 {
    if limit > MAX_HISTORY {
        tracing::warn!(
            requested = limit,
            max = MAX_HISTORY,
            "history request clamped"
        );
        MAX_HISTORY
    } else {
        limit
    }
}

/// Read backwards from `before` until `limit` messages are in hand or a page
/// comes back short or empty. If nothing a person wrote has turned up, reading
/// goes on past `limit` up to `MAX_HISTORY`, so a run of system notices is not
/// taken for the start of the channel.
async fn fetch_pages(
    pages: &impl MessagePages,
    channel: Id<ChannelMarker>,
    before: Option<Id<MessageMarker>>,
    limit: u32,
) -> Result<Vec<TwMessage>, ActionError> {
    let mut found = Vec::new();
    if limit == 0 {
        return Ok(found);
    }
    let (mut scanned, mut written) = (0u32, 0usize);
    let mut cursor = before;
    loop {
        let target = if scanned < limit {
            limit
        } else if written == 0 && scanned < MAX_HISTORY {
            MAX_HISTORY
        } else {
            break;
        };
        let want = u16::try_from((target - scanned).min(PAGE)).unwrap_or(100);
        let page = pages.page(channel, cursor, want).await?;
        let Some(oldest) = page.iter().map(|m| m.id).min() else {
            break;
        };
        scanned = scanned.saturating_add(u32::try_from(page.len()).unwrap_or(u32::MAX));
        written += page.iter().filter(|m| map::is_user_message(m.kind)).count();
        let short = page.len() < usize::from(want);
        found.extend(page);
        if short {
            break;
        }
        cursor = Some(oldest);
    }
    Ok(found)
}

fn reaction_type(emoji: &EmojiRef) -> RequestReactionType<'_> {
    match emoji {
        EmojiRef::Unicode(name) => RequestReactionType::Unicode { name },
        EmojiRef::Custom { name, id } => RequestReactionType::Custom {
            id: *id,
            name: name.as_deref(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::sync::Mutex;

    const CHANNEL: u64 = 20;

    /// A channel that answers like Discord: the newest messages older than
    /// `before`, newest first, never more than `limit`.
    struct FakeChannel {
        /// Message ids and their Discord type numbers, oldest first.
        messages: Vec<(u64, u8)>,
        calls: Mutex<Vec<(Option<u64>, u16)>>,
        fail_on_call: Option<usize>,
    }

    impl FakeChannel {
        fn with_ids(ids: impl IntoIterator<Item = u64>) -> Self {
            FakeChannel {
                messages: ids.into_iter().map(|id| (id, 0)).collect(),
                calls: Mutex::new(Vec::new()),
                fail_on_call: None,
            }
        }

        fn calls(&self) -> Vec<(Option<u64>, u16)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MessagePages for FakeChannel {
        async fn page(
            &self,
            _channel: Id<ChannelMarker>,
            before: Option<Id<MessageMarker>>,
            limit: u16,
        ) -> Result<Vec<TwMessage>, ActionError> {
            let before = before.map(Id::get);
            let call = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((before, limit));
                calls.len()
            };
            if self.fail_on_call == Some(call) {
                return Err(ActionError::Request("503".to_owned()));
            }
            Ok(self
                .messages
                .iter()
                .rev()
                .filter(|(id, _)| before.is_none_or(|b| *id < b))
                .take(usize::from(limit))
                .map(|(id, kind)| tw_message_of_kind(*id, CHANNEL, "m", *kind))
                .collect())
        }
    }

    async fn fetch(
        channel: &FakeChannel,
        before: Option<u64>,
        limit: u32,
    ) -> Result<Vec<u64>, ActionError> {
        let before = before.map(Id::new);
        let found = fetch_pages(channel, cid(CHANNEL), before, limit).await?;
        Ok(found.iter().map(|m| m.id.get()).collect())
    }

    #[tokio::test]
    async fn a_limit_of_250_is_three_calls_of_100_100_and_50() {
        let channel = FakeChannel::with_ids(1..=1000);
        let ids = fetch(&channel, None, 250).await.unwrap();
        assert_eq!(
            channel.calls(),
            [(None, 100), (Some(901), 100), (Some(801), 50)]
        );
        assert_eq!(ids.len(), 250);
        assert_eq!(ids.first(), Some(&1000));
        assert_eq!(ids.last(), Some(&751));
    }

    #[tokio::test]
    async fn a_limit_within_one_page_is_one_call() {
        let channel = FakeChannel::with_ids(1..=1000);
        assert_eq!(fetch(&channel, None, 30).await.unwrap().len(), 30);
        assert_eq!(channel.calls(), [(None, 30)]);

        let channel = FakeChannel::with_ids(1..=1000);
        assert_eq!(fetch(&channel, None, 100).await.unwrap().len(), 100);
        assert_eq!(channel.calls(), [(None, 100)]);
    }

    #[tokio::test]
    async fn before_starts_the_first_call_and_threads_through_the_rest() {
        let channel = FakeChannel::with_ids(1..=1000);
        let ids = fetch(&channel, Some(500), 150).await.unwrap();
        assert_eq!(channel.calls(), [(Some(500), 100), (Some(400), 50)]);
        assert_eq!(ids.first(), Some(&499));
        assert_eq!(ids.last(), Some(&350));
    }

    #[tokio::test]
    async fn the_cursor_is_the_oldest_id_of_the_last_page() {
        let channel = FakeChannel::with_ids((1..=500).map(|n| n * 7));
        fetch(&channel, None, 300).await.unwrap();
        // 500 * 7 = 3500 is the newest; 100 messages back is 401 * 7 = 2807.
        assert_eq!(channel.calls()[1].0, Some(2807));
    }

    #[tokio::test]
    async fn an_empty_page_ends_the_loop() {
        let channel = FakeChannel::with_ids(1..=200);
        let ids = fetch(&channel, None, 500).await.unwrap();
        assert_eq!(ids.len(), 200);
        assert_eq!(
            channel.calls(),
            [(None, 100), (Some(101), 100), (Some(1), 100)]
        );

        let empty = FakeChannel::with_ids([]);
        assert!(fetch(&empty, None, 100).await.unwrap().is_empty());
        assert_eq!(empty.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_short_page_means_the_channel_is_exhausted() {
        let channel = FakeChannel::with_ids(1..=150);
        let ids = fetch(&channel, None, 500).await.unwrap();
        assert_eq!(ids.len(), 150);
        assert_eq!(channel.calls(), [(None, 100), (Some(51), 100)]);
    }

    #[tokio::test]
    async fn paging_before_the_oldest_message_returns_nothing() {
        let channel = FakeChannel::with_ids(10..=20);
        assert!(fetch(&channel, Some(10), 100).await.unwrap().is_empty());
        assert_eq!(channel.calls(), [(Some(10), 100)]);
    }

    #[tokio::test]
    async fn a_limit_of_zero_makes_no_call() {
        let channel = FakeChannel::with_ids(1..=10);
        assert!(fetch(&channel, None, 0).await.unwrap().is_empty());
        assert!(channel.calls().is_empty());
    }

    #[tokio::test]
    async fn a_failed_call_fails_the_whole_fetch() {
        let mut channel = FakeChannel::with_ids(1..=1000);
        channel.fail_on_call = Some(2);
        let error = fetch(&channel, None, 250).await.unwrap_err();
        assert_eq!(error.to_string(), "503");
        assert_eq!(channel.calls().len(), 2);
    }

    #[tokio::test]
    async fn a_page_of_system_notices_does_not_look_like_the_start() {
        let mut channel = FakeChannel::with_ids(1..=100);
        channel.messages.extend((101..=200).map(|id| (id, 7)));
        let ids = fetch(&channel, None, 100).await.unwrap();
        assert_eq!(channel.calls(), [(None, 100), (Some(101), 100)]);
        assert_eq!(ids.len(), 200);
    }

    #[tokio::test]
    async fn reading_past_system_notices_stops_at_the_cap() {
        let mut channel = FakeChannel::with_ids([]);
        channel.messages = (1..=1500).map(|id| (id, 7)).collect();
        let ids = fetch(&channel, None, 100).await.unwrap();
        assert_eq!(ids.len(), 1000);
        assert_eq!(channel.calls().len(), 10);
    }

    #[test]
    fn requests_above_the_cap_are_clamped() {
        assert_eq!(clamp_limit(0), 0);
        assert_eq!(clamp_limit(250), 250);
        assert_eq!(clamp_limit(1000), 1000);
        assert_eq!(clamp_limit(1001), 1000);
        assert_eq!(clamp_limit(u32::MAX), 1000);
    }

    #[tokio::test]
    async fn a_clamped_request_reads_at_most_ten_pages() {
        let channel = FakeChannel::with_ids(1..=5000);
        let ids = fetch(&channel, None, clamp_limit(5000)).await.unwrap();
        assert_eq!(ids.len(), 1000);
        assert_eq!(channel.calls().len(), 10);
    }

    #[tokio::test]
    async fn a_bad_before_id_fails_before_any_call() {
        let mut actions = Actions::new(
            Arc::new(Client::new("token".to_owned())),
            "Q".to_owned(),
            Arc::new(OnceLock::new()),
            DmRooms::default(),
        );
        let error = actions
            .perform(OutboundAction::FetchHistory {
                room: "20".into(),
                limit: 10,
                before: Some("soon".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, ActionError::Id(_)));
    }

    fn offline_actions() -> Actions {
        Actions::new(
            Arc::new(Client::new("token".to_owned())),
            "Q".to_owned(),
            Arc::new(OnceLock::new()),
            DmRooms::default(),
        )
    }

    fn poll_of(options: usize, room: &str) -> OutboundAction {
        OutboundAction::CreatePoll {
            room: room.into(),
            question: "when?".to_owned(),
            options: (1..=options).map(|n| format!("option {n}")).collect(),
            multiple: false,
            hours: 24,
        }
    }

    #[tokio::test]
    async fn a_poll_with_too_few_or_too_many_options_fails_before_any_call() {
        for options in [0, 1, 11] {
            let error = offline_actions()
                .perform(poll_of(options, "20"))
                .await
                .unwrap_err();
            assert!(matches!(error, ActionError::Poll(_)), "{options} options");
        }
    }

    #[tokio::test]
    async fn a_poll_in_a_bad_room_fails_before_any_call() {
        let error = offline_actions()
            .perform(poll_of(2, "general"))
            .await
            .unwrap_err();
        assert!(matches!(error, ActionError::Id(_)));
    }

    #[tokio::test]
    async fn ending_a_poll_needs_real_ids() {
        let error = offline_actions()
            .perform(OutboundAction::EndPoll {
                room: "20".into(),
                message: "soon".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, ActionError::Id(_)));
    }
}
