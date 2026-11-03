//! What the connector remembers about the workspace and the rule that turns
//! a Slack event into Ours events. It performs no I/O: what it lacks is
//! named by `missing` for the caller to fetch before calling `handle`.

use crate::emoji;
use crate::event::Event;
use crate::map::{self, Line, UserRecord};
use ours_types::{InboundEvent, Instant, Member, PlatformRole, PlatformRoomId, RoomKind};
use slack_morphism::events::{SlackMessageEvent, SlackMessageEventType};
use slack_morphism::{
    SlackBotId, SlackChannelId, SlackChannelInfo, SlackTeamId, SlackTs, SlackUser,
    SlackUserGroupId, SlackUserId,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// Threads remembered per channel. A forgotten thread is synced again when
/// it next has a message.
pub const THREADS_PER_CHANNEL: usize = 100;

/// Thread replies remembered so that reactions to them land in the thread.
const PLACED: usize = 10_000;

/// Something to fetch before an event can be handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    User(SlackUserId),
    Channel(SlackChannelId),
}

/// Ordered maps and sets are keyed by the id's string, since Slack's id
/// types have no ordering.
#[derive(Debug, Default)]
struct ChannelRecord {
    name: Option<String>,
    members: BTreeSet<String>,
    /// Threads seen, least recently active first.
    threads: VecDeque<SlackTs>,
}

impl ChannelRecord {
    /// Note a thread as active. True when it was not remembered.
    fn touch(&mut self, ts: &SlackTs) -> bool {
        if let Some(at) = self.threads.iter().position(|t| t == ts) {
            if let Some(thread) = self.threads.remove(at) {
                self.threads.push_back(thread);
            }
            return false;
        }
        self.threads.push_back(ts.clone());
        if self.threads.len() > THREADS_PER_CHANNEL {
            self.threads.pop_front();
        }
        true
    }

    fn rooms(&self, id: &SlackChannelId) -> Vec<PlatformRoomId> {
        std::iter::once(map::channel_room_id(id))
            .chain(self.threads.iter().map(|ts| map::thread_room_id(id, ts)))
            .collect()
    }
}

/// Which thread a reply is in, for events that only carry the reply's ts.
#[derive(Debug, Default)]
struct Placed {
    parents: HashMap<(SlackChannelId, SlackTs), SlackTs>,
    order: VecDeque<(SlackChannelId, SlackTs)>,
}

impl Placed {
    fn insert(&mut self, channel: &SlackChannelId, ts: &SlackTs, parent: &SlackTs) {
        let key = (channel.clone(), ts.clone());
        if self.parents.insert(key.clone(), parent.clone()).is_none() {
            self.order.push_back(key);
            if self.order.len() > PLACED
                && let Some(oldest) = self.order.pop_front()
            {
                self.parents.remove(&oldest);
            }
        }
    }

    fn get(&self, channel: &SlackChannelId, ts: &SlackTs) -> Option<&SlackTs> {
        self.parents.get(&(channel.clone(), ts.clone()))
    }
}

#[derive(Debug)]
pub struct State {
    agent_name: String,
    me: SlackUserId,
    bot_id: Option<SlackBotId>,
    team: SlackTeamId,
    leads_group: Option<SlackUserGroupId>,
    leads: HashSet<SlackUserId>,
    users: HashMap<SlackUserId, UserRecord>,
    channels: BTreeMap<String, ChannelRecord>,
    dms: BTreeMap<String, SlackUserId>,
    placed: Placed,
}

/// The parent of a reply, or `None` for a message that is not in a thread.
fn parent_of<'a>(thread_ts: Option<&'a SlackTs>, ts: &SlackTs) -> Option<&'a SlackTs> {
    thread_ts.filter(|parent| *parent != ts)
}

impl State {
    pub fn new(
        agent_name: String,
        me: SlackUserId,
        bot_id: Option<SlackBotId>,
        team: SlackTeamId,
    ) -> Self {
        State {
            agent_name,
            me,
            bot_id,
            team,
            leads_group: None,
            leads: HashSet::new(),
            users: HashMap::new(),
            channels: BTreeMap::new(),
            dms: BTreeMap::new(),
            placed: Placed::default(),
        }
    }

    pub fn me(&self) -> &SlackUserId {
        &self.me
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    fn is_me(&self, user: &SlackUserId, bot: Option<&SlackBotId>) -> bool {
        *user == self.me || (bot.is_some() && bot == self.bot_id.as_ref())
    }

    /// How a user appears in a room of `kind`. A DM carries no platform rank,
    /// but a guest stays a guest.
    fn member(&self, id: &SlackUserId, kind: RoomKind) -> Member {
        let Some(user) = self.users.get(id) else {
            return map::unknown_member(id);
        };
        let role = map::platform_role(user, &self.team, self.leads.contains(id));
        let role = if kind == RoomKind::Dm && role != PlatformRole::External {
            PlatformRole::Regular
        } else {
            role
        };
        map::member(user, role)
    }

    fn channel_members(&self, record: &ChannelRecord) -> Vec<Member> {
        record
            .members
            .iter()
            .map(|id| self.member(&SlackUserId(id.clone()), RoomKind::Channel))
            .collect()
    }

    fn channel_sync(&self, id: &SlackChannelId) -> Option<InboundEvent> {
        let record = self.channels.get(&id.0)?;
        Some(InboundEvent::RoomSync {
            room: map::channel_room(id, record.name.as_deref(), &self.team),
            members: self.channel_members(record),
        })
    }

    fn thread_sync(&self, id: &SlackChannelId, ts: &SlackTs) -> Option<InboundEvent> {
        let record = self.channels.get(&id.0)?;
        Some(InboundEvent::RoomSync {
            room: map::thread_room(id, ts, &self.team),
            members: self.channel_members(record),
        })
    }

    fn dm_sync(&self, channel: &SlackChannelId, user: &SlackUserId) -> InboundEvent {
        InboundEvent::RoomSync {
            room: map::dm_room(channel, &self.team),
            members: vec![self.member(user, RoomKind::Dm)],
        }
    }

    /// A channel and every thread of it the engine knows, each synced.
    fn channel_and_thread_syncs(&self, id: &SlackChannelId) -> Vec<InboundEvent> {
        let Some(record) = self.channels.get(&id.0) else {
            return Vec::new();
        };
        self.channel_sync(id)
            .into_iter()
            .chain(
                record
                    .threads
                    .iter()
                    .filter_map(|ts| self.thread_sync(id, ts)),
            )
            .collect()
    }

    pub fn is_dm(&self, channel: &SlackChannelId) -> bool {
        map::is_dm_id(channel) || self.dms.contains_key(&channel.0)
    }

    pub fn knows_user(&self, id: &SlackUserId) -> bool {
        self.users.contains_key(id)
    }

    /// The ids among `ids` with no record yet, once each.
    pub fn unknown_users(&self, ids: &[SlackUserId]) -> Vec<SlackUserId> {
        let mut seen = HashSet::new();
        ids.iter()
            .filter(|id| !self.users.contains_key(*id) && seen.insert((*id).clone()))
            .cloned()
            .collect()
    }

    /// Fill the user cache before any room is synced.
    pub fn load_users(&mut self, users: &[SlackUser]) {
        for user in users {
            self.users
                .insert(user.id.clone(), UserRecord::from_user(user));
        }
    }

    /// Record a user and report every room where they now look different.
    pub fn apply_user(&mut self, user: &SlackUser) -> Vec<InboundEvent> {
        let record = UserRecord::from_user(user);
        let id = record.id.clone();
        self.update_members(std::slice::from_ref(&id), |state| {
            state.users.insert(id.clone(), record);
        })
    }

    /// Set the user group whose members count as admins, and who is in it.
    pub fn set_leads(
        &mut self,
        group: SlackUserGroupId,
        users: Vec<SlackUserId>,
    ) -> Vec<InboundEvent> {
        let new: HashSet<SlackUserId> = users.into_iter().collect();
        let changed: Vec<SlackUserId> = self.leads.symmetric_difference(&new).cloned().collect();
        self.leads_group = Some(group);
        self.update_members(&changed, |state| state.leads = new)
    }

    fn change_leads(
        &mut self,
        added: &[SlackUserId],
        removed: &[SlackUserId],
    ) -> Vec<InboundEvent> {
        let ids: Vec<SlackUserId> = added.iter().chain(removed).cloned().collect();
        self.update_members(&ids, |state| {
            for id in removed {
                state.leads.remove(id);
            }
            for id in added {
                state.leads.insert(id.clone());
            }
        })
    }

    /// Apply `change`, then report `MemberUpdated` in every room where one of
    /// `ids` looks different from before.
    fn update_members(
        &mut self,
        ids: &[SlackUserId],
        change: impl FnOnce(&mut Self),
    ) -> Vec<InboundEvent> {
        let before: Vec<(Member, Member)> = ids
            .iter()
            .map(|id| {
                (
                    self.member(id, RoomKind::Channel),
                    self.member(id, RoomKind::Dm),
                )
            })
            .collect();
        change(self);
        let mut events = Vec::new();
        let mut done = HashSet::new();
        for (id, (in_channel, in_dm)) in ids.iter().zip(before) {
            if !done.insert(id.clone()) {
                continue;
            }
            let now_in_channel = self.member(id, RoomKind::Channel);
            if now_in_channel != in_channel {
                for (channel, record) in self
                    .channels
                    .iter()
                    .filter(|(_, r)| r.members.contains(&id.0))
                {
                    let channel = SlackChannelId(channel.clone());
                    events.extend(record.rooms(&channel).into_iter().map(|room| {
                        InboundEvent::MemberUpdated {
                            room,
                            member: now_in_channel.clone(),
                        }
                    }));
                }
            }
            let now_in_dm = self.member(id, RoomKind::Dm);
            if now_in_dm != in_dm {
                events.extend(self.dms.iter().filter(|(_, u)| *u == id).map(|(dm, _)| {
                    InboundEvent::MemberUpdated {
                        room: map::channel_room_id(&SlackChannelId(dm.clone())),
                        member: now_in_dm.clone(),
                    }
                }));
            }
        }
        events
    }

    /// Record a channel the agent is in, with its members. A new channel, or
    /// one whose name or members changed, is synced with its threads.
    pub fn apply_channel(
        &mut self,
        info: &SlackChannelInfo,
        members: &[SlackUserId],
    ) -> Vec<InboundEvent> {
        if info.flags.is_im == Some(true) {
            return Vec::new();
        }
        let name = if info.flags.is_mpim == Some(true) {
            None
        } else {
            info.name.clone()
        };
        let members: BTreeSet<String> = members.iter().map(|m| m.0.clone()).collect();
        match self.channels.get_mut(&info.id.0) {
            Some(record) if record.name == name && record.members == members => Vec::new(),
            Some(record) => {
                record.name = name;
                record.members = members;
                self.channel_and_thread_syncs(&info.id)
            }
            None => {
                self.channels.insert(
                    info.id.0.clone(),
                    ChannelRecord {
                        name,
                        members,
                        threads: VecDeque::new(),
                    },
                );
                self.channel_sync(&info.id).into_iter().collect()
            }
        }
    }

    /// The agent is out of a channel: leave it and every thread of it.
    pub fn drop_channel(&mut self, channel: &SlackChannelId, now: Instant) -> Vec<InboundEvent> {
        let Some(record) = self.channels.remove(&channel.0) else {
            return Vec::new();
        };
        record
            .rooms(channel)
            .into_iter()
            .map(|room| InboundEvent::MemberLeft {
                room,
                user: map::user_id(&self.me),
                at: now,
            })
            .collect()
    }

    /// Drop every channel not in `keep`, after a full listing.
    pub fn retain_channels(
        &mut self,
        keep: &HashSet<SlackChannelId>,
        now: Instant,
    ) -> Vec<InboundEvent> {
        let gone: Vec<SlackChannelId> = self
            .channels
            .keys()
            .map(|id| SlackChannelId(id.clone()))
            .filter(|id| !keep.contains(id))
            .collect();
        gone.iter()
            .flat_map(|id| self.drop_channel(id, now))
            .collect()
    }

    /// The parent of a reply the connector has seen or sent.
    pub fn thread_parent(&self, channel: &SlackChannelId, ts: &SlackTs) -> Option<SlackTs> {
        self.placed.get(channel, ts).cloned()
    }

    /// A thread the agent is posting into. Synced the first time.
    pub fn open_thread(&mut self, channel: &SlackChannelId, parent: &SlackTs) -> Vec<InboundEvent> {
        self.see_thread(channel, parent)
    }

    pub fn record_reply(&mut self, channel: &SlackChannelId, ts: &SlackTs, parent: &SlackTs) {
        self.placed.insert(channel, ts, parent);
    }

    pub fn dm_with(&self, user: &SlackUserId) -> Option<SlackChannelId> {
        self.dms
            .iter()
            .find(|(_, u)| *u == user)
            .map(|(channel, _)| SlackChannelId(channel.clone()))
    }

    /// A DM the agent opened. Synced the first time.
    pub fn open_dm(&mut self, channel: &SlackChannelId, user: &SlackUserId) -> Vec<InboundEvent> {
        self.see_dm(channel, user).into_iter().collect()
    }

    fn see_dm(&mut self, channel: &SlackChannelId, user: &SlackUserId) -> Option<InboundEvent> {
        if self.dms.contains_key(&channel.0) {
            return None;
        }
        self.dms.insert(channel.0.clone(), user.clone());
        Some(self.dm_sync(channel, user))
    }

    fn see_thread(&mut self, channel: &SlackChannelId, parent: &SlackTs) -> Vec<InboundEvent> {
        let new = self
            .channels
            .get_mut(&channel.0)
            .is_some_and(|record| record.touch(parent));
        if new {
            self.thread_sync(channel, parent).into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// What must be fetched before `event` can be handled.
    pub fn missing(&self, event: &Event) -> Vec<Want> {
        let mut wants = Vec::new();
        let want_user = |wants: &mut Vec<Want>, user: &SlackUserId| {
            if !self.knows_user(user) && *user != self.me {
                wants.push(Want::User(user.clone()));
            }
        };
        match event {
            Event::Message { event, .. } => {
                let Some(channel) = event.origin.channel.as_ref() else {
                    return wants;
                };
                let author = match event.subtype {
                    Some(SlackMessageEventType::MessageDeleted) => None,
                    Some(SlackMessageEventType::MessageChanged) => {
                        event.message.as_ref().and_then(|m| m.sender.user.as_ref())
                    }
                    _ => event.sender.user.as_ref(),
                };
                if !self.is_dm(channel) && !is_im(event) && !self.channels.contains_key(&channel.0)
                {
                    wants.push(Want::Channel(channel.clone()));
                }
                if let Some(author) = author {
                    want_user(&mut wants, author);
                }
            }
            Event::Reaction { user, channel, .. } => {
                if self.is_dm(channel) {
                    if !self.dms.contains_key(&channel.0) {
                        want_user(&mut wants, user);
                    }
                } else if !self.channels.contains_key(&channel.0) {
                    wants.push(Want::Channel(channel.clone()));
                }
            }
            Event::MemberJoined { user, channel } => {
                if *user == self.me || !self.channels.contains_key(&channel.0) {
                    wants.push(Want::Channel(channel.clone()));
                }
                want_user(&mut wants, user);
            }
            Event::Unarchived { channel } if !self.channels.contains_key(&channel.0) => {
                wants.push(Want::Channel(channel.clone()));
            }
            _ => {}
        }
        wants
    }

    pub fn handle(&mut self, event: &Event, now: Instant) -> Vec<InboundEvent> {
        match event {
            Event::Message { event, thread } => self.on_message(event, thread.as_ref(), now),
            Event::Reaction {
                added,
                user,
                channel,
                ts,
                name,
                at,
            } => self.on_reaction(*added, user, channel, ts, name, at, now),
            Event::MemberJoined { user, channel } => self.on_join(user, channel, now),
            Event::MemberLeft { user, channel } => self.on_leave(user, channel, now),
            Event::Gone { channel } => self.drop_channel(channel, now),
            Event::Renamed { channel, name } => match self.channels.get_mut(&channel.0) {
                Some(record) if record.name != *name => {
                    record.name = name.clone();
                    self.channel_sync(channel).into_iter().collect()
                }
                _ => Vec::new(),
            },
            Event::User(user) => self.apply_user(user),
            Event::GroupMembers {
                group,
                added,
                removed,
            } if self.leads_group.as_ref() == Some(group) => self.change_leads(added, removed),
            Event::GroupMembers { .. }
            | Event::Unarchived { .. }
            | Event::Uninstalled
            | Event::Ignored(_) => Vec::new(),
        }
    }

    /// Whether the room a message is in is one the engine has been told of,
    /// announcing a DM on first sight.
    fn place(
        &mut self,
        channel: &SlackChannelId,
        dm: bool,
        user: &SlackUserId,
    ) -> Option<Vec<InboundEvent>> {
        if dm {
            Some(self.see_dm(channel, user).into_iter().collect())
        } else if self.channels.contains_key(&channel.0) {
            Some(Vec::new())
        } else {
            tracing::debug!(channel = %channel.0, "message in a channel the agent is not in");
            None
        }
    }

    fn on_message(
        &mut self,
        event: &SlackMessageEvent,
        thread: Option<&SlackTs>,
        now: Instant,
    ) -> Vec<InboundEvent> {
        let Some(channel) = event.origin.channel.as_ref() else {
            return Vec::new();
        };
        let dm = self.is_dm(channel) || is_im(event);
        match event.subtype {
            Some(SlackMessageEventType::MessageChanged) => {
                return self.on_edit(channel, event, thread, dm, now);
            }
            Some(SlackMessageEventType::MessageDeleted) => {
                return self.on_delete(channel, event, thread, dm);
            }
            _ => {}
        }
        let Some(user) = event.sender.user.as_ref() else {
            return Vec::new();
        };
        if !map::is_person_message(event.subtype.as_ref(), true)
            || self.is_me(user, event.sender.bot_id.as_ref())
        {
            return Vec::new();
        }
        let Some(mut events) = self.place(channel, dm, user) else {
            return Vec::new();
        };
        let ts = &event.origin.ts;
        if !dm && let Some(parent) = parent_of(event.origin.thread_ts.as_ref(), ts) {
            events.extend(self.see_thread(channel, parent));
            self.placed.insert(channel, ts, parent);
        }
        let content = event.content.as_ref();
        let line = Line {
            channel,
            ts,
            thread_ts: event.origin.thread_ts.as_ref(),
            user,
            text: content.and_then(|c| c.text.as_deref()),
            files: content.and_then(|c| c.files.as_deref()).unwrap_or_default(),
            edited: false,
        };
        events.push(InboundEvent::Message(map::message(
            line,
            dm,
            &self.me,
            &self.agent_name,
            now,
        )));
        events
    }

    /// A real edit, not Slack updating a message for a link preview or a reply count.
    fn on_edit(
        &mut self,
        channel: &SlackChannelId,
        event: &SlackMessageEvent,
        thread: Option<&SlackTs>,
        dm: bool,
        now: Instant,
    ) -> Vec<InboundEvent> {
        let Some(edited) = event.message.as_ref() else {
            return Vec::new();
        };
        let Some(user) = edited.sender.user.as_ref() else {
            return Vec::new();
        };
        if edited.edited.is_none() || self.is_me(user, edited.sender.bot_id.as_ref()) {
            return Vec::new();
        }
        let Some(mut events) = self.place(channel, dm, user) else {
            return Vec::new();
        };
        let content = edited.content.as_ref();
        let line = Line {
            channel,
            ts: &edited.ts,
            thread_ts: thread,
            user,
            text: content.and_then(|c| c.text.as_deref()),
            files: content.and_then(|c| c.files.as_deref()).unwrap_or_default(),
            edited: true,
        };
        events.push(InboundEvent::MessageEdited(map::message(
            line,
            dm,
            &self.me,
            &self.agent_name,
            now,
        )));
        events
    }

    fn on_delete(
        &self,
        channel: &SlackChannelId,
        event: &SlackMessageEvent,
        thread: Option<&SlackTs>,
        dm: bool,
    ) -> Vec<InboundEvent> {
        let Some(ts) = event.deleted_ts.as_ref() else {
            return Vec::new();
        };
        let known = if dm {
            self.dms.contains_key(&channel.0)
        } else {
            self.channels.contains_key(&channel.0)
        };
        if !known {
            return Vec::new();
        }
        let (room, _) = map::placement(channel, thread, ts, dm);
        vec![InboundEvent::MessageDeleted {
            room,
            id: map::message_id(ts),
        }]
    }

    #[allow(clippy::too_many_arguments)]
    fn on_reaction(
        &mut self,
        added: bool,
        user: &SlackUserId,
        channel: &SlackChannelId,
        ts: &SlackTs,
        name: &str,
        at: &SlackTs,
        now: Instant,
    ) -> Vec<InboundEvent> {
        if *user == self.me {
            return Vec::new();
        }
        let dm = self.is_dm(channel);
        let Some(mut events) = self.place(channel, dm, user) else {
            return Vec::new();
        };
        let room = match self.placed.get(channel, ts) {
            Some(parent) if !dm => map::thread_room_id(channel, parent),
            _ => map::channel_room_id(channel),
        };
        let (message, user, emoji) = (
            map::message_id(ts),
            map::user_id(user),
            emoji::from_slack(name),
        );
        let at = map::instant(at).unwrap_or(now);
        events.push(if added {
            InboundEvent::ReactionAdded {
                room,
                message,
                user,
                emoji,
                at,
            }
        } else {
            InboundEvent::ReactionRemoved {
                room,
                message,
                user,
                emoji,
                at,
            }
        });
        events
    }

    fn on_join(
        &mut self,
        user: &SlackUserId,
        channel: &SlackChannelId,
        now: Instant,
    ) -> Vec<InboundEvent> {
        if *user == self.me {
            return Vec::new();
        }
        let added = self
            .channels
            .get_mut(&channel.0)
            .is_some_and(|record| record.members.insert(user.0.clone()));
        if !added {
            return Vec::new();
        }
        let member = self.member(user, RoomKind::Channel);
        self.channels
            .get(&channel.0)
            .map(|record| record.rooms(channel))
            .unwrap_or_default()
            .into_iter()
            .map(|room| InboundEvent::MemberJoined {
                room,
                member: member.clone(),
                at: now,
            })
            .collect()
    }

    fn on_leave(
        &mut self,
        user: &SlackUserId,
        channel: &SlackChannelId,
        now: Instant,
    ) -> Vec<InboundEvent> {
        if *user == self.me {
            return self.drop_channel(channel, now);
        }
        let Some(record) = self.channels.get_mut(&channel.0) else {
            return Vec::new();
        };
        if !record.members.remove(&user.0) {
            return Vec::new();
        }
        record
            .rooms(channel)
            .into_iter()
            .map(|room| InboundEvent::MemberLeft {
                room,
                user: map::user_id(user),
                at: now,
            })
            .collect()
    }
}

/// Whether Slack marked the message as being in a DM.
fn is_im(event: &SlackMessageEvent) -> bool {
    event
        .origin
        .channel_type
        .as_ref()
        .is_some_and(|t| t.0 == "im")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use ours_types::{Message, PlatformRole as R};
    use serde_json::json;

    const ME: &str = "UBOT";

    fn at() -> Instant {
        Instant::from_millis(1_793_613_600_000).unwrap()
    }

    fn users() -> Vec<SlackUser> {
        [
            json!({ "id": ME, "team_id": "T1", "name": "q", "is_bot": true }),
            json!({ "id": "UPRIYA", "team_id": "T1", "name": "priya", "is_admin": true, "tz": "Europe/London" }),
            json!({ "id": "USAM", "team_id": "T1", "name": "sam" }),
            json!({ "id": "UJO", "team_id": "T1", "name": "jo", "is_owner": true }),
            json!({ "id": "UALEX", "team_id": "T1", "name": "alex" }),
            json!({ "id": "UCLIENT", "team_id": "T9", "name": "client" }),
            json!({ "id": "UGUEST", "team_id": "T1", "name": "guest", "is_restricted": true }),
        ]
        .into_iter()
        .map(slack_user)
        .collect()
    }

    fn started() -> State {
        let mut state = State::new(
            "Q".to_owned(),
            SlackUserId(ME.to_owned()),
            Some(slack_morphism::SlackBotId("BQ".to_owned())),
            SlackTeamId("T1".to_owned()),
        );
        state.load_users(&users());
        state
    }

    fn ids(list: &[&str]) -> Vec<SlackUserId> {
        list.iter()
            .map(|id| SlackUserId((*id).to_owned()))
            .collect()
    }

    /// Started, with #launch holding everyone and USAM in the leads group.
    fn synced() -> State {
        let mut state = started();
        state.set_leads(SlackUserGroupId("S1".to_owned()), ids(&["USAM"]));
        state.apply_channel(
            &channel_info("C1", "launch"),
            &ids(&[ME, "UPRIYA", "USAM", "UJO", "UALEX", "UCLIENT"]),
        );
        state
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

    fn handle(state: &mut State, value: serde_json::Value) -> Vec<InboundEvent> {
        state.handle(&event(value), at())
    }

    #[test]
    fn a_channel_syncs_with_the_roles_slack_reports() {
        let mut state = started();
        state.set_leads(SlackUserGroupId("S1".to_owned()), ids(&["USAM"]));
        let events = state.apply_channel(
            &channel_info("C1", "launch"),
            &ids(&[ME, "UPRIYA", "USAM", "UJO", "UALEX", "UCLIENT", "UGUEST"]),
        );
        let [InboundEvent::RoomSync { room, members }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(room.id.as_str(), "C1");
        assert_eq!(room.name.as_deref(), Some("launch"));
        assert_eq!(room.container.as_deref(), Some("T1"));
        assert_eq!(
            roles(members),
            [
                ("UALEX", R::Regular),
                ("UBOT", R::Regular),
                ("UCLIENT", R::External),
                ("UGUEST", R::External),
                ("UJO", R::Owner),
                ("UPRIYA", R::Admin),
                ("USAM", R::Admin),
            ]
        );
        let priya = members.iter().find(|m| m.id.as_str() == "UPRIYA").unwrap();
        assert_eq!(priya.timezone.as_deref(), Some("Europe/London"));
        assert!(members.iter().find(|m| m.id.as_str() == ME).unwrap().is_bot);
    }

    #[test]
    fn a_channel_seen_again_unchanged_is_silent() {
        let mut state = synced();
        let again = state.apply_channel(
            &channel_info("C1", "launch"),
            &ids(&[ME, "UPRIYA", "USAM", "UJO", "UALEX", "UCLIENT"]),
        );
        assert!(again.is_empty());
        let changed = state.apply_channel(&channel_info("C1", "launch"), &ids(&[ME, "UPRIYA"]));
        assert!(
            matches!(changed.as_slice(), [InboundEvent::RoomSync { members, .. }] if members.len() == 2)
        );
    }

    #[test]
    fn a_group_dm_is_a_channel_without_a_name() {
        let mut state = started();
        let mut info = channel_info("G5", "mpdm-priya--sam--q-1");
        info.flags.is_mpim = Some(true);
        let events = state.apply_channel(&info, &ids(&[ME, "UPRIYA", "USAM"]));
        let [InboundEvent::RoomSync { room, .. }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!((room.kind, room.name.as_deref()), (RoomKind::Channel, None));
    }

    #[test]
    fn a_users_unknown_until_looked_up_is_external_then_updated() {
        let mut state = synced();
        let events = state.apply_channel(&channel_info("C2", "ops"), &ids(&["UPRIYA", "UNEW"]));
        let [InboundEvent::RoomSync { members, .. }] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(
            roles(members),
            [("UNEW", R::External), ("UPRIYA", R::Admin)]
        );
        assert_eq!(
            state.unknown_users(&ids(&["UNEW", "UNEW", "USAM"])),
            ids(&["UNEW"])
        );

        let updates = state.apply_user(&slack_user(
            json!({ "id": "UNEW", "team_id": "T1", "name": "new" }),
        ));
        let [InboundEvent::MemberUpdated { room, member }] = updates.as_slice() else {
            panic!("{updates:?}")
        };
        assert_eq!((room.as_str(), member.platform_role), ("C2", R::Regular));
    }

    #[test]
    fn a_first_thread_reply_syncs_the_thread_with_the_channels_members() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "channel_type": "channel", "user": "UALEX",
                "text": "Beacon were cheaper", "ts": "101.000000", "thread_ts": "100.000000" }),
        );
        let [
            InboundEvent::RoomSync { room, members },
            InboundEvent::Message(message),
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!(room.id.as_str(), "C1:100.000000");
        assert_eq!(room.kind, RoomKind::Thread);
        assert_eq!(room.parent.as_ref().map(|p| p.as_str()), Some("C1"));
        assert_eq!(members.len(), 6);
        assert_eq!(message.room.as_str(), "C1:100.000000");
        assert_eq!(
            message.reply_to.as_ref().map(|m| m.as_str()),
            Some("100.000000")
        );

        let next = handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "user": "USAM",
                "text": "Acme, the date matters more", "ts": "102.000000", "thread_ts": "100.000000" }),
        );
        assert_eq!(only_message(&next).room.as_str(), "C1:100.000000");
    }

    #[test]
    fn a_threads_opening_message_stays_in_the_channel() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "user": "UJO",
                "text": "are we going with Acme or Beacon?", "ts": "100.000000", "thread_ts": "100.000000" }),
        );
        let message = only_message(&events);
        assert_eq!(
            (message.room.as_str(), message.reply_to.as_ref()),
            ("C1", None)
        );
    }

    #[test]
    fn forgotten_threads_sync_again_when_they_come_back() {
        let mut state = synced();
        for n in 0..=THREADS_PER_CHANNEL {
            let thread = format!("{}.000000", 1000 + n);
            let events = handle(
                &mut state,
                json!({ "type": "message", "channel": "C1", "user": "UALEX", "text": "x",
                    "ts": format!("{}.000001", 1000 + n), "thread_ts": thread }),
            );
            assert_eq!(events.len(), 2, "thread {n}");
        }
        let back = handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "user": "UALEX", "text": "x",
                "ts": "5000.000000", "thread_ts": "1000.000000" }),
        );
        assert!(matches!(
            back.as_slice(),
            [InboundEvent::RoomSync { .. }, InboundEvent::Message(_)]
        ));
    }

    #[test]
    fn a_first_dm_announces_the_room_then_the_message() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "message", "channel": "D1", "channel_type": "im", "user": "UPRIYA",
                "text": "nudge me, not them", "ts": "200.000000" }),
        );
        let [
            InboundEvent::RoomSync { room, members },
            InboundEvent::Message(message),
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!(room.kind, RoomKind::Dm);
        assert_eq!(room.id.as_str(), "D1");
        assert_eq!(roles(members), [("UPRIYA", R::Regular)]);
        assert_eq!(message.room.as_str(), "D1");

        let again = handle(
            &mut state,
            json!({ "type": "message", "channel": "D1", "channel_type": "im", "user": "UPRIYA",
                "text": "thanks", "ts": "201.000000", "thread_ts": "200.000000" }),
        );
        let message = only_message(&again);
        assert_eq!(message.room.as_str(), "D1");
        assert_eq!(
            message.reply_to.as_ref().map(|m| m.as_str()),
            Some("200.000000")
        );
    }

    #[test]
    fn a_guest_in_a_dm_is_still_a_guest() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "message", "channel": "D2", "channel_type": "im", "user": "UCLIENT",
                "text": "can you move the review", "ts": "200.000000" }),
        );
        let InboundEvent::RoomSync { members, .. } = &events[0] else {
            panic!("{events:?}")
        };
        assert_eq!(roles(members), [("UCLIENT", R::External)]);
    }

    #[test]
    fn the_agents_own_posts_and_hooks_are_ignored() {
        let mut state = synced();
        for own in [
            json!({ "type": "message", "channel": "C1", "user": ME, "text": "📌 Open", "ts": "1.000000" }),
            json!({ "type": "message", "channel": "C1", "user": "UOTHER", "bot_id": "BQ", "text": "x", "ts": "2.000000" }),
            json!({ "type": "message", "subtype": "bot_message", "channel": "C1", "bot_id": "B9",
                "text": "deploy finished", "ts": "3.000000" }),
            json!({ "type": "message", "subtype": "channel_join", "channel": "C1", "user": "UALEX",
                "text": "<@UALEX> has joined", "ts": "4.000000" }),
        ] {
            assert!(handle(&mut state, own.clone()).is_empty(), "{own}");
        }
        let app = handle(
            &mut state,
            json!({ "type": "message", "subtype": "bot_message", "channel": "C1", "bot_id": "B9",
                "user": "UALEX", "text": "posted for Alex", "ts": "5.000000" }),
        );
        assert_eq!(only_message(&app).author.as_str(), "UALEX");
    }

    #[test]
    fn a_message_in_an_unknown_channel_asks_for_it_first() {
        let mut state = synced();
        let value = json!({ "type": "message", "channel": "C7", "user": "UNEW", "text": "hi", "ts": "1.000000" });
        assert_eq!(
            state.missing(&event(value.clone())),
            [
                Want::Channel(SlackChannelId("C7".to_owned())),
                Want::User(SlackUserId("UNEW".to_owned()))
            ]
        );
        assert!(handle(&mut state, value).is_empty());
        let known = json!({ "type": "message", "channel": "C1", "user": "USAM", "text": "hi", "ts": "1.000000" });
        assert!(state.missing(&event(known)).is_empty());
    }

    #[test]
    fn edits_land_in_the_thread_and_previews_are_not_edits() {
        let mut state = synced();
        let edit = json!({
            "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "300.000000", "hidden": true,
            "message": { "type": "message", "user": "UALEX", "text": "Acme hit the date", "ts": "101.000000",
                "thread_ts": "100.000000", "edited": { "user": "UALEX", "ts": "300.000000" } },
            "previous_message": { "type": "message", "user": "UALEX", "text": "Acme", "ts": "101.000000",
                "thread_ts": "100.000000" }
        });
        let events = handle(&mut state, edit);
        let [InboundEvent::MessageEdited(message)] = events.as_slice() else {
            panic!("{events:?}")
        };
        assert_eq!(message.room.as_str(), "C1:100.000000");
        assert_eq!(message.id.as_str(), "101.000000");
        assert_eq!(message.text, "Acme hit the date");
        assert!(message.edited);

        let preview = json!({
            "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "301.000000",
            "message": { "type": "message", "user": "UALEX", "text": "see https://x", "ts": "102.000000" }
        });
        assert!(handle(&mut state, preview).is_empty());

        let own = json!({
            "type": "message", "subtype": "message_changed", "channel": "C1", "ts": "302.000000",
            "message": { "type": "message", "user": ME, "text": "fixed", "ts": "103.000000",
                "edited": { "user": ME, "ts": "302.000000" } }
        });
        assert!(handle(&mut state, own).is_empty());
    }

    #[test]
    fn deletes_name_the_room_the_message_was_in() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "message", "subtype": "message_deleted", "channel": "C1", "ts": "400.000000",
                "hidden": true, "deleted_ts": "101.000000",
                "previous_message": { "type": "message", "user": "UALEX", "text": "x", "ts": "101.000000",
                    "thread_ts": "100.000000" } }),
        );
        assert_eq!(
            events,
            [InboundEvent::MessageDeleted {
                room: "C1:100.000000".into(),
                id: "101.000000".into()
            }]
        );
        let top = handle(
            &mut state,
            json!({ "type": "message", "subtype": "message_deleted", "channel": "C1", "ts": "401.000000",
                "deleted_ts": "99.000000" }),
        );
        assert!(
            matches!(&top[..], [InboundEvent::MessageDeleted { room, .. }] if room.as_str() == "C1")
        );
    }

    fn reaction(user: &str, channel: &str, ts: &str, name: &str, added: bool) -> serde_json::Value {
        json!({
            "type": if added { "reaction_added" } else { "reaction_removed" },
            "user": user, "reaction": name,
            "item": { "type": "message", "channel": channel, "ts": ts },
            "event_ts": "500.000000"
        })
    }

    #[test]
    fn reactions_on_thread_replies_land_in_the_thread() {
        let mut state = synced();
        handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "user": "UALEX", "text": "I'll have the draft",
                "ts": "101.000000", "thread_ts": "100.000000" }),
        );
        let events = handle(
            &mut state,
            reaction("USAM", "C1", "101.000000", "white_check_mark", true),
        );
        let [
            InboundEvent::ReactionAdded {
                room,
                message,
                user,
                emoji,
                at,
            },
        ] = events.as_slice()
        else {
            panic!("{events:?}")
        };
        assert_eq!(room.as_str(), "C1:100.000000");
        assert_eq!(
            (message.as_str(), user.as_str(), emoji.as_str()),
            ("101.000000", "USAM", "✅")
        );
        assert_eq!(at.as_millis(), 500_000);

        let top = handle(
            &mut state,
            reaction("USAM", "C1", "100.000000", "+1::skin-tone-2", false),
        );
        let [InboundEvent::ReactionRemoved { room, emoji, .. }] = top.as_slice() else {
            panic!("{top:?}")
        };
        assert_eq!((room.as_str(), emoji.as_str()), ("C1", "👍"));
        assert!(handle(&mut state, reaction(ME, "C1", "100.000000", "eyes", true)).is_empty());
    }

    #[test]
    fn a_check_on_an_old_dm_announces_the_dm_first() {
        let mut state = synced();
        let events = handle(
            &mut state,
            reaction("UPRIYA", "D3", "600.000000", "white_check_mark", true),
        );
        assert!(matches!(
            events.as_slice(),
            [InboundEvent::RoomSync { room, .. }, InboundEvent::ReactionAdded { emoji, .. }]
                if room.kind == RoomKind::Dm && emoji == "✅"
        ));
    }

    #[test]
    fn joins_and_leaves_reach_the_channel_and_its_threads() {
        let mut state = synced();
        handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "user": "UALEX", "text": "x",
                "ts": "101.000000", "thread_ts": "100.000000" }),
        );
        let joined = handle(
            &mut state,
            json!({ "type": "member_joined_channel", "user": "UGUEST", "channel": "C1", "channel_type": "C", "team": "T1" }),
        );
        let rooms: Vec<_> = joined
            .iter()
            .map(|e| match e {
                InboundEvent::MemberJoined { room, member, .. } => {
                    (room.as_str(), member.platform_role)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(rooms, [("C1", R::External), ("C1:100.000000", R::External)]);
        let twice = handle(
            &mut state,
            json!({ "type": "member_joined_channel", "user": "UGUEST", "channel": "C1", "channel_type": "C", "team": "T1" }),
        );
        assert!(twice.is_empty());

        let left = handle(
            &mut state,
            json!({ "type": "member_left_channel", "user": "UGUEST", "channel": "C1", "channel_type": "C", "team": "T1" }),
        );
        assert_eq!(left.len(), 2);
        assert!(left.iter().all(
            |e| matches!(e, InboundEvent::MemberLeft { user, .. } if user.as_str() == "UGUEST")
        ));
    }

    #[test]
    fn the_agent_joining_a_channel_asks_for_it() {
        let state = synced();
        let joined = event(
            json!({ "type": "member_joined_channel", "user": ME, "channel": "C8",
            "channel_type": "C", "team": "T1" }),
        );
        assert_eq!(
            state.missing(&joined),
            [Want::Channel(SlackChannelId("C8".to_owned()))]
        );
        let stranger = event(
            json!({ "type": "member_joined_channel", "user": "UNEW", "channel": "C1",
            "channel_type": "C", "team": "T1" }),
        );
        assert_eq!(
            state.missing(&stranger),
            [Want::User(SlackUserId("UNEW".to_owned()))]
        );
    }

    #[test]
    fn losing_a_channel_takes_the_agent_out_of_it_and_its_threads() {
        for gone in [
            json!({ "type": "channel_archive", "channel": "C1", "user": "UJO" }),
            json!({ "type": "channel_deleted", "channel": "C1" }),
            json!({ "type": "channel_left", "channel": "C1" }),
            json!({ "type": "member_left_channel", "user": ME, "channel": "C1", "channel_type": "C", "team": "T1" }),
        ] {
            let mut state = synced();
            handle(
                &mut state,
                json!({ "type": "message", "channel": "C1", "user": "UALEX", "text": "x",
                    "ts": "101.000000", "thread_ts": "100.000000" }),
            );
            let events = handle(&mut state, gone.clone());
            let left: Vec<_> = events
                .iter()
                .map(|e| match e {
                    InboundEvent::MemberLeft { room, user, .. } => (room.as_str(), user.as_str()),
                    other => panic!("{other:?}"),
                })
                .collect();
            assert_eq!(left, [("C1", ME), ("C1:100.000000", ME)], "{gone}");
            assert!(handle(&mut state, gone).is_empty());
        }
    }

    #[test]
    fn a_channel_missing_from_a_fresh_listing_is_left() {
        let mut state = synced();
        state.apply_channel(&channel_info("C2", "ops"), &ids(&["UPRIYA"]));
        let keep: HashSet<_> = [SlackChannelId("C1".to_owned())].into_iter().collect();
        let events = state.retain_channels(&keep, at());
        assert!(
            matches!(events.as_slice(), [InboundEvent::MemberLeft { room, .. }] if room.as_str() == "C2")
        );
    }

    #[test]
    fn a_rename_resyncs_the_channel_with_its_new_name() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "channel_rename", "channel": { "id": "C1", "name": "launch-v2", "created": 1 } }),
        );
        assert!(matches!(
            events.as_slice(),
            [InboundEvent::RoomSync { room, .. }] if room.name.as_deref() == Some("launch-v2")
        ));
        let unknown = handle(
            &mut state,
            json!({ "type": "channel_rename", "channel": { "id": "C9", "name": "x", "created": 1 } }),
        );
        assert!(unknown.is_empty());
    }

    #[test]
    fn a_promotion_updates_the_member_everywhere_but_their_dm() {
        let mut state = synced();
        handle(
            &mut state,
            json!({ "type": "message", "channel": "D1", "channel_type": "im", "user": "UALEX", "text": "hi", "ts": "1.000000" }),
        );
        handle(
            &mut state,
            json!({ "type": "message", "channel": "C1", "user": "UALEX", "text": "x", "ts": "101.000000", "thread_ts": "100.000000" }),
        );
        let events = handle(
            &mut state,
            json!({ "type": "user_change", "event_ts": "1.0", "cache_ts": 1,
                "user": { "id": "UALEX", "team_id": "T1", "name": "alex", "is_admin": true } }),
        );
        let updated: Vec<_> = events
            .iter()
            .map(|e| match e {
                InboundEvent::MemberUpdated { room, member } => {
                    (room.as_str(), member.platform_role)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(updated, [("C1", R::Admin), ("C1:100.000000", R::Admin)]);

        let same = handle(
            &mut state,
            json!({ "type": "user_change", "event_ts": "2.0", "cache_ts": 2,
                "user": { "id": "UALEX", "team_id": "T1", "name": "alex", "is_admin": true } }),
        );
        assert!(same.is_empty());

        let renamed = handle(
            &mut state,
            json!({ "type": "user_change", "event_ts": "3.0", "cache_ts": 3,
                "user": { "id": "UALEX", "team_id": "T1", "name": "alex", "is_admin": true,
                    "profile": { "display_name": "Alex K" } } }),
        );
        assert_eq!(renamed.len(), 3, "{renamed:?}");
    }

    #[test]
    fn the_leads_group_changing_moves_people_in_and_out_of_admin() {
        let mut state = synced();
        let events = handle(
            &mut state,
            json!({ "type": "subteam_members_changed", "subteam_id": "S1", "team_id": "T1",
                "added_users": ["UALEX"], "removed_users": ["USAM"] }),
        );
        let mut updated: Vec<_> = events
            .iter()
            .map(|e| match e {
                InboundEvent::MemberUpdated { member, .. } => {
                    (member.id.as_str(), member.platform_role)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        updated.sort_by_key(|(id, _)| *id);
        assert_eq!(updated, [("UALEX", R::Admin), ("USAM", R::Regular)]);

        let other_group = handle(
            &mut state,
            json!({ "type": "subteam_members_changed", "subteam_id": "S2", "team_id": "T1",
                "added_users": ["UJO"], "removed_users": [] }),
        );
        assert!(other_group.is_empty());
    }

    #[test]
    fn a_lead_group_member_from_another_workspace_stays_external() {
        let mut state = synced();
        let events = state.set_leads(SlackUserGroupId("S1".to_owned()), ids(&["USAM", "UCLIENT"]));
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn posting_into_a_thread_syncs_it_once_and_places_the_reply() {
        let mut state = synced();
        let channel = SlackChannelId("C1".to_owned());
        let parent = SlackTs("100.000000".to_owned());
        let first = state.open_thread(&channel, &parent);
        assert!(
            matches!(first.as_slice(), [InboundEvent::RoomSync { room, .. }] if room.id.as_str() == "C1:100.000000")
        );
        assert!(state.open_thread(&channel, &parent).is_empty());

        let reply = SlackTs("105.000000".to_owned());
        state.record_reply(&channel, &reply, &parent);
        assert_eq!(state.thread_parent(&channel, &reply), Some(parent));
        let events = handle(
            &mut state,
            reaction("UJO", "C1", "105.000000", "tada", true),
        );
        assert!(
            matches!(events.as_slice(), [InboundEvent::ReactionAdded { room, .. }] if room.as_str() == "C1:100.000000")
        );
    }

    #[test]
    fn a_dm_the_agent_opens_is_synced_once() {
        let mut state = synced();
        let dm = SlackChannelId("D9".to_owned());
        let user = SlackUserId("UPRIYA".to_owned());
        assert_eq!(state.dm_with(&user), None);
        assert_eq!(state.open_dm(&dm, &user).len(), 1);
        assert!(state.open_dm(&dm, &user).is_empty());
        assert_eq!(state.dm_with(&user), Some(dm.clone()));
        assert!(state.is_dm(&dm));
    }
}
