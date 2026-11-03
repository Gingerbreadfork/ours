//! Pure conversions from twilight types to the Ours event model, and the
//! permission arithmetic behind roles and room membership. Nothing here
//! touches the network.

use ours_types::{
    Attachment, Instant, Member, Message, Platform, PlatformMessageId, PlatformRole,
    PlatformRoomId, PlatformUserId, Room, RoomKind,
};
use std::collections::HashMap;
use std::str::FromStr;
use twilight_model::channel::message::{EmojiReactionType, MessageReferenceType, MessageType};
use twilight_model::channel::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType};
use twilight_model::channel::{Attachment as TwAttachment, ChannelType, Message as TwMessage};
use twilight_model::guild::Permissions;
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ChannelMarker, GuildMarker, MessageMarker, RoleMarker, UserMarker,
};
use twilight_model::user::User;
use twilight_model::util::Timestamp;

/// Permissions that make a member a moderator.
const MODERATOR: Permissions = Permissions::MANAGE_MESSAGES.union(Permissions::MODERATE_MEMBERS);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MapError {
    #[error("`{0}` is not a Discord id")]
    BadId(String),
}

pub fn parse_id<M>(value: &str) -> Result<Id<M>, MapError> {
    Id::<M>::from_str(value.trim()).map_err(|_| MapError::BadId(value.to_owned()))
}

pub fn user_id(id: Id<UserMarker>) -> PlatformUserId {
    PlatformUserId::new(id.to_string())
}

pub fn room_id(id: Id<ChannelMarker>) -> PlatformRoomId {
    PlatformRoomId::new(id.to_string())
}

pub fn message_id(id: Id<MessageMarker>) -> PlatformMessageId {
    PlatformMessageId::new(id.to_string())
}

/// A Discord timestamp as an `Instant`, or `None` when it is out of range.
pub fn instant(ts: Timestamp) -> Option<Instant> {
    Instant::from_millis(ts.as_micros().div_euclid(1000))
}

/// The name people see: server nickname, then global display name, then username.
pub fn display_name(nick: Option<&str>, user: &User) -> String {
    [nick, user.global_name.as_deref()]
        .into_iter()
        .flatten()
        .find(|name| !name.is_empty())
        .map_or_else(|| user.name.clone(), str::to_owned)
}

/// What the platform says a member is: owner, administrator, moderator or nothing special.
pub fn platform_role(is_owner: bool, permissions: Permissions) -> PlatformRole {
    if is_owner {
        PlatformRole::Owner
    } else if permissions.contains(Permissions::ADMINISTRATOR) {
        PlatformRole::Admin
    } else if permissions.intersects(MODERATOR) {
        PlatformRole::Moderator
    } else {
        PlatformRole::Regular
    }
}

pub fn member(user: &User, nick: Option<&str>, role: PlatformRole) -> Member {
    Member {
        id: user_id(user.id),
        display_name: display_name(nick, user),
        platform_role: role,
        is_bot: user.bot,
        timezone: None,
    }
}

/// Guild-wide permissions: the `@everyone` role, whose id is the guild id, plus
/// every role the member holds. Owners and administrators hold everything.
pub fn base_permissions(
    guild: Id<GuildMarker>,
    owner: Id<UserMarker>,
    user: Id<UserMarker>,
    member_roles: &[Id<RoleMarker>],
    roles: &HashMap<Id<RoleMarker>, Permissions>,
) -> Permissions {
    if user == owner {
        return Permissions::all();
    }
    let everyone = roles
        .get(&Id::new(guild.get()))
        .copied()
        .unwrap_or_else(Permissions::empty);
    let combined = member_roles
        .iter()
        .filter_map(|role| roles.get(role))
        .fold(everyone, |acc, perms| acc | *perms);
    if combined.contains(Permissions::ADMINISTRATOR) {
        Permissions::all()
    } else {
        combined
    }
}

/// Permissions in one channel: `base` adjusted by the channel's overwrites in
/// Discord's order, which is `@everyone`, then roles, then the member.
pub fn channel_permissions(
    guild: Id<GuildMarker>,
    user: Id<UserMarker>,
    member_roles: &[Id<RoleMarker>],
    base: Permissions,
    overwrites: &[PermissionOverwrite],
) -> Permissions {
    if base.contains(Permissions::ADMINISTRATOR) {
        return Permissions::all();
    }
    let mut perms = base;

    if let Some(everyone) = overwrites
        .iter()
        .find(|o| o.kind == PermissionOverwriteType::Role && o.id.get() == guild.get())
    {
        perms = (perms & !everyone.deny) | everyone.allow;
    }

    let (mut allow, mut deny) = (Permissions::empty(), Permissions::empty());
    for overwrite in overwrites.iter().filter(|o| {
        o.kind == PermissionOverwriteType::Role
            && o.id.get() != guild.get()
            && member_roles.iter().any(|role| role.get() == o.id.get())
    }) {
        allow |= overwrite.allow;
        deny |= overwrite.deny;
    }
    perms = (perms & !deny) | allow;

    if let Some(own) = overwrites
        .iter()
        .find(|o| o.kind == PermissionOverwriteType::Member && o.id.get() == user.get())
    {
        perms = (perms & !own.deny) | own.allow;
    }

    if perms.contains(Permissions::VIEW_CHANNEL) {
        perms
    } else {
        Permissions::empty()
    }
}

/// Whether a channel type is one the agent treats as a room of its own.
pub fn is_text_channel(kind: ChannelType) -> bool {
    matches!(
        kind,
        ChannelType::GuildText | ChannelType::GuildAnnouncement
    )
}

/// Whether a channel type can hold threads that should be tracked.
pub fn can_parent_threads(kind: ChannelType) -> bool {
    matches!(
        kind,
        ChannelType::GuildText
            | ChannelType::GuildAnnouncement
            | ChannelType::GuildForum
            | ChannelType::GuildMedia
    )
}

/// `Some(true)` for a private thread, `Some(false)` for a public one, `None` otherwise.
pub fn thread_privacy(kind: ChannelType) -> Option<bool> {
    match kind {
        ChannelType::PrivateThread => Some(true),
        ChannelType::PublicThread | ChannelType::AnnouncementThread => Some(false),
        _ => None,
    }
}

pub fn channel_room(id: Id<ChannelMarker>, name: Option<&str>, guild: Id<GuildMarker>) -> Room {
    Room {
        platform: Platform::Discord,
        id: room_id(id),
        name: name.map(str::to_owned),
        kind: RoomKind::Channel,
        parent: None,
        container: Some(guild.to_string()),
    }
}

pub fn thread_room(
    id: Id<ChannelMarker>,
    name: Option<&str>,
    parent: Id<ChannelMarker>,
    guild: Id<GuildMarker>,
) -> Room {
    Room {
        platform: Platform::Discord,
        id: room_id(id),
        name: name.map(str::to_owned),
        kind: RoomKind::Thread,
        parent: Some(room_id(parent)),
        container: Some(guild.to_string()),
    }
}

pub fn dm_room(id: Id<ChannelMarker>) -> Room {
    Room {
        platform: Platform::Discord,
        id: room_id(id),
        name: None,
        kind: RoomKind::Dm,
        parent: None,
        container: None,
    }
}

/// Whether the platform message is something a person wrote, not a system notice.
pub fn is_user_message(kind: MessageType) -> bool {
    matches!(kind, MessageType::Regular | MessageType::Reply)
}

/// The attachment kind the engine sees: `image`, `video`, `audio` or `file`.
pub fn attachment_kind(content_type: Option<&str>) -> &'static str {
    let top = content_type
        .and_then(|t| t.split('/').next())
        .map(str::trim)
        .unwrap_or_default();
    if top.eq_ignore_ascii_case("image") {
        "image"
    } else if top.eq_ignore_ascii_case("video") {
        "video"
    } else if top.eq_ignore_ascii_case("audio") {
        "audio"
    } else {
        "file"
    }
}

pub fn attachment(attachment: &TwAttachment) -> Attachment {
    Attachment {
        kind: attachment_kind(attachment.content_type.as_deref()).to_owned(),
        url: Some(attachment.url.clone()),
        name: Some(attachment.filename.clone()),
    }
}

/// The message this one replies to. Forwards are not replies.
pub fn reply_to(message: &TwMessage) -> Option<PlatformMessageId> {
    let reference = message.reference.as_ref()?;
    if reference.kind != MessageReferenceType::Default {
        return None;
    }
    reference.message_id.map(message_id)
}

/// The text the engine sees and whether the agent was addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressed {
    pub mentions_me: bool,
    pub text: String,
}

/// Decide whether a message is addressed to the agent and normalise the text.
///
/// The agent is addressed by a platform mention, by its name at the start
/// followed by a comma, colon or space, or by "@" and its name at the start.
/// A leading platform mention or "@name" is rewritten to "name, " so the
/// engine sees the same shape whichever way it was addressed.
pub fn address(
    content: &str,
    mentioned: bool,
    me: Option<Id<UserMarker>>,
    agent_name: &str,
) -> Addressed {
    let trimmed = content.trim_start();
    let mut mentions_me = mentioned;

    let lead_mention = me.and_then(|me| {
        [format!("<@{me}>"), format!("<@!{me}>")]
            .iter()
            .find_map(|tag| trimmed.strip_prefix(tag.as_str()))
    });
    if let Some(rest) = lead_mention {
        return Addressed {
            mentions_me: true,
            text: addressed_text(agent_name, rest, content),
        };
    }

    if !agent_name.is_empty() {
        if let Some(rest) = trimmed
            .strip_prefix('@')
            .and_then(|s| strip_name(s, agent_name))
        {
            return Addressed {
                mentions_me: true,
                text: addressed_text(agent_name, rest, content),
            };
        }
        if strip_name(trimmed, agent_name).is_some() {
            mentions_me = true;
        }
    }
    Addressed {
        mentions_me,
        text: content.to_owned(),
    }
}

/// `name, rest` with the separator punctuation after a mention removed.
fn addressed_text(agent_name: &str, rest: &str, original: &str) -> String {
    let rest = rest.trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace());
    if agent_name.is_empty() {
        original.to_owned()
    } else if rest.is_empty() {
        agent_name.to_owned()
    } else {
        format!("{agent_name}, {rest}")
    }
}

/// If `s` starts with `name`, ignoring case, and then a comma, colon,
/// whitespace or the end, return what follows the name.
fn strip_name<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let mut chars = s.char_indices();
    for expected in name.chars() {
        let (_, actual) = chars.next()?;
        if !actual.to_lowercase().eq(expected.to_lowercase()) {
            return None;
        }
    }
    let rest = chars.next().map_or("", |(at, _)| &s[at..]);
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c == ',' || c == ':' || c.is_whitespace() => Some(rest),
        Some(_) => None,
    }
}

/// Convert a Discord message. `me`, the bot's managed role and
/// `agent_name` drive addressing: Discord's autocomplete often picks the
/// bot's role rather than its user, so a mention of that role counts and
/// is rewritten to the user mention the engine understands.
pub fn message(
    source: &TwMessage,
    me: Option<Id<UserMarker>>,
    bot_role: Option<Id<RoleMarker>>,
    agent_name: &str,
    fallback: Instant,
) -> Message {
    let role_mentioned = bot_role.is_some_and(|role| source.mention_roles.contains(&role));
    let mentioned =
        role_mentioned || me.is_some_and(|me| source.mentions.iter().any(|m| m.id == me));
    let content = match (role_mentioned, bot_role, me) {
        (true, Some(role), Some(me)) => source
            .content
            .replace(&format!("<@&{role}>"), &format!("<@{me}>")),
        _ => source.content.clone(),
    };
    let addressed = address(&content, mentioned, me, agent_name);
    Message {
        id: message_id(source.id),
        room: room_id(source.channel_id),
        author: user_id(source.author.id),
        text: addressed.text,
        at: instant(source.timestamp).unwrap_or(fallback),
        reply_to: reply_to(source),
        mentions: source.mentions.iter().map(|m| user_id(m.id)).collect(),
        mentions_me: addressed.mentions_me,
        attachments: source.attachments.iter().map(attachment).collect(),
        edited: source.edited_timestamp.is_some(),
    }
}

/// A fetched page as the engine wants it: oldest first, one entry per id,
/// system messages dropped, and no more than the `limit` newest. REST
/// messages carry no guild context, so role mentions stay as typed.
pub fn history(
    page: &[TwMessage],
    me: Option<Id<UserMarker>>,
    agent_name: &str,
    fallback: Instant,
    limit: usize,
) -> Vec<Message> {
    let mut kept: Vec<&TwMessage> = page.iter().filter(|m| is_user_message(m.kind)).collect();
    kept.sort_by_key(|m| m.id);
    kept.dedup_by_key(|m| m.id);
    let start = kept.len().saturating_sub(limit);
    kept[start..]
        .iter()
        .map(|m| message(m, me, None, agent_name, fallback))
        .collect()
}

/// An emoji as the engine names it: the unicode string, or `name:id` for a custom one.
pub fn emoji_name(emoji: &EmojiReactionType) -> String {
    match emoji {
        EmojiReactionType::Unicode { name } => name.clone(),
        EmojiReactionType::Custom { id, name, .. } => {
            format!("{}:{id}", name.as_deref().unwrap_or("_"))
        }
    }
}

/// An emoji to react with, parsed from the engine's naming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmojiRef {
    Unicode(String),
    Custom {
        name: Option<String>,
        id: Id<twilight_model::id::marker::EmojiMarker>,
    },
}

/// Parse `name:id`, `:name:id:` or `<:name:id>` as a custom emoji. Anything
/// else is taken as a unicode emoji.
pub fn parse_emoji(value: &str) -> EmojiRef {
    let inner = value.trim().trim_start_matches('<').trim_end_matches('>');
    let inner = inner.strip_prefix("a:").unwrap_or(inner);
    let inner = inner.trim_matches(':');
    if let Some((name, id)) = inner.rsplit_once(':')
        && let Ok(id) = Id::from_str(id)
    {
        let name = name.trim_matches(':');
        let name = (!name.is_empty() && name != "_").then(|| name.to_owned());
        return EmojiRef::Custom { name, id };
    }
    EmojiRef::Unicode(value.trim().to_owned())
}

/// The channel and message ids of a raw gateway MESSAGE_UPDATE that carries
/// new content or a poll but could not be read as a full message. Updates
/// with neither, such as link previews, give `None`.
pub fn partial_update_target(raw: &str) -> Option<(Id<ChannelMarker>, Id<MessageMarker>)> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if value.get("t")?.as_str()? != "MESSAGE_UPDATE" {
        return None;
    }
    let data = value.get("d")?;
    data.get("content").or_else(|| data.get("poll"))?;
    let channel = data.get("channel_id")?.as_str()?;
    let id = data.get("id")?.as_str()?;
    Some((parse_id(channel).ok()?, parse_id(id).ok()?))
}

/// A sentence for the host about why the gateway closed for good.
pub fn close_reason(code: Option<u16>, text: &str) -> String {
    let hint = match code {
        Some(4004) => "the bot token was rejected; check the token in the config",
        Some(4010..=4012) => "the shard settings were rejected",
        Some(4013) => "the intents sent were invalid",
        Some(4014) => {
            "a privileged intent is not enabled; turn on Server Members Intent and Message Content Intent \
             under Bot in the Discord developer portal"
        }
        _ => "",
    };
    let code = code.map_or_else(|| "none".to_owned(), |c| c.to_string());
    let mut reason = format!("gateway closed for good (code {code})");
    if !hint.is_empty() {
        reason.push_str(": ");
        reason.push_str(hint);
    } else if !text.is_empty() {
        reason.push_str(": ");
        reason.push_str(text);
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;

    const VIEW: u64 = 1 << 10;
    const ADMIN: u64 = 1 << 3;
    const MANAGE_MESSAGES: u64 = 1 << 13;
    const MODERATE_MEMBERS: u64 = 1 << 40;

    fn role_table(entries: &[(u64, u64)]) -> HashMap<Id<RoleMarker>, Permissions> {
        entries
            .iter()
            .map(|(id, bits)| (rid(*id), Permissions::from_bits_truncate(*bits)))
            .collect()
    }

    fn role_of(
        user: u64,
        owner: u64,
        roles: &[u64],
        table: &HashMap<Id<RoleMarker>, Permissions>,
    ) -> PlatformRole {
        let held: Vec<_> = roles.iter().map(|r| rid(*r)).collect();
        let perms = base_permissions(gid(1), uid(owner), uid(user), &held, table);
        platform_role(user == owner, perms)
    }

    #[test]
    fn owner_outranks_administrator() {
        let table = role_table(&[(1, VIEW), (10, ADMIN)]);
        assert_eq!(role_of(5, 5, &[10], &table), PlatformRole::Owner);
    }

    #[test]
    fn administrator_permission_makes_an_admin() {
        let table = role_table(&[(1, VIEW), (10, ADMIN)]);
        assert_eq!(role_of(6, 5, &[10], &table), PlatformRole::Admin);
    }

    #[test]
    fn moderation_permissions_make_a_moderator() {
        let table = role_table(&[(1, VIEW), (11, MANAGE_MESSAGES), (12, MODERATE_MEMBERS)]);
        assert_eq!(role_of(6, 5, &[11], &table), PlatformRole::Moderator);
        assert_eq!(role_of(6, 5, &[12], &table), PlatformRole::Moderator);
    }

    #[test]
    fn everyone_role_counts_toward_moderator() {
        let table = role_table(&[(1, MANAGE_MESSAGES)]);
        assert_eq!(role_of(6, 5, &[], &table), PlatformRole::Moderator);
    }

    #[test]
    fn plain_member_is_regular_never_external() {
        let table = role_table(&[(1, VIEW), (13, 0)]);
        assert_eq!(role_of(6, 5, &[13], &table), PlatformRole::Regular);
        assert_eq!(role_of(6, 5, &[], &table), PlatformRole::Regular);
    }

    #[test]
    fn bot_flag_and_display_name_come_from_the_user() {
        let bot = tw_bot_member(9, "helper", &[]);
        let mapped = member(&bot.user, bot.nick.as_deref(), PlatformRole::Regular);
        assert!(mapped.is_bot);
        assert_eq!(mapped.display_name, "helper");
        assert_eq!(mapped.timezone, None);
        assert_eq!(mapped.id.as_str(), "9");

        let person = tw_user(10, "sam_k", false);
        assert!(!member(&person, None, PlatformRole::Regular).is_bot);
        assert_eq!(display_name(Some("Sam"), &person), "Sam");
        assert_eq!(display_name(Some(""), &person), "sam_k");
    }

    #[test]
    fn global_name_is_preferred_to_username() {
        let mut person = tw_user(10, "sam_k", false);
        person.global_name = Some("Sam K".to_owned());
        assert_eq!(display_name(None, &person), "Sam K");
        assert_eq!(display_name(Some("Sammy"), &person), "Sammy");
    }

    fn overwrite(id: u64, kind: u8, allow: u64, deny: u64) -> PermissionOverwrite {
        serde_json::from_value(json!({
            "id": id.to_string(), "type": kind, "allow": allow.to_string(), "deny": deny.to_string()
        }))
        .unwrap()
    }

    #[test]
    fn channel_overwrites_apply_in_order() {
        let base = Permissions::from_bits_truncate(VIEW);
        let held = [rid(20)];
        let hide_from_everyone = overwrite(1, 0, 0, VIEW);
        let role_allows = overwrite(20, 0, VIEW, 0);
        let member_denied = overwrite(7, 1, 0, VIEW);

        let hidden = channel_permissions(gid(1), uid(7), &[], base, &[hide_from_everyone]);
        assert!(!hidden.contains(Permissions::VIEW_CHANNEL));

        let via_role = channel_permissions(
            gid(1),
            uid(7),
            &held,
            base,
            &[hide_from_everyone, role_allows],
        );
        assert!(via_role.contains(Permissions::VIEW_CHANNEL));

        let overridden = channel_permissions(
            gid(1),
            uid(7),
            &held,
            base,
            &[hide_from_everyone, role_allows, member_denied],
        );
        assert!(!overridden.contains(Permissions::VIEW_CHANNEL));
    }

    #[test]
    fn administrators_see_every_channel() {
        let base = base_permissions(
            gid(1),
            uid(5),
            uid(6),
            &[rid(10)],
            &role_table(&[(1, 0), (10, ADMIN)]),
        );
        let perms = channel_permissions(
            gid(1),
            uid(6),
            &[rid(10)],
            base,
            &[overwrite(1, 0, 0, VIEW)],
        );
        assert!(perms.contains(Permissions::VIEW_CHANNEL));
    }

    const ME: u64 = 777;

    fn me() -> Option<Id<UserMarker>> {
        Some(uid(ME))
    }

    #[test]
    fn name_at_the_start_addresses_the_agent() {
        for text in ["Q, book it", "q: book it", "Q book it", "  Q, book it", "Q"] {
            let addressed = address(text, false, me(), "Q");
            assert!(addressed.mentions_me, "{text}");
            assert_eq!(addressed.text, text);
        }
    }

    #[test]
    fn a_longer_word_or_a_later_name_is_not_addressing() {
        for text in ["Quincy, hi", "Qatar is nice", "hey Q, book it", "Q?"] {
            assert!(!address(text, false, me(), "Q").mentions_me, "{text}");
        }
        assert!(!address("Q, hi", false, me(), "").mentions_me);
    }

    #[test]
    fn at_name_is_addressing_and_is_rewritten() {
        let addressed = address("@q book it", false, me(), "Q");
        assert!(addressed.mentions_me);
        assert_eq!(addressed.text, "Q, book it");
        assert_eq!(address("@Q: book it", false, me(), "Q").text, "Q, book it");
        assert!(!address("@Quincy book it", false, me(), "Q").mentions_me);
    }

    #[test]
    fn leading_platform_mention_is_stripped_to_the_name() {
        for text in [
            format!("<@{ME}> book it"),
            format!("<@!{ME}>, book it"),
            format!("  <@{ME}>: book it"),
        ] {
            let addressed = address(&text, true, me(), "Q");
            assert!(addressed.mentions_me, "{text}");
            assert_eq!(addressed.text, "Q, book it", "{text}");
        }
        assert_eq!(address(&format!("<@{ME}>"), true, me(), "Q").text, "Q");
    }

    #[test]
    fn mention_in_the_list_counts_wherever_it_sits() {
        let text = format!("can you <@{ME}> book it");
        let addressed = address(&text, true, me(), "Q");
        assert!(addressed.mentions_me);
        assert_eq!(addressed.text, text);
        assert!(!address("book it", false, me(), "Q").mentions_me);
    }

    #[test]
    fn someone_elses_mention_is_left_alone() {
        let addressed = address("<@42> book it", false, me(), "Q");
        assert!(!addressed.mentions_me);
        assert_eq!(addressed.text, "<@42> book it");
    }

    #[test]
    fn reply_reference_becomes_reply_to() {
        let author = user_json(5, "sam", false);
        let reference = json!({ "message_id": "900", "channel_id": "20" });
        let reply = serde_json::from_value(message_json(
            901,
            20,
            Some(1),
            author.clone(),
            "yes",
            vec![],
            Some(reference),
            vec![],
        ))
        .unwrap();
        assert_eq!(
            reply_to(&reply).map(|m| m.to_string()),
            Some("900".to_owned())
        );

        let forward = json!({ "message_id": "900", "channel_id": "20", "type": 1 });
        let forwarded = serde_json::from_value(message_json(
            902,
            20,
            Some(1),
            author.clone(),
            "fyi",
            vec![],
            Some(forward),
            vec![],
        ))
        .unwrap();
        assert_eq!(reply_to(&forwarded), None);

        let plain = serde_json::from_value(message_json(
            903,
            20,
            Some(1),
            author,
            "hi",
            vec![],
            None,
            vec![],
        ))
        .unwrap();
        assert_eq!(reply_to(&plain), None);
    }

    fn attachment_json(id: u64, name: &str, content_type: Option<&str>) -> serde_json::Value {
        json!({
            "id": id.to_string(), "filename": name, "content_type": content_type, "size": 10,
            "url": format!("https://cdn.example/{name}"), "proxy_url": format!("https://proxy.example/{name}")
        })
    }

    #[test]
    fn attachments_get_a_kind_url_and_name() {
        let author = user_json(5, "sam", false);
        let source: TwMessage = serde_json::from_value(message_json(
            1,
            20,
            Some(1),
            author,
            "",
            vec![],
            None,
            vec![
                attachment_json(1, "plan.png", Some("image/png")),
                attachment_json(2, "clip.mp4", Some("video/mp4")),
                attachment_json(3, "memo.ogg", Some("audio/ogg")),
                attachment_json(4, "notes.pdf", Some("application/pdf")),
                attachment_json(5, "mystery", None),
            ],
        ))
        .unwrap();
        let kinds: Vec<_> = source
            .attachments
            .iter()
            .map(|a| attachment(a).kind)
            .collect();
        assert_eq!(kinds, ["image", "video", "audio", "file", "file"]);
        let first = attachment(&source.attachments[0]);
        assert_eq!(first.url.as_deref(), Some("https://cdn.example/plan.png"));
        assert_eq!(first.name.as_deref(), Some("plan.png"));
    }

    #[test]
    fn a_mention_of_the_bots_role_counts_as_addressing() {
        let author = user_json(5, "sam", false);
        let mut value = message_json(
            78,
            20,
            Some(1),
            author,
            "<@&900> what model are you running?",
            vec![],
            None,
            vec![],
        );
        value["mention_roles"] = json!(["900"]);
        let source: TwMessage = serde_json::from_value(value).unwrap();
        let mapped = message(&source, me(), Some(rid(900)), "Q", Instant::UNIX_EPOCH);
        assert!(mapped.mentions_me);
        assert_eq!(mapped.text, "Q, what model are you running?");
        let other_role = message(&source, me(), Some(rid(901)), "Q", Instant::UNIX_EPOCH);
        assert!(!other_role.mentions_me);
        assert_eq!(other_role.text, "<@&900> what model are you running?");
    }

    #[test]
    fn message_carries_ids_time_mentions_and_edit_flag() {
        let author = user_json(5, "sam", false);
        let mut value = message_json(
            77,
            20,
            Some(1),
            author,
            &format!("<@{ME}> book it"),
            vec![user_json(ME, "q", true)],
            Some(json!({ "message_id": "70" })),
            vec![],
        );
        value["edited_timestamp"] = json!("2026-11-02T10:05:00.000000+00:00");
        let source: TwMessage = serde_json::from_value(value).unwrap();
        let mapped = message(&source, me(), None, "Q", Instant::UNIX_EPOCH);
        assert_eq!(mapped.id.as_str(), "77");
        assert_eq!(mapped.room.as_str(), "20");
        assert_eq!(mapped.author.as_str(), "5");
        assert_eq!(mapped.text, "Q, book it");
        assert!(mapped.mentions_me);
        assert_eq!(mapped.mentions, vec![PlatformUserId::new(ME.to_string())]);
        assert_eq!(
            mapped.reply_to.map(|m| m.to_string()),
            Some("70".to_owned())
        );
        assert!(mapped.edited);
        assert_eq!(mapped.at.to_string(), "2026-11-02T10:00:00Z");
    }

    #[test]
    fn a_history_page_is_oldest_first_without_system_messages() {
        let page = [
            tw_message_of_kind(50, 20, "five", 0),
            tw_message_of_kind(40, 20, "someone joined", 7),
            tw_message_of_kind(30, 20, "three", 0),
            tw_message_of_kind(20, 20, "two", 0),
        ];
        let mapped = history(&page, me(), "Q", Instant::UNIX_EPOCH, 100);
        let seen: Vec<_> = mapped
            .iter()
            .map(|m| (m.id.as_str(), m.text.as_str()))
            .collect();
        assert_eq!(seen, [("20", "two"), ("30", "three"), ("50", "five")]);
    }

    #[test]
    fn history_uses_the_live_mapping_and_repeats_nothing() {
        let page = [
            tw_message_of_kind(30, 20, &format!("<@{ME}> hi"), 0),
            tw_message_of_kind(30, 20, &format!("<@{ME}> hi"), 0),
        ];
        let mapped = history(&page, me(), "Q", Instant::UNIX_EPOCH, 100);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].text, "Q, hi");
        assert!(mapped[0].mentions_me);
        assert!(history(&[], me(), "Q", Instant::UNIX_EPOCH, 100).is_empty());
    }

    #[test]
    fn history_keeps_only_the_newest_up_to_the_limit() {
        let page: Vec<_> = (1..=5)
            .rev()
            .map(|n| tw_message_of_kind(n * 10, 20, "m", 0))
            .collect();
        let ids: Vec<_> = history(&page, None, "Q", Instant::UNIX_EPOCH, 2)
            .iter()
            .map(|m| m.id.to_string())
            .collect();
        assert_eq!(ids, ["40", "50"]);
        assert!(history(&page, None, "Q", Instant::UNIX_EPOCH, 0).is_empty());
    }

    #[test]
    fn user_messages_are_regular_and_replies_only() {
        assert!(is_user_message(MessageType::Regular));
        assert!(is_user_message(MessageType::Reply));
        assert!(!is_user_message(MessageType::UserJoin));
        assert!(!is_user_message(MessageType::ChannelMessagePinned));
    }

    #[test]
    fn emoji_names_are_unicode_or_name_colon_id() {
        let unicode = EmojiReactionType::Unicode {
            name: "👍".to_owned(),
        };
        assert_eq!(emoji_name(&unicode), "👍");
        let custom = EmojiReactionType::Custom {
            animated: false,
            id: Id::new(555),
            name: Some("party".to_owned()),
        };
        assert_eq!(emoji_name(&custom), "party:555");
        let nameless = EmojiReactionType::Custom {
            animated: true,
            id: Id::new(555),
            name: None,
        };
        assert_eq!(emoji_name(&nameless), "_:555");
    }

    #[test]
    fn emoji_names_parse_back() {
        assert_eq!(parse_emoji("👍"), EmojiRef::Unicode("👍".to_owned()));
        assert_eq!(parse_emoji("✅"), EmojiRef::Unicode("✅".to_owned()));
        let custom = EmojiRef::Custom {
            name: Some("party".to_owned()),
            id: Id::new(555),
        };
        assert_eq!(parse_emoji("party:555"), custom);
        assert_eq!(parse_emoji(":party:555:"), custom);
        assert_eq!(parse_emoji("<:party:555>"), custom);
        assert_eq!(parse_emoji("<a:party:555>"), custom);
        assert_eq!(
            parse_emoji("_:555"),
            EmojiRef::Custom {
                name: None,
                id: Id::new(555)
            }
        );
        assert_eq!(
            parse_emoji("party:zero"),
            EmojiRef::Unicode("party:zero".to_owned())
        );
    }

    #[test]
    fn ids_must_be_nonzero_numbers() {
        assert!(parse_id::<ChannelMarker>("123").is_ok());
        assert!(parse_id::<ChannelMarker>("0").is_err());
        assert!(parse_id::<ChannelMarker>("general").is_err());
    }

    #[test]
    fn only_edits_with_content_are_fetched() {
        let with_content = r#"{"t":"MESSAGE_UPDATE","s":3,"op":0,"d":{"id":"9","channel_id":"8","content":"new"}}"#;
        assert_eq!(
            partial_update_target(with_content).map(|(c, m)| (c.get(), m.get())),
            Some((8, 9))
        );
        let preview =
            r#"{"t":"MESSAGE_UPDATE","s":3,"op":0,"d":{"id":"9","channel_id":"8","embeds":[]}}"#;
        assert_eq!(partial_update_target(preview), None);
        let other =
            r#"{"t":"MESSAGE_CREATE","s":3,"op":0,"d":{"id":"9","channel_id":"8","content":"x"}}"#;
        assert_eq!(partial_update_target(other), None);
        assert_eq!(partial_update_target("not json"), None);
    }

    #[test]
    fn partial_updates_with_a_poll_are_fetched() {
        let poll = r#"{"t":"MESSAGE_UPDATE","s":3,"op":0,"d":{"id":"9","channel_id":"8","poll":{"results":{"is_finalized":true,"answer_counts":[]}}}}"#;
        assert_eq!(
            partial_update_target(poll).map(|(c, m)| (c.get(), m.get())),
            Some((8, 9))
        );
    }

    #[test]
    fn close_reasons_explain_the_fix() {
        assert!(
            close_reason(Some(4014), "Disallowed intent(s).").contains("Message Content Intent")
        );
        assert!(close_reason(Some(4004), "").contains("token"));
        assert_eq!(
            close_reason(Some(4012), "x"),
            "gateway closed for good (code 4012): the shard settings were rejected"
        );
        assert_eq!(
            close_reason(None, "bye"),
            "gateway closed for good (code none): bye"
        );
    }

    #[test]
    fn rooms_name_their_container_and_parent() {
        let channel = channel_room(cid(20), Some("general"), gid(1));
        assert_eq!(channel.kind, RoomKind::Channel);
        assert_eq!(channel.container.as_deref(), Some("1"));
        assert_eq!(channel.parent, None);
        assert_eq!(channel.name.as_deref(), Some("general"));

        let thread = thread_room(cid(21), Some("plans"), cid(20), gid(1));
        assert_eq!(thread.kind, RoomKind::Thread);
        assert_eq!(thread.parent, Some(PlatformRoomId::new("20")));

        let dm = dm_room(cid(22));
        assert_eq!(
            (dm.kind, dm.container, dm.parent),
            (RoomKind::Dm, None, None)
        );
        assert_eq!(dm.platform, Platform::Discord);
    }

    #[test]
    fn timestamps_become_instants() {
        let ts = Timestamp::from_micros(1_788_343_200_123_000).unwrap();
        assert_eq!(instant(ts).map(Instant::as_millis), Some(1_788_343_200_123));
    }
}
