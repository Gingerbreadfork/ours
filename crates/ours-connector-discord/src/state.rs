//! What the connector remembers about each guild and the rule that turns a
//! gateway event into Ours events. It performs no I/O: anything that needs
//! the network comes back as a `Step`.

use crate::{map, poll};
use ours_types::{InboundEvent, Instant, Member, Platform, PlatformRole, Room};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use twilight_gateway::Event;
use twilight_model::channel::permission_overwrite::PermissionOverwrite;
use twilight_model::channel::{Channel, ChannelType, Message as TwMessage};
use twilight_model::gateway::GatewayReaction;
use twilight_model::gateway::payload::incoming::{
    GuildDelete, MemberChunk, MemberUpdate, MessageDelete, Ready, ThreadMembersUpdate,
};
use twilight_model::guild::{Guild, Member as TwMember, Permissions, Role};
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ChannelMarker, GuildMarker, MessageMarker, RoleMarker, UserMarker,
};
use twilight_model::user::User;

/// Something for the I/O layer to do after the cache has looked at an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Emit(InboundEvent),
    /// Ask the gateway for every member of the guild.
    RequestMembers(Id<GuildMarker>),
    /// Read the member list of a private thread over REST.
    FetchThreadMembers(Id<ChannelMarker>),
}

/// DM channels that have already been announced to the engine.
#[derive(Debug, Clone, Default)]
pub struct DmRooms(Arc<Mutex<HashSet<Id<ChannelMarker>>>>);

impl DmRooms {
    /// Record a DM channel. True when it was not known before.
    pub fn insert(&self, channel: Id<ChannelMarker>) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(channel)
    }
}

#[derive(Debug, Clone)]
struct MemberRecord {
    id: Id<UserMarker>,
    display_name: String,
    is_bot: bool,
    roles: Vec<Id<RoleMarker>>,
}

impl MemberRecord {
    fn from_member(member: &TwMember) -> Self {
        MemberRecord {
            id: member.user.id,
            display_name: map::display_name(member.nick.as_deref(), &member.user),
            is_bot: member.user.bot,
            roles: member.roles.clone(),
        }
    }

    fn from_update(update: &MemberUpdate) -> Self {
        MemberRecord {
            id: update.user.id,
            display_name: map::display_name(update.nick.as_deref(), &update.user),
            is_bot: update.user.bot,
            roles: update.roles.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelRecord {
    name: Option<String>,
    kind: ChannelType,
    overwrites: Vec<PermissionOverwrite>,
}

#[derive(Debug, Clone)]
struct ThreadRecord {
    name: Option<String>,
    parent: Id<ChannelMarker>,
    private: bool,
    /// Known only for private threads, once read.
    members: Option<HashSet<Id<UserMarker>>>,
}

#[derive(Debug)]
struct GuildState {
    name: String,
    owner: Id<UserMarker>,
    roles: HashMap<Id<RoleMarker>, Permissions>,
    /// The managed role Discord gives the bot, which people mention by mistake.
    bot_role: Option<Id<RoleMarker>>,
    channels: BTreeMap<Id<ChannelMarker>, ChannelRecord>,
    threads: BTreeMap<Id<ChannelMarker>, ThreadRecord>,
    members: HashMap<Id<UserMarker>, MemberRecord>,
    awaiting_chunks: bool,
}

/// How a member relates to one room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    In,
    Out,
}

impl GuildState {
    fn base(&self, guild: Id<GuildMarker>, record: &MemberRecord) -> Permissions {
        map::base_permissions(guild, self.owner, record.id, &record.roles, &self.roles)
    }

    fn member(&self, guild: Id<GuildMarker>, record: &MemberRecord) -> Member {
        let role = map::platform_role(record.id == self.owner, self.base(guild, record));
        Member {
            id: map::user_id(record.id),
            display_name: record.display_name.clone(),
            platform_role: role,
            is_bot: record.is_bot,
            timezone: None,
        }
    }

    /// Whether the member can see a channel or thread.
    fn presence(
        &self,
        guild: Id<GuildMarker>,
        record: &MemberRecord,
        room: Id<ChannelMarker>,
    ) -> Presence {
        let visible = if let Some(thread) = self.threads.get(&room) {
            if thread.private {
                thread
                    .members
                    .as_ref()
                    .is_some_and(|m| m.contains(&record.id))
            } else {
                self.presence(guild, record, thread.parent) == Presence::In
            }
        } else if let Some(channel) = self.channels.get(&room) {
            map::channel_permissions(
                guild,
                record.id,
                &record.roles,
                self.base(guild, record),
                &channel.overwrites,
            )
            .contains(Permissions::VIEW_CHANNEL)
        } else {
            false
        };
        if visible { Presence::In } else { Presence::Out }
    }

    /// The agent's own record, or an empty one when the member list lacks it.
    fn me(&self, me: Option<Id<UserMarker>>) -> Option<MemberRecord> {
        let me = me?;
        Some(self.members.get(&me).cloned().unwrap_or(MemberRecord {
            id: me,
            display_name: String::new(),
            is_bot: true,
            roles: Vec::new(),
        }))
    }

    fn room(&self, guild: Id<GuildMarker>, room: Id<ChannelMarker>) -> Option<Room> {
        if let Some(thread) = self.threads.get(&room) {
            return Some(map::thread_room(
                room,
                thread.name.as_deref(),
                thread.parent,
                guild,
            ));
        }
        let channel = self.channels.get(&room)?;
        map::is_text_channel(channel.kind)
            .then(|| map::channel_room(room, channel.name.as_deref(), guild))
    }

    fn room_sync(&self, guild: Id<GuildMarker>, room: Id<ChannelMarker>) -> Option<InboundEvent> {
        let descriptor = self.room(guild, room)?;
        let mut records: Vec<&MemberRecord> = self
            .members
            .values()
            .filter(|r| self.presence(guild, r, room) == Presence::In)
            .collect();
        records.sort_by_key(|r| r.id);
        let members = records.into_iter().map(|r| self.member(guild, r)).collect();
        Some(InboundEvent::RoomSync {
            room: descriptor,
            members,
        })
    }

    /// Every room id the guild holds that is a room to the engine.
    fn room_ids(&self) -> Vec<Id<ChannelMarker>> {
        let channels = self
            .channels
            .iter()
            .filter(|(_, c)| map::is_text_channel(c.kind))
            .map(|(id, _)| *id);
        let threads = self
            .threads
            .iter()
            .filter(|(_, t)| self.channels.contains_key(&t.parent))
            .map(|(id, _)| *id);
        channels.chain(threads).collect()
    }

    fn threads_of(&self, channel: Id<ChannelMarker>) -> Vec<Id<ChannelMarker>> {
        self.threads
            .iter()
            .filter(|(_, t)| t.parent == channel)
            .map(|(id, _)| *id)
            .collect()
    }
}

/// Polls already reported as ended, so a later update to the same message
/// does not report it again. The oldest are forgotten past `LIMIT`.
#[derive(Debug, Default)]
struct EndedPolls {
    seen: HashSet<Id<MessageMarker>>,
    order: VecDeque<Id<MessageMarker>>,
}

impl EndedPolls {
    const LIMIT: usize = 1024;

    /// Record a poll. True when it was not known before.
    fn insert(&mut self, message: Id<MessageMarker>) -> bool {
        if !self.seen.insert(message) {
            return false;
        }
        self.order.push_back(message);
        if self.order.len() > Self::LIMIT
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }
        true
    }
}

#[derive(Debug)]
pub struct Cache {
    agent_name: String,
    allowed: Option<HashSet<Id<GuildMarker>>>,
    me: Option<Id<UserMarker>>,
    guilds: HashMap<Id<GuildMarker>, GuildState>,
    dms: DmRooms,
    ended_polls: EndedPolls,
}

impl Cache {
    pub fn new(
        agent_name: String,
        allowed: Option<HashSet<Id<GuildMarker>>>,
        dms: DmRooms,
    ) -> Self {
        Cache {
            agent_name,
            allowed,
            me: None,
            guilds: HashMap::new(),
            dms,
            ended_polls: EndedPolls::default(),
        }
    }

    pub fn me(&self) -> Option<Id<UserMarker>> {
        self.me
    }

    pub fn handle(&mut self, event: &Event, now: Instant) -> Vec<Step> {
        match event {
            Event::Ready(ready) => self.on_ready(ready),
            Event::GuildCreate(create) => match &**create {
                twilight_model::gateway::payload::incoming::GuildCreate::Available(guild) => {
                    self.on_guild_create(guild)
                }
                twilight_model::gateway::payload::incoming::GuildCreate::Unavailable(_) => {
                    Vec::new()
                }
            },
            Event::GuildDelete(delete) => self.on_guild_delete(delete, now),
            Event::GuildUpdate(update) => {
                let roles = update.roles.clone();
                self.on_guild_change(update.id, |g| {
                    g.name = update.name.clone();
                    g.owner = update.owner_id;
                    g.roles = role_table(&roles);
                })
            }
            Event::RoleCreate(e) => self.on_role(e.guild_id, |roles| {
                roles.insert(e.role.id, e.role.permissions);
            }),
            Event::RoleUpdate(e) => self.on_role(e.guild_id, |roles| {
                roles.insert(e.role.id, e.role.permissions);
            }),
            Event::RoleDelete(e) => self.on_role(e.guild_id, |roles| {
                roles.remove(&e.role_id);
            }),
            Event::ChannelCreate(e) => self.on_channel(&e.0),
            Event::ChannelUpdate(e) => self.on_channel(&e.0),
            Event::ChannelDelete(e) => self.on_channel_delete(&e.0, now),
            Event::ThreadCreate(e) => self.on_thread(e.0.guild_id, &e.0),
            Event::ThreadUpdate(e) => self.on_thread(e.0.guild_id, &e.0),
            Event::ThreadDelete(e) => self.on_thread_delete(e.guild_id, e.id, now),
            Event::ThreadListSync(e) => {
                let mut steps = Vec::new();
                for thread in &e.threads {
                    steps.extend(self.on_thread(Some(e.guild_id), thread));
                }
                steps
            }
            Event::ThreadMembersUpdate(e) => self.on_thread_members_update(e),
            Event::MemberChunk(chunk) => self.on_member_chunk(chunk),
            Event::MemberAdd(add) => self.on_member_add(add.guild_id, &add.member, now),
            Event::MemberRemove(remove) => {
                self.on_member_remove(remove.guild_id, remove.user.id, now)
            }
            Event::MemberUpdate(update) => self.on_member_update(update, now),
            Event::MessageCreate(create) => self.on_message(&create.message, false, now),
            Event::MessageUpdate(update) => self.on_message_update(&update.0, now),
            Event::MessageDelete(delete) => self.on_message_delete(delete),
            Event::MessageDeleteBulk(bulk) => {
                if !self.in_scope(bulk.guild_id) {
                    return Vec::new();
                }
                bulk.ids
                    .iter()
                    .map(|id| {
                        Step::Emit(InboundEvent::MessageDeleted {
                            room: map::room_id(bulk.channel_id),
                            id: map::message_id(*id),
                        })
                    })
                    .collect()
            }
            Event::ReactionAdd(reaction) => self.on_reaction(&reaction.0, true, now),
            Event::ReactionRemove(reaction) => self.on_reaction(&reaction.0, false, now),
            Event::MessagePollVoteAdd(vote) => self.on_poll_vote(
                (vote.guild_id, vote.channel_id, vote.message_id),
                vote.user_id,
                vote.answer_id,
                true,
                now,
            ),
            Event::MessagePollVoteRemove(vote) => self.on_poll_vote(
                (vote.guild_id, vote.channel_id, vote.message_id),
                vote.user_id,
                vote.answer_id,
                false,
                now,
            ),
            _ => Vec::new(),
        }
    }

    /// A message fetched after a partial update, handled as the edit it is.
    /// REST responses carry no guild id, so it is filled in from the channel.
    pub fn handle_fetched_edit(&mut self, message: &TwMessage, now: Instant) -> Vec<Step> {
        let mut message = message.clone();
        if message.guild_id.is_none() {
            message.guild_id = self.guild_of(message.channel_id);
        }
        self.on_message_update(&message, now)
    }

    fn guild_of(&self, channel: Id<ChannelMarker>) -> Option<Id<GuildMarker>> {
        self.guilds
            .iter()
            .find(|(_, g)| g.channels.contains_key(&channel) || g.threads.contains_key(&channel))
            .map(|(id, _)| *id)
    }

    /// The member list of a private thread, read over REST.
    pub fn apply_thread_members(
        &mut self,
        thread: Id<ChannelMarker>,
        users: Vec<Id<UserMarker>>,
    ) -> Vec<Step> {
        let Some((guild_id, guild)) = self
            .guilds
            .iter_mut()
            .find(|(_, g)| g.threads.contains_key(&thread))
            .map(|(id, g)| (*id, g))
        else {
            return Vec::new();
        };
        if let Some(record) = guild.threads.get_mut(&thread) {
            record.members = Some(users.into_iter().collect());
        }
        let me = guild.me(self.me);
        let visible = me.is_some_and(|m| guild.presence(guild_id, &m, thread) == Presence::In);
        match (visible, guild.room_sync(guild_id, thread)) {
            (true, Some(sync)) => vec![Step::Emit(sync)],
            _ => Vec::new(),
        }
    }

    fn in_scope(&self, guild: Option<Id<GuildMarker>>) -> bool {
        guild.is_none_or(|g| self.guilds.contains_key(&g))
    }

    fn on_ready(&mut self, ready: &Ready) -> Vec<Step> {
        self.me = Some(ready.user.id);
        vec![Step::Emit(InboundEvent::Ready {
            platform: Platform::Discord,
            me: map::user_id(ready.user.id),
            me_name: ready.user.name.clone(),
            capabilities: crate::capabilities(),
        })]
    }

    fn on_guild_create(&mut self, guild: &Guild) -> Vec<Step> {
        if self
            .allowed
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(&guild.id))
        {
            return Vec::new();
        }
        let mut channels = BTreeMap::new();
        for channel in &guild.channels {
            if channel.kind != ChannelType::Private {
                channels.insert(channel.id, channel_record(channel));
            }
        }
        let mut threads = BTreeMap::new();
        for thread in &guild.threads {
            if let Some(record) = thread_record(thread) {
                threads.insert(thread.id, record);
            }
        }
        let members: HashMap<_, _> = guild
            .members
            .iter()
            .map(|m| (m.user.id, MemberRecord::from_member(m)))
            .collect();
        let complete = guild
            .member_count
            .is_some_and(|n| members.len() as u64 >= n);
        let state = GuildState {
            name: guild.name.clone(),
            owner: guild.owner_id,
            roles: role_table(&guild.roles),
            bot_role: guild
                .roles
                .iter()
                .find(|r| {
                    r.tags
                        .as_ref()
                        .and_then(|t| t.bot_id)
                        .is_some_and(|id| Some(id) == self.me)
                })
                .map(|r| r.id),
            channels,
            threads,
            members,
            awaiting_chunks: !complete,
        };
        tracing::info!(guild = %guild.id, name = %state.name, complete, "guild available");
        self.guilds.insert(guild.id, state);
        if complete {
            self.sync_guild(guild.id)
        } else {
            vec![Step::RequestMembers(guild.id)]
        }
    }

    fn on_member_chunk(&mut self, chunk: &MemberChunk) -> Vec<Step> {
        let Some(guild) = self.guilds.get_mut(&chunk.guild_id) else {
            return Vec::new();
        };
        if !guild.awaiting_chunks {
            return Vec::new();
        }
        for member in &chunk.members {
            guild
                .members
                .insert(member.user.id, MemberRecord::from_member(member));
        }
        if chunk.chunk_index + 1 < chunk.chunk_count {
            return Vec::new();
        }
        guild.awaiting_chunks = false;
        self.sync_guild(chunk.guild_id)
    }

    /// A RoomSync for every room the agent can see in the guild.
    fn sync_guild(&self, guild_id: Id<GuildMarker>) -> Vec<Step> {
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Vec::new();
        };
        let rooms = guild.room_ids();
        self.sync_rooms(guild_id, &rooms)
    }

    fn sync_rooms(&self, guild_id: Id<GuildMarker>, rooms: &[Id<ChannelMarker>]) -> Vec<Step> {
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Vec::new();
        };
        if guild.awaiting_chunks {
            return Vec::new();
        }
        let me = guild.me(self.me);
        let mut steps = Vec::new();
        for room in rooms {
            if let Some(thread) = guild.threads.get(room)
                && thread.private
                && thread.members.is_none()
            {
                steps.push(Step::FetchThreadMembers(*room));
                continue;
            }
            let visible = me
                .as_ref()
                .is_some_and(|m| guild.presence(guild_id, m, *room) == Presence::In);
            if visible && let Some(sync) = guild.room_sync(guild_id, *room) {
                steps.push(Step::Emit(sync));
            }
        }
        steps
    }

    fn on_guild_change(
        &mut self,
        guild_id: Id<GuildMarker>,
        change: impl FnOnce(&mut GuildState),
    ) -> Vec<Step> {
        let Some(guild) = self.guilds.get_mut(&guild_id) else {
            return Vec::new();
        };
        change(guild);
        self.sync_guild(guild_id)
    }

    fn on_role(
        &mut self,
        guild_id: Id<GuildMarker>,
        change: impl FnOnce(&mut HashMap<Id<RoleMarker>, Permissions>),
    ) -> Vec<Step> {
        self.on_guild_change(guild_id, |g| change(&mut g.roles))
    }

    fn on_channel(&mut self, channel: &Channel) -> Vec<Step> {
        let Some(guild_id) = channel.guild_id else {
            return Vec::new();
        };
        let Some(guild) = self.guilds.get_mut(&guild_id) else {
            return Vec::new();
        };
        if thread_record(channel).is_some() || channel.kind == ChannelType::Private {
            return Vec::new();
        }
        guild.channels.insert(channel.id, channel_record(channel));
        let mut rooms = vec![channel.id];
        rooms.extend(guild.threads_of(channel.id));
        self.sync_rooms(guild_id, &rooms)
    }

    fn on_channel_delete(&mut self, channel: &Channel, now: Instant) -> Vec<Step> {
        let Some(guild_id) = channel.guild_id else {
            return Vec::new();
        };
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Vec::new();
        };
        let mut rooms = vec![channel.id];
        rooms.extend(guild.threads_of(channel.id));
        let steps = self.bot_left(guild_id, &rooms, now);
        if let Some(guild) = self.guilds.get_mut(&guild_id) {
            guild.channels.remove(&channel.id);
            guild.threads.retain(|_, t| t.parent != channel.id);
        }
        steps
    }

    fn on_thread_delete(
        &mut self,
        guild_id: Id<GuildMarker>,
        thread: Id<ChannelMarker>,
        now: Instant,
    ) -> Vec<Step> {
        let steps = self.bot_left(guild_id, &[thread], now);
        if let Some(guild) = self.guilds.get_mut(&guild_id) {
            guild.threads.remove(&thread);
        }
        steps
    }

    /// The bot was removed from a guild, or the guild was deleted. A
    /// temporary outage keeps its rooms, so it ends nothing.
    fn on_guild_delete(&mut self, delete: &GuildDelete, now: Instant) -> Vec<Step> {
        let steps = match self.guilds.get(&delete.id) {
            Some(guild) if delete.unavailable != Some(true) => {
                self.bot_left(delete.id, &guild.room_ids(), now)
            }
            _ => Vec::new(),
        };
        self.guilds.remove(&delete.id);
        steps
    }

    /// The agent leaving each of `rooms` it was in. The shared event model
    /// has no room-removed event, so the engine learns of a vanished room
    /// from the agent's own departure.
    fn bot_left(
        &self,
        guild_id: Id<GuildMarker>,
        rooms: &[Id<ChannelMarker>],
        at: Instant,
    ) -> Vec<Step> {
        let (Some(guild), Some(me)) = (self.guilds.get(&guild_id), self.me) else {
            return Vec::new();
        };
        if guild.awaiting_chunks {
            return Vec::new();
        }
        let Some(record) = guild.me(Some(me)) else {
            return Vec::new();
        };
        rooms
            .iter()
            .filter(|room| {
                guild.room(guild_id, **room).is_some()
                    && guild.presence(guild_id, &record, **room) == Presence::In
            })
            .map(|room| {
                Step::Emit(InboundEvent::MemberLeft {
                    room: map::room_id(*room),
                    user: map::user_id(me),
                    at,
                })
            })
            .collect()
    }

    /// `guild` is passed separately because list syncs may leave it off the thread.
    fn on_thread(&mut self, guild: Option<Id<GuildMarker>>, thread: &Channel) -> Vec<Step> {
        let Some(guild_id) = guild.or(thread.guild_id) else {
            return Vec::new();
        };
        let Some(guild) = self.guilds.get_mut(&guild_id) else {
            return Vec::new();
        };
        let Some(mut record) = thread_record(thread) else {
            return Vec::new();
        };
        if let Some(previous) = guild.threads.get(&thread.id) {
            record.members = previous.members.clone();
        }
        guild.threads.insert(thread.id, record);
        self.sync_rooms(guild_id, &[thread.id])
    }

    fn on_thread_members_update(&mut self, update: &ThreadMembersUpdate) -> Vec<Step> {
        let Some(guild) = self.guilds.get_mut(&update.guild_id) else {
            return Vec::new();
        };
        let Some(thread) = guild.threads.get_mut(&update.id) else {
            return Vec::new();
        };
        if !thread.private {
            return Vec::new();
        }
        let Some(members) = thread.members.as_mut() else {
            return vec![Step::FetchThreadMembers(update.id)];
        };
        for added in &update.added_members {
            if let Some(user) = added.user_id {
                members.insert(user);
            }
        }
        for removed in &update.removed_member_ids {
            members.remove(removed);
        }
        self.sync_rooms(update.guild_id, &[update.id])
    }

    fn on_member_add(
        &mut self,
        guild_id: Id<GuildMarker>,
        member: &TwMember,
        now: Instant,
    ) -> Vec<Step> {
        let Some(guild) = self.guilds.get_mut(&guild_id) else {
            return Vec::new();
        };
        let record = MemberRecord::from_member(member);
        guild.members.insert(record.id, record);
        if guild.awaiting_chunks {
            return Vec::new();
        }
        let at = member.joined_at.and_then(map::instant).unwrap_or(now);
        self.transitions(guild_id, member.user.id, None, at)
    }

    fn on_member_remove(
        &mut self,
        guild_id: Id<GuildMarker>,
        user: Id<UserMarker>,
        now: Instant,
    ) -> Vec<Step> {
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Vec::new();
        };
        let before = guild.members.get(&user).cloned();
        let steps = if guild.awaiting_chunks {
            Vec::new()
        } else {
            self.departures(guild_id, before.as_ref(), now)
        };
        if let Some(guild) = self.guilds.get_mut(&guild_id) {
            guild.members.remove(&user);
            for thread in guild.threads.values_mut() {
                if let Some(members) = thread.members.as_mut() {
                    members.remove(&user);
                }
            }
        }
        steps
    }

    fn on_member_update(&mut self, update: &MemberUpdate, now: Instant) -> Vec<Step> {
        let Some(guild) = self.guilds.get_mut(&update.guild_id) else {
            return Vec::new();
        };
        let before = guild.members.get(&update.user.id).cloned();
        let record = MemberRecord::from_update(update);
        guild.members.insert(record.id, record);
        if guild.awaiting_chunks {
            return Vec::new();
        }
        self.transitions(update.guild_id, update.user.id, before, now)
    }

    /// Compare a member's rooms before and after a change and emit a join,
    /// leave or update for each room that differs. `before` of `None` means
    /// the member was not in the guild.
    fn transitions(
        &self,
        guild_id: Id<GuildMarker>,
        user: Id<UserMarker>,
        before: Option<MemberRecord>,
        at: Instant,
    ) -> Vec<Step> {
        let Some(guild) = self.guilds.get(&guild_id) else {
            return Vec::new();
        };
        let Some(after) = guild.members.get(&user) else {
            return Vec::new();
        };
        let me = guild.me(self.me);
        let after_member = guild.member(guild_id, after);
        let before_member = before.as_ref().map(|b| guild.member(guild_id, b));
        let mut steps = Vec::new();
        for room in guild.room_ids() {
            let seen = me
                .as_ref()
                .is_some_and(|m| guild.presence(guild_id, m, room) == Presence::In);
            if !seen {
                continue;
            }
            let was = before
                .as_ref()
                .map_or(Presence::Out, |b| guild.presence(guild_id, b, room));
            let is = guild.presence(guild_id, after, room);
            let room_id = map::room_id(room);
            let event = match (was, is) {
                (Presence::Out, Presence::In) => Some(InboundEvent::MemberJoined {
                    room: room_id,
                    member: after_member.clone(),
                    at,
                }),
                (Presence::In, Presence::Out) => Some(InboundEvent::MemberLeft {
                    room: room_id,
                    user: map::user_id(user),
                    at,
                }),
                (Presence::In, Presence::In) if before_member.as_ref() != Some(&after_member) => {
                    Some(InboundEvent::MemberUpdated {
                        room: room_id,
                        member: after_member.clone(),
                    })
                }
                _ => None,
            };
            steps.extend(event.map(Step::Emit));
        }
        steps
    }

    fn departures(
        &self,
        guild_id: Id<GuildMarker>,
        record: Option<&MemberRecord>,
        at: Instant,
    ) -> Vec<Step> {
        let (Some(guild), Some(record)) = (self.guilds.get(&guild_id), record) else {
            return Vec::new();
        };
        let me = guild.me(self.me);
        guild
            .room_ids()
            .into_iter()
            .filter(|room| {
                me.as_ref()
                    .is_some_and(|m| guild.presence(guild_id, m, *room) == Presence::In)
            })
            .filter(|room| guild.presence(guild_id, record, *room) == Presence::In)
            .map(|room| {
                Step::Emit(InboundEvent::MemberLeft {
                    room: map::room_id(room),
                    user: map::user_id(record.id),
                    at,
                })
            })
            .collect()
    }

    /// A changed message: the end of its poll, if it carries one, then the
    /// edit itself. Poll results are read before the own-message filter
    /// because the agent's polls are its own messages.
    fn on_message_update(&mut self, message: &TwMessage, now: Instant) -> Vec<Step> {
        let mut steps = self.on_poll_results(message);
        steps.extend(self.on_message(message, true, now));
        steps
    }

    /// A `PollEnded` the first time a message shows final results.
    fn on_poll_results(&mut self, message: &TwMessage) -> Vec<Step> {
        let Some(counts) = message.poll.as_ref().and_then(poll::final_counts) else {
            return Vec::new();
        };
        if !self.in_scope(message.guild_id) || !self.ended_polls.insert(message.id) {
            return Vec::new();
        }
        vec![Step::Emit(InboundEvent::PollEnded {
            room: map::room_id(message.channel_id),
            message: map::message_id(message.id),
            counts,
        })]
    }

    /// A vote added to or taken off a poll, by someone other than the agent.
    /// `place` is the guild, channel and message the vote was cast on.
    fn on_poll_vote(
        &self,
        place: (
            Option<Id<GuildMarker>>,
            Id<ChannelMarker>,
            Id<MessageMarker>,
        ),
        user: Id<UserMarker>,
        answer_id: u8,
        added: bool,
        now: Instant,
    ) -> Vec<Step> {
        let (guild, channel, message) = place;
        if Some(user) == self.me || !self.in_scope(guild) {
            return Vec::new();
        }
        let Some(option) = poll::option_index(answer_id) else {
            tracing::debug!(answer_id, "poll vote for an answer numbered 0");
            return Vec::new();
        };
        vec![Step::Emit(InboundEvent::PollVote {
            room: map::room_id(channel),
            message: map::message_id(message),
            user: map::user_id(user),
            option,
            added,
            at: now,
        })]
    }

    fn on_message(&mut self, message: &TwMessage, edit: bool, now: Instant) -> Vec<Step> {
        if Some(message.author.id) == self.me
            || !map::is_user_message(message.kind)
            || !self.in_scope(message.guild_id)
            || (edit && message.edited_timestamp.is_none())
        {
            return Vec::new();
        }
        let mut steps = Vec::new();
        if message.guild_id.is_none() && self.dms.insert(message.channel_id) {
            steps.push(Step::Emit(dm_sync(message.channel_id, &message.author)));
        }
        let bot_role = message
            .guild_id
            .and_then(|g| self.guilds.get(&g))
            .and_then(|g| g.bot_role);
        let converted = map::message(message, self.me, bot_role, &self.agent_name, now);
        steps.push(Step::Emit(if edit {
            InboundEvent::MessageEdited(converted)
        } else {
            InboundEvent::Message(converted)
        }));
        steps
    }

    fn on_message_delete(&self, delete: &MessageDelete) -> Vec<Step> {
        if !self.in_scope(delete.guild_id) {
            return Vec::new();
        }
        vec![Step::Emit(InboundEvent::MessageDeleted {
            room: map::room_id(delete.channel_id),
            id: map::message_id(delete.id),
        })]
    }

    fn on_reaction(&self, reaction: &GatewayReaction, added: bool, now: Instant) -> Vec<Step> {
        if Some(reaction.user_id) == self.me || !self.in_scope(reaction.guild_id) {
            return Vec::new();
        }
        let room = map::room_id(reaction.channel_id);
        let message = map::message_id(reaction.message_id);
        let user = map::user_id(reaction.user_id);
        let emoji = map::emoji_name(&reaction.emoji);
        let event = if added {
            InboundEvent::ReactionAdded {
                room,
                message,
                user,
                emoji,
                at: now,
            }
        } else {
            InboundEvent::ReactionRemoved {
                room,
                message,
                user,
                emoji,
                at: now,
            }
        };
        vec![Step::Emit(event)]
    }
}

/// The RoomSync for a one-to-one DM with `user`.
pub fn dm_sync(channel: Id<ChannelMarker>, user: &User) -> InboundEvent {
    InboundEvent::RoomSync {
        room: map::dm_room(channel),
        members: vec![map::member(user, None, PlatformRole::Regular)],
    }
}

fn role_table(roles: &[Role]) -> HashMap<Id<RoleMarker>, Permissions> {
    roles.iter().map(|r| (r.id, r.permissions)).collect()
}

fn channel_record(channel: &Channel) -> ChannelRecord {
    ChannelRecord {
        name: channel.name.clone(),
        kind: channel.kind,
        overwrites: channel.permission_overwrites.clone().unwrap_or_default(),
    }
}

fn thread_record(channel: &Channel) -> Option<ThreadRecord> {
    let private = map::thread_privacy(channel.kind)?;
    Some(ThreadRecord {
        name: channel.name.clone(),
        parent: channel.parent_id?,
        private,
        members: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use ours_types::RoomKind;
    use serde_json::json;
    use twilight_model::gateway::payload::incoming::{
        GuildCreate, MemberRemove, MessageCreate, MessagePollVoteAdd, MessagePollVoteRemove,
    };

    const GUILD: u64 = 100;
    const OWNER: u64 = 1;
    const ADMIN: u64 = 2;
    const MOD_MESSAGES: u64 = 3;
    const MOD_TIMEOUT: u64 = 4;
    const REGULAR: u64 = 5;
    const ME: u64 = 6;
    const INSIDER: u64 = 7;
    const GENERAL: u64 = 300;
    const PRIVATE: u64 = 301;
    const VIEW: u64 = 1 << 10;

    fn roles() -> Vec<Role> {
        vec![
            tw_role(GUILD, "@everyone", VIEW | (1 << 11) | (1 << 16)),
            tw_role(200, "admin", 1 << 3),
            tw_role(201, "mod", 1 << 13),
            tw_role(202, "timeout", 1 << 40),
            tw_role(203, "insiders", 0),
        ]
    }

    fn everyone() -> Vec<TwMember> {
        vec![
            tw_member(OWNER, "owner", &[]),
            tw_member(ADMIN, "admin", &[200]),
            tw_member(MOD_MESSAGES, "mod-a", &[201]),
            tw_member(MOD_TIMEOUT, "mod-b", &[202]),
            tw_member(REGULAR, "regular", &[]),
            tw_bot_member(ME, "q", &[203]),
            tw_member(INSIDER, "insider", &[203]),
        ]
    }

    fn channels() -> Vec<Channel> {
        let hidden = json!([
            { "id": GUILD.to_string(), "type": 0, "allow": "0", "deny": VIEW.to_string() },
            { "id": "203", "type": 0, "allow": VIEW.to_string(), "deny": "0" }
        ]);
        vec![
            tw_channel(GENERAL, 0, "general", GUILD, json!([])),
            tw_channel(PRIVATE, 0, "private", GUILD, hidden),
            tw_channel(302, 4, "category", GUILD, json!([])),
            tw_channel(303, 2, "voice", GUILD, json!([])),
        ]
    }

    fn guild_create(members: Vec<TwMember>, count: u64) -> Event {
        let guild = tw_guild(GUILD, OWNER, count, roles(), members, channels());
        Event::GuildCreate(Box::new(GuildCreate::Available(guild)))
    }

    fn chunk(members: Vec<TwMember>, index: u32, of: u32) -> Event {
        Event::MemberChunk(MemberChunk {
            chunk_count: of,
            chunk_index: index,
            guild_id: gid(GUILD),
            members,
            nonce: None,
            not_found: Vec::new(),
            presences: Vec::new(),
        })
    }

    fn ready() -> Event {
        let user = json!({
            "id": ME.to_string(), "username": "q", "discriminator": "0", "avatar": null,
            "bot": true, "mfa_enabled": true
        });
        Event::Ready(
            serde_json::from_value(json!({
                "application": { "id": "50", "flags": 0 }, "guilds": [],
                "resume_gateway_url": "wss://example", "session_id": "s", "user": user, "v": 10
            }))
            .unwrap(),
        )
    }

    fn at() -> Instant {
        Instant::from_millis(1_788_343_200_000).unwrap()
    }

    fn started() -> Cache {
        let mut cache = Cache::new("Q".to_owned(), None, DmRooms::default());
        let steps = cache.handle(&ready(), at());
        assert_eq!(
            steps,
            vec![Step::Emit(InboundEvent::Ready {
                platform: Platform::Discord,
                me: ours_types::PlatformUserId::new(ME.to_string()),
                me_name: "q".to_owned(),
                capabilities: crate::capabilities(),
            })]
        );
        cache
    }

    /// A cache that has finished syncing the fixture guild.
    fn synced() -> Cache {
        let mut cache = started();
        cache.handle(&guild_create(vec![tw_bot_member(ME, "q", &[203])], 7), at());
        cache.handle(&chunk(everyone(), 0, 1), at());
        cache
    }

    fn syncs(steps: Vec<Step>) -> Vec<(Room, Vec<Member>)> {
        steps
            .into_iter()
            .filter_map(|s| match s {
                Step::Emit(InboundEvent::RoomSync { room, members }) => Some((room, members)),
                _ => None,
            })
            .collect()
    }

    fn events(steps: Vec<Step>) -> Vec<InboundEvent> {
        steps
            .into_iter()
            .filter_map(|s| match s {
                Step::Emit(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn roles_in(members: &[Member]) -> Vec<(String, PlatformRole, bool)> {
        members
            .iter()
            .map(|m| (m.display_name.clone(), m.platform_role, m.is_bot))
            .collect()
    }

    #[test]
    fn a_partial_member_list_is_completed_with_a_chunk_request() {
        let mut cache = started();
        let steps = cache.handle(&guild_create(vec![tw_bot_member(ME, "q", &[203])], 7), at());
        assert_eq!(steps, vec![Step::RequestMembers(gid(GUILD))]);
    }

    #[test]
    fn a_complete_member_list_syncs_without_asking() {
        let mut cache = started();
        let steps = cache.handle(&guild_create(everyone(), 7), at());
        assert!(!steps.iter().any(|s| matches!(s, Step::RequestMembers(_))));
        assert_eq!(syncs(steps).len(), 2);
    }

    #[test]
    fn chunks_are_collected_before_anything_is_synced() {
        let mut cache = started();
        cache.handle(&guild_create(vec![tw_bot_member(ME, "q", &[203])], 7), at());
        let first = cache.handle(&chunk(everyone()[..4].to_vec(), 0, 2), at());
        assert!(first.is_empty());
        let last = cache.handle(&chunk(everyone()[4..].to_vec(), 1, 2), at());
        let rooms = syncs(last);
        assert_eq!(rooms.len(), 2);
        assert_eq!(rooms[0].1.len(), 7);
    }

    #[test]
    fn text_channels_sync_with_the_roles_discord_reports() {
        let mut cache = started();
        cache.handle(&guild_create(vec![tw_bot_member(ME, "q", &[203])], 7), at());
        let rooms = syncs(cache.handle(&chunk(everyone(), 0, 1), at()));

        let (general, members) = &rooms[0];
        assert_eq!(general.id.as_str(), "300");
        assert_eq!(general.name.as_deref(), Some("general"));
        assert_eq!(general.kind, RoomKind::Channel);
        assert_eq!(general.parent, None);
        assert_eq!(general.container.as_deref(), Some("100"));
        assert_eq!(
            roles_in(members),
            [
                ("owner".to_owned(), PlatformRole::Owner, false),
                ("admin".to_owned(), PlatformRole::Admin, false),
                ("mod-a".to_owned(), PlatformRole::Moderator, false),
                ("mod-b".to_owned(), PlatformRole::Moderator, false),
                ("regular".to_owned(), PlatformRole::Regular, false),
                ("q".to_owned(), PlatformRole::Regular, true),
                ("insider".to_owned(), PlatformRole::Regular, false),
            ]
        );
        assert!(members.iter().all(|m| m.timezone.is_none()));
    }

    #[test]
    fn a_private_channel_lists_only_those_who_can_see_it() {
        let mut cache = started();
        cache.handle(&guild_create(vec![tw_bot_member(ME, "q", &[203])], 7), at());
        let rooms = syncs(cache.handle(&chunk(everyone(), 0, 1), at()));
        let (private, members) = &rooms[1];
        assert_eq!(private.id.as_str(), "301");
        let names: Vec<_> = members.iter().map(|m| m.display_name.as_str()).collect();
        assert_eq!(names, ["owner", "admin", "q", "insider"]);
    }

    #[test]
    fn channels_the_bot_cannot_see_are_not_synced() {
        let mut cache = started();
        let blind_bot = tw_bot_member(ME, "q", &[]);
        let mut members = everyone();
        members[5] = blind_bot.clone();
        cache.handle(&guild_create(vec![blind_bot], 7), at());
        let rooms = syncs(cache.handle(&chunk(members, 0, 1), at()));
        let ids: Vec<_> = rooms
            .iter()
            .map(|(r, _)| r.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["300"]);
    }

    #[test]
    fn a_guild_outside_the_allowlist_is_ignored() {
        let allowed = Some([gid(999)].into_iter().collect());
        let mut cache = Cache::new("Q".to_owned(), allowed, DmRooms::default());
        cache.handle(&ready(), at());
        assert!(cache.handle(&guild_create(everyone(), 7), at()).is_empty());
        let message = tw_message(1, GENERAL, Some(GUILD), REGULAR, "hello", &[]);
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message,
        }));
        assert!(cache.handle(&create, at()).is_empty());
    }

    #[test]
    fn public_threads_sync_under_their_parent_with_its_members() {
        let mut cache = synced();
        let thread = Event::ThreadCreate(Box::new(
            serde_json::from_value(
                serde_json::to_value(tw_thread(400, 11, "plans", GUILD, GENERAL)).unwrap(),
            )
            .unwrap(),
        ));
        let rooms = syncs(cache.handle(&thread, at()));
        assert_eq!(rooms.len(), 1);
        let (room, members) = &rooms[0];
        assert_eq!(room.kind, RoomKind::Thread);
        assert_eq!(room.parent.as_ref().map(|p| p.as_str()), Some("300"));
        assert_eq!(room.container.as_deref(), Some("100"));
        assert_eq!(members.len(), 7);
    }

    #[test]
    fn a_thread_list_sync_does_not_need_guild_ids_on_the_threads() {
        let mut cache = synced();
        let mut thread = tw_thread(402, 11, "old plans", GUILD, GENERAL);
        thread.guild_id = None;
        let sync =
            Event::ThreadListSync(twilight_model::gateway::payload::incoming::ThreadListSync {
                channel_ids: vec![cid(GENERAL)],
                guild_id: gid(GUILD),
                members: Vec::new(),
                threads: vec![thread],
            });
        let rooms = syncs(cache.handle(&sync, at()));
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].0.id.as_str(), "402");
    }

    #[test]
    fn private_threads_wait_for_their_member_list() {
        let mut cache = synced();
        let thread = Event::ThreadCreate(Box::new(
            serde_json::from_value(
                serde_json::to_value(tw_thread(401, 12, "secret", GUILD, GENERAL)).unwrap(),
            )
            .unwrap(),
        ));
        assert_eq!(
            cache.handle(&thread, at()),
            vec![Step::FetchThreadMembers(cid(401))]
        );

        let rooms = syncs(cache.apply_thread_members(cid(401), vec![uid(ME), uid(REGULAR)]));
        let names: Vec<_> = rooms[0].1.iter().map(|m| m.display_name.as_str()).collect();
        assert_eq!(names, ["regular", "q"]);
    }

    #[test]
    fn a_first_dm_announces_the_room_then_the_message() {
        let mut cache = started();
        let author = user_json(REGULAR, "regular", false);
        let message = serde_json::from_value(message_json(
            8,
            900,
            None,
            author,
            "Q, hi",
            vec![],
            None,
            vec![],
        ))
        .unwrap();
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message,
        }));
        let out = events(cache.handle(&create, at()));
        assert_eq!(out.len(), 2);
        let InboundEvent::RoomSync { room, members } = &out[0] else {
            panic!("{:?}", out[0])
        };
        assert_eq!(
            (room.kind, room.id.as_str(), room.container.clone()),
            (RoomKind::Dm, "900", None)
        );
        assert_eq!(
            roles_in(members),
            [("regular".to_owned(), PlatformRole::Regular, false)]
        );
        assert!(matches!(&out[1], InboundEvent::Message(m) if m.text == "Q, hi" && m.mentions_me));

        let again = events(cache.handle(&create, at()));
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn the_bots_own_messages_and_reactions_are_ignored() {
        let mut cache = synced();
        let own = tw_message(5, GENERAL, Some(GUILD), ME, "done", &[]);
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message: own,
        }));
        assert!(cache.handle(&create, at()).is_empty());

        let reaction: GatewayReaction = serde_json::from_value(json!({
            "burst": false, "channel_id": "300", "guild_id": "100", "message_id": "5",
            "user_id": ME.to_string(), "emoji": { "id": null, "name": "👀" }
        }))
        .unwrap();
        assert!(
            cache
                .handle(
                    &Event::ReactionAdd(Box::new(
                        twilight_model::gateway::payload::incoming::ReactionAdd(reaction)
                    )),
                    at()
                )
                .is_empty()
        );
    }

    #[test]
    fn messages_in_a_known_guild_are_relayed_with_addressing() {
        let mut cache = synced();
        let message = tw_message(6, GENERAL, Some(GUILD), REGULAR, "Q, book it", &[]);
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message,
        }));
        let out = events(cache.handle(&create, at()));
        let [InboundEvent::Message(m)] = out.as_slice() else {
            panic!("{out:?}")
        };
        assert_eq!(
            (m.id.as_str(), m.room.as_str(), m.author.as_str()),
            ("6", "300", "5")
        );
        assert!(m.mentions_me);
        assert!(!m.edited);
    }

    #[test]
    fn only_real_edits_become_message_edited() {
        let mut cache = synced();
        let plain = tw_message(6, GENERAL, Some(GUILD), REGULAR, "Q, book it", &[]);
        let unedited = Event::MessageUpdate(Box::new(
            twilight_model::gateway::payload::incoming::MessageUpdate(plain.clone()),
        ));
        assert!(cache.handle(&unedited, at()).is_empty());

        let mut edited = plain;
        edited.edited_timestamp =
            Some(twilight_model::util::Timestamp::from_micros(1_788_343_300_000_000).unwrap());
        edited.content = "Q, book it for six".to_owned();
        let update = Event::MessageUpdate(Box::new(
            twilight_model::gateway::payload::incoming::MessageUpdate(edited),
        ));
        let out = events(cache.handle(&update, at()));
        assert!(
            matches!(out.as_slice(), [InboundEvent::MessageEdited(m)] if m.edited && m.text == "Q, book it for six")
        );
    }

    #[test]
    fn a_fetched_edit_is_placed_in_its_guild() {
        let mut cache = synced();
        let mut fetched = tw_message(6, GENERAL, None, REGULAR, "Q, book it for six", &[]);
        fetched.edited_timestamp =
            Some(twilight_model::util::Timestamp::from_micros(1_788_343_300_000_000).unwrap());
        let out = events(cache.handle_fetched_edit(&fetched, at()));
        assert!(
            matches!(out.as_slice(), [InboundEvent::MessageEdited(m)] if m.room.as_str() == "300")
        );
    }

    #[test]
    fn reactions_and_deletes_are_relayed() {
        let mut cache = synced();
        let reaction: GatewayReaction = serde_json::from_value(json!({
            "burst": false, "channel_id": "300", "guild_id": "100", "message_id": "5",
            "user_id": REGULAR.to_string(), "emoji": { "id": "555", "name": "party" }
        }))
        .unwrap();
        let add = Event::ReactionAdd(Box::new(
            twilight_model::gateway::payload::incoming::ReactionAdd(reaction.clone()),
        ));
        let out = events(cache.handle(&add, at()));
        assert!(matches!(
            out.as_slice(),
            [InboundEvent::ReactionAdded { emoji, user, room, message, .. }]
                if emoji == "party:555" && user.as_str() == "5" && room.as_str() == "300" && message.as_str() == "5"
        ));
        let remove = Event::ReactionRemove(Box::new(
            twilight_model::gateway::payload::incoming::ReactionRemove(reaction),
        ));
        assert!(matches!(
            events(cache.handle(&remove, at())).as_slice(),
            [InboundEvent::ReactionRemoved { .. }]
        ));

        let delete = Event::MessageDelete(MessageDelete {
            channel_id: cid(GENERAL),
            guild_id: Some(gid(GUILD)),
            id: id(5),
        });
        assert_eq!(
            events(cache.handle(&delete, at())),
            vec![InboundEvent::MessageDeleted {
                room: map::room_id(cid(GENERAL)),
                id: map::message_id(id(5)),
            }]
        );
    }

    fn member_update(user: u64, name: &str, roles: &[u64]) -> Event {
        let roles: Vec<String> = roles.iter().map(u64::to_string).collect();
        Event::MemberUpdate(Box::new(
            serde_json::from_value(json!({
                "guild_id": GUILD.to_string(), "roles": roles, "user": user_json(user, name, false),
                "nick": null, "avatar": null, "joined_at": null, "premium_since": null,
                "communication_disabled_until": null, "pending": false
            }))
            .unwrap(),
        ))
    }

    #[test]
    fn a_promotion_updates_the_member_and_opens_rooms_admins_can_see() {
        let mut cache = synced();
        let out = events(cache.handle(&member_update(REGULAR, "regular", &[200]), at()));
        assert_eq!(out.len(), 2);
        assert!(matches!(
            &out[0],
            InboundEvent::MemberUpdated { room, member }
                if room.as_str() == "300" && member.platform_role == PlatformRole::Admin
        ));
        assert!(matches!(
            &out[1],
            InboundEvent::MemberJoined { room, member, .. }
                if room.as_str() == "301" && member.platform_role == PlatformRole::Admin
        ));
    }

    #[test]
    fn gaining_access_to_a_private_channel_is_a_join() {
        let mut cache = synced();
        let out = events(cache.handle(&member_update(REGULAR, "regular", &[203]), at()));
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            InboundEvent::MemberJoined { room, member, .. } if room.as_str() == "301" && member.id.as_str() == "5"
        ));
    }

    #[test]
    fn an_unchanged_member_update_is_silent() {
        let mut cache = synced();
        assert!(
            cache
                .handle(&member_update(REGULAR, "regular", &[]), at())
                .is_empty()
        );
    }

    #[test]
    fn new_members_join_the_rooms_they_can_see() {
        let mut cache = synced();
        let add = Event::MemberAdd(Box::new(serde_json::from_value(json!({
            "guild_id": GUILD.to_string(),
            "user": user_json(55, "newbie", false), "roles": [], "nick": null, "joined_at": "2026-11-02T10:00:00.000000+00:00",
            "deaf": false, "mute": false, "flags": 0, "communication_disabled_until": null
        })).unwrap()));
        let out = events(cache.handle(&add, at()));
        assert_eq!(out.len(), 1);
        let InboundEvent::MemberJoined {
            room,
            member,
            at: joined,
        } = &out[0]
        else {
            panic!("{:?}", out[0])
        };
        assert_eq!(
            (room.as_str(), member.display_name.as_str()),
            ("300", "newbie")
        );
        assert_eq!(joined.to_string(), "2026-11-02T10:00:00Z");
    }

    #[test]
    fn leaving_removes_the_member_from_rooms_they_could_see() {
        let mut cache = synced();
        let leave = Event::MemberRemove(MemberRemove {
            guild_id: gid(GUILD),
            user: tw_user(INSIDER, "insider", false),
        });
        let out = events(cache.handle(&leave, at()));
        let rooms: Vec<_> = out
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
                ("300".to_owned(), "7".to_owned()),
                ("301".to_owned(), "7".to_owned())
            ]
        );

        let again = events(cache.handle(&leave, at()));
        assert!(again.is_empty());
    }

    #[test]
    fn a_role_change_resyncs_every_room() {
        let mut cache = synced();
        let update = Event::RoleUpdate(twilight_model::gateway::payload::incoming::RoleUpdate {
            guild_id: gid(GUILD),
            role: tw_role(203, "insiders", 1 << 13),
        });
        let rooms = syncs(cache.handle(&update, at()));
        assert_eq!(rooms.len(), 2);
        let insider = rooms[0]
            .1
            .iter()
            .find(|m| m.display_name == "insider")
            .unwrap();
        assert_eq!(insider.platform_role, PlatformRole::Moderator);
    }

    fn thread_create(id: u64, kind: u8, parent: u64) -> Event {
        Event::ThreadCreate(Box::new(
            serde_json::from_value(
                serde_json::to_value(tw_thread(id, kind, "plans", GUILD, parent)).unwrap(),
            )
            .unwrap(),
        ))
    }

    fn channel_delete(id: u64, kind: u8) -> Event {
        let channel = tw_channel(id, kind, "gone", GUILD, json!([]));
        Event::ChannelDelete(Box::new(
            serde_json::from_value(serde_json::to_value(channel).unwrap()).unwrap(),
        ))
    }

    fn guild_delete(unavailable: Option<bool>) -> Event {
        Event::GuildDelete(GuildDelete {
            id: gid(GUILD),
            unavailable,
        })
    }

    /// The rooms the agent itself was reported leaving.
    fn bot_departures(steps: Vec<Step>) -> Vec<String> {
        events(steps)
            .into_iter()
            .map(|e| match e {
                InboundEvent::MemberLeft { room, user, .. } => {
                    assert_eq!(user.as_str(), "6");
                    room.as_str().to_owned()
                }
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_deleted_channel_takes_the_agent_out_of_it_and_its_threads() {
        let mut cache = synced();
        cache.handle(&thread_create(400, 11, GENERAL), at());
        let left = bot_departures(cache.handle(&channel_delete(GENERAL, 0), at()));
        assert_eq!(left, ["300", "400"]);
        assert!(cache.handle(&channel_delete(GENERAL, 0), at()).is_empty());
    }

    #[test]
    fn deleting_something_that_was_never_a_room_is_silent() {
        let mut cache = synced();
        assert!(cache.handle(&channel_delete(302, 4), at()).is_empty());
        assert!(cache.handle(&channel_delete(303, 2), at()).is_empty());
    }

    #[test]
    fn a_channel_the_agent_could_not_see_ends_silently() {
        let mut cache = synced();
        let hidden =
            json!([{ "id": GUILD.to_string(), "type": 0, "allow": "0", "deny": VIEW.to_string() }]);
        let create = Event::ChannelCreate(Box::new(
            serde_json::from_value(
                serde_json::to_value(tw_channel(304, 0, "staff", GUILD, hidden)).unwrap(),
            )
            .unwrap(),
        ));
        assert!(syncs(cache.handle(&create, at())).is_empty());
        assert!(cache.handle(&channel_delete(304, 0), at()).is_empty());
    }

    #[test]
    fn a_deleted_thread_takes_the_agent_out_of_it_once() {
        let mut cache = synced();
        cache.handle(&thread_create(400, 11, GENERAL), at());
        let delete = Event::ThreadDelete(
            serde_json::from_value(json!({
                "guild_id": GUILD.to_string(), "id": "400", "type": 11,
                "parent_id": GENERAL.to_string()
            }))
            .unwrap(),
        );
        assert_eq!(bot_departures(cache.handle(&delete, at())), ["400"]);
        assert!(cache.handle(&delete, at()).is_empty());
        let still_there = bot_departures(cache.handle(&channel_delete(GENERAL, 0), at()));
        assert_eq!(still_there, ["300"]);
    }

    #[test]
    fn losing_a_guild_takes_the_agent_out_of_every_room() {
        let mut cache = synced();
        cache.handle(&thread_create(400, 11, GENERAL), at());
        let left = bot_departures(cache.handle(&guild_delete(None), at()));
        assert_eq!(left, ["300", "301", "400"]);

        let message = tw_message(1, GENERAL, Some(GUILD), REGULAR, "hello", &[]);
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message,
        }));
        assert!(cache.handle(&create, at()).is_empty());
        assert!(cache.handle(&guild_delete(None), at()).is_empty());
    }

    #[test]
    fn a_guild_outage_does_not_end_any_room() {
        let mut cache = synced();
        assert!(cache.handle(&guild_delete(Some(true)), at()).is_empty());
        let back = syncs(cache.handle(&guild_create(everyone(), 7), at()));
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn rooms_never_announced_are_not_ended() {
        let mut cache = started();
        cache.handle(&guild_create(vec![tw_bot_member(ME, "q", &[203])], 7), at());
        assert!(cache.handle(&guild_delete(None), at()).is_empty());
    }

    fn vote_add(user: u64, answer: u8, guild: Option<u64>) -> Event {
        Event::MessagePollVoteAdd(MessagePollVoteAdd {
            answer_id: answer,
            channel_id: cid(GENERAL),
            guild_id: guild.map(gid),
            message_id: id(5),
            user_id: uid(user),
        })
    }

    fn vote_remove(user: u64, answer: u8, guild: Option<u64>) -> Event {
        Event::MessagePollVoteRemove(MessagePollVoteRemove {
            answer_id: answer,
            channel_id: cid(GENERAL),
            guild_id: guild.map(gid),
            message_id: id(5),
            user_id: uid(user),
        })
    }

    #[test]
    fn votes_become_zero_based_options() {
        let mut cache = synced();
        let out = events(cache.handle(&vote_add(REGULAR, 1, Some(GUILD)), at()));
        assert_eq!(
            out,
            vec![InboundEvent::PollVote {
                room: map::room_id(cid(GENERAL)),
                message: map::message_id(id(5)),
                user: map::user_id(uid(REGULAR)),
                option: 0,
                added: true,
                at: at(),
            }]
        );

        let out = events(cache.handle(&vote_remove(INSIDER, 3, Some(GUILD)), at()));
        assert!(matches!(
            out.as_slice(),
            [InboundEvent::PollVote { option: 2, added: false, user, .. }] if user.as_str() == "7"
        ));
    }

    #[test]
    fn the_bots_own_votes_are_dropped() {
        let mut cache = synced();
        assert!(cache.handle(&vote_add(ME, 1, Some(GUILD)), at()).is_empty());
        assert!(
            cache
                .handle(&vote_remove(ME, 1, Some(GUILD)), at())
                .is_empty()
        );
    }

    #[test]
    fn a_vote_for_answer_zero_is_dropped() {
        let mut cache = synced();
        assert!(
            cache
                .handle(&vote_add(REGULAR, 0, Some(GUILD)), at())
                .is_empty()
        );
    }

    #[test]
    fn votes_in_a_guild_outside_the_allowlist_are_ignored() {
        let allowed = Some([gid(999)].into_iter().collect());
        let mut cache = Cache::new("Q".to_owned(), allowed, DmRooms::default());
        cache.handle(&ready(), at());
        assert!(
            cache
                .handle(&vote_add(REGULAR, 1, Some(GUILD)), at())
                .is_empty()
        );
        let in_dm = events(cache.handle(&vote_add(REGULAR, 2, None), at()));
        assert!(matches!(
            in_dm.as_slice(),
            [InboundEvent::PollVote { option: 1, .. }]
        ));
    }

    /// A message from `author` that carries a poll with `answers` options
    /// and the given `(finalized, counts)` results.
    fn poll_message(author: u64, answers: u8, results: Option<(bool, &[(u8, u64)])>) -> TwMessage {
        let mut value = message_json(
            5,
            GENERAL,
            Some(GUILD),
            user_json(author, "author", author == ME),
            "",
            vec![],
            None,
            vec![],
        );
        let answers: Vec<_> = (1..=answers)
            .map(|n| json!({ "answer_id": n, "poll_media": { "text": format!("option {n}") } }))
            .collect();
        let results = results.map(|(is_finalized, counts)| {
            let counts: Vec<_> = counts
                .iter()
                .map(|(id, count)| json!({ "id": id, "count": count, "me_voted": false }))
                .collect();
            json!({ "answer_counts": counts, "is_finalized": is_finalized })
        });
        value["poll"] = json!({
            "question": { "text": "when?" }, "answers": answers, "allow_multiselect": false,
            "expiry": "2026-11-04T10:00:00.000000+00:00", "layout_type": 1, "results": results
        });
        serde_json::from_value(value).unwrap()
    }

    fn message_update(message: TwMessage) -> Event {
        Event::MessageUpdate(Box::new(
            twilight_model::gateway::payload::incoming::MessageUpdate(message),
        ))
    }

    #[test]
    fn final_results_end_the_poll_once_with_counts_in_option_order() {
        let mut cache = synced();
        let ended = message_update(poll_message(ME, 3, Some((true, &[(3, 4), (1, 2)]))));
        let out = events(cache.handle(&ended, at()));
        assert_eq!(
            out,
            vec![InboundEvent::PollEnded {
                room: map::room_id(cid(GENERAL)),
                message: map::message_id(id(5)),
                counts: vec![2, 0, 4],
            }]
        );
        assert!(cache.handle(&ended, at()).is_empty());
    }

    #[test]
    fn polls_still_open_end_nothing() {
        let mut cache = synced();
        let open = message_update(poll_message(ME, 2, Some((false, &[(1, 1)]))));
        assert!(cache.handle(&open, at()).is_empty());
        let unscored = message_update(poll_message(ME, 2, None));
        assert!(cache.handle(&unscored, at()).is_empty());

        let ended = message_update(poll_message(ME, 2, Some((true, &[(1, 1)]))));
        assert_eq!(events(cache.handle(&ended, at())).len(), 1);
    }

    #[test]
    fn a_polls_end_is_found_in_a_fetched_message_too() {
        let mut cache = synced();
        let mut fetched = poll_message(ME, 2, Some((true, &[(2, 3)])));
        fetched.guild_id = None;
        let out = events(cache.handle_fetched_edit(&fetched, at()));
        assert!(matches!(
            out.as_slice(),
            [InboundEvent::PollEnded { room, counts, .. }]
                if room.as_str() == "300" && counts == &[0, 3]
        ));
        assert!(cache.handle_fetched_edit(&fetched, at()).is_empty());
    }

    #[test]
    fn a_poll_that_ended_in_an_unknown_guild_is_ignored() {
        let allowed = Some([gid(999)].into_iter().collect());
        let mut cache = Cache::new("Q".to_owned(), allowed, DmRooms::default());
        cache.handle(&ready(), at());
        let ended = message_update(poll_message(ME, 2, Some((true, &[(1, 1)]))));
        assert!(cache.handle(&ended, at()).is_empty());
    }

    #[test]
    fn ended_polls_are_remembered_up_to_a_limit() {
        let mut ended = EndedPolls::default();
        assert!(ended.insert(id(1)));
        assert!(!ended.insert(id(1)));
        for n in 2..=(EndedPolls::LIMIT as u64 + 1) {
            assert!(ended.insert(id(n)));
        }
        assert_eq!(ended.seen.len(), EndedPolls::LIMIT);
        assert!(ended.insert(id(1)));
    }

    #[test]
    fn the_poll_result_notice_is_not_a_chat_message() {
        let mut cache = synced();
        let mut notice = tw_message(6, GENERAL, Some(GUILD), ME, "", &[]);
        notice.kind = twilight_model::channel::message::MessageType::PollResult;
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message: notice,
        }));
        assert!(cache.handle(&create, at()).is_empty());

        let mut by_person = tw_message(7, GENERAL, Some(GUILD), REGULAR, "", &[]);
        by_person.kind = twilight_model::channel::message::MessageType::PollResult;
        let create = Event::MessageCreate(Box::new(MessageCreate {
            channel_type: None,
            message: by_person,
        }));
        assert!(cache.handle(&create, at()).is_empty());
    }
}
