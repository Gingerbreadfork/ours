//! Slack values for tests, built from the JSON Slack sends.

use crate::event::{self, Event};
use serde_json::{Value, json};
use slack_morphism::events::SlackEventCallbackBody;
use slack_morphism::{SlackChannelInfo, SlackHistoryMessage, SlackUser};

pub fn slack_user(value: Value) -> SlackUser {
    serde_json::from_value(value).unwrap()
}

pub fn history_message(value: Value) -> SlackHistoryMessage {
    serde_json::from_value(value).unwrap()
}

pub fn channel_info(id: &str, name: &str) -> SlackChannelInfo {
    serde_json::from_value(json!({
        "id": id, "name": name, "created": 1_700_000_000,
        "is_channel": true, "is_member": true
    }))
    .unwrap()
}

/// An event as it arrives inside an `events_api` envelope.
pub fn event(mut value: Value) -> Event {
    event::without_decorations(&mut value);
    let body: SlackEventCallbackBody = serde_json::from_value(value.clone()).unwrap();
    event::from_callback(body, &value)
}
