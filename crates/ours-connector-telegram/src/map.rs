//! Pure conversions from Bot API values to the Ours event model: ids,
//! rooms, roles, members, messages and attachments. Nothing here touches
//! the network.
//!
//! A group is a room named by its chat id. A forum topic is a room of its
//! own, `<chat>:<topic>`, hanging off the group. A private chat is a DM
//! named by its chat id, which is the person's user id.

use crate::text::{self, Me};
use frankenstein::types::{ChatMember, ChatType, Message as TgMessage, User};
use ours_types::{
    Attachment, Instant, Member, Message, Platform, PlatformMessageId, PlatformRole,
    PlatformRoomId, PlatformUserId, Room, RoomKind,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    #[error("`{0}` is not a Telegram room id")]
    BadRoom(String),
    #[error("`{0}` is not a Telegram message id")]
    BadMessage(String),
    #[error("`{0}` is not a Telegram user id")]
    BadUser(String),
}

/// Where something goes on Telegram: a chat, and the forum topic in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Target {
    pub chat: i64,
    pub topic: Option<i32>,
}

impl Target {
    pub fn chat(chat: i64) -> Self {
        Target { chat, topic: None }
    }

    pub fn room_id(self) -> PlatformRoomId {
        match self.topic {
            Some(topic) => topic_room_id(self.chat, topic),
            None => chat_room_id(self.chat),
        }
    }
}

/// The engine's room id back as a chat and topic.
pub fn parse_room(room: &PlatformRoomId) -> Result<Target, MapError> {
    let raw = room.as_str().trim();
    let bad = || MapError::BadRoom(room.as_str().to_owned());
    let (chat, topic) = match raw.split_once(':') {
        Some((chat, topic)) => (chat, Some(topic)),
        None => (raw, None),
    };
    let chat: i64 = chat.parse().map_err(|_| bad())?;
    if chat == 0 {
        return Err(bad());
    }
    let topic = match topic {
        Some(topic) => Some(
            topic
                .parse::<i32>()
                .ok()
                .filter(|t| *t > 0)
                .ok_or_else(bad)?,
        ),
        None => None,
    };
    Ok(Target { chat, topic })
}

pub fn parse_message(id: &PlatformMessageId) -> Result<i32, MapError> {
    id.as_str()
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| MapError::BadMessage(id.as_str().to_owned()))
}

pub fn parse_user(id: &PlatformUserId) -> Result<u64, MapError> {
    id.as_str()
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| MapError::BadUser(id.as_str().to_owned()))
}

pub fn user_id(id: u64) -> PlatformUserId {
    PlatformUserId::new(id.to_string())
}

pub fn message_id(id: i32) -> PlatformMessageId {
    PlatformMessageId::new(id.to_string())
}

pub fn chat_room_id(chat: i64) -> PlatformRoomId {
    PlatformRoomId::new(chat.to_string())
}

pub fn topic_room_id(chat: i64, topic: i32) -> PlatformRoomId {
    PlatformRoomId::new(format!("{chat}:{topic}"))
}

/// A Unix time in seconds as an `Instant`.
pub fn instant(seconds: u64) -> Option<Instant> {
    Instant::from_millis(i64::try_from(seconds).ok()?.checked_mul(1000)?)
}

pub fn group_room(chat: i64, title: Option<&str>) -> Room {
    Room {
        platform: Platform::Telegram,
        id: chat_room_id(chat),
        name: title.map(str::to_owned),
        kind: RoomKind::Channel,
        parent: None,
        container: Some(chat.to_string()),
    }
}

pub fn topic_room(chat: i64, topic: i32, name: Option<&str>) -> Room {
    Room {
        platform: Platform::Telegram,
        id: topic_room_id(chat, topic),
        name: name.map(str::to_owned),
        kind: RoomKind::Thread,
        parent: Some(chat_room_id(chat)),
        container: Some(chat.to_string()),
    }
}

pub fn dm_room(chat: i64) -> Room {
    Room {
        platform: Platform::Telegram,
        id: chat_room_id(chat),
        name: None,
        kind: RoomKind::Dm,
        parent: None,
        container: None,
    }
}

/// Whether the connector serves a chat of this type. Channels are not rooms.
pub fn is_group(kind: ChatType) -> bool {
    matches!(kind, ChatType::Group | ChatType::Supergroup)
}

/// The parts of a Telegram user the connector keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub id: u64,
    pub name: String,
    pub username: Option<String>,
    pub bot: bool,
}

impl UserRecord {
    pub fn from_user(user: &User) -> Self {
        UserRecord {
            id: user.id,
            name: display_name(user),
            username: user.username.clone().filter(|u| !u.is_empty()),
            bot: user.is_bot,
        }
    }
}

/// First name and last name, else the username, else the id.
pub fn display_name(user: &User) -> String {
    let full = [Some(user.first_name.as_str()), user.last_name.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !full.is_empty() {
        return full;
    }
    user.username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map_or_else(|| user.id.to_string(), str::to_owned)
}

pub fn member(user: &UserRecord, role: PlatformRole) -> Member {
    Member {
        id: user_id(user.id),
        display_name: user.name.clone(),
        platform_role: role,
        is_bot: user.bot,
        timezone: None,
    }
}

/// The group's creator is its owner and administrators are admins; every
/// other status is a regular member.
pub fn platform_role(member: &ChatMember) -> PlatformRole {
    match member {
        ChatMember::Creator(_) => PlatformRole::Owner,
        ChatMember::Administrator(_) => PlatformRole::Admin,
        ChatMember::Member(_)
        | ChatMember::Restricted(_)
        | ChatMember::Left(_)
        | ChatMember::Kicked(_) => PlatformRole::Regular,
    }
}

pub fn member_user(member: &ChatMember) -> &User {
    match member {
        ChatMember::Creator(m) => &m.user,
        ChatMember::Administrator(m) => &m.user,
        ChatMember::Member(m) => &m.user,
        ChatMember::Restricted(m) => &m.user,
        ChatMember::Left(m) => &m.user,
        ChatMember::Kicked(m) => &m.user,
    }
}

/// Whether the status means the user is in the chat.
pub fn is_present(member: &ChatMember) -> bool {
    match member {
        ChatMember::Creator(_) | ChatMember::Administrator(_) | ChatMember::Member(_) => true,
        ChatMember::Restricted(m) => m.is_member,
        ChatMember::Left(_) | ChatMember::Kicked(_) => false,
    }
}

/// One entry of `getChatAdministrators`. An entry with fields this client
/// does not know is still read for its user and status, so a new Bot API
/// field never costs a lead.
pub fn admin_entry(value: &serde_json::Value) -> Option<(User, PlatformRole)> {
    if let Ok(member) = serde_json::from_value::<ChatMember>(value.clone()) {
        let role = platform_role(&member);
        return Some((member_user(&member).clone(), role));
    }
    let user = serde_json::from_value::<User>(value.get("user")?.clone()).ok()?;
    let role = match value.get("status")?.as_str()? {
        "creator" => PlatformRole::Owner,
        "administrator" => PlatformRole::Admin,
        _ => PlatformRole::Regular,
    };
    Some((user, role))
}

/// The forum topic a message is in. Outside forums `message_thread_id`
/// names a reply chain, which is not a room.
pub fn topic_of(message: &TgMessage) -> Option<i32> {
    let forum = message.chat.is_forum == Some(true);
    (forum && message.is_topic_message == Some(true))
        .then_some(message.message_thread_id)
        .flatten()
}

/// A topic's name, when the message carries it: the topic's opening
/// service message, or that message as the one a topic message hangs off.
pub fn topic_name(message: &TgMessage) -> Option<String> {
    message
        .forum_topic_created
        .as_ref()
        .or_else(|| {
            message
                .reply_to_message
                .as_ref()
                .and_then(|r| r.forum_topic_created.as_ref())
        })
        .map(|t| t.name.clone())
}

/// The message this one replies to. In a topic, every message without a
/// reply of its own hangs off the topic's opening message; that is not a
/// reply.
pub fn reply_target(message: &TgMessage, topic: Option<i32>) -> Option<&TgMessage> {
    let reply = message.reply_to_message.as_deref()?;
    if reply.forum_topic_created.is_some() || Some(reply.message_id) == topic {
        return None;
    }
    Some(reply)
}

/// A reference to a file the Bot API can fetch with `getFile`. Nothing is
/// downloaded here.
pub fn file_ref(file_id: &str) -> String {
    format!("tg://file/{file_id}")
}

/// Photos, documents, videos, audio and voice notes, in the kinds the other
/// connectors use: `image`, `file`, `video` and `audio`.
pub fn attachments(message: &TgMessage) -> Vec<Attachment> {
    let attachment = |kind: &str, file_id: &str, name: Option<&String>| Attachment {
        kind: kind.to_owned(),
        url: Some(file_ref(file_id)),
        name: name.cloned(),
    };
    let mut found = Vec::new();
    if let Some(photo) = message.photo.as_ref().and_then(|sizes| sizes.last()) {
        found.push(attachment("image", &photo.file_id, None));
    }
    if let Some(document) = &message.document {
        found.push(attachment(
            "file",
            &document.file_id,
            document.file_name.as_ref(),
        ));
    }
    if let Some(video) = &message.video {
        found.push(attachment(
            "video",
            &video.file_id,
            video.file_name.as_ref(),
        ));
    }
    if let Some(note) = &message.video_note {
        found.push(attachment("video", &note.file_id, None));
    }
    if let Some(audio) = &message.audio {
        let name = audio.file_name.as_ref().or(audio.title.as_ref());
        found.push(attachment("audio", &audio.file_id, name));
    }
    if let Some(voice) = &message.voice {
        found.push(attachment("audio", &voice.file_id, None));
    }
    found
}

/// Convert a message a person sent, or `None` when it has nothing for the
/// engine: no author, or neither text nor an attachment. `lookup` finds a
/// user by username for mentions.
pub fn message(
    source: &TgMessage,
    me: &Me,
    topic: Option<i32>,
    edited: bool,
    lookup: impl Fn(&str) -> Option<u64>,
    fallback: Instant,
) -> Option<Message> {
    let from = source.from.as_deref()?;
    let (raw, entities) = match (&source.text, &source.caption) {
        (Some(text), _) => (text.as_str(), source.entities.as_deref()),
        (None, Some(caption)) => (caption.as_str(), source.caption_entities.as_deref()),
        (None, None) => ("", None),
    };
    let attachments = attachments(source);
    if raw.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    let private = source.chat.type_field == ChatType::Private;
    let read = text::read(raw, entities.unwrap_or_default(), me, private, lookup);
    let reply = reply_target(source, topic);
    let replies_to_me = reply
        .and_then(|r| r.from.as_deref())
        .is_some_and(|author| author.id == me.id);
    let room = match topic {
        Some(topic) => topic_room_id(source.chat.id, topic),
        None => chat_room_id(source.chat.id),
    };
    Some(Message {
        id: message_id(source.message_id),
        room,
        author: user_id(from.id),
        text: read.text,
        at: instant(source.date).unwrap_or(fallback),
        reply_to: reply.map(|r| message_id(r.message_id)),
        mentions: read.mentions.into_iter().map(user_id).collect(),
        mentions_me: read.mentions_me || replies_to_me,
        attachments,
        edited,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;

    fn me() -> Me {
        Me {
            id: BOT,
            username: Some("ours_q_bot".to_owned()),
            agent_name: "Q".to_owned(),
        }
    }

    fn convert(value: serde_json::Value) -> Option<Message> {
        let source = tg_message(value);
        let topic = topic_of(&source);
        message(&source, &me(), topic, false, |_| None, at())
    }

    #[test]
    fn room_ids_round_trip() {
        assert_eq!(
            parse_room(&"-1001234567890".into()),
            Ok(Target::chat(-1_001_234_567_890))
        );
        assert_eq!(
            parse_room(&"-1001234567890:42".into()),
            Ok(Target {
                chat: -1_001_234_567_890,
                topic: Some(42)
            })
        );
        assert_eq!(parse_room(&"5551234".into()), Ok(Target::chat(5_551_234)));
        for bad in ["general", "", "0", "-100:x", "-100:0", "-100:-3", "C1"] {
            assert_eq!(
                parse_room(&bad.into()),
                Err(MapError::BadRoom(bad.to_owned())),
                "{bad}"
            );
        }
        let topic = Target {
            chat: -1005,
            topic: Some(9),
        };
        assert_eq!(parse_room(&topic.room_id()), Ok(topic));
        assert_eq!(parse_message(&"77".into()), Ok(77));
        assert!(parse_message(&"1.0".into()).is_err());
        assert!(parse_message(&"-4".into()).is_err());
        assert_eq!(parse_user(&"42".into()), Ok(42));
        assert!(parse_user(&"U42".into()).is_err());
    }

    #[test]
    fn rooms_carry_the_chat_as_their_container() {
        let group = group_room(-1007, Some("The Six"));
        assert_eq!(group.id.as_str(), "-1007");
        assert_eq!(
            (
                group.kind,
                group.name.as_deref(),
                group.container.as_deref()
            ),
            (RoomKind::Channel, Some("The Six"), Some("-1007"))
        );
        assert_eq!(group.parent, None);
        assert_eq!(group.platform, Platform::Telegram);

        let topic = topic_room(-1007, 3, Some("Trip"));
        assert_eq!(topic.id.as_str(), "-1007:3");
        assert_eq!(topic.kind, RoomKind::Thread);
        assert_eq!(topic.parent.as_ref().map(|p| p.as_str()), Some("-1007"));

        let dm = dm_room(42);
        assert_eq!((dm.id.as_str(), dm.kind), ("42", RoomKind::Dm));
    }

    #[test]
    fn roles_follow_the_chat_member_status() {
        let status = |value: serde_json::Value| platform_role(&chat_member(value));
        assert_eq!(status(creator(SAM)), PlatformRole::Owner);
        assert_eq!(status(administrator(JO)), PlatformRole::Admin);
        assert_eq!(status(plain_member(ALEX)), PlatformRole::Regular);
        assert_eq!(
            status(json!({ "status": "left", "user": user_json(ALEX, "Alex") })),
            PlatformRole::Regular
        );
        assert!(is_present(&chat_member(plain_member(ALEX))));
        assert!(!is_present(&chat_member(json!({ "status": "kicked",
            "user": user_json(ALEX, "Alex"), "until_date": 0 }))));
    }

    #[test]
    fn an_admin_entry_with_unknown_fields_is_still_read() {
        let (user, role) = admin_entry(&creator(SAM)).unwrap();
        assert_eq!((user.id, role), (SAM, PlatformRole::Owner));
        let odd = json!({ "status": "administrator", "user": user_json(JO, "Jo"),
            "a_field_from_the_future": true });
        let (user, role) = admin_entry(&odd).unwrap();
        assert_eq!((user.id, role), (JO, PlatformRole::Admin));
        assert_eq!(admin_entry(&json!({ "status": "administrator" })), None);
    }

    #[test]
    fn display_names_are_first_and_last_then_username() {
        let user = |value: serde_json::Value| display_name(&serde_json::from_value(value).unwrap());
        assert_eq!(
            user(json!({ "id": 1, "is_bot": false, "first_name": "Priya", "last_name": "Shah" })),
            "Priya Shah"
        );
        assert_eq!(
            user(json!({ "id": 1, "is_bot": false, "first_name": "Priya" })),
            "Priya"
        );
        assert_eq!(
            user(json!({ "id": 1, "is_bot": false, "first_name": " ", "username": "pshah" })),
            "pshah"
        );
        assert_eq!(
            user(json!({ "id": 7, "is_bot": false, "first_name": "" })),
            "7"
        );
    }

    #[test]
    fn a_text_message_maps_with_its_reply_and_time() {
        let message = convert(json!({
            "message_id": 501, "date": 1_793_613_600, "chat": group_chat(GROUP, false),
            "from": user_json(SAM, "Sam"), "text": "I'll get the tickets",
            "reply_to_message": { "message_id": 500, "date": 1_793_613_500,
                "chat": group_chat(GROUP, false), "from": user_json(JO, "Jo"), "text": "who?" }
        }))
        .unwrap();
        assert_eq!(message.id.as_str(), "501");
        assert_eq!(message.room.as_str(), GROUP.to_string());
        assert_eq!(message.author.as_str(), SAM.to_string());
        assert_eq!(message.text, "I'll get the tickets");
        assert_eq!(message.at.as_millis(), 1_793_613_600_000);
        assert_eq!(message.reply_to.as_ref().map(|r| r.as_str()), Some("500"));
        assert!(!message.mentions_me && !message.edited);
    }

    #[test]
    fn a_reply_to_the_agent_addresses_it() {
        let message = convert(json!({
            "message_id": 9, "date": 1, "chat": group_chat(GROUP, false),
            "from": user_json(SAM, "Sam"), "text": "yes, Friday",
            "reply_to_message": { "message_id": 8, "date": 1, "chat": group_chat(GROUP, false),
                "from": bot_json(), "text": "Thursday or Friday?" }
        }))
        .unwrap();
        assert!(message.mentions_me);
        assert_eq!(message.text, "yes, Friday");
    }

    #[test]
    fn a_topic_message_lands_in_the_topic_and_its_root_is_not_a_reply() {
        let message = convert(json!({
            "message_id": 30, "date": 1, "chat": group_chat(GROUP, true),
            "message_thread_id": 12, "is_topic_message": true,
            "from": user_json(SAM, "Sam"), "text": "booked",
            "reply_to_message": { "message_id": 12, "date": 1, "chat": group_chat(GROUP, true),
                "from": user_json(JO, "Jo"), "forum_topic_created": { "name": "Trip", "icon_color": 1 } }
        }))
        .unwrap();
        assert_eq!(message.room.as_str(), format!("{GROUP}:12"));
        assert_eq!(message.reply_to, None);
        let source = tg_message(json!({
            "message_id": 30, "date": 1, "chat": group_chat(GROUP, true),
            "message_thread_id": 12, "is_topic_message": true, "from": user_json(SAM, "Sam"),
            "reply_to_message": { "message_id": 12, "date": 1, "chat": group_chat(GROUP, true),
                "forum_topic_created": { "name": "Trip", "icon_color": 1 } }, "text": "x"
        }));
        assert_eq!(topic_name(&source).as_deref(), Some("Trip"));
    }

    #[test]
    fn a_reply_chain_outside_a_forum_is_not_a_topic() {
        let source = tg_message(json!({
            "message_id": 31, "date": 1, "chat": group_chat(GROUP, false),
            "message_thread_id": 20, "from": user_json(SAM, "Sam"), "text": "x"
        }));
        assert_eq!(topic_of(&source), None);
    }

    #[test]
    fn captions_and_attachments_come_through_as_references() {
        let message = convert(json!({
            "message_id": 40, "date": 1, "chat": group_chat(GROUP, false),
            "from": user_json(SAM, "Sam"), "caption": "the receipt",
            "photo": [
                { "file_id": "small", "file_unique_id": "s", "width": 90, "height": 90 },
                { "file_id": "large", "file_unique_id": "l", "width": 1280, "height": 1280 }
            ]
        }))
        .unwrap();
        assert_eq!(message.text, "the receipt");
        assert_eq!(
            message.attachments,
            [Attachment {
                kind: "image".to_owned(),
                url: Some("tg://file/large".to_owned()),
                name: None
            }]
        );

        let files = convert(json!({
            "message_id": 41, "date": 1, "chat": group_chat(GROUP, false), "from": user_json(SAM, "Sam"),
            "document": { "file_id": "doc", "file_unique_id": "d", "file_name": "plan.pdf" },
            "voice": { "file_id": "v", "file_unique_id": "v", "duration": 3 }
        }))
        .unwrap();
        assert_eq!(files.text, "");
        let kinds: Vec<_> = files
            .attachments
            .iter()
            .map(|a| (a.kind.as_str(), a.url.as_deref(), a.name.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("file", Some("tg://file/doc"), Some("plan.pdf")),
                ("audio", Some("tg://file/v"), None)
            ]
        );

        let video = convert(json!({
            "message_id": 42, "date": 1, "chat": group_chat(GROUP, false), "from": user_json(SAM, "Sam"),
            "video": { "file_id": "vid", "file_unique_id": "u", "width": 1, "height": 1, "duration": 1,
                "file_name": "clip.mp4" },
            "audio": { "file_id": "aud", "file_unique_id": "a", "duration": 1, "title": "Song" }
        }))
        .unwrap();
        let kinds: Vec<_> = video
            .attachments
            .iter()
            .map(|a| (a.kind.as_str(), a.name.as_deref()))
            .collect();
        assert_eq!(
            kinds,
            [("video", Some("clip.mp4")), ("audio", Some("Song"))]
        );
    }

    #[test]
    fn nothing_to_say_is_nothing() {
        assert_eq!(
            convert(
                json!({ "message_id": 50, "date": 1, "chat": group_chat(GROUP, false),
                "from": user_json(SAM, "Sam"),
                "sticker": { "file_id": "x", "file_unique_id": "x", "type": "regular",
                    "width": 1, "height": 1, "is_animated": false, "is_video": false } })
            ),
            None
        );
        assert_eq!(
            convert(
                json!({ "message_id": 51, "date": 1, "chat": group_chat(GROUP, false),
                "text": "posted by a channel" })
            ),
            None
        );
    }

    #[test]
    fn start_in_a_private_chat_reads_as_hello() {
        let message = convert(json!({
            "message_id": 1, "date": 1, "chat": private_chat(SAM, "Sam"),
            "from": user_json(SAM, "Sam"), "text": "/start",
            "entities": [{ "type": "bot_command", "offset": 0, "length": 6 }]
        }))
        .unwrap();
        assert_eq!(message.text, "hello");
        assert_eq!(message.room.as_str(), SAM.to_string());
    }

    #[test]
    fn instants_are_seconds() {
        assert_eq!(
            instant(1_793_613_600).unwrap().as_millis(),
            1_793_613_600_000
        );
        assert_eq!(instant(u64::MAX), None);
    }
}
