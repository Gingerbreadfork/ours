//! Socket Mode frames and the events inside them, reduced to the few shapes
//! the connector acts on. Pure: the socket task parses, the state decides.

use serde_json::Value;
use slack_morphism::events::{SlackEventCallbackBody, SlackMessageEvent, SlackPushEventCallback};
use slack_morphism::{
    SlackChannelId, SlackReactionsItem, SlackTs, SlackUser, SlackUserGroupId, SlackUserId,
};

/// One Socket Mode frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Hello,
    /// Slack is about to close the connection; the reason says why.
    Disconnect(String),
    Event {
        envelope: String,
        /// Slack's id for the event, the same on a redelivery.
        id: String,
        event: Box<Event>,
    },
    /// Anything else: acknowledged when it carries an envelope, then dropped.
    Other {
        envelope: Option<String>,
    },
}

/// What the connector does something with.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Message {
        event: Box<SlackMessageEvent>,
        /// For an edit or delete, the thread the message was in, which
        /// slack-morphism's model of the inner message leaves out.
        thread: Option<SlackTs>,
    },
    Reaction {
        added: bool,
        user: SlackUserId,
        channel: SlackChannelId,
        ts: SlackTs,
        name: String,
        at: SlackTs,
    },
    MemberJoined {
        user: SlackUserId,
        channel: SlackChannelId,
    },
    MemberLeft {
        user: SlackUserId,
        channel: SlackChannelId,
    },
    /// The agent can no longer read the channel: it left, was removed, or
    /// the channel was archived or deleted.
    Gone {
        channel: SlackChannelId,
    },
    Renamed {
        channel: SlackChannelId,
        name: Option<String>,
    },
    Unarchived {
        channel: SlackChannelId,
    },
    /// A profile changed or someone joined the workspace.
    User(Box<SlackUser>),
    GroupMembers {
        group: SlackUserGroupId,
        added: Vec<SlackUserId>,
        removed: Vec<SlackUserId>,
    },
    /// The app was uninstalled or its tokens revoked.
    Uninstalled,
    Ignored(String),
}

impl Event {
    /// A short name for logs and spans.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Message { .. } => "message",
            Event::Reaction { added: true, .. } => "reaction_added",
            Event::Reaction { added: false, .. } => "reaction_removed",
            Event::MemberJoined { .. } => "member_joined",
            Event::MemberLeft { .. } => "member_left",
            Event::Gone { .. } => "channel_gone",
            Event::Renamed { .. } => "channel_rename",
            Event::Unarchived { .. } => "channel_unarchive",
            Event::User(_) => "user",
            Event::GroupMembers { .. } => "subteam_members_changed",
            Event::Uninstalled => "uninstalled",
            Event::Ignored(_) => "ignored",
        }
    }
}

/// The acknowledgement Slack expects for an envelope.
pub fn ack(envelope: &str) -> String {
    serde_json::json!({ "envelope_id": envelope }).to_string()
}

pub fn parse_frame(text: &str) -> Frame {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Frame::Other { envelope: None };
    };
    let envelope = value
        .get("envelope_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match value.get("type").and_then(Value::as_str) {
        Some("hello") => Frame::Hello,
        Some("disconnect") => Frame::Disconnect(
            value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        ),
        Some("events_api") => {
            let (Some(envelope), Some(mut payload)) =
                (envelope.clone(), value.get("payload").cloned())
            else {
                return Frame::Other { envelope };
            };
            if let Some(event) = payload.get_mut("event") {
                without_decorations(event);
            }
            match serde_json::from_value::<SlackPushEventCallback>(payload.clone()) {
                Ok(callback) => {
                    let raw = payload.get("event").cloned().unwrap_or(Value::Null);
                    Frame::Event {
                        envelope,
                        id: callback.event_id.0,
                        event: Box::new(from_callback(callback.event, &raw)),
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "unreadable events_api payload");
                    Frame::Other {
                        envelope: Some(envelope),
                    }
                }
            }
        }
        _ => Frame::Other { envelope },
    }
}

/// Reduce one event to what the connector needs. `raw` is the event's JSON,
/// for the types slack-morphism does not model.
pub fn from_callback(body: SlackEventCallbackBody, raw: &Value) -> Event {
    match body {
        SlackEventCallbackBody::Message(event) => Event::Message {
            event: Box::new(event),
            thread: edited_thread(raw),
        },
        SlackEventCallbackBody::ReactionAdded(e) => {
            reaction(true, e.user, e.reaction.0, e.item, e.event_ts)
        }
        SlackEventCallbackBody::ReactionRemoved(e) => {
            reaction(false, e.user, e.reaction.0, e.item, e.event_ts)
        }
        SlackEventCallbackBody::MemberJoinedChannel(e) => Event::MemberJoined {
            user: e.user,
            channel: e.channel,
        },
        SlackEventCallbackBody::MemberLeftChannel(e) => Event::MemberLeft {
            user: e.user,
            channel: e.channel,
        },
        SlackEventCallbackBody::ChannelArchive(e) => Event::Gone { channel: e.channel },
        SlackEventCallbackBody::ChannelDeleted(e) => Event::Gone { channel: e.channel },
        SlackEventCallbackBody::ChannelUnarchive(e) => Event::Unarchived { channel: e.channel },
        SlackEventCallbackBody::ChannelRename(e) => Event::Renamed {
            channel: e.channel.id,
            name: e.channel.name,
        },
        SlackEventCallbackBody::UserChange(e) => Event::User(Box::new(e.user)),
        SlackEventCallbackBody::TeamJoin(e) => Event::User(Box::new(e.user)),
        SlackEventCallbackBody::AppUninstalled(_) => Event::Uninstalled,
        _ => from_raw(raw),
    }
}

fn reaction(
    added: bool,
    user: SlackUserId,
    name: String,
    item: SlackReactionsItem,
    at: SlackTs,
) -> Event {
    match item {
        SlackReactionsItem::Message(message) => match message.origin.channel {
            Some(channel) => Event::Reaction {
                added,
                user,
                channel,
                ts: message.origin.ts,
                name,
                at,
            },
            None => Event::Ignored("reaction without a channel".to_owned()),
        },
        SlackReactionsItem::File(_) => Event::Ignored("reaction on a file".to_owned()),
    }
}

/// The thread of the message an edit or delete is about.
fn edited_thread(raw: &Value) -> Option<SlackTs> {
    ["message", "previous_message"]
        .iter()
        .filter_map(|key| raw.get(key)?.get("thread_ts")?.as_str())
        .map(|ts| SlackTs(ts.to_owned()))
        .next()
}

fn channel_field(raw: &Value) -> Option<SlackChannelId> {
    let channel = raw.get("channel")?;
    let id = channel
        .as_str()
        .or_else(|| channel.get("id").and_then(Value::as_str))?;
    Some(SlackChannelId(id.to_owned()))
}

fn users_field(raw: &Value, key: &str) -> Vec<SlackUserId> {
    raw.get(key)
        .and_then(Value::as_array)
        .map(|users| {
            users
                .iter()
                .filter_map(Value::as_str)
                .map(|u| SlackUserId(u.to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// Events slack-morphism does not model, or whose payload did not fit its model.
fn from_raw(raw: &Value) -> Event {
    let kind = raw.get("type").and_then(Value::as_str).unwrap_or_default();
    let channel = channel_field(raw);
    match (kind, channel) {
        (
            "channel_left" | "group_left" | "channel_archive" | "group_archive" | "channel_deleted"
            | "group_deleted",
            Some(channel),
        ) => Event::Gone { channel },
        ("channel_unarchive" | "group_unarchive", Some(channel)) => Event::Unarchived { channel },
        ("channel_rename" | "group_rename", Some(channel)) => Event::Renamed {
            channel,
            name: raw
                .get("channel")
                .and_then(|c| c.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        ("subteam_members_changed", _) => match raw.get("subteam_id").and_then(Value::as_str) {
            Some(group) => Event::GroupMembers {
                group: SlackUserGroupId(group.to_owned()),
                added: users_field(raw, "added_users"),
                removed: users_field(raw, "removed_users"),
            },
            None => Event::Ignored(kind.to_owned()),
        },
        ("tokens_revoked", _) => {
            let bot = raw.pointer("/tokens/bot").and_then(Value::as_array);
            if bot.is_some_and(|tokens| !tokens.is_empty()) {
                Event::Uninstalled
            } else {
                Event::Ignored(kind.to_owned())
            }
        }
        ("app_uninstalled", _) => Event::Uninstalled,
        _ => Event::Ignored(kind.to_owned()),
    }
}

/// Drop the attachments and blocks from a message and the messages inside
/// it. The connector reads neither, and one that does not fit
/// slack-morphism's model would otherwise lose the message's text with it.
pub fn without_decorations(message: &mut Value) {
    for key in ["", "/message", "/previous_message"] {
        if let Some(Value::Object(fields)) = message.pointer_mut(key) {
            fields.remove("attachments");
            fields.remove("blocks");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(event: Value) -> String {
        json!({
            "type": "events_api",
            "envelope_id": "env-1",
            "accepts_response_payload": false,
            "payload": {
                "type": "event_callback",
                "team_id": "T1",
                "api_app_id": "A1",
                "event_id": "Ev1",
                "event_time": 1793613600,
                "event": event
            }
        })
        .to_string()
    }

    fn event_of(event: Value) -> Event {
        match parse_frame(&envelope(event)) {
            Frame::Event {
                envelope,
                id,
                event,
            } => {
                assert_eq!((envelope.as_str(), id.as_str()), ("env-1", "Ev1"));
                *event
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hello_and_disconnect_frames() {
        let hello = json!({ "type": "hello", "num_connections": 1,
            "connection_info": { "app_id": "A1" }, "debug_info": { "host": "h" } });
        assert_eq!(parse_frame(&hello.to_string()), Frame::Hello);
        let bye = json!({ "type": "disconnect", "reason": "refresh_requested", "debug_info": { "host": "h" } });
        assert_eq!(
            parse_frame(&bye.to_string()),
            Frame::Disconnect("refresh_requested".to_owned())
        );
    }

    #[test]
    fn other_envelopes_are_still_acknowledged() {
        let interactive = json!({ "type": "interactive", "envelope_id": "env-2", "payload": {} });
        assert_eq!(
            parse_frame(&interactive.to_string()),
            Frame::Other {
                envelope: Some("env-2".to_owned())
            }
        );
        assert_eq!(parse_frame("not json"), Frame::Other { envelope: None });
        assert_eq!(ack("env-2"), r#"{"envelope_id":"env-2"}"#);
    }

    #[test]
    fn a_channel_message_arrives_whole() {
        let Event::Message { event, thread } = event_of(json!({
            "type": "message", "channel": "C1", "channel_type": "channel",
            "user": "U1", "text": "hi", "ts": "100.000001", "thread_ts": "99.000001"
        })) else {
            panic!()
        };
        assert_eq!(thread, None);
        assert_eq!(
            event.origin.channel.as_ref().map(|c| c.0.as_str()),
            Some("C1")
        );
        assert_eq!(
            event.origin.thread_ts.as_ref().map(|t| t.0.as_str()),
            Some("99.000001")
        );
        assert_eq!(event.sender.user.as_ref().map(|u| u.0.as_str()), Some("U1"));
    }

    #[test]
    fn a_message_with_an_odd_attachment_still_gets_through() {
        let raw = json!({
            "type": "message", "channel": "C1", "user": "U1", "text": "see this", "ts": "100.000001",
            "attachments": [{ "id": "not-a-number", "fallback": "card" }]
        });
        let direct: SlackMessageEvent = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            direct.content, None,
            "the fixture must lose its text unstripped"
        );
        let Event::Message { event, .. } = event_of(raw) else {
            panic!()
        };
        assert_eq!(
            event.content.and_then(|c| c.text).as_deref(),
            Some("see this")
        );
    }

    #[test]
    fn an_edit_keeps_the_thread_slack_morphism_drops() {
        let Event::Message { thread, .. } = event_of(json!({
            "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "200.000000",
            "hidden": true,
            "message": { "type": "message", "user": "U1", "text": "new", "ts": "105.000002",
                "thread_ts": "100.000001", "edited": { "user": "U1", "ts": "200.000000" } },
            "previous_message": { "type": "message", "user": "U1", "text": "old", "ts": "105.000002",
                "thread_ts": "100.000001" }
        })) else {
            panic!()
        };
        assert_eq!(thread, Some(SlackTs("100.000001".to_owned())));
    }

    #[test]
    fn reactions_carry_the_message_they_are_on() {
        let event = event_of(json!({
            "type": "reaction_added", "user": "U2", "reaction": "white_check_mark",
            "item_user": "UBOT", "item": { "type": "message", "channel": "D1", "ts": "100.000001" },
            "event_ts": "101.000000"
        }));
        assert_eq!(
            event,
            Event::Reaction {
                added: true,
                user: SlackUserId("U2".to_owned()),
                channel: SlackChannelId("D1".to_owned()),
                ts: SlackTs("100.000001".to_owned()),
                name: "white_check_mark".to_owned(),
                at: SlackTs("101.000000".to_owned()),
            }
        );
        let removed = event_of(json!({
            "type": "reaction_removed", "user": "U2", "reaction": "x",
            "item": { "type": "message", "channel": "C1", "ts": "100.000001" },
            "event_ts": "102.000000"
        }));
        assert_eq!(removed.kind(), "reaction_removed");
    }

    #[test]
    fn membership_and_channel_lifecycle() {
        assert!(matches!(
            event_of(
                json!({ "type": "member_joined_channel", "user": "U3", "channel": "C1",
                "channel_type": "C", "team": "T1" })
            ),
            Event::MemberJoined { .. }
        ));
        assert!(matches!(
            event_of(
                json!({ "type": "member_left_channel", "user": "U3", "channel": "C1",
                "channel_type": "C", "team": "T1" })
            ),
            Event::MemberLeft { .. }
        ));
        for gone in [
            json!({ "type": "channel_left", "channel": "C1", "actor_id": "U1" }),
            json!({ "type": "group_left", "channel": "G1" }),
            json!({ "type": "channel_archive", "channel": "C1", "user": "U1" }),
            json!({ "type": "group_archive", "channel": "G1" }),
            json!({ "type": "channel_deleted", "channel": "C1" }),
            json!({ "type": "group_deleted", "channel": "G1" }),
        ] {
            assert!(
                matches!(event_of(gone.clone()), Event::Gone { .. }),
                "{gone}"
            );
        }
        assert_eq!(
            event_of(
                json!({ "type": "group_rename", "channel": { "id": "G1", "name": "secret", "created": 1 } })
            ),
            Event::Renamed {
                channel: SlackChannelId("G1".to_owned()),
                name: Some("secret".to_owned())
            }
        );
        assert_eq!(
            event_of(
                json!({ "type": "channel_rename", "channel": { "id": "C1", "name": "launch-2", "created": 1 } })
            ),
            Event::Renamed {
                channel: SlackChannelId("C1".to_owned()),
                name: Some("launch-2".to_owned())
            }
        );
        assert!(matches!(
            event_of(json!({ "type": "group_unarchive", "channel": "G1" })),
            Event::Unarchived { .. }
        ));
    }

    #[test]
    fn user_group_changes_list_who_came_and_went() {
        assert_eq!(
            event_of(
                json!({ "type": "subteam_members_changed", "subteam_id": "S1", "team_id": "T1",
                "added_users": ["U1", "U2"], "added_users_count": "2",
                "removed_users": ["U3"], "removed_users_count": "1" })
            ),
            Event::GroupMembers {
                group: SlackUserGroupId("S1".to_owned()),
                added: vec![SlackUserId("U1".to_owned()), SlackUserId("U2".to_owned())],
                removed: vec![SlackUserId("U3".to_owned())],
            }
        );
    }

    #[test]
    fn profile_changes_and_uninstalls() {
        assert!(matches!(
            event_of(
                json!({ "type": "user_change", "event_ts": "1.0", "cache_ts": 1,
                "user": { "id": "U1", "name": "sam" } })
            ),
            Event::User(_)
        ));
        assert!(matches!(
            event_of(json!({ "type": "team_join", "user": { "id": "U9", "name": "new" } })),
            Event::User(_)
        ));
        assert_eq!(
            event_of(json!({ "type": "tokens_revoked", "tokens": { "bot": ["U1"] } })),
            Event::Uninstalled
        );
        assert_eq!(
            event_of(json!({ "type": "tokens_revoked", "tokens": { "oauth": ["U2"] } })).kind(),
            "ignored"
        );
        assert_eq!(
            event_of(json!({ "type": "app_uninstalled" })),
            Event::Uninstalled
        );
        assert_eq!(
            event_of(json!({ "type": "emoji_changed", "subtype": "add", "event_ts": "1.0" }))
                .kind(),
            "ignored"
        );
    }
}
