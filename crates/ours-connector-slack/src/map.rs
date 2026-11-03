//! Pure conversions from slack-morphism types to the Ours event model:
//! ids, rooms, roles, messages and history. Nothing here touches the network.
//!
//! A thread is a room of its own, `<channel>:<thread ts>`, hanging off its
//! channel. Messages are identified by their `ts`.

use crate::text;
use ours_types::{
    Attachment, Instant, Member, Message, Platform, PlatformMessageId, PlatformRole,
    PlatformRoomId, PlatformUserId, Room, RoomKind,
};
use slack_morphism::events::SlackMessageEventType;
use slack_morphism::{
    SlackChannelId, SlackFile, SlackHistoryMessage, SlackTeamId, SlackTs, SlackUser, SlackUserId,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    #[error("`{0}` is not a Slack room id")]
    BadRoom(String),
    #[error("`{0}` is not a Slack message timestamp")]
    BadTs(String),
    #[error("`{0}` is not a Slack user id")]
    BadUser(String),
}

/// Where a message goes on Slack: a channel or DM, or a thread inside one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Channel(SlackChannelId),
    Thread(SlackChannelId, SlackTs),
}

impl Target {
    pub fn channel(&self) -> &SlackChannelId {
        match self {
            Target::Channel(c) | Target::Thread(c, _) => c,
        }
    }

    pub fn room_id(&self) -> PlatformRoomId {
        match self {
            Target::Channel(c) => channel_room_id(c),
            Target::Thread(c, ts) => thread_room_id(c, ts),
        }
    }
}

fn is_channel_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('C' | 'G' | 'D'))
        && id.len() > 1
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Whether a channel id names a direct message.
pub fn is_dm_id(channel: &SlackChannelId) -> bool {
    channel.0.starts_with('D')
}

/// The engine's room id back as a Slack target.
pub fn parse_room(room: &PlatformRoomId) -> Result<Target, MapError> {
    let raw = room.as_str().trim();
    let bad = || MapError::BadRoom(room.as_str().to_owned());
    match raw.split_once(':') {
        Some((channel, ts)) if is_channel_id(channel) => {
            let ts = parse_ts(ts).map_err(|_| bad())?;
            Ok(Target::Thread(SlackChannelId(channel.to_owned()), ts))
        }
        None if is_channel_id(raw) => Ok(Target::Channel(SlackChannelId(raw.to_owned()))),
        _ => Err(bad()),
    }
}

/// A message timestamp: digits, a dot and digits.
pub fn parse_ts(value: &str) -> Result<SlackTs, MapError> {
    let raw = value.trim();
    let valid = raw.split_once('.').is_some_and(|(secs, frac)| {
        !secs.is_empty()
            && !frac.is_empty()
            && secs.chars().all(|c| c.is_ascii_digit())
            && frac.chars().all(|c| c.is_ascii_digit())
    });
    if valid {
        Ok(SlackTs(raw.to_owned()))
    } else {
        Err(MapError::BadTs(value.to_owned()))
    }
}

pub fn parse_user(value: &str) -> Result<SlackUserId, MapError> {
    let raw = value.trim();
    if text::is_user_id(raw) {
        Ok(SlackUserId(raw.to_owned()))
    } else {
        Err(MapError::BadUser(value.to_owned()))
    }
}

/// A `ts` as seconds and microseconds, for ordering.
pub fn ts_key(ts: &SlackTs) -> (i64, u32) {
    let (secs, frac) = ts.0.split_once('.').unwrap_or((&ts.0, ""));
    let secs = secs.parse().unwrap_or(0);
    let mut digits: String = frac.chars().take(6).collect();
    while digits.len() < 6 {
        digits.push('0');
    }
    (secs, digits.parse().unwrap_or(0))
}

/// A `ts` as an `Instant`, or `None` when it is not a timestamp.
pub fn instant(ts: &SlackTs) -> Option<Instant> {
    parse_ts(&ts.0).ok()?;
    let (secs, micros) = ts_key(ts);
    Instant::from_millis(
        secs.checked_mul(1000)?
            .checked_add(i64::from(micros / 1000))?,
    )
}

pub fn user_id(id: &SlackUserId) -> PlatformUserId {
    PlatformUserId::new(id.0.clone())
}

pub fn message_id(ts: &SlackTs) -> PlatformMessageId {
    PlatformMessageId::new(ts.0.clone())
}

pub fn channel_room_id(channel: &SlackChannelId) -> PlatformRoomId {
    PlatformRoomId::new(channel.0.clone())
}

pub fn thread_room_id(channel: &SlackChannelId, ts: &SlackTs) -> PlatformRoomId {
    PlatformRoomId::new(format!("{}:{}", channel.0, ts.0))
}

pub fn channel_room(channel: &SlackChannelId, name: Option<&str>, team: &SlackTeamId) -> Room {
    Room {
        platform: Platform::Slack,
        id: channel_room_id(channel),
        name: name.map(str::to_owned),
        kind: RoomKind::Channel,
        parent: None,
        container: Some(team.0.clone()),
    }
}

pub fn thread_room(channel: &SlackChannelId, ts: &SlackTs, team: &SlackTeamId) -> Room {
    Room {
        platform: Platform::Slack,
        id: thread_room_id(channel, ts),
        name: None,
        kind: RoomKind::Thread,
        parent: Some(channel_room_id(channel)),
        container: Some(team.0.clone()),
    }
}

pub fn dm_room(channel: &SlackChannelId, team: &SlackTeamId) -> Room {
    Room {
        platform: Platform::Slack,
        id: channel_room_id(channel),
        name: None,
        kind: RoomKind::Dm,
        parent: None,
        container: Some(team.0.clone()),
    }
}

/// The parts of a Slack user the connector keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub id: SlackUserId,
    pub name: String,
    pub team: Option<SlackTeamId>,
    /// Workspaces an Enterprise Grid account belongs to.
    pub teams: Vec<SlackTeamId>,
    pub owner: bool,
    pub admin: bool,
    /// A guest account, single or multi channel.
    pub guest: bool,
    /// From another workspace that shares a channel.
    pub stranger: bool,
    pub bot: bool,
    pub timezone: Option<String>,
}

impl UserRecord {
    pub fn from_user(user: &SlackUser) -> Self {
        let flags = &user.flags;
        let set = |flag: Option<bool>| flag == Some(true);
        UserRecord {
            id: user.id.clone(),
            name: display_name(user),
            team: user.team_id.clone(),
            teams: user
                .enterprise_user
                .as_ref()
                .and_then(|e| e.teams.clone())
                .unwrap_or_default(),
            owner: set(flags.is_owner) || set(flags.is_primary_owner),
            admin: set(flags.is_admin),
            guest: set(flags.is_restricted) || set(flags.is_ultra_restricted),
            stranger: set(flags.is_stranger),
            bot: set(flags.is_bot) || user.id.0 == "USLACKBOT",
            timezone: user.tz.clone().filter(|tz| !tz.is_empty()),
        }
    }

    /// Whether the account belongs to `home`, directly or through Enterprise Grid.
    fn is_home(&self, home: &SlackTeamId) -> bool {
        self.team.as_ref().is_none_or(|team| team == home) || self.teams.contains(home)
    }
}

/// The name people see: display name, then real name, then username.
pub fn display_name(user: &SlackUser) -> String {
    let profile = user.profile.as_ref();
    [
        profile.and_then(|p| p.display_name.as_deref()),
        profile.and_then(|p| p.real_name.as_deref()),
        user.real_name.as_deref(),
        user.name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|name| !name.is_empty())
    .map_or_else(|| user.id.0.clone(), str::to_owned)
}

/// What Slack says a user is. External and guest accounts are checked first,
/// so an owner of another workspace in a shared channel is still a guest here.
pub fn platform_role(user: &UserRecord, home: &SlackTeamId, in_leads_group: bool) -> PlatformRole {
    if user.guest || user.stranger || !user.is_home(home) {
        PlatformRole::External
    } else if user.owner {
        PlatformRole::Owner
    } else if user.admin || in_leads_group {
        PlatformRole::Admin
    } else {
        PlatformRole::Regular
    }
}

pub fn member(user: &UserRecord, role: PlatformRole) -> Member {
    Member {
        id: user_id(&user.id),
        display_name: user.name.clone(),
        platform_role: role,
        is_bot: user.bot,
        timezone: user.timezone.clone(),
    }
}

/// A user the connector could not look up. Treated as external until known,
/// so a failed lookup never grants anything.
pub fn unknown_member(id: &SlackUserId) -> Member {
    Member {
        id: user_id(id),
        display_name: id.0.clone(),
        platform_role: PlatformRole::External,
        is_bot: false,
        timezone: None,
    }
}

/// The room a message lives in and the message it replies to. A reply in a
/// thread lives in the thread's room; the thread's first message stays in the
/// channel. DMs have no thread rooms, so a threaded DM reply stays in the DM.
pub fn placement(
    channel: &SlackChannelId,
    thread_ts: Option<&SlackTs>,
    ts: &SlackTs,
    dm: bool,
) -> (PlatformRoomId, Option<PlatformMessageId>) {
    match thread_ts.filter(|parent| *parent != ts) {
        Some(parent) if dm => (channel_room_id(channel), Some(message_id(parent))),
        Some(parent) => (thread_room_id(channel, parent), Some(message_id(parent))),
        None => (channel_room_id(channel), None),
    }
}

/// Whether a message subtype is something a person wrote. Bot posts count
/// only when they carry a user, which is how Slack marks an app speaking for
/// someone; webhooks and legacy integrations carry none.
pub fn is_person_message(subtype: Option<&SlackMessageEventType>, has_user: bool) -> bool {
    has_user
        && matches!(
            subtype,
            None | Some(
                SlackMessageEventType::FileShare
                    | SlackMessageEventType::ThreadBroadcast
                    | SlackMessageEventType::MeMessage
                    | SlackMessageEventType::BotMessage
            )
        )
}

/// The attachment kind the engine sees: `image`, `video`, `audio` or `file`.
pub fn attachment_kind(mimetype: Option<&str>) -> &'static str {
    let top = mimetype
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

pub fn attachment(file: &SlackFile) -> Attachment {
    Attachment {
        kind: attachment_kind(file.mimetype.as_ref().map(|m| m.0.as_str())).to_owned(),
        url: file.url_private.as_ref().map(|u| u.to_string()),
        name: file.name.clone().or_else(|| file.title.clone()),
    }
}

/// The text the engine sees and whether the agent was addressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addressed {
    pub mentions_me: bool,
    pub text: String,
}

/// Decide whether a message is addressed to the agent and normalise it.
///
/// The agent is addressed by a mention anywhere, by its name at the start
/// followed by a comma, colon or space, or by "@" and its name at the start.
/// A leading mention or "@name" is rewritten to "name, " so the engine sees
/// one shape however it was addressed.
pub fn address(text: &str, mentioned: bool, me: &SlackUserId, agent_name: &str) -> Addressed {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix(&format!("<@{}>", me.0)) {
        return Addressed {
            mentions_me: true,
            text: addressed_text(agent_name, rest, text),
        };
    }
    let mut mentions_me = mentioned;
    if !agent_name.is_empty() {
        if let Some(rest) = trimmed
            .strip_prefix('@')
            .and_then(|s| strip_name(s, agent_name))
        {
            return Addressed {
                mentions_me: true,
                text: addressed_text(agent_name, rest, text),
            };
        }
        if strip_name(trimmed, agent_name).is_some() {
            mentions_me = true;
        }
    }
    Addressed {
        mentions_me,
        text: text.to_owned(),
    }
}

/// `name, rest` with the punctuation after a mention removed.
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

/// One Slack message, whatever event or API reply it came from.
#[derive(Debug, Clone, Copy)]
pub struct Line<'a> {
    pub channel: &'a SlackChannelId,
    pub ts: &'a SlackTs,
    pub thread_ts: Option<&'a SlackTs>,
    pub user: &'a SlackUserId,
    pub text: Option<&'a str>,
    pub files: &'a [SlackFile],
    pub edited: bool,
}

/// Convert a Slack message. `fallback` is used when the `ts` cannot be read.
pub fn message(
    line: Line<'_>,
    dm: bool,
    me: &SlackUserId,
    agent_name: &str,
    fallback: Instant,
) -> Message {
    let parsed = text::normalise(line.text.unwrap_or_default());
    let me_id = user_id(me);
    let mentioned = parsed.mentions.contains(&me_id);
    let addressed = address(&parsed.text, mentioned, me, agent_name);
    let (room, reply_to) = placement(line.channel, line.thread_ts, line.ts, dm);
    Message {
        id: message_id(line.ts),
        room,
        author: user_id(line.user),
        text: addressed.text,
        at: instant(line.ts).unwrap_or(fallback),
        reply_to,
        mentions: parsed.mentions,
        mentions_me: addressed.mentions_me,
        attachments: line.files.iter().map(attachment).collect(),
        edited: line.edited,
    }
}

/// A history message as a `Line`, when a person wrote it.
pub fn history_line<'a>(
    channel: &'a SlackChannelId,
    message: &'a SlackHistoryMessage,
) -> Option<Line<'a>> {
    let user = message.sender.user.as_ref()?;
    is_person_message(message.subtype.as_ref(), true).then_some(Line {
        channel,
        ts: &message.origin.ts,
        thread_ts: message.origin.thread_ts.as_ref(),
        user,
        text: message.content.text.as_deref(),
        files: message.content.files.as_deref().unwrap_or_default(),
        edited: message.edited.is_some(),
    })
}

/// Whether a fetched message belongs in the history of `target`: written by
/// a person, and in that room rather than a thread off it or the thread's
/// opening message.
pub fn belongs(target: &Target, message: &SlackHistoryMessage) -> bool {
    if history_line(target.channel(), message).is_none() {
        return false;
    }
    let ts = &message.origin.ts;
    let thread = message.origin.thread_ts.as_ref();
    match target {
        Target::Channel(channel) => is_dm_id(channel) || thread.is_none_or(|t| t == ts),
        Target::Thread(_, parent) => ts != parent,
    }
}

/// A fetched batch as the engine wants it: oldest first, one entry per `ts`,
/// only what belongs in the room, and no more than the `limit` newest.
pub fn history(
    fetched: &[SlackHistoryMessage],
    target: &Target,
    dm: bool,
    me: &SlackUserId,
    agent_name: &str,
    fallback: Instant,
    limit: usize,
) -> Vec<Message> {
    let mut kept: Vec<&SlackHistoryMessage> =
        fetched.iter().filter(|m| belongs(target, m)).collect();
    kept.sort_by_key(|m| ts_key(&m.origin.ts));
    kept.dedup_by(|a, b| a.origin.ts == b.origin.ts);
    let start = kept.len().saturating_sub(limit);
    kept[start..]
        .iter()
        .filter_map(|m| history_line(target.channel(), m))
        .map(|line| message(line, dm, me, agent_name, fallback))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;

    const HOME: &str = "T1";

    fn role(value: serde_json::Value, lead: bool) -> PlatformRole {
        let user = UserRecord::from_user(&slack_user(value));
        platform_role(&user, &SlackTeamId(HOME.to_owned()), lead)
    }

    #[test]
    fn owners_admins_and_everyone_else() {
        let base = json!({ "id": "U1", "team_id": HOME, "name": "priya" });
        let with = |flag: &str| {
            let mut v = base.clone();
            v[flag] = json!(true);
            v
        };
        assert_eq!(role(with("is_owner"), false), PlatformRole::Owner);
        assert_eq!(role(with("is_primary_owner"), false), PlatformRole::Owner);
        assert_eq!(role(with("is_admin"), false), PlatformRole::Admin);
        assert_eq!(role(base.clone(), false), PlatformRole::Regular);
        let mut both = with("is_owner");
        both["is_admin"] = json!(true);
        assert_eq!(role(both, false), PlatformRole::Owner);
    }

    #[test]
    fn the_leads_user_group_counts_as_admin() {
        let user = json!({ "id": "U2", "team_id": HOME, "name": "sam" });
        assert_eq!(role(user.clone(), true), PlatformRole::Admin);
        assert_eq!(role(user, false), PlatformRole::Regular);
    }

    #[test]
    fn guest_accounts_are_external() {
        for flag in ["is_restricted", "is_ultra_restricted", "is_stranger"] {
            let mut user = json!({ "id": "U3", "team_id": HOME, "name": "client" });
            user[flag] = json!(true);
            assert_eq!(role(user, false), PlatformRole::External, "{flag}");
        }
    }

    #[test]
    fn slack_connect_users_from_another_workspace_are_external() {
        let client = json!({ "id": "U4", "team_id": "T9", "name": "am" });
        assert_eq!(role(client.clone(), false), PlatformRole::External);
        let mut owner_elsewhere = client;
        owner_elsewhere["is_owner"] = json!(true);
        owner_elsewhere["is_admin"] = json!(true);
        assert_eq!(role(owner_elsewhere.clone(), true), PlatformRole::External);
    }

    #[test]
    fn a_grid_account_that_includes_this_workspace_is_not_external() {
        let user = json!({
            "id": "W5", "team_id": "T8", "name": "jo",
            "enterprise_user": { "id": "W5", "enterprise_id": "E1", "teams": ["T8", HOME] }
        });
        assert_eq!(role(user, false), PlatformRole::Regular);
    }

    #[test]
    fn members_carry_name_bot_flag_and_zone() {
        let user = slack_user(json!({
            "id": "U6", "team_id": HOME, "name": "alex_k", "real_name": "Alex K",
            "tz": "Europe/London", "profile": { "display_name": "Alex", "real_name": "Alex K" }
        }));
        let mapped = member(&UserRecord::from_user(&user), PlatformRole::Regular);
        assert_eq!(mapped.id.as_str(), "U6");
        assert_eq!(mapped.display_name, "Alex");
        assert_eq!(mapped.timezone.as_deref(), Some("Europe/London"));
        assert!(!mapped.is_bot);

        let bot = slack_user(json!({ "id": "U7", "name": "deploybot", "is_bot": true,
            "profile": { "display_name": "", "real_name": "Deploy Bot" } }));
        let mapped = member(&UserRecord::from_user(&bot), PlatformRole::Regular);
        assert!(mapped.is_bot);
        assert_eq!(mapped.display_name, "Deploy Bot");
        assert!(UserRecord::from_user(&slack_user(json!({ "id": "USLACKBOT" }))).bot);
        assert_eq!(display_name(&slack_user(json!({ "id": "U8" }))), "U8");
    }

    #[test]
    fn an_unknown_user_is_external_until_looked_up() {
        let unknown = unknown_member(&SlackUserId("U9".to_owned()));
        assert_eq!(unknown.platform_role, PlatformRole::External);
        assert_eq!(unknown.display_name, "U9");
    }

    fn me() -> SlackUserId {
        SlackUserId("UBOT".to_owned())
    }

    #[test]
    fn name_at_the_start_addresses_the_agent() {
        for text in ["Q, book it", "q: book it", "Q book it", "  Q, book it", "Q"] {
            let addressed = address(text, false, &me(), "Q");
            assert!(addressed.mentions_me, "{text}");
            assert_eq!(addressed.text, text);
        }
        for text in ["Quincy, hi", "hey Q, book it", "Q?"] {
            assert!(!address(text, false, &me(), "Q").mentions_me, "{text}");
        }
    }

    #[test]
    fn a_leading_bot_mention_becomes_the_name() {
        for text in ["<@UBOT> book it", "<@UBOT>, book it", "  <@UBOT>: book it"] {
            let addressed = address(text, true, &me(), "Q");
            assert!(addressed.mentions_me, "{text}");
            assert_eq!(addressed.text, "Q, book it", "{text}");
        }
        assert_eq!(address("<@UBOT>", true, &me(), "Q").text, "Q");
        assert_eq!(address("@q book it", false, &me(), "Q").text, "Q, book it");
    }

    #[test]
    fn a_mention_later_on_addresses_but_is_not_rewritten() {
        let addressed = address("can you <@UBOT> book it", true, &me(), "Q");
        assert!(addressed.mentions_me);
        assert_eq!(addressed.text, "can you <@UBOT> book it");
        let other = address("<@U42> book it", false, &me(), "Q");
        assert!(!other.mentions_me);
        assert_eq!(other.text, "<@U42> book it");
    }

    #[test]
    fn a_live_message_maps_mentions_links_and_the_bot_rewrite() {
        let channel = SlackChannelId("C1".to_owned());
        let ts = SlackTs("1793613600.000200".to_owned());
        let user = SlackUserId("U1".to_owned());
        let line = Line {
            channel: &channel,
            ts: &ts,
            thread_ts: None,
            user: &user,
            text: Some("<@UBOT|q> ask <@U2> about <https://x.example/a|the doc> &amp; reply"),
            files: &[],
            edited: false,
        };
        let mapped = message(line, false, &me(), "Q", Instant::UNIX_EPOCH);
        assert_eq!(
            mapped.text,
            "Q, ask <@U2> about the doc (https://x.example/a) & reply"
        );
        assert!(mapped.mentions_me);
        assert_eq!(
            mapped.mentions,
            vec![PlatformUserId::new("UBOT"), PlatformUserId::new("U2")]
        );
        assert_eq!(mapped.id.as_str(), "1793613600.000200");
        assert_eq!(mapped.room.as_str(), "C1");
        assert_eq!(mapped.reply_to, None);
        assert_eq!(mapped.at.to_string(), "2026-11-02T10:00:00Z");
    }

    #[test]
    fn thread_replies_live_in_the_thread_room() {
        let c = SlackChannelId("C1".to_owned());
        let parent = SlackTs("100.000001".to_owned());
        let reply = SlackTs("105.000002".to_owned());
        let (room, reply_to) = placement(&c, Some(&parent), &reply, false);
        assert_eq!(room.as_str(), "C1:100.000001");
        assert_eq!(
            reply_to.map(|m| m.to_string()).as_deref(),
            Some("100.000001")
        );

        let (room, reply_to) = placement(&c, Some(&parent), &parent, false);
        assert_eq!((room.as_str(), reply_to), ("C1", None));

        let (room, reply_to) = placement(&c, None, &reply, false);
        assert_eq!((room.as_str(), reply_to), ("C1", None));

        let d = SlackChannelId("D1".to_owned());
        let (room, reply_to) = placement(&d, Some(&parent), &reply, true);
        assert_eq!(room.as_str(), "D1");
        assert_eq!(
            reply_to.map(|m| m.to_string()).as_deref(),
            Some("100.000001")
        );
    }

    #[test]
    fn thread_rooms_name_their_parent_and_workspace() {
        let team = SlackTeamId(HOME.to_owned());
        let c = SlackChannelId("C1".to_owned());
        let thread = thread_room(&c, &SlackTs("100.000001".to_owned()), &team);
        assert_eq!(thread.kind, RoomKind::Thread);
        assert_eq!(thread.id.as_str(), "C1:100.000001");
        assert_eq!(thread.parent, Some(PlatformRoomId::new("C1")));
        assert_eq!(thread.name, None);
        assert_eq!(thread.container.as_deref(), Some(HOME));

        let channel = channel_room(&c, Some("launch"), &team);
        assert_eq!(
            (channel.kind, channel.name.as_deref(), channel.parent),
            (RoomKind::Channel, Some("launch"), None)
        );
        let dm = dm_room(&SlackChannelId("D1".to_owned()), &team);
        assert_eq!((dm.kind, dm.platform), (RoomKind::Dm, Platform::Slack));
    }

    #[test]
    fn room_ids_parse_back_to_targets() {
        assert_eq!(
            parse_room(&"C1".into()).unwrap(),
            Target::Channel(SlackChannelId("C1".to_owned()))
        );
        assert_eq!(
            parse_room(&"C1:100.000001".into()).unwrap(),
            Target::Thread(
                SlackChannelId("C1".to_owned()),
                SlackTs("100.000001".to_owned())
            )
        );
        assert!(parse_room(&"D0AB".into()).is_ok());
        for bad in ["general", "C1:soon", "", "c1", "C1:100"] {
            assert!(parse_room(&bad.into()).is_err(), "{bad}");
        }
        let target = parse_room(&"G7:1.2".into()).unwrap();
        assert_eq!(target.room_id().as_str(), "G7:1.2");
        assert_eq!(target.channel().0, "G7");
    }

    #[test]
    fn timestamps_and_user_ids_are_checked() {
        assert!(parse_ts("1700000000.000100").is_ok());
        assert!(parse_ts("1700000000").is_err());
        assert!(parse_ts("17x.1").is_err());
        assert!(parse_user("U1").is_ok());
        assert!(parse_user("sam").is_err());
    }

    #[test]
    fn ts_become_instants_to_the_millisecond() {
        let at = instant(&SlackTs("1788343200.123456".to_owned())).unwrap();
        assert_eq!(at.as_millis(), 1_788_343_200_123);
        assert_eq!(instant(&SlackTs("nope".to_owned())), None);
        assert!(ts_key(&SlackTs("99.5".to_owned())) < ts_key(&SlackTs("100.000001".to_owned())));
        assert_eq!(ts_key(&SlackTs("1.5".to_owned())), (1, 500_000));
    }

    #[test]
    fn files_become_attachments_with_kind_url_and_name() {
        let files: Vec<SlackFile> = serde_json::from_value(json!([
            { "id": "F1", "name": "plan.png", "mimetype": "image/png",
              "url_private": "https://files.slack.com/F1/plan.png" },
            { "id": "F2", "title": "Notes", "mimetype": "application/pdf" },
            { "id": "F3", "name": "clip.mp4", "mimetype": "video/mp4" },
            { "id": "F4", "name": "memo.m4a", "mimetype": "audio/mp4" },
            { "id": "F5" }
        ]))
        .unwrap();
        let mapped: Vec<_> = files.iter().map(attachment).collect();
        let kinds: Vec<_> = mapped.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["image", "file", "video", "audio", "file"]);
        assert_eq!(
            mapped[0].url.as_deref(),
            Some("https://files.slack.com/F1/plan.png")
        );
        assert_eq!(mapped[0].name.as_deref(), Some("plan.png"));
        assert_eq!(mapped[1].name.as_deref(), Some("Notes"));
        assert_eq!(mapped[4].url, None);
    }

    #[test]
    fn only_messages_people_wrote_count() {
        use SlackMessageEventType as T;
        assert!(is_person_message(None, true));
        assert!(is_person_message(Some(&T::FileShare), true));
        assert!(is_person_message(Some(&T::ThreadBroadcast), true));
        assert!(is_person_message(Some(&T::BotMessage), true));
        assert!(!is_person_message(Some(&T::BotMessage), false));
        assert!(!is_person_message(None, false));
        for system in [
            T::ChannelJoin,
            T::ChannelTopic,
            T::PinnedItem,
            T::ChannelLeave,
        ] {
            assert!(!is_person_message(Some(&system), true));
        }
        assert!(!is_person_message(
            Some(&T::Other("huddle".to_owned())),
            true
        ));
    }

    #[test]
    fn channel_history_is_oldest_first_without_notices_or_thread_replies() {
        let fetched = vec![
            history_message(json!({ "ts": "50.000000", "user": "U1", "text": "five" })),
            history_message(
                json!({ "ts": "40.000000", "user": "U1", "subtype": "channel_join", "text": "joined" }),
            ),
            history_message(
                json!({ "ts": "35.000000", "user": "U2", "thread_ts": "30.000000", "subtype": "thread_broadcast", "text": "also" }),
            ),
            history_message(
                json!({ "ts": "30.000000", "user": "U2", "thread_ts": "30.000000", "text": "three" }),
            ),
            history_message(
                json!({ "ts": "20.000000", "bot_id": "B1", "subtype": "bot_message", "text": "hook" }),
            ),
            history_message(
                json!({ "ts": "10.000000", "user": "U3", "text": "one", "edited": { "user": "U3", "ts": "11.0" } }),
            ),
        ];
        let target = Target::Channel(SlackChannelId("C1".to_owned()));
        let mapped = history(
            &fetched,
            &target,
            false,
            &me(),
            "Q",
            Instant::UNIX_EPOCH,
            100,
        );
        let seen: Vec<_> = mapped
            .iter()
            .map(|m| (m.id.as_str(), m.text.as_str(), m.room.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                ("10.000000", "one", "C1"),
                ("30.000000", "three", "C1"),
                ("50.000000", "five", "C1")
            ]
        );
        assert!(mapped[0].edited);
    }

    #[test]
    fn thread_history_leaves_out_the_opening_message() {
        let fetched = vec![
            history_message(
                json!({ "ts": "30.000000", "thread_ts": "30.000000", "user": "U1", "text": "acme or beacon?" }),
            ),
            history_message(
                json!({ "ts": "31.000000", "thread_ts": "30.000000", "user": "U2", "text": "<@UBOT> what did we pay?" }),
            ),
            history_message(
                json!({ "ts": "32.000000", "thread_ts": "30.000000", "user": "U3", "text": "acme" }),
            ),
        ];
        let target = Target::Thread(
            SlackChannelId("C1".to_owned()),
            SlackTs("30.000000".to_owned()),
        );
        let mapped = history(
            &fetched,
            &target,
            false,
            &me(),
            "Q",
            Instant::UNIX_EPOCH,
            100,
        );
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].room.as_str(), "C1:30.000000");
        assert_eq!(
            mapped[0].reply_to.as_ref().map(|m| m.as_str()),
            Some("30.000000")
        );
        assert_eq!(mapped[0].text, "Q, what did we pay?");
        assert!(mapped[0].mentions_me);
    }

    #[test]
    fn history_keeps_the_newest_up_to_the_limit_once_each() {
        let fetched: Vec<_> = [5, 4, 3, 3, 2, 1]
            .iter()
            .map(|n| {
                history_message(json!({ "ts": format!("{n}.000000"), "user": "U1", "text": "m" }))
            })
            .collect();
        let target = Target::Channel(SlackChannelId("C1".to_owned()));
        let ids: Vec<_> = history(&fetched, &target, false, &me(), "Q", Instant::UNIX_EPOCH, 3)
            .iter()
            .map(|m| m.id.to_string())
            .collect();
        assert_eq!(ids, ["3.000000", "4.000000", "5.000000"]);
        assert!(history(&fetched, &target, false, &me(), "Q", Instant::UNIX_EPOCH, 0).is_empty());
    }
}
