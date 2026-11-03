//! Builders for twilight values, made from JSON so the tests stay short.

use serde_json::{Value, json};
use twilight_model::channel::{Channel, Message};
use twilight_model::guild::{Guild, Member, Role};
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker, RoleMarker, UserMarker};
use twilight_model::user::User;

pub fn id<M>(n: u64) -> Id<M> {
    Id::new(n)
}

pub fn user_json(id: u64, name: &str, bot: bool) -> Value {
    json!({ "id": id.to_string(), "username": name, "discriminator": "0", "avatar": null, "bot": bot, "public_flags": 0 })
}

pub fn tw_user(id: u64, name: &str, bot: bool) -> User {
    serde_json::from_value(user_json(id, name, bot)).unwrap()
}

pub fn tw_role(id: u64, name: &str, permissions: u64) -> Role {
    serde_json::from_value(json!({
        "id": id.to_string(), "name": name, "permissions": permissions.to_string(),
        "color": 0, "colors": { "primary_color": 0 }, "hoist": false, "managed": false,
        "mentionable": false, "position": 0, "flags": 0
    }))
    .unwrap()
}

pub fn member_json(id: u64, name: &str, roles: &[u64], bot: bool) -> Value {
    let roles: Vec<String> = roles.iter().map(u64::to_string).collect();
    json!({
        "user": user_json(id, name, bot), "roles": roles, "nick": null, "joined_at": null,
        "deaf": false, "mute": false, "flags": 0, "communication_disabled_until": null
    })
}

pub fn tw_member(id: u64, name: &str, roles: &[u64]) -> Member {
    serde_json::from_value(member_json(id, name, roles, false)).unwrap()
}

pub fn tw_bot_member(id: u64, name: &str, roles: &[u64]) -> Member {
    serde_json::from_value(member_json(id, name, roles, true)).unwrap()
}

pub fn tw_channel(id: u64, kind: u8, name: &str, guild: u64, overwrites: Value) -> Channel {
    serde_json::from_value(json!({
        "id": id.to_string(), "type": kind, "name": name, "guild_id": guild.to_string(),
        "permission_overwrites": overwrites
    }))
    .unwrap()
}

pub fn tw_thread(id: u64, kind: u8, name: &str, guild: u64, parent: u64) -> Channel {
    serde_json::from_value(json!({
        "id": id.to_string(), "type": kind, "name": name, "guild_id": guild.to_string(),
        "parent_id": parent.to_string()
    }))
    .unwrap()
}

/// A guild with the given roles, members and channels.
pub fn tw_guild(
    id: u64,
    owner: u64,
    member_count: u64,
    roles: Vec<Role>,
    members: Vec<Member>,
    channels: Vec<Channel>,
) -> Guild {
    serde_json::from_value(json!({
        "id": id.to_string(), "name": "Test", "owner_id": owner.to_string(), "afk_timeout": 300,
        "default_message_notifications": 0, "emojis": [], "explicit_content_filter": 0,
        "features": [], "large": false, "mfa_level": 0, "nsfw_level": 0, "preferred_locale": "en-US",
        "premium_progress_bar_enabled": false, "system_channel_flags": 0, "verification_level": 0,
        "member_count": member_count, "roles": roles, "members": members, "channels": channels,
        "afk_channel_id": null, "application_id": null, "banner": null, "description": null,
        "discovery_splash": null, "icon": null, "rules_channel_id": null, "splash": null,
        "system_channel_id": null, "vanity_url_code": null, "public_updates_channel_id": null
    }))
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
pub fn message_json(
    id: u64,
    channel: u64,
    guild: Option<u64>,
    author: Value,
    content: &str,
    mentions: Vec<Value>,
    reference: Option<Value>,
    attachments: Vec<Value>,
) -> Value {
    let mut value = json!({
        "id": id.to_string(), "channel_id": channel.to_string(), "author": author,
        "content": content, "timestamp": "2026-11-02T10:00:00.000000+00:00",
        "edited_timestamp": null, "tts": false, "mention_everyone": false,
        "mention_roles": [], "mentions": mentions, "attachments": attachments, "embeds": [],
        "pinned": false, "type": 0
    });
    if let Some(guild) = guild {
        value["guild_id"] = json!(guild.to_string());
    }
    if let Some(reference) = reference {
        value["message_reference"] = reference;
    }
    value
}

pub fn tw_message(
    id: u64,
    channel: u64,
    guild: Option<u64>,
    author_id: u64,
    content: &str,
    mentions: &[u64],
) -> Message {
    let mentions = mentions
        .iter()
        .map(|m| user_json(*m, "someone", false))
        .collect();
    let value = message_json(
        id,
        channel,
        guild,
        user_json(author_id, "author", false),
        content,
        mentions,
        None,
        vec![],
    );
    serde_json::from_value(value).unwrap()
}

/// A message of the given Discord type number; 0 is a regular message and 7 a join notice.
pub fn tw_message_of_kind(id: u64, channel: u64, content: &str, kind: u8) -> Message {
    let mut value = message_json(
        id,
        channel,
        None,
        user_json(5, "author", false),
        content,
        vec![],
        None,
        vec![],
    );
    value["type"] = json!(kind);
    serde_json::from_value(value).unwrap()
}

pub fn gid(n: u64) -> Id<GuildMarker> {
    id(n)
}

pub fn cid(n: u64) -> Id<ChannelMarker> {
    id(n)
}

pub fn uid(n: u64) -> Id<UserMarker> {
    id(n)
}

pub fn rid(n: u64) -> Id<RoleMarker> {
    id(n)
}
