//! Bot API values for tests, built from the JSON Telegram sends.

use frankenstein::types::{ChatMember, Message as TgMessage, User};
use frankenstein::updates::{Update, UpdateContent};
use ours_types::Instant;
use serde_json::{Value, json};

pub const BOT: u64 = 900;
pub const SAM: u64 = 11;
pub const JO: u64 = 12;
pub const ALEX: u64 = 13;
pub const PRIYA: u64 = 14;
pub const GROUP: i64 = -1_001_234_567_890;

pub fn at() -> Instant {
    Instant::from_millis(1_793_613_600_000).unwrap()
}

pub fn user_json(id: u64, name: &str) -> Value {
    json!({ "id": id, "is_bot": false, "first_name": name, "username": name.to_lowercase() })
}

pub fn bot_json() -> Value {
    json!({ "id": BOT, "is_bot": true, "first_name": "Q", "username": "ours_q_bot",
        "can_join_groups": true, "can_read_all_group_messages": true })
}

pub fn bot_user() -> User {
    serde_json::from_value(bot_json()).unwrap()
}

/// The test people by id, so every fixture names them the same way.
pub fn person(id: u64) -> Value {
    match id {
        BOT => bot_json(),
        SAM => user_json(SAM, "Sam"),
        JO => user_json(JO, "Jo"),
        ALEX => user_json(ALEX, "Alex"),
        PRIYA => user_json(PRIYA, "Priya"),
        other => user_json(other, &format!("user{other}")),
    }
}

pub fn group_chat(id: i64, forum: bool) -> Value {
    if forum {
        json!({ "id": id, "type": "supergroup", "title": "The Six", "is_forum": true })
    } else {
        json!({ "id": id, "type": "supergroup", "title": "The Six" })
    }
}

pub fn private_chat(id: u64, name: &str) -> Value {
    json!({ "id": id, "type": "private", "first_name": name, "username": name.to_lowercase() })
}

pub fn tg_message(value: Value) -> TgMessage {
    serde_json::from_value(value).unwrap()
}

pub fn chat_member(value: Value) -> ChatMember {
    serde_json::from_value(value).unwrap()
}

pub fn creator(id: u64) -> Value {
    json!({ "status": "creator", "user": person(id), "is_anonymous": false })
}

pub fn administrator(id: u64) -> Value {
    json!({
        "status": "administrator", "user": person(id),
        "can_be_edited": false, "is_anonymous": false, "can_manage_chat": true,
        "can_delete_messages": true, "can_manage_video_chats": true, "can_restrict_members": true,
        "can_promote_members": false, "can_change_info": true, "can_invite_users": true,
        "can_send_welcome_messages": false
    })
}

pub fn plain_member(id: u64) -> Value {
    json!({ "status": "member", "user": person(id) })
}

pub fn left(id: u64) -> Value {
    json!({ "status": "left", "user": person(id) })
}

pub fn update(value: Value) -> UpdateContent {
    let mut value = value;
    if let Some(object) = value.as_object_mut() {
        object.entry("update_id").or_insert(json!(1));
    }
    serde_json::from_value::<Update>(value).unwrap().content
}

/// A text message from `from` in the test group.
pub fn said(id: i32, from: u64, name: &str, text: &str) -> Value {
    json!({ "message": {
        "message_id": id, "date": 1_793_613_600, "chat": group_chat(GROUP, false),
        "from": user_json(from, name), "text": text
    } })
}
