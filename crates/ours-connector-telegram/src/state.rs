//! What the connector remembers about its chats and the rule that turns an
//! update into Ours events. It performs no I/O: what it lacks is named by
//! `missing` for the caller to fetch before calling `handle`.
//!
//! Telegram does not list a group's members to bots, so a group's roster is
//! its administrators plus everyone seen speaking, reacting or voting, and
//! it grows as people are seen.

use crate::disk::Saver;
use crate::map::{self, Target, UserRecord};
use crate::poll;
use crate::reaction;
use crate::text::Me;
use frankenstein::types::{
    Chat, ChatFullInfo, ChatMemberUpdated, ChatType, Message as TgMessage, MessageReactionUpdated,
    Poll, PollAnswer, User,
};
use frankenstein::updates::UpdateContent;
use ours_types::{InboundEvent, Instant, Member, PlatformRole, PlatformRoomId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// Topics remembered per group. A forgotten topic is synced again when it
/// next has a message.
pub const TOPICS_PER_CHAT: usize = 100;

/// Topic messages remembered so that reactions to them land in the topic.
const PLACED: usize = 10_000;

/// Polls remembered so that votes find their message.
const POLLS: usize = 1_000;

/// How long an administrator list is trusted before it is read again.
pub const ADMINS_FRESH_SECONDS: i64 = 3600;

/// How long to wait before trying a failed administrator read again.
pub const ADMINS_RETRY_SECONDS: i64 = 60;

/// Something to fetch before an update can be handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Read a group and its administrators. The chat is as the update
    /// showed it, for when the read fails.
    Chat(Box<Chat>),
    /// Read a group's administrators again.
    Admins(i64),
}

/// A group as `getChat` or an update describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInfo {
    pub id: i64,
    pub title: Option<String>,
    pub forum: bool,
}

impl From<&Chat> for ChatInfo {
    fn from(chat: &Chat) -> Self {
        ChatInfo {
            id: chat.id,
            title: chat.title.clone(),
            forum: chat.is_forum == Some(true),
        }
    }
}

impl From<&ChatFullInfo> for ChatInfo {
    fn from(chat: &ChatFullInfo) -> Self {
        ChatInfo {
            id: chat.id,
            title: chat.title.clone(),
            forum: chat.is_forum == Some(true),
        }
    }
}

/// An administrator list: each user with owner or admin.
pub type Admins = Vec<(User, PlatformRole)>;

#[derive(Debug)]
struct Topic {
    id: i32,
    name: Option<String>,
}

#[derive(Debug, Default)]
struct ChatRecord {
    title: Option<String>,
    members: BTreeSet<u64>,
    admins: BTreeMap<u64, PlatformRole>,
    admins_at: Option<Instant>,
    admins_tried: Option<Instant>,
    /// Topics seen, least recently active first.
    topics: VecDeque<Topic>,
}

impl ChatRecord {
    fn role(&self, user: u64) -> PlatformRole {
        self.admins.get(&user).copied().unwrap_or_default()
    }

    fn rooms(&self, chat: i64) -> Vec<PlatformRoomId> {
        std::iter::once(map::chat_room_id(chat))
            .chain(self.topics.iter().map(|t| map::topic_room_id(chat, t.id)))
            .collect()
    }

    fn admins_due(&self, now: Instant) -> bool {
        match (self.admins_at, self.admins_tried) {
            (Some(at), _) if now.seconds_since(at) < ADMINS_FRESH_SECONDS => false,
            (_, Some(tried)) if now.seconds_since(tried) < ADMINS_RETRY_SECONDS => false,
            _ => true,
        }
    }
}

/// Which topic a message is in, for updates that carry only its id.
#[derive(Debug, Default)]
struct Placed {
    topics: HashMap<(i64, i32), i32>,
    order: VecDeque<(i64, i32)>,
}

impl Placed {
    fn insert(&mut self, chat: i64, message: i32, topic: i32) {
        if self.topics.insert((chat, message), topic).is_none() {
            self.order.push_back((chat, message));
            if self.order.len() > PLACED
                && let Some(oldest) = self.order.pop_front()
            {
                self.topics.remove(&oldest);
            }
        }
    }

    fn get(&self, chat: i64, message: i32) -> Option<i32> {
        self.topics.get(&(chat, message)).copied()
    }
}

#[derive(Debug)]
struct PollRecord {
    target: Target,
    message: i32,
    options: usize,
    votes: BTreeMap<u64, BTreeSet<u32>>,
    ended: bool,
}

impl PollRecord {
    /// An ended poll takes no more votes, so who voted for what is let go.
    fn end(&mut self) {
        self.ended = true;
        self.votes.clear();
    }
}

/// A poll record as it is kept on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPoll {
    id: String,
    chat: i64,
    topic: Option<i32>,
    message: i32,
    options: usize,
    #[serde(default)]
    votes: BTreeMap<u64, BTreeSet<u32>>,
    #[serde(default)]
    ended: bool,
}

#[derive(Debug)]
pub struct State {
    me: Me,
    me_record: UserRecord,
    users: HashMap<u64, UserRecord>,
    /// Lowercased usernames, for mentions.
    usernames: HashMap<String, u64>,
    chats: BTreeMap<i64, ChatRecord>,
    dms: BTreeSet<i64>,
    placed: Placed,
    polls: HashMap<String, PollRecord>,
    poll_order: VecDeque<String>,
    saver: Option<Saver>,
}

impl State {
    pub fn new(agent_name: String, me: &User) -> Self {
        State {
            me: Me {
                id: me.id,
                username: me.username.clone(),
                agent_name,
            },
            me_record: UserRecord::from_user(me),
            users: HashMap::new(),
            usernames: HashMap::new(),
            chats: BTreeMap::new(),
            dms: BTreeSet::new(),
            placed: Placed::default(),
            polls: HashMap::new(),
            poll_order: VecDeque::new(),
            saver: None,
        }
    }

    pub fn knows_chat(&self, chat: i64) -> bool {
        self.chats.contains_key(&chat)
    }

    fn member(&self, user: u64, chat: Option<&ChatRecord>) -> Member {
        let role = chat.map_or(PlatformRole::Regular, |c| c.role(user));
        let record = if user == self.me.id {
            Some(&self.me_record)
        } else {
            self.users.get(&user)
        };
        match record {
            Some(record) => map::member(record, role),
            None => Member {
                id: map::user_id(user),
                display_name: user.to_string(),
                platform_role: role,
                is_bot: false,
                timezone: None,
            },
        }
    }

    fn roster(&self, record: &ChatRecord) -> Vec<Member> {
        record
            .members
            .iter()
            .map(|id| self.member(*id, Some(record)))
            .collect()
    }

    fn chat_sync(&self, chat: i64) -> Option<InboundEvent> {
        let record = self.chats.get(&chat)?;
        Some(InboundEvent::RoomSync {
            room: map::group_room(chat, record.title.as_deref()),
            members: self.roster(record),
        })
    }

    fn topic_sync(&self, chat: i64, topic: i32) -> Option<InboundEvent> {
        let record = self.chats.get(&chat)?;
        let name = record
            .topics
            .iter()
            .find(|t| t.id == topic)
            .and_then(|t| t.name.as_deref());
        Some(InboundEvent::RoomSync {
            room: map::topic_room(chat, topic, name),
            members: self.roster(record),
        })
    }

    fn dm_sync(&self, chat: i64) -> InboundEvent {
        let user = u64::try_from(chat).unwrap_or_default();
        InboundEvent::RoomSync {
            room: map::dm_room(chat),
            members: vec![self.member(user, None)],
        }
    }

    /// Record a user's latest name and username, and report every room
    /// where they now look different.
    fn see_user(&mut self, user: &User) -> Vec<InboundEvent> {
        if user.id == self.me.id {
            return Vec::new();
        }
        let record = UserRecord::from_user(user);
        let old = self.users.insert(user.id, record.clone());
        if let Some(name) = old.as_ref().and_then(|o| o.username.as_deref())
            && Some(name) != record.username.as_deref()
            && self.usernames.get(&name.to_lowercase()) == Some(&user.id)
        {
            self.usernames.remove(&name.to_lowercase());
        }
        if let Some(name) = &record.username {
            self.usernames.insert(name.to_lowercase(), user.id);
        }
        match old {
            Some(old) if old.name != record.name || old.bot != record.bot => {
                self.updated_everywhere(user.id)
            }
            _ => Vec::new(),
        }
    }

    fn updated_everywhere(&self, user: u64) -> Vec<InboundEvent> {
        let mut events = Vec::new();
        for (chat, record) in self.chats.iter().filter(|(_, r)| r.members.contains(&user)) {
            events.extend(self.updated_in(*chat, record, user));
        }
        if let Ok(dm) = i64::try_from(user)
            && self.dms.contains(&dm)
        {
            events.push(InboundEvent::MemberUpdated {
                room: map::chat_room_id(dm),
                member: self.member(user, None),
            });
        }
        events
    }

    fn updated_in(&self, chat: i64, record: &ChatRecord, user: u64) -> Vec<InboundEvent> {
        let member = self.member(user, Some(record));
        record
            .rooms(chat)
            .into_iter()
            .map(|room| InboundEvent::MemberUpdated {
                room,
                member: member.clone(),
            })
            .collect()
    }

    /// Add a user to a group's roster and announce them in the group and
    /// its topics. Nothing when they are already on it. Only someone who
    /// just joined is reported as a join; a member seen for the first time
    /// since a restart is an update, so the engine does not welcome them.
    fn join(&mut self, chat: i64, user: u64, now: Instant, arrival: Arrival) -> Vec<InboundEvent> {
        let added = self
            .chats
            .get_mut(&chat)
            .is_some_and(|record| record.members.insert(user));
        let Some(record) = self.chats.get(&chat).filter(|_| added) else {
            return Vec::new();
        };
        let member = self.member(user, Some(record));
        record
            .rooms(chat)
            .into_iter()
            .map(|room| match arrival {
                Arrival::Joined => InboundEvent::MemberJoined {
                    room,
                    member: member.clone(),
                    at: now,
                },
                Arrival::Seen => InboundEvent::MemberUpdated {
                    room,
                    member: member.clone(),
                },
            })
            .collect()
    }

    fn leave(&mut self, chat: i64, user: u64, now: Instant) -> Vec<InboundEvent> {
        if user == self.me.id {
            return self.drop_chat(chat, now);
        }
        let Some(record) = self.chats.get_mut(&chat) else {
            return Vec::new();
        };
        record.admins.remove(&user);
        if !record.members.remove(&user) {
            return Vec::new();
        }
        record
            .rooms(chat)
            .into_iter()
            .map(|room| InboundEvent::MemberLeft {
                room,
                user: map::user_id(user),
                at: now,
            })
            .collect()
    }

    /// Set a user's role in a group, reporting it where they are on the roster.
    fn set_role(&mut self, chat: i64, user: u64, role: PlatformRole) -> Vec<InboundEvent> {
        let Some(record) = self.chats.get_mut(&chat) else {
            return Vec::new();
        };
        let before = record.role(user);
        if role == PlatformRole::Regular {
            record.admins.remove(&user);
        } else {
            record.admins.insert(user, role);
        }
        let Some(record) = self.chats.get(&chat) else {
            return Vec::new();
        };
        if before == role || !record.members.contains(&user) {
            return Vec::new();
        }
        self.updated_in(chat, record, user)
    }

    /// The agent is out of a group: leave it and every topic of it.
    pub fn drop_chat(&mut self, chat: i64, now: Instant) -> Vec<InboundEvent> {
        let Some(record) = self.chats.remove(&chat) else {
            return Vec::new();
        };
        record
            .rooms(chat)
            .into_iter()
            .map(|room| InboundEvent::MemberLeft {
                room,
                user: map::user_id(self.me.id),
                at: now,
            })
            .collect()
    }

    /// Record a group the agent is in, with its administrators when they
    /// could be read. A new group is synced; a known one is updated.
    pub fn apply_chat(
        &mut self,
        info: ChatInfo,
        admins: Option<Admins>,
        now: Instant,
    ) -> Vec<InboundEvent> {
        let chat = info.id;
        if let Some(record) = self.chats.get_mut(&chat) {
            let renamed = record.title != info.title;
            record.title = info.title;
            let mut events: Vec<InboundEvent> = Vec::new();
            if renamed {
                events.extend(self.chat_sync(chat));
            }
            events.extend(self.apply_admins(chat, admins, now));
            return events;
        }
        let mut events = Vec::new();
        let mut record = ChatRecord {
            title: info.title,
            admins_tried: Some(now),
            ..ChatRecord::default()
        };
        record.members.insert(self.me.id);
        if let Some(list) = admins {
            for (user, role) in &list {
                events.extend(self.see_user(user));
                record.members.insert(user.id);
                if *role != PlatformRole::Regular {
                    record.admins.insert(user.id, *role);
                }
            }
            record.admins_at = Some(now);
        }
        self.chats.insert(chat, record);
        events.extend(self.chat_sync(chat));
        events
    }

    /// Take a fresh administrator list. New administrators join the roster;
    /// anyone whose role changed is updated in the group and its topics.
    pub fn apply_admins(
        &mut self,
        chat: i64,
        admins: Option<Admins>,
        now: Instant,
    ) -> Vec<InboundEvent> {
        let Some(record) = self.chats.get_mut(&chat) else {
            return Vec::new();
        };
        record.admins_tried = Some(now);
        let Some(list) = admins else {
            return Vec::new();
        };
        let mut events: Vec<InboundEvent> = Vec::new();
        for (user, _) in &list {
            events.extend(self.see_user(user));
        }
        let fresh: BTreeMap<u64, PlatformRole> = list
            .iter()
            .filter(|(_, role)| *role != PlatformRole::Regular)
            .map(|(user, role)| (user.id, *role))
            .collect();
        let Some(record) = self.chats.get_mut(&chat) else {
            return events;
        };
        record.admins_at = Some(now);
        let old = std::mem::replace(&mut record.admins, fresh);
        let changed: BTreeSet<u64> = old
            .keys()
            .chain(record.admins.keys())
            .filter(|id| old.get(id) != record.admins.get(id))
            .copied()
            .collect();
        let newcomers: Vec<u64> = record
            .admins
            .keys()
            .filter(|id| !record.members.contains(id))
            .copied()
            .collect();
        for id in &newcomers {
            events.extend(self.join(chat, *id, now, Arrival::Seen));
        }
        if let Some(record) = self.chats.get(&chat) {
            for id in changed.iter().filter(|id| !newcomers.contains(id)) {
                if record.members.contains(id) {
                    events.extend(self.updated_in(chat, record, *id));
                }
            }
        }
        events
    }

    /// What must be fetched before `update` can be handled.
    pub fn missing(&self, update: &UpdateContent, now: Instant) -> Vec<Want> {
        let chat = match update {
            UpdateContent::Message(m) | UpdateContent::EditedMessage(m) => {
                let leaving = m
                    .left_chat_member
                    .as_ref()
                    .is_some_and(|u| u.id == self.me.id);
                if leaving || m.migrate_to_chat_id.is_some() {
                    return Vec::new();
                }
                &m.chat
            }
            UpdateContent::MessageReaction(r) => &r.chat,
            UpdateContent::ChatMember(u) => &u.chat,
            UpdateContent::MyChatMember(u) => {
                if !map::is_group(u.chat.type_field) || !map::is_present(&u.new_chat_member) {
                    return Vec::new();
                }
                return if self.chats.contains_key(&u.chat.id) {
                    vec![Want::Admins(u.chat.id)]
                } else {
                    vec![Want::Chat(Box::new(u.chat.clone()))]
                };
            }
            _ => return Vec::new(),
        };
        if !map::is_group(chat.type_field) {
            return Vec::new();
        }
        match self.chats.get(&chat.id) {
            None => vec![Want::Chat(Box::new(chat.clone()))],
            Some(record) if record.admins_due(now) => vec![Want::Admins(chat.id)],
            Some(_) => Vec::new(),
        }
    }

    pub fn handle(&mut self, update: &UpdateContent, now: Instant) -> Vec<InboundEvent> {
        match update {
            UpdateContent::Message(m) => self.on_message(m, false, now),
            UpdateContent::EditedMessage(m) => self.on_message(m, true, now),
            UpdateContent::MessageReaction(r) => self.on_reaction(r, now),
            UpdateContent::ChatMember(u) => self.on_member(u, now),
            UpdateContent::MyChatMember(u) => self.on_my_member(u, now),
            UpdateContent::Poll(p) => self.on_poll(p).into_iter().collect(),
            UpdateContent::PollAnswer(a) => self.on_answer(a, now),
            _ => Vec::new(),
        }
    }

    fn on_message(&mut self, m: &TgMessage, edited: bool, now: Instant) -> Vec<InboundEvent> {
        match m.chat.type_field {
            ChatType::Private => self.on_private(m, edited, now),
            ChatType::Group | ChatType::Supergroup => self.on_group(m, edited, now),
            ChatType::Channel => Vec::new(),
        }
    }

    fn lookup(&self) -> impl Fn(&str) -> Option<u64> + '_ {
        |name: &str| self.usernames.get(&name.to_lowercase()).copied()
    }

    fn on_group(&mut self, m: &TgMessage, edited: bool, now: Instant) -> Vec<InboundEvent> {
        let chat = m.chat.id;
        if let Some(to) = m.migrate_to_chat_id {
            tracing::warn!(
                from = chat,
                to,
                "the group became a supergroup with a new chat id; put {to} under the group's rooms"
            );
            return self.drop_chat(chat, now);
        }
        if !self.chats.contains_key(&chat) {
            tracing::debug!(chat, "message in a group the agent could not read");
            return Vec::new();
        }
        let mut events = Vec::new();
        if let Some(users) = &m.new_chat_members {
            let me = self.me.id;
            for user in users.iter().filter(|u| u.id != me) {
                events.extend(self.see_user(user));
                events.extend(self.join(chat, user.id, now, Arrival::Joined));
            }
            return events;
        }
        if let Some(user) = &m.left_chat_member {
            return self.leave(chat, user.id, now);
        }
        if let Some(title) = &m.new_chat_title {
            if let Some(record) = self.chats.get_mut(&chat) {
                record.title = Some(title.clone());
            }
            return self.chat_sync(chat).into_iter().collect();
        }
        let topic = map::topic_of(m);
        if let Some(edit) = &m.forum_topic_edited {
            return match (topic.or(m.message_thread_id), &edit.name) {
                (Some(topic), Some(name)) => self.see_topic(chat, topic, Some(name)),
                _ => Vec::new(),
            };
        }
        let name = map::topic_name(m);
        if m.forum_topic_created.is_some() {
            return match topic {
                Some(topic) => self.see_topic(chat, topic, name.as_deref()),
                None => Vec::new(),
            };
        }
        let Some(from) = m.from.as_deref() else {
            return events;
        };
        // Anonymous admins and channels post as a chat, not as a person.
        if from.id == self.me.id || m.sender_chat.is_some() {
            return events;
        }
        events.extend(self.see_user(from));
        events.extend(self.join(chat, from.id, now, Arrival::Seen));
        if let Some(topic) = topic {
            events.extend(self.see_topic(chat, topic, name.as_deref()));
            self.placed.insert(chat, m.message_id, topic);
        }
        if let Some(message) = map::message(m, &self.me, topic, edited, self.lookup(), now) {
            events.push(if edited {
                InboundEvent::MessageEdited(message)
            } else {
                InboundEvent::Message(message)
            });
        }
        events
    }

    fn on_private(&mut self, m: &TgMessage, edited: bool, now: Instant) -> Vec<InboundEvent> {
        let Some(from) = m.from.as_deref() else {
            return Vec::new();
        };
        if from.id == self.me.id {
            return Vec::new();
        }
        let mut events = self.see_user(from);
        events.extend(self.see_dm(m.chat.id));
        if let Some(message) = map::message(m, &self.me, None, edited, self.lookup(), now) {
            events.push(if edited {
                InboundEvent::MessageEdited(message)
            } else {
                InboundEvent::Message(message)
            });
        }
        events
    }

    fn see_dm(&mut self, chat: i64) -> Option<InboundEvent> {
        self.dms.insert(chat).then(|| self.dm_sync(chat))
    }

    /// Note a topic as active, syncing it the first time and when its name
    /// becomes known or changes.
    fn see_topic(&mut self, chat: i64, topic: i32, name: Option<&str>) -> Vec<InboundEvent> {
        let Some(record) = self.chats.get_mut(&chat) else {
            return Vec::new();
        };
        let changed = match record.topics.iter().position(|t| t.id == topic) {
            Some(at) => {
                let Some(mut entry) = record.topics.remove(at) else {
                    return Vec::new();
                };
                let renamed = name.is_some() && entry.name.as_deref() != name;
                if renamed {
                    entry.name = name.map(str::to_owned);
                }
                record.topics.push_back(entry);
                renamed
            }
            None => {
                record.topics.push_back(Topic {
                    id: topic,
                    name: name.map(str::to_owned),
                });
                if record.topics.len() > TOPICS_PER_CHAT {
                    record.topics.pop_front();
                }
                true
            }
        };
        if changed {
            self.topic_sync(chat, topic).into_iter().collect()
        } else {
            Vec::new()
        }
    }

    fn on_reaction(&mut self, r: &MessageReactionUpdated, now: Instant) -> Vec<InboundEvent> {
        let Some(user) = &r.user else {
            return Vec::new();
        };
        if user.id == self.me.id {
            return Vec::new();
        }
        let chat = r.chat.id;
        let mut events = Vec::new();
        let room = match r.chat.type_field {
            ChatType::Private => {
                events.extend(self.see_user(user));
                events.extend(self.see_dm(chat));
                map::chat_room_id(chat)
            }
            ChatType::Group | ChatType::Supergroup if self.chats.contains_key(&chat) => {
                events.extend(self.see_user(user));
                events.extend(self.join(chat, user.id, now, Arrival::Seen));
                match self.placed.get(chat, r.message_id) {
                    Some(topic) => {
                        events.extend(self.see_topic(chat, topic, None));
                        map::topic_room_id(chat, topic)
                    }
                    None => map::chat_room_id(chat),
                }
            }
            _ => return Vec::new(),
        };
        let (added, removed) = reaction::diff(&r.old_reaction, &r.new_reaction);
        let at = map::instant(r.date).unwrap_or(now);
        let (message, who) = (map::message_id(r.message_id), map::user_id(user.id));
        events.extend(
            removed
                .into_iter()
                .map(|emoji| InboundEvent::ReactionRemoved {
                    room: room.clone(),
                    message: message.clone(),
                    user: who.clone(),
                    emoji,
                    at,
                }),
        );
        events.extend(added.into_iter().map(|emoji| InboundEvent::ReactionAdded {
            room: room.clone(),
            message: message.clone(),
            user: who.clone(),
            emoji,
            at,
        }));
        events
    }

    fn on_member(&mut self, u: &ChatMemberUpdated, now: Instant) -> Vec<InboundEvent> {
        let chat = u.chat.id;
        let user = map::member_user(&u.new_chat_member);
        if !map::is_group(u.chat.type_field) || user.id == self.me.id || !self.knows_chat(chat) {
            return Vec::new();
        }
        let mut events = self.see_user(user);
        if !map::is_present(&u.new_chat_member) {
            events.extend(self.leave(chat, user.id, now));
            return events;
        }
        let role = map::platform_role(&u.new_chat_member);
        events.extend(self.set_role(chat, user.id, role));
        let arrival = if map::is_present(&u.old_chat_member) {
            Arrival::Seen
        } else {
            Arrival::Joined
        };
        events.extend(self.join(chat, user.id, now, arrival));
        events
    }

    fn on_my_member(&mut self, u: &ChatMemberUpdated, now: Instant) -> Vec<InboundEvent> {
        if !map::is_group(u.chat.type_field) {
            return Vec::new();
        }
        if !map::is_present(&u.new_chat_member) {
            return self.drop_chat(u.chat.id, now);
        }
        let role = map::platform_role(&u.new_chat_member);
        self.set_role(u.chat.id, self.me.id, role)
    }

    fn on_poll(&mut self, poll: &Poll) -> Option<InboundEvent> {
        if !poll.is_closed {
            return None;
        }
        let Some(record) = self.polls.get_mut(&poll.id) else {
            tracing::debug!(poll = %poll.id, "a poll closed that the agent has no record of");
            return None;
        };
        if record.ended {
            return None;
        }
        record.end();
        let ended = InboundEvent::PollEnded {
            room: record.target.room_id(),
            message: map::message_id(record.message),
            counts: poll::counts(poll),
        };
        self.polls_changed();
        Some(ended)
    }

    fn on_answer(&mut self, answer: &PollAnswer, now: Instant) -> Vec<InboundEvent> {
        let Some(user) = answer.user.as_deref() else {
            return Vec::new();
        };
        let Some(record) = self.polls.get(&answer.poll_id) else {
            tracing::debug!(poll = %answer.poll_id, "a vote on a poll the agent has no record of");
            return Vec::new();
        };
        if user.id == self.me.id {
            return Vec::new();
        }
        let (target, message, options) = (record.target, record.message, record.options);
        let mut events = self.see_user(user);
        events.extend(self.join(target.chat, user.id, now, Arrival::Seen));
        if let Some(topic) = target.topic {
            events.extend(self.see_topic(target.chat, topic, None));
        }
        let after = poll::choice(&answer.option_ids, options);
        let Some(record) = self.polls.get_mut(&answer.poll_id) else {
            return events;
        };
        let before = if after.is_empty() {
            record.votes.remove(&user.id)
        } else {
            record.votes.insert(user.id, after.clone())
        }
        .unwrap_or_default();
        let changes = poll::changes(&before, &after);
        if !changes.is_empty() {
            self.polls_changed();
        }
        events.extend(
            changes
                .into_iter()
                .map(|(option, added)| InboundEvent::PollVote {
                    room: target.room_id(),
                    message: map::message_id(message),
                    user: map::user_id(user.id),
                    option,
                    added,
                    at: now,
                }),
        );
        events
    }

    /// A message the agent sent, so reactions to it in a topic land there.
    pub fn record_sent(&mut self, target: Target, message: i32) {
        if let Some(topic) = target.topic {
            self.placed.insert(target.chat, message, topic);
        }
    }

    /// A DM the agent wrote into. Synced the first time.
    pub fn open_dm(&mut self, chat: &Chat) -> Vec<InboundEvent> {
        if self.dms.contains(&chat.id) {
            return Vec::new();
        }
        if let Ok(user) = u64::try_from(chat.id)
            && !self.users.contains_key(&user)
        {
            self.users.insert(user, user_from_chat(user, chat));
        }
        self.see_dm(chat.id).into_iter().collect()
    }

    /// A poll the agent created, so votes on it can be placed.
    pub fn record_poll(&mut self, id: String, target: Target, message: i32, options: usize) {
        let record = PollRecord {
            target,
            message,
            options,
            votes: BTreeMap::new(),
            ended: false,
        };
        self.keep_poll(id, record);
        self.polls_changed();
    }

    /// Remember a poll, forgetting the oldest past `POLLS`.
    fn keep_poll(&mut self, id: String, record: PollRecord) {
        if self.polls.insert(id.clone(), record).is_none() {
            self.poll_order.push_back(id);
            if self.poll_order.len() > POLLS
                && let Some(oldest) = self.poll_order.pop_front()
            {
                self.polls.remove(&oldest);
            }
        }
    }

    /// Take back the poll records saved before a restart, oldest first.
    pub fn restore_polls(&mut self, saved: Vec<SavedPoll>) {
        for poll in saved {
            let record = PollRecord {
                target: Target {
                    chat: poll.chat,
                    topic: poll.topic,
                },
                message: poll.message,
                options: poll.options,
                votes: poll.votes,
                ended: poll.ended,
            };
            self.keep_poll(poll.id, record);
        }
    }

    /// Hand every later change to the poll records to `saver`.
    pub fn save_polls(&mut self, saver: Saver) {
        self.saver = Some(saver);
    }

    /// The poll records as they are saved, oldest first.
    pub fn saved_polls(&self) -> Vec<SavedPoll> {
        self.poll_order
            .iter()
            .filter_map(|id| {
                let record = self.polls.get(id)?;
                Some(SavedPoll {
                    id: id.clone(),
                    chat: record.target.chat,
                    topic: record.target.topic,
                    message: record.message,
                    options: record.options,
                    votes: record.votes.clone(),
                    ended: record.ended,
                })
            })
            .collect()
    }

    fn polls_changed(&self) {
        if let Some(saver) = &self.saver {
            saver.save(self.saved_polls());
        }
    }

    /// A poll the agent stopped. The result is reported once, whether it
    /// arrives here or as an update.
    pub fn close_poll(
        &mut self,
        poll: &Poll,
        target: Target,
        message: i32,
    ) -> Option<InboundEvent> {
        let (room, message) = match self.polls.get_mut(&poll.id) {
            Some(record) if record.ended => return None,
            Some(record) => {
                record.end();
                let place = (record.target.room_id(), record.message);
                self.polls_changed();
                place
            }
            None => (target.room_id(), message),
        };
        Some(InboundEvent::PollEnded {
            room,
            message: map::message_id(message),
            counts: poll::counts(poll),
        })
    }
}

/// A person known only from their private chat with the agent.
fn user_from_chat(id: u64, chat: &Chat) -> UserRecord {
    let name = [chat.first_name.as_deref(), chat.last_name.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let username = chat.username.clone().filter(|u| !u.is_empty());
    let name = if name.is_empty() {
        username.clone().unwrap_or_else(|| id.to_string())
    } else {
        name
    };
    UserRecord {
        id,
        name,
        username,
        bot: false,
    }
}

/// How someone came to be on a group's roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// They joined the group.
    Joined,
    /// They were already in it and are seen for the first time.
    Seen,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use ours_types::{Message, PlatformRole as R, RoomKind};
    use serde_json::{Value, json};

    fn admins(list: &[Value]) -> Option<Admins> {
        Some(
            list.iter()
                .map(|v| {
                    let member = chat_member(v.clone());
                    (
                        map::member_user(&member).clone(),
                        map::platform_role(&member),
                    )
                })
                .collect(),
        )
    }

    fn info(forum: bool) -> ChatInfo {
        ChatInfo {
            id: GROUP,
            title: Some("The Six".to_owned()),
            forum,
        }
    }

    /// Started, with the group synced: Sam created it, Jo and the bot are admins.
    fn synced(forum: bool) -> State {
        let mut state = State::new("Q".to_owned(), &bot_user());
        state.apply_chat(
            info(forum),
            admins(&[creator(SAM), administrator(JO), administrator(BOT)]),
            at(),
        );
        state
    }

    fn handle(state: &mut State, value: Value) -> Vec<InboundEvent> {
        state.handle(&update(value), at())
    }

    fn roles(members: &[Member]) -> Vec<(&str, R)> {
        members
            .iter()
            .map(|m| (m.id.as_str(), m.platform_role))
            .collect()
    }

    fn only_message(events: &[InboundEvent]) -> &Message {
        match events {
            [InboundEvent::Message(m)] => m,
            other => panic!("{other:?}"),
        }
    }

    fn topic_message(id: i32, from: u64, name: &str, topic: i32, text: &str) -> Value {
        json!({ "message": {
            "message_id": id, "date": 1_793_613_600, "chat": group_chat(GROUP, true),
            "message_thread_id": topic, "is_topic_message": true,
            "from": user_json(from, name), "text": text,
            "reply_to_message": { "message_id": topic, "date": 1, "chat": group_chat(GROUP, true),
                "forum_topic_created": { "name": "Trip", "icon_color": 1 } }
        } })
    }

    #[test]
    fn a_group_syncs_with_its_creator_as_owner_and_admins_as_admins() {
        let mut state = State::new("Q".to_owned(), &bot_user());
        let events = state.apply_chat(
            info(false),
            admins(&[creator(SAM), administrator(JO), administrator(BOT)]),
            at(),
        );
        let [InboundEvent::RoomSync { room, members }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(room.id.as_str(), "-1001234567890");
        assert_eq!(room.name.as_deref(), Some("The Six"));
        assert_eq!(room.kind, RoomKind::Channel);
        assert_eq!(room.container.as_deref(), Some("-1001234567890"));
        assert_eq!(
            roles(members),
            [("11", R::Owner), ("12", R::Admin), ("900", R::Admin)]
        );
        let bot = members.iter().find(|m| m.id.as_str() == "900").unwrap();
        assert!(bot.is_bot);
        assert!(members.iter().all(|m| m.timezone.is_none()));
    }

    #[test]
    fn a_group_whose_admins_could_not_be_read_still_syncs_and_asks_again() {
        let mut state = State::new("Q".to_owned(), &bot_user());
        let events = state.apply_chat(info(false), None, at());
        let [InboundEvent::RoomSync { members, .. }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(roles(members), [("900", R::Regular)]);
        let message = update(said(1, ALEX, "Alex", "hi"));
        assert!(state.missing(&message, at().plus_seconds(10)).is_empty());
        assert_eq!(
            state.missing(&message, at().plus_seconds(ADMINS_RETRY_SECONDS)),
            [Want::Admins(GROUP)]
        );
    }

    #[test]
    fn the_roster_grows_as_people_speak() {
        let mut state = synced(false);
        let events = handle(&mut state, said(1, ALEX, "Alex", "dinner thursday?"));
        let [
            InboundEvent::MemberUpdated { room, member },
            InboundEvent::Message(message),
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!(room.as_str(), GROUP.to_string());
        assert_eq!(
            (
                member.id.as_str(),
                member.display_name.as_str(),
                member.platform_role
            ),
            ("13", "Alex", R::Regular)
        );
        assert_eq!(message.text, "dinner thursday?");
        let again = handle(&mut state, said(2, ALEX, "Alex", "or friday"));
        assert_eq!(only_message(&again).id.as_str(), "2");
        let admin = handle(&mut state, said(3, JO, "Jo", "friday"));
        assert_eq!(only_message(&admin).author.as_str(), "12");
    }

    #[test]
    fn a_message_in_an_unknown_group_asks_for_it_first() {
        let state = State::new("Q".to_owned(), &bot_user());
        let message = update(said(1, ALEX, "Alex", "hi"));
        let wants = state.missing(&message, at());
        let [Want::Chat(chat)] = wants.as_slice() else {
            panic!()
        };
        assert_eq!(chat.id, GROUP);
        let synced = synced(false);
        assert!(synced.missing(&message, at().plus_seconds(60)).is_empty());
        assert_eq!(
            synced.missing(&message, at().plus_seconds(ADMINS_FRESH_SECONDS)),
            [Want::Admins(GROUP)]
        );
        let dm = update(json!({ "message": { "message_id": 1, "date": 1,
            "chat": private_chat(SAM, "Sam"), "from": user_json(SAM, "Sam"), "text": "hi" } }));
        assert!(state.missing(&dm, at()).is_empty());
    }

    #[test]
    fn a_message_in_a_group_the_agent_could_not_read_is_dropped() {
        let mut state = State::new("Q".to_owned(), &bot_user());
        assert!(handle(&mut state, said(1, ALEX, "Alex", "hi")).is_empty());
    }

    #[test]
    fn a_topic_is_a_room_under_the_group_with_the_groups_roster() {
        let mut state = synced(true);
        let events = handle(
            &mut state,
            topic_message(30, SAM, "Sam", 12, "booked the train"),
        );
        let [
            InboundEvent::RoomSync { room, members },
            InboundEvent::Message(message),
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!(room.id.as_str(), format!("{GROUP}:12"));
        assert_eq!(room.kind, RoomKind::Thread);
        assert_eq!(room.name.as_deref(), Some("Trip"));
        assert_eq!(
            room.parent.as_ref().map(|p| p.as_str()),
            Some("-1001234567890")
        );
        assert_eq!(members.len(), 3);
        assert_eq!(message.room.as_str(), format!("{GROUP}:12"));
        assert_eq!(message.reply_to, None);

        let next = handle(&mut state, topic_message(31, JO, "Jo", 12, "thanks"));
        assert_eq!(only_message(&next).room.as_str(), format!("{GROUP}:12"));

        let newcomer = handle(&mut state, said(32, ALEX, "Alex", "hi all"));
        let joined: Vec<_> = newcomer
            .iter()
            .filter_map(|e| match e {
                InboundEvent::MemberUpdated { room, .. } => Some(room.as_str().to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(joined, [GROUP.to_string(), format!("{GROUP}:12")]);
    }

    #[test]
    fn topics_named_and_renamed_are_synced_again() {
        let mut state = synced(true);
        let opened = handle(
            &mut state,
            json!({ "message": { "message_id": 40, "date": 1, "chat": group_chat(GROUP, true),
                "message_thread_id": 40, "is_topic_message": true, "from": user_json(SAM, "Sam"),
                "forum_topic_created": { "name": "Budget", "icon_color": 1 } } }),
        );
        assert!(matches!(opened.as_slice(),
            [InboundEvent::RoomSync { room, .. }] if room.name.as_deref() == Some("Budget")));
        let renamed = handle(
            &mut state,
            json!({ "message": { "message_id": 41, "date": 1, "chat": group_chat(GROUP, true),
                "message_thread_id": 40, "is_topic_message": true, "from": user_json(SAM, "Sam"),
                "forum_topic_edited": { "name": "Budget 2027" } } }),
        );
        assert!(matches!(renamed.as_slice(),
            [InboundEvent::RoomSync { room, .. }] if room.name.as_deref() == Some("Budget 2027")));
        let same = handle(
            &mut state,
            json!({ "message": { "message_id": 42, "date": 1, "chat": group_chat(GROUP, true),
                "message_thread_id": 40, "is_topic_message": true, "from": user_json(SAM, "Sam"),
                "forum_topic_edited": { "name": "Budget 2027" } } }),
        );
        assert!(same.is_empty());
    }

    #[test]
    fn forgotten_topics_sync_again_when_they_come_back() {
        let mut state = synced(true);
        let count = i32::try_from(TOPICS_PER_CHAT).unwrap();
        for topic in 1..=count + 1 {
            let events = handle(
                &mut state,
                topic_message(1000 + topic, SAM, "Sam", topic, "x"),
            );
            assert!(
                matches!(events.first(), Some(InboundEvent::RoomSync { .. })),
                "{topic}"
            );
        }
        let back = handle(&mut state, topic_message(5000, SAM, "Sam", 1, "x"));
        assert!(matches!(
            back.as_slice(),
            [InboundEvent::RoomSync { .. }, InboundEvent::Message(_)]
        ));
    }

    #[test]
    fn a_private_chat_is_a_dm_with_one_regular_member() {
        let mut state = synced(false);
        let dm = |id: i32, text: &str| {
            json!({ "message": { "message_id": id, "date": 1, "chat": private_chat(SAM, "Sam"),
                "from": user_json(SAM, "Sam"), "text": text } })
        };
        let events = handle(&mut state, dm(1, "nudge me, not them"));
        let [
            InboundEvent::RoomSync { room, members },
            InboundEvent::Message(message),
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!((room.id.as_str(), room.kind), ("11", RoomKind::Dm));
        assert_eq!(roles(members), [("11", R::Regular)]);
        assert_eq!(message.room.as_str(), "11");
        assert_eq!(
            only_message(&handle(&mut state, dm(2, "thanks"))).text,
            "thanks"
        );
    }

    #[test]
    fn start_opens_the_first_conversation() {
        let mut state = synced(false);
        let events = handle(
            &mut state,
            json!({ "message": { "message_id": 1, "date": 1, "chat": private_chat(SAM, "Sam"),
                "from": user_json(SAM, "Sam"), "text": "/start",
                "entities": [{ "type": "bot_command", "offset": 0, "length": 6 }] } }),
        );
        let InboundEvent::Message(message) = &events[1] else {
            panic!("{events:?}")
        };
        assert_eq!(message.text, "hello");
    }

    #[test]
    fn the_agents_own_posts_and_anonymous_admins_are_ignored() {
        let mut state = synced(false);
        assert!(
            handle(
                &mut state,
                json!({ "message": { "message_id": 1, "date": 1, "chat": group_chat(GROUP, false),
                    "from": bot_json(), "text": "📌 Open" } })
            )
            .is_empty()
        );
        assert!(
            handle(
                &mut state,
                json!({ "message": { "message_id": 2, "date": 1, "chat": group_chat(GROUP, false),
                    "from": { "id": 1_087_968_824u64, "is_bot": true, "first_name": "Group",
                        "username": "GroupAnonymousBot" },
                    "sender_chat": group_chat(GROUP, false), "text": "Q, poll thai or pizza" } })
            )
            .is_empty()
        );
    }

    #[test]
    fn edits_arrive_as_edits() {
        let mut state = synced(false);
        handle(&mut state, said(5, SAM, "Sam", "Acme"));
        let events = handle(
            &mut state,
            json!({ "edited_message": { "message_id": 5, "date": 1_793_613_600, "edit_date": 1_793_613_700,
                "chat": group_chat(GROUP, false), "from": user_json(SAM, "Sam"), "text": "Acme hit the date" } }),
        );
        let [InboundEvent::MessageEdited(message)] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(
            (message.id.as_str(), message.text.as_str()),
            ("5", "Acme hit the date")
        );
        assert!(message.edited);
    }

    #[test]
    fn mentions_by_username_resolve_from_what_was_seen() {
        let mut state = synced(false);
        handle(&mut state, said(1, ALEX, "Alex", "hi"));
        let events = handle(
            &mut state,
            json!({ "message": { "message_id": 2, "date": 1, "chat": group_chat(GROUP, false),
                "from": user_json(JO, "Jo"), "text": "@Alex can you and @ours_q_bot remind us",
                "entities": [{ "type": "mention", "offset": 0, "length": 5 },
                    { "type": "mention", "offset": 18, "length": 11 }] } }),
        );
        let message = only_message(&events);
        let mentions: Vec<_> = message.mentions.iter().map(|m| m.as_str()).collect();
        assert_eq!(mentions, ["13", "900"]);
        assert!(message.mentions_me);
    }

    #[test]
    fn joins_and_leaves_reach_the_group_and_its_topics() {
        let mut state = synced(true);
        handle(&mut state, topic_message(30, SAM, "Sam", 12, "x"));
        let joined = handle(
            &mut state,
            json!({ "message": { "message_id": 50, "date": 1, "chat": group_chat(GROUP, true),
                "from": user_json(PRIYA, "Priya"), "new_chat_members": [user_json(PRIYA, "Priya")] } }),
        );
        let rooms: Vec<_> = joined
            .iter()
            .map(|e| match e {
                InboundEvent::MemberJoined { room, member, .. } => {
                    (room.as_str().to_owned(), member.platform_role)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            rooms,
            [
                (GROUP.to_string(), R::Regular),
                (format!("{GROUP}:12"), R::Regular)
            ]
        );
        let left = handle(
            &mut state,
            json!({ "message": { "message_id": 51, "date": 1, "chat": group_chat(GROUP, true),
                "from": user_json(PRIYA, "Priya"), "left_chat_member": user_json(PRIYA, "Priya") } }),
        );
        assert_eq!(left.len(), 2);
        assert!(
            left.iter().all(
                |e| matches!(e, InboundEvent::MemberLeft { user, .. } if user.as_str() == "14")
            )
        );
        let twice = handle(
            &mut state,
            json!({ "message": { "message_id": 52, "date": 1, "chat": group_chat(GROUP, true),
                "from": user_json(PRIYA, "Priya"), "left_chat_member": user_json(PRIYA, "Priya") } }),
        );
        assert!(twice.is_empty());
    }

    fn member_update(old: Value, new: Value) -> Value {
        json!({ "chat_member": { "chat": group_chat(GROUP, false), "from": user_json(SAM, "Sam"),
            "date": 1_793_613_600, "old_chat_member": old, "new_chat_member": new } })
    }

    #[test]
    fn chat_member_updates_move_roles_and_the_roster() {
        let mut state = synced(false);
        let joined = handle(&mut state, member_update(left(ALEX), plain_member(ALEX)));
        assert!(matches!(joined.as_slice(),
            [InboundEvent::MemberJoined { member, .. }] if member.platform_role == R::Regular));

        let promoted = handle(
            &mut state,
            member_update(plain_member(ALEX), administrator(ALEX)),
        );
        assert!(matches!(promoted.as_slice(),
            [InboundEvent::MemberUpdated { member, .. }] if member.platform_role == R::Admin));

        let demoted = handle(
            &mut state,
            member_update(administrator(JO), plain_member(JO)),
        );
        assert!(matches!(demoted.as_slice(),
            [InboundEvent::MemberUpdated { member, .. }]
                if member.id.as_str() == "12" && member.platform_role == R::Regular));

        let kicked = handle(
            &mut state,
            member_update(
                administrator(ALEX),
                json!({ "status": "kicked", "user": person(ALEX), "until_date": 0 }),
            ),
        );
        assert!(matches!(kicked.as_slice(),
            [InboundEvent::MemberLeft { user, .. }] if user.as_str() == "13"));

        let promoted_stranger =
            handle(&mut state, member_update(left(PRIYA), administrator(PRIYA)));
        assert!(matches!(promoted_stranger.as_slice(),
            [InboundEvent::MemberJoined { member, .. }] if member.platform_role == R::Admin));
    }

    #[test]
    fn a_member_seen_first_in_a_role_change_has_not_just_joined() {
        let mut state = synced(false);
        let promoted = handle(
            &mut state,
            member_update(plain_member(PRIYA), administrator(PRIYA)),
        );
        assert!(matches!(promoted.as_slice(),
            [InboundEvent::MemberUpdated { member, .. }]
                if member.id.as_str() == "14" && member.platform_role == R::Admin));
    }

    fn my_update(old: Value, new: Value) -> Value {
        json!({ "my_chat_member": { "chat": group_chat(GROUP, false), "from": user_json(SAM, "Sam"),
            "date": 1_793_613_600, "old_chat_member": old, "new_chat_member": new } })
    }

    #[test]
    fn being_added_asks_for_the_group_and_being_removed_ends_it() {
        let fresh = State::new("Q".to_owned(), &bot_user());
        let added = update(my_update(left(BOT), plain_member(BOT)));
        assert!(matches!(
            fresh.missing(&added, at()).as_slice(),
            [Want::Chat(_)]
        ));

        let mut state = synced(true);
        handle(&mut state, topic_message(30, SAM, "Sam", 12, "x"));
        let removed = handle(&mut state, my_update(administrator(BOT), left(BOT)));
        let rooms: Vec<_> = removed
            .iter()
            .map(|e| match e {
                InboundEvent::MemberLeft { room, user, .. } => {
                    (room.as_str().to_owned(), user.as_str().to_owned())
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            rooms,
            [
                (GROUP.to_string(), "900".to_owned()),
                (format!("{GROUP}:12"), "900".to_owned())
            ]
        );
        assert!(!state.knows_chat(GROUP));
        assert!(handle(&mut state, my_update(administrator(BOT), left(BOT))).is_empty());
    }

    #[test]
    fn the_bot_leaving_by_service_message_ends_the_group_too() {
        let mut state = synced(false);
        let leaving = json!({ "message": { "message_id": 60, "date": 1, "chat": group_chat(GROUP, false),
            "from": user_json(SAM, "Sam"), "left_chat_member": bot_json() } });
        assert!(state.missing(&update(leaving.clone()), at()).is_empty());
        let events = handle(&mut state, leaving);
        assert!(matches!(events.as_slice(),
            [InboundEvent::MemberLeft { user, .. }] if user.as_str() == "900"));
    }

    #[test]
    fn a_fresh_admin_list_updates_roles_and_adds_new_admins() {
        let mut state = synced(false);
        handle(&mut state, said(1, ALEX, "Alex", "hi"));
        let events = state.apply_admins(
            GROUP,
            admins(&[
                creator(SAM),
                administrator(ALEX),
                administrator(PRIYA),
                administrator(BOT),
            ]),
            at(),
        );
        let mut seen: Vec<_> = events
            .iter()
            .map(|e| match e {
                InboundEvent::MemberJoined { member, .. } => (
                    "joined",
                    member.id.as_str().to_owned(),
                    member.platform_role,
                ),
                InboundEvent::MemberUpdated { member, .. } => (
                    "updated",
                    member.id.as_str().to_owned(),
                    member.platform_role,
                ),
                other => panic!("{other:?}"),
            })
            .collect();
        seen.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        assert_eq!(
            seen,
            [
                ("updated", "12".to_owned(), R::Regular),
                ("updated", "13".to_owned(), R::Admin),
                ("updated", "14".to_owned(), R::Admin),
            ]
        );
        assert!(
            state
                .apply_admins(
                    GROUP,
                    admins(&[
                        creator(SAM),
                        administrator(ALEX),
                        administrator(PRIYA),
                        administrator(BOT)
                    ]),
                    at()
                )
                .is_empty()
        );
    }

    #[test]
    fn a_renamed_person_is_updated_everywhere() {
        let mut state = synced(false);
        handle(&mut state, said(1, ALEX, "Alex", "hi"));
        handle(
            &mut state,
            json!({ "message": { "message_id": 1, "date": 1, "chat": private_chat(ALEX, "Alex"),
                "from": user_json(ALEX, "Alex"), "text": "hi" } }),
        );
        let events = handle(&mut state, said(2, ALEX, "Alexandra", "new name"));
        let updated: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                InboundEvent::MemberUpdated { room, member } => {
                    Some((room.as_str().to_owned(), member.display_name.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            updated,
            [
                (GROUP.to_string(), "Alexandra".to_owned()),
                ("13".to_owned(), "Alexandra".to_owned())
            ]
        );
    }

    #[test]
    fn a_new_title_resyncs_the_group() {
        let mut state = synced(false);
        let events = handle(
            &mut state,
            json!({ "message": { "message_id": 70, "date": 1, "chat": group_chat(GROUP, false),
                "from": user_json(SAM, "Sam"), "new_chat_title": "The Seven" } }),
        );
        assert!(matches!(events.as_slice(),
            [InboundEvent::RoomSync { room, .. }] if room.name.as_deref() == Some("The Seven")));
    }

    #[test]
    fn a_group_that_became_a_supergroup_is_left_under_its_old_id() {
        let mut state = synced(false);
        let events = handle(
            &mut state,
            json!({ "message": { "message_id": 80, "date": 1, "chat": group_chat(GROUP, false),
                "from": user_json(SAM, "Sam"), "migrate_to_chat_id": -1_009_999_999i64 } }),
        );
        assert!(matches!(
            events.as_slice(),
            [InboundEvent::MemberLeft { .. }]
        ));
        assert!(!state.knows_chat(GROUP));
    }

    fn reaction(user: Value, message: i32, old: &[&str], new: &[&str]) -> Value {
        let list = |emoji: &[&str]| -> Vec<Value> {
            emoji
                .iter()
                .map(|e| json!({ "type": "emoji", "emoji": e }))
                .collect()
        };
        json!({ "message_reaction": { "chat": group_chat(GROUP, true), "message_id": message,
            "user": user, "date": 1_793_613_700, "old_reaction": list(old), "new_reaction": list(new) } })
    }

    #[test]
    fn reactions_are_diffed_and_land_in_the_topic() {
        let mut state = synced(true);
        handle(
            &mut state,
            topic_message(30, SAM, "Sam", 12, "I'll book it"),
        );
        let events = handle(
            &mut state,
            reaction(user_json(JO, "Jo"), 30, &["👍"], &["🔥", "❤"]),
        );
        let seen: Vec<_> = events
            .iter()
            .map(|e| match e {
                InboundEvent::ReactionAdded {
                    room,
                    message,
                    user,
                    emoji,
                    at,
                } => {
                    assert_eq!(at.as_millis(), 1_793_613_700_000);
                    (
                        "added",
                        room.as_str().to_owned(),
                        message.as_str().to_owned(),
                        user.as_str().to_owned(),
                        emoji.clone(),
                    )
                }
                InboundEvent::ReactionRemoved {
                    room,
                    message,
                    user,
                    emoji,
                    ..
                } => (
                    "removed",
                    room.as_str().to_owned(),
                    message.as_str().to_owned(),
                    user.as_str().to_owned(),
                    emoji.clone(),
                ),
                other => panic!("{other:?}"),
            })
            .collect();
        let topic = format!("{GROUP}:12");
        assert_eq!(
            seen,
            [
                (
                    "removed",
                    topic.clone(),
                    "30".to_owned(),
                    "12".to_owned(),
                    "👍".to_owned()
                ),
                (
                    "added",
                    topic.clone(),
                    "30".to_owned(),
                    "12".to_owned(),
                    "🔥".to_owned()
                ),
                (
                    "added",
                    topic,
                    "30".to_owned(),
                    "12".to_owned(),
                    "❤".to_owned()
                ),
            ]
        );
        let unplaced = handle(&mut state, reaction(user_json(JO, "Jo"), 99, &[], &["👀"]));
        assert!(matches!(unplaced.as_slice(),
            [InboundEvent::ReactionAdded { room, .. }] if room.as_str() == GROUP.to_string()));
    }

    #[test]
    fn a_reactor_joins_the_roster_and_the_agents_own_reactions_are_ignored() {
        let mut state = synced(false);
        let events = handle(
            &mut state,
            reaction(user_json(ALEX, "Alex"), 5, &[], &["👍"]),
        );
        assert!(matches!(
            events.as_slice(),
            [
                InboundEvent::MemberUpdated { .. },
                InboundEvent::ReactionAdded { .. }
            ]
        ));
        assert!(handle(&mut state, reaction(bot_json(), 5, &[], &["👀"])).is_empty());
        let anonymous = json!({ "message_reaction": { "chat": group_chat(GROUP, false), "message_id": 5,
            "actor_chat": group_chat(GROUP, false), "date": 1,
            "old_reaction": [], "new_reaction": [{ "type": "emoji", "emoji": "👍" }] } });
        assert!(handle(&mut state, anonymous).is_empty());
    }

    fn answer(user: u64, options: &[u8]) -> Value {
        vote("P1", user, options)
    }

    fn vote(poll: &str, user: u64, options: &[u8]) -> Value {
        json!({ "poll_answer": { "poll_id": poll, "user": user_json(user, "Voter"),
            "option_ids": options, "option_persistent_ids": [] } })
    }

    fn closed(poll: &str) -> Value {
        json!({ "poll": { "id": poll, "question": "when?", "total_voter_count": 3,
            "is_closed": true, "is_anonymous": false, "type": "regular",
            "options": [{ "text": "Thu", "voter_count": 2 }, { "text": "Fri", "voter_count": 1 }] } })
    }

    /// Each vote event as `(option, added)`.
    fn votes(events: &[InboundEvent]) -> Vec<(u32, bool)> {
        events
            .iter()
            .filter_map(|e| match e {
                InboundEvent::PollVote { option, added, .. } => Some((*option, *added)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn votes_are_diffed_per_person() {
        let mut state = synced(false);
        state.record_poll("P1".to_owned(), Target::chat(GROUP), 300, 3);
        let first = handle(&mut state, answer(ALEX, &[0]));
        let votes = |events: &[InboundEvent]| -> Vec<(u32, bool)> {
            events
                .iter()
                .filter_map(|e| match e {
                    InboundEvent::PollVote {
                        room,
                        message,
                        option,
                        added,
                        ..
                    } => {
                        assert_eq!(
                            (room.as_str(), message.as_str()),
                            (GROUP.to_string().as_str(), "300")
                        );
                        Some((*option, *added))
                    }
                    _ => None,
                })
                .collect()
        };
        assert!(matches!(
            first.first(),
            Some(InboundEvent::MemberUpdated { .. })
        ));
        assert_eq!(votes(&first), [(0, true)]);
        assert_eq!(
            votes(&handle(&mut state, answer(ALEX, &[1, 2]))),
            [(0, false), (1, true), (2, true)]
        );
        assert_eq!(
            votes(&handle(&mut state, answer(ALEX, &[]))),
            [(1, false), (2, false)]
        );
        assert_eq!(
            votes(&handle(&mut state, answer(SAM, &[2, 7]))),
            [(2, true)]
        );
        let unknown = json!({ "poll_answer": { "poll_id": "P404", "user": user_json(SAM, "Sam"),
            "option_ids": [0], "option_persistent_ids": [] } });
        assert!(handle(&mut state, unknown).is_empty());
    }

    #[test]
    fn a_closed_poll_is_reported_once() {
        let mut state = synced(false);
        state.record_poll(
            "P1".to_owned(),
            Target {
                chat: GROUP,
                topic: Some(12),
            },
            300,
            2,
        );
        let poll = |closed: bool| {
            json!({ "poll": { "id": "P1", "question": "when?", "total_voter_count": 3,
                "is_closed": closed, "is_anonymous": false, "type": "regular",
                "options": [{ "text": "Thu", "voter_count": 2 }, { "text": "Fri", "voter_count": 1 }] } })
        };
        assert!(handle(&mut state, poll(false)).is_empty());
        assert_eq!(
            handle(&mut state, poll(true)),
            [InboundEvent::PollEnded {
                room: format!("{GROUP}:12").into(),
                message: "300".into(),
                counts: vec![2, 1]
            }]
        );
        assert!(handle(&mut state, poll(true)).is_empty());
    }

    #[test]
    fn votes_and_closes_on_a_poll_from_before_a_restart_still_count() {
        let mut before = synced(false);
        before.record_poll("P1".to_owned(), Target::chat(GROUP), 300, 2);
        handle(&mut before, answer(ALEX, &[0]));

        let mut after = synced(false);
        after.restore_polls(before.saved_polls());
        assert_eq!(
            votes(&handle(&mut after, answer(ALEX, &[1]))),
            [(0, false), (1, true)]
        );
        assert_eq!(
            handle(&mut after, closed("P1")),
            [InboundEvent::PollEnded {
                room: GROUP.to_string().into(),
                message: "300".into(),
                counts: vec![2, 1]
            }]
        );

        let mut again = synced(false);
        again.restore_polls(after.saved_polls());
        assert!(handle(&mut again, closed("P1")).is_empty());
    }

    #[test]
    fn every_change_to_a_poll_is_saved() {
        let (saver, mut changes) = crate::disk::watched();
        let mut state = synced(false);
        state.save_polls(saver);
        let mut saved = || {
            changes
                .has_changed()
                .unwrap()
                .then(|| crate::disk::decode(&changes.borrow_and_update()))
        };
        state.record_poll("P1".to_owned(), Target::chat(GROUP), 300, 2);
        assert_eq!(saved(), Some(state.saved_polls()));
        handle(&mut state, answer(ALEX, &[1]));
        let after_vote = saved().unwrap();
        assert_eq!(
            after_vote[0].votes,
            BTreeMap::from([(ALEX, BTreeSet::from([1]))])
        );
        handle(&mut state, answer(ALEX, &[1]));
        handle(&mut state, said(1, SAM, "Sam", "hi"));
        assert_eq!(saved(), None);
        handle(&mut state, closed("P1"));
        let after_close = saved().unwrap();
        assert!(after_close[0].ended && after_close[0].votes.is_empty());
    }

    #[test]
    fn polls_past_the_cap_forget_the_oldest() {
        let mut state = synced(false);
        for n in 0..=POLLS {
            let message = i32::try_from(n).unwrap() + 1;
            state.record_poll(format!("P{n}"), Target::chat(GROUP), message, 2);
        }
        assert_eq!((state.polls.len(), state.poll_order.len()), (POLLS, POLLS));
        assert!(handle(&mut state, vote("P0", ALEX, &[0])).is_empty());
        assert_eq!(
            votes(&handle(&mut state, vote("P1", ALEX, &[0]))),
            [(0, true)]
        );

        let mut older = synced(false);
        older.record_poll("P0".to_owned(), Target::chat(GROUP), 1, 2);
        let mut saved = older.saved_polls();
        saved.extend(state.saved_polls());
        let mut restored = synced(false);
        restored.restore_polls(saved);
        assert_eq!(restored.polls.len(), POLLS);
        assert_eq!(restored.poll_order.front().map(String::as_str), Some("P1"));
        assert!(handle(&mut restored, vote("P0", SAM, &[0])).is_empty());
    }

    #[test]
    fn placed_messages_past_the_cap_forget_the_oldest() {
        let mut placed = Placed::default();
        let count = i32::try_from(PLACED).unwrap();
        for message in 1..=count {
            placed.insert(GROUP, message, 7);
        }
        placed.insert(GROUP, 1, 7);
        assert_eq!(placed.order.len(), PLACED);
        placed.insert(GROUP, count + 1, 8);
        assert_eq!(placed.get(GROUP, 1), None);
        assert_eq!(placed.get(GROUP, 2), Some(7));
        assert_eq!(placed.get(GROUP, count + 1), Some(8));
        assert_eq!((placed.topics.len(), placed.order.len()), (PLACED, PLACED));
    }
}
