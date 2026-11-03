//! Carrying out `OutboundAction`s over the Web API. A failure comes back as
//! an error for the caller to report, never a panic.
//!
//! History is read a page at a time. A channel's history comes newest first
//! and is read back from `before` until enough is in hand. A thread's replies
//! come oldest first, so its pages are read through and the newest kept.

use crate::api::{Api, ApiError, Page};
use crate::emoji::{self, EmojiError};
use crate::map::{self, MapError, Target};
use crate::state::State;
use ours_types::{InboundEvent, OutboundAction, PlatformMessageId, PlatformRoomId, PlatformUserId};
use slack_morphism::{SlackChannelId, SlackHistoryMessage, SlackTs};
use std::sync::{Arc, Mutex, MutexGuard};

/// Messages asked for in one history call.
const PAGE: u32 = 200;

/// The most one history request may return, however much the caller asks for.
pub const MAX_HISTORY: u32 = 1000;

/// The most messages read for one request, system notices included.
const MAX_SCAN: u32 = 2000;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error(transparent)]
    Map(#[from] MapError),
    #[error(transparent)]
    Emoji(#[from] EmojiError),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("Slack has no native polls; use reactions")]
    NoPolls,
}

/// What a finished action produced.
#[derive(Debug, Default)]
pub struct Outcome {
    pub message: Option<PlatformMessageId>,
    /// Events to deliver before the action's own result.
    pub events: Vec<InboundEvent>,
}

/// The shared state, recovered if a holder panicked.
pub fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug)]
pub struct Actions {
    api: Arc<Api>,
    state: Arc<Mutex<State>>,
}

impl Actions {
    pub fn new(api: Arc<Api>, state: Arc<Mutex<State>>) -> Self {
        Actions { api, state }
    }

    pub async fn perform(&self, action: OutboundAction) -> Result<Outcome, ActionError> {
        match action {
            OutboundAction::SendMessage {
                room,
                text,
                reply_to,
            } => self.send(&room, &text, reply_to.as_ref()).await,
            OutboundAction::EditMessage { room, id, text } => {
                let (target, ts) = (map::parse_room(&room)?, map::parse_ts(id.as_str())?);
                self.api.update(target.channel(), &ts, &text).await?;
                Ok(Outcome::default())
            }
            OutboundAction::DeleteMessage { room, id } => {
                let (target, ts) = (map::parse_room(&room)?, map::parse_ts(id.as_str())?);
                self.api.delete(target.channel(), &ts).await?;
                Ok(Outcome::default())
            }
            OutboundAction::React {
                room,
                message,
                emoji,
            } => {
                let (target, ts) = (map::parse_room(&room)?, map::parse_ts(message.as_str())?);
                let name = emoji::to_slack(&emoji)?;
                self.api.react(target.channel(), &ts, &name).await?;
                Ok(Outcome::default())
            }
            OutboundAction::Unreact {
                room,
                message,
                emoji,
            } => {
                let (target, ts) = (map::parse_room(&room)?, map::parse_ts(message.as_str())?);
                let name = emoji::to_slack(&emoji)?;
                self.api.unreact(target.channel(), &ts, &name).await?;
                Ok(Outcome::default())
            }
            OutboundAction::SendDm { user, text } => self.send_dm(&user, &text).await,
            OutboundAction::Pin { room, message } => {
                let (target, ts) = (map::parse_room(&room)?, map::parse_ts(message.as_str())?);
                self.api.pin(target.channel(), &ts).await?;
                Ok(Outcome::default())
            }
            OutboundAction::CreatePoll { .. } | OutboundAction::EndPoll { .. } => {
                Err(ActionError::NoPolls)
            }
            OutboundAction::Typing { .. } => Ok(Outcome::default()),
            OutboundAction::FetchHistory {
                room,
                limit,
                before,
            } => self.history(&room, limit, before.as_ref()).await,
        }
    }

    async fn send(
        &self,
        room: &PlatformRoomId,
        text: &str,
        reply_to: Option<&PlatformMessageId>,
    ) -> Result<Outcome, ActionError> {
        let target = map::parse_room(room)?;
        let reply = reply_to.map(|id| map::parse_ts(id.as_str())).transpose()?;
        let (channel, thread) = thread_for(&lock(&self.state), target, reply);
        let ts = self.api.post(&channel, text, thread.as_ref()).await?;
        let mut state = lock(&self.state);
        let events = match &thread {
            Some(parent) => {
                let events = state.open_thread(&channel, parent);
                state.record_reply(&channel, &ts, parent);
                events
            }
            None => Vec::new(),
        };
        Ok(Outcome {
            message: Some(map::message_id(&ts)),
            events,
        })
    }

    async fn send_dm(&self, user: &PlatformUserId, text: &str) -> Result<Outcome, ActionError> {
        let user = map::parse_user(user.as_str())?;
        let known = lock(&self.state).dm_with(&user);
        let channel = match known {
            Some(channel) => channel,
            None => self.api.open_dm(&user).await?,
        };
        let mut found = None;
        if !lock(&self.state).knows_user(&user) {
            match self.api.user(&user).await {
                Ok(record) => found = Some(record),
                Err(error) => {
                    tracing::warn!(user = %user.0, %error, "could not look up a DM recipient")
                }
            }
        }
        let ts = self.api.post(&channel, text, None).await?;
        let mut state = lock(&self.state);
        let mut events = found.map(|u| state.apply_user(&u)).unwrap_or_default();
        events.extend(state.open_dm(&channel, &user));
        Ok(Outcome {
            message: Some(map::message_id(&ts)),
            events,
        })
    }

    async fn history(
        &self,
        room: &PlatformRoomId,
        limit: u32,
        before: Option<&PlatformMessageId>,
    ) -> Result<Outcome, ActionError> {
        let target = map::parse_room(room)?;
        let before = before.map(|id| map::parse_ts(id.as_str())).transpose()?;
        let limit = clamp_limit(limit);
        let fetched = fetch_history(&*self.api, &target, before.as_ref(), limit).await?;
        let mut state = lock(&self.state);
        if let Target::Thread(channel, parent) = &target {
            for message in fetched.iter().filter(|m| m.origin.ts != *parent) {
                state.record_reply(channel, &message.origin.ts, parent);
            }
        }
        let dm = state.is_dm(target.channel());
        let messages = map::history(
            &fetched,
            &target,
            dm,
            state.me(),
            state.agent_name(),
            crate::now(),
            usize::try_from(limit).unwrap_or(usize::MAX),
        );
        Ok(Outcome {
            message: None,
            events: vec![InboundEvent::History {
                room: target.room_id(),
                messages,
            }],
        })
    }
}

/// Where a post goes. In a thread room it goes into that thread; a reply in a
/// channel goes into the thread of the message it answers, or that message's
/// own thread when it is a reply already. A DM never threads.
pub fn thread_for(
    state: &State,
    target: Target,
    reply: Option<SlackTs>,
) -> (SlackChannelId, Option<SlackTs>) {
    match target {
        Target::Thread(channel, parent) => (channel, Some(parent)),
        Target::Channel(channel) if state.is_dm(&channel) => (channel, None),
        Target::Channel(channel) => {
            let parent = reply.map(|ts| state.thread_parent(&channel, &ts).unwrap_or(ts));
            (channel, parent)
        }
    }
}

/// One call to `conversations.history` or `conversations.replies`.
trait HistoryPages {
    async fn page(
        &self,
        target: &Target,
        latest: Option<&SlackTs>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<Page, ApiError>;
}

impl HistoryPages for Api {
    async fn page(
        &self,
        target: &Target,
        latest: Option<&SlackTs>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<Page, ApiError> {
        match target {
            Target::Channel(channel) => self.history_page(channel, latest, cursor, limit).await,
            Target::Thread(channel, thread) => {
                self.replies_page(channel, thread, latest, cursor, limit)
                    .await
            }
        }
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

/// Read pages before `before` until `limit` messages that belong in the room
/// are in hand, the room runs out, or `MAX_SCAN` messages have been read.
async fn fetch_history(
    pages: &impl HistoryPages,
    target: &Target,
    before: Option<&SlackTs>,
    limit: u32,
) -> Result<Vec<SlackHistoryMessage>, ApiError> {
    let mut found = Vec::new();
    if limit == 0 {
        return Ok(found);
    }
    let newest_first = matches!(target, Target::Channel(_));
    let (mut kept, mut scanned) = (0u32, 0u32);
    let mut cursor: Option<String> = None;
    loop {
        let want = if newest_first {
            (limit - kept).min(PAGE)
        } else {
            PAGE
        };
        let page = pages
            .page(
                target,
                before,
                cursor.as_deref(),
                u16::try_from(want).unwrap_or(200),
            )
            .await?;
        let size = u32::try_from(page.messages.len()).unwrap_or(u32::MAX);
        scanned = scanned.saturating_add(size);
        let belonging = page
            .messages
            .iter()
            .filter(|m| map::belongs(target, m))
            .count();
        kept = kept.saturating_add(u32::try_from(belonging).unwrap_or(u32::MAX));
        found.extend(page.messages);
        let enough = newest_first && kept >= limit;
        if enough || size == 0 || scanned >= MAX_SCAN {
            break;
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;
    use slack_morphism::{SlackTeamId, SlackUserId};

    /// One page request: `latest`, the cursor and the page size.
    type Call = (Option<String>, Option<String>, u16);

    /// A room that answers like Slack. A channel lists the newest messages
    /// older than `latest` first; a thread lists its opening message and then
    /// its replies, oldest first. The cursor is an offset into that list.
    struct FakeRoom {
        /// Message numbers and subtypes, oldest first. In a thread, 0 opens it.
        messages: Vec<(u32, Option<&'static str>)>,
        thread: bool,
        calls: Mutex<Vec<Call>>,
        fail_on_call: Option<usize>,
    }

    impl FakeRoom {
        fn channel(numbers: impl IntoIterator<Item = u32>) -> Self {
            FakeRoom {
                messages: numbers.into_iter().map(|n| (n, None)).collect(),
                thread: false,
                calls: Mutex::new(Vec::new()),
                fail_on_call: None,
            }
        }

        fn thread(replies: u32) -> Self {
            FakeRoom {
                thread: true,
                ..FakeRoom::channel(0..=replies)
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn json(&self, n: u32, subtype: Option<&str>) -> SlackHistoryMessage {
            let mut value = json!({ "ts": ts(n), "user": "U1", "text": format!("m{n}") });
            if self.thread {
                value["thread_ts"] = json!(ts(0));
            }
            if let Some(subtype) = subtype {
                value["subtype"] = json!(subtype);
            }
            history_message(value)
        }
    }

    fn ts(n: u32) -> String {
        format!("{}.000000", 1000 + n)
    }

    impl HistoryPages for FakeRoom {
        async fn page(
            &self,
            _target: &Target,
            latest: Option<&SlackTs>,
            cursor: Option<&str>,
            limit: u16,
        ) -> Result<Page, ApiError> {
            let call = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((
                    latest.map(|t| t.0.clone()),
                    cursor.map(str::to_owned),
                    limit,
                ));
                calls.len()
            };
            if self.fail_on_call == Some(call) {
                return Err(ApiError::new("conversations.history", "fatal_error"));
            }
            let mut listed: Vec<_> = self
                .messages
                .iter()
                .filter(|(n, _)| {
                    latest.is_none_or(|l| map::ts_key(&SlackTs(ts(*n))) < map::ts_key(l))
                })
                .collect();
            if !self.thread {
                listed.reverse();
            }
            let offset: usize = cursor.map_or(0, |c| c.parse().unwrap());
            let messages: Vec<_> = listed
                .iter()
                .skip(offset)
                .take(usize::from(limit))
                .map(|(n, subtype)| self.json(*n, *subtype))
                .collect();
            let end = offset + messages.len();
            Ok(Page {
                next: (end < listed.len()).then(|| end.to_string()),
                messages,
            })
        }
    }

    fn channel() -> Target {
        Target::Channel(SlackChannelId("C1".to_owned()))
    }

    fn thread() -> Target {
        Target::Thread(SlackChannelId("C1".to_owned()), SlackTs(ts(0)))
    }

    async fn fetch(
        room: &FakeRoom,
        target: &Target,
        before: Option<u32>,
        limit: u32,
    ) -> Result<Vec<u32>, ApiError> {
        let before = before.map(|n| SlackTs(ts(n)));
        let found = fetch_history(room, target, before.as_ref(), limit).await?;
        Ok(found
            .iter()
            .map(|m| map::ts_key(&m.origin.ts).0 as u32 - 1000)
            .collect())
    }

    fn call(latest: Option<u32>, cursor: Option<&str>, limit: u16) -> Call {
        (latest.map(ts), cursor.map(str::to_owned), limit)
    }

    #[tokio::test]
    async fn a_limit_of_250_is_a_page_of_200_then_50() {
        let room = FakeRoom::channel(1..=1000);
        let found = fetch(&room, &channel(), None, 250).await.unwrap();
        assert_eq!(
            room.calls(),
            [call(None, None, 200), call(None, Some("200"), 50)]
        );
        assert_eq!(found.len(), 250);
        assert_eq!((found.first(), found.last()), (Some(&1000), Some(&751)));
    }

    #[tokio::test]
    async fn a_limit_within_one_page_is_one_call() {
        let room = FakeRoom::channel(1..=1000);
        assert_eq!(fetch(&room, &channel(), None, 30).await.unwrap().len(), 30);
        assert_eq!(room.calls(), [call(None, None, 30)]);
    }

    #[tokio::test]
    async fn before_bounds_every_call_and_the_cursor_threads_through() {
        let room = FakeRoom::channel(1..=1000);
        let found = fetch(&room, &channel(), Some(500), 300).await.unwrap();
        assert_eq!(
            room.calls(),
            [
                call(Some(500), None, 200),
                call(Some(500), Some("200"), 100)
            ]
        );
        assert_eq!((found.first(), found.last()), (Some(&499), Some(&200)));
    }

    #[tokio::test]
    async fn the_room_running_out_ends_the_loop() {
        let room = FakeRoom::channel(1..=150);
        assert_eq!(
            fetch(&room, &channel(), None, 500).await.unwrap().len(),
            150
        );
        assert_eq!(room.calls(), [call(None, None, 200)]);

        let room = FakeRoom::channel(1..=250);
        assert_eq!(
            fetch(&room, &channel(), None, 500).await.unwrap().len(),
            250
        );
        assert_eq!(room.calls().len(), 2);

        let empty = FakeRoom::channel([]);
        assert!(
            fetch(&empty, &channel(), None, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(empty.calls().len(), 1);
    }

    #[tokio::test]
    async fn a_limit_of_zero_makes_no_call() {
        let room = FakeRoom::channel(1..=10);
        assert!(fetch(&room, &channel(), None, 0).await.unwrap().is_empty());
        assert!(room.calls().is_empty());
    }

    #[tokio::test]
    async fn a_failed_call_fails_the_whole_fetch() {
        let mut room = FakeRoom::channel(1..=1000);
        room.fail_on_call = Some(2);
        let error = fetch(&room, &channel(), None, 300).await.unwrap_err();
        assert_eq!(error.to_string(), "conversations.history: fatal_error");
        assert_eq!(room.calls().len(), 2);
    }

    #[tokio::test]
    async fn system_notices_do_not_count_toward_the_limit() {
        let mut room = FakeRoom::channel(1..=100);
        room.messages
            .extend((101..=200).map(|n| (n, Some("channel_join"))));
        let found = fetch(&room, &channel(), None, 100).await.unwrap();
        assert_eq!(
            room.calls(),
            [call(None, None, 100), call(None, Some("100"), 100)]
        );
        assert_eq!(found.len(), 200);
    }

    #[tokio::test]
    async fn reading_past_notices_stops_at_the_scan_cap() {
        let mut room = FakeRoom::channel([]);
        room.messages = (1..=3000).map(|n| (n, Some("channel_join"))).collect();
        let found = fetch(&room, &channel(), None, 100).await.unwrap();
        assert_eq!(found.len(), 2000);
        assert_eq!(room.calls().len(), 20);
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
    async fn a_thread_is_read_through_and_its_newest_replies_kept() {
        let room = FakeRoom::thread(450);
        let fetched = fetch_history(&room, &thread(), None, 100).await.unwrap();
        assert_eq!(
            room.calls(),
            [
                call(None, None, 200),
                call(None, Some("200"), 200),
                call(None, Some("400"), 200)
            ]
        );
        let me = SlackUserId("UBOT".to_owned());
        let mapped = map::history(&fetched, &thread(), false, &me, "Q", crate::now(), 100);
        assert_eq!(mapped.len(), 100);
        assert_eq!(mapped.first().map(|m| m.id.to_string()), Some(ts(351)));
        assert_eq!(mapped.last().map(|m| m.id.to_string()), Some(ts(450)));
        assert!(
            mapped
                .iter()
                .all(|m| m.room.as_str() == format!("C1:{}", ts(0)))
        );

        let short = FakeRoom::thread(3);
        let fetched = fetch_history(&short, &thread(), None, 100).await.unwrap();
        let mapped = map::history(&fetched, &thread(), false, &me, "Q", crate::now(), 100);
        assert_eq!(mapped.len(), 3, "the opening message stays in the channel");
    }

    fn state() -> State {
        let mut state = State::new(
            "Q".to_owned(),
            SlackUserId("UBOT".to_owned()),
            None,
            SlackTeamId("T1".to_owned()),
        );
        state.apply_channel(
            &channel_info("C1", "launch"),
            &[SlackUserId("U1".to_owned())],
        );
        state
    }

    #[test]
    fn a_reply_in_a_channel_goes_into_the_thread_it_answers() {
        let mut state = state();
        let c1 = SlackChannelId("C1".to_owned());
        let m = |s: &str| SlackTs(s.to_owned());

        assert_eq!(thread_for(&state, channel(), None), (c1.clone(), None));
        assert_eq!(
            thread_for(&state, channel(), Some(m("5.0"))),
            (c1.clone(), Some(m("5.0")))
        );
        assert_eq!(
            thread_for(&state, thread(), None),
            (c1.clone(), Some(SlackTs(ts(0))))
        );

        state.record_reply(&c1, &m("6.0"), &m("5.0"));
        assert_eq!(
            thread_for(&state, channel(), Some(m("6.0"))),
            (c1, Some(m("5.0")))
        );

        let dm = Target::Channel(SlackChannelId("D1".to_owned()));
        assert_eq!(
            thread_for(&state, dm, Some(m("5.0"))),
            (SlackChannelId("D1".to_owned()), None)
        );
    }

    fn offline() -> Actions {
        let tls = crate::tls::client_config().unwrap();
        let api = Arc::new(Api::new(tls, "xoxb-test", "xapp-test"));
        Actions::new(api, Arc::new(Mutex::new(state())))
    }

    #[tokio::test]
    async fn bad_ids_and_unknown_emoji_fail_before_any_call() {
        let actions = offline();
        let bad_room = actions
            .perform(OutboundAction::SendMessage {
                room: "general".into(),
                text: "hi".to_owned(),
                reply_to: None,
            })
            .await
            .unwrap_err();
        assert_eq!(bad_room.to_string(), "`general` is not a Slack room id");

        let bad_ts = actions
            .perform(OutboundAction::EditMessage {
                room: "C1".into(),
                id: "soon".into(),
                text: "x".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(bad_ts, ActionError::Map(MapError::BadTs(_))));

        let emoji = actions
            .perform(OutboundAction::React {
                room: "C1".into(),
                message: "1.000000".into(),
                emoji: "é".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(emoji, ActionError::Emoji(_)));
        assert!(emoji.to_string().contains("no reaction name"));

        let dm = actions
            .perform(OutboundAction::SendDm {
                user: "sam".into(),
                text: "x".to_owned(),
            })
            .await
            .unwrap_err();
        assert!(matches!(dm, ActionError::Map(MapError::BadUser(_))));

        let history = actions
            .perform(OutboundAction::FetchHistory {
                room: "C1".into(),
                limit: 10,
                before: Some("later".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(history, ActionError::Map(MapError::BadTs(_))));
    }

    #[tokio::test]
    async fn polls_fail_plainly_and_typing_does_nothing() {
        let actions = offline();
        for poll in [
            OutboundAction::CreatePoll {
                room: "C1".into(),
                question: "when?".to_owned(),
                options: vec!["Thu".to_owned(), "Fri".to_owned()],
                multiple: false,
                hours: 24,
            },
            OutboundAction::EndPoll {
                room: "C1".into(),
                message: "1.000000".into(),
            },
        ] {
            let error = actions.perform(poll).await.unwrap_err();
            assert_eq!(
                error.to_string(),
                "Slack has no native polls; use reactions"
            );
        }
        let typing = actions
            .perform(OutboundAction::Typing { room: "C1".into() })
            .await
            .unwrap();
        assert_eq!((typing.message, typing.events.len()), (None, 0));
    }
}
