//! Where an inbound event goes. Configured rooms map to their group; DM
//! rooms are learned from who is in them; action results follow the action.

use ours_core::Input;
use ours_types::{
    ActionId, GroupId, InboundEvent, Platform, PlatformRoomId, PlatformUserId, RoomKind,
};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Debug, Default)]
struct Tables {
    rooms: HashMap<(Platform, PlatformRoomId), GroupId>,
    members: HashMap<GroupId, HashSet<PlatformUserId>>,
    actions: HashMap<ActionId, GroupId>,
    /// Rooms already reported as belonging to no group.
    ignored: HashSet<(Platform, PlatformRoomId)>,
}

#[derive(Debug)]
pub struct Router {
    tables: Mutex<Tables>,
    by_platform: HashMap<Platform, Vec<GroupId>>,
    queues: HashMap<GroupId, mpsc::Sender<Input>>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Router {
            tables: Mutex::new(Tables::default()),
            by_platform: HashMap::new(),
            queues: HashMap::new(),
        }
    }

    /// Registers a group, the rooms it was configured with, and its queue.
    pub fn register(
        &mut self,
        group: GroupId,
        rooms: &[(Platform, PlatformRoomId)],
        queue: mpsc::Sender<Input>,
    ) {
        let mut tables = self.tables.lock().unwrap_or_else(|e| e.into_inner());
        for (platform, room) in rooms {
            tables
                .rooms
                .insert((*platform, room.clone()), group.clone());
            let on_platform = self.by_platform.entry(*platform).or_default();
            if !on_platform.contains(&group) {
                on_platform.push(group.clone());
            }
        }
        tables.members.entry(group.clone()).or_default();
        self.queues.insert(group, queue);
    }

    /// Remembers which group sent an action so its result can come back.
    pub fn expect_result(&self, action: ActionId, group: GroupId) {
        self.tables
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .actions
            .insert(action, group);
    }

    /// Delivers the event to the group or groups it concerns.
    pub async fn dispatch(&self, platform: Platform, event: InboundEvent) {
        for (group, input) in self.route(platform, event) {
            if let Some(queue) = self.queues.get(&group)
                && queue.send(input).await.is_err()
            {
                tracing::warn!(%group, "group queue is closed");
            }
        }
    }

    fn route(&self, platform: Platform, event: InboundEvent) -> Vec<(GroupId, Input)> {
        let mut tables = self.tables.lock().unwrap_or_else(|e| e.into_inner());
        match &event {
            InboundEvent::Ready { .. } | InboundEvent::Disconnected { .. } => {
                return self
                    .by_platform
                    .get(&platform)
                    .into_iter()
                    .flatten()
                    .map(|g| (g.clone(), Input::Inbound(event.clone())))
                    .collect();
            }
            InboundEvent::ActionResult { id, .. } => {
                return tables
                    .actions
                    .remove(id)
                    .map(|g| (g, Input::Inbound(event)))
                    .into_iter()
                    .collect();
            }
            InboundEvent::RoomSync { room, members } => {
                let key = (platform, room.id.clone());
                let known = tables.rooms.get(&key).cloned();
                let group = match known {
                    Some(group) => group,
                    None if room.kind == RoomKind::Thread => {
                        let Some(group) = room.parent.as_ref().and_then(|parent| {
                            tables.rooms.get(&(platform, parent.clone())).cloned()
                        }) else {
                            tracing::debug!(room = %room.id, "thread under a room in no group; ignored");
                            return Vec::new();
                        };
                        tables.rooms.insert(key, group.clone());
                        group
                    }
                    None if room.kind == RoomKind::Dm && members.len() == 1 => {
                        let user = &members[0].id;
                        let Some(group) = self
                            .by_platform
                            .get(&platform)
                            .into_iter()
                            .flatten()
                            .find(|g| tables.members.get(*g).is_some_and(|m| m.contains(user)))
                            .cloned()
                        else {
                            tracing::debug!(room = %room.id, "DM from someone in no group; ignored");
                            return Vec::new();
                        };
                        tables.rooms.insert(key, group.clone());
                        group
                    }
                    None => {
                        if tables.ignored.insert(key) {
                            tracing::info!(
                                platform = ?platform,
                                room = %room.id,
                                name = room.name.as_deref().unwrap_or(""),
                                "room in no group; add its id under a group's rooms to include it"
                            );
                        }
                        return Vec::new();
                    }
                };
                if room.kind != RoomKind::Dm {
                    let set = tables.members.entry(group.clone()).or_default();
                    set.extend(members.iter().map(|m| m.id.clone()));
                }
                return vec![(group, Input::Inbound(event))];
            }
            _ => {}
        }
        let Some(room) = room_of(&event) else {
            return Vec::new();
        };
        let Some(group) = tables.rooms.get(&(platform, room.clone())).cloned() else {
            tracing::debug!(%room, "event for a room in no group; ignored");
            return Vec::new();
        };
        if let InboundEvent::MemberJoined { member, .. }
        | InboundEvent::MemberUpdated { member, .. } = &event
        {
            tables
                .members
                .entry(group.clone())
                .or_default()
                .insert(member.id.clone());
        }
        vec![(group, Input::Inbound(event))]
    }
}

fn room_of(event: &InboundEvent) -> Option<&PlatformRoomId> {
    match event {
        InboundEvent::Message(m) | InboundEvent::MessageEdited(m) => Some(&m.room),
        InboundEvent::MessageDeleted { room, .. }
        | InboundEvent::ReactionAdded { room, .. }
        | InboundEvent::ReactionRemoved { room, .. }
        | InboundEvent::MemberJoined { room, .. }
        | InboundEvent::MemberLeft { room, .. }
        | InboundEvent::MemberUpdated { room, .. }
        | InboundEvent::PollVote { room, .. }
        | InboundEvent::PollEnded { room, .. }
        | InboundEvent::History { room, .. } => Some(room),
        InboundEvent::RoomSync { room, .. } => Some(&room.id),
        InboundEvent::Ready { .. }
        | InboundEvent::ActionResult { .. }
        | InboundEvent::Disconnected { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_types::{Member, PlatformRole, Room};

    fn member(id: &str) -> Member {
        Member {
            id: id.into(),
            display_name: id.to_owned(),
            platform_role: PlatformRole::Regular,
            is_bot: false,
            timezone: None,
        }
    }

    fn room(id: &str, kind: RoomKind) -> Room {
        Room {
            platform: Platform::Sim,
            id: id.into(),
            name: None,
            kind,
            parent: None,
            container: None,
        }
    }

    #[test]
    fn dm_rooms_are_learned_from_membership() {
        let mut router = Router::new();
        let (tx, _rx) = mpsc::channel(4);
        router.register("g".into(), &[(Platform::Sim, "general".into())], tx);
        let sync = InboundEvent::RoomSync {
            room: room("general", RoomKind::Channel),
            members: vec![member("sam")],
        };
        assert_eq!(router.route(Platform::Sim, sync).len(), 1);
        let stranger = InboundEvent::RoomSync {
            room: room("dm-x", RoomKind::Dm),
            members: vec![member("nobody")],
        };
        assert!(router.route(Platform::Sim, stranger).is_empty());
        let dm = InboundEvent::RoomSync {
            room: room("dm-1", RoomKind::Dm),
            members: vec![member("sam")],
        };
        let routed = router.route(Platform::Sim, dm);
        assert_eq!(routed[0].0, GroupId::new("g"));
        let deleted = InboundEvent::MessageDeleted {
            room: "dm-1".into(),
            id: "m".into(),
        };
        assert_eq!(router.route(Platform::Sim, deleted).len(), 1);
    }

    #[test]
    fn thread_rooms_join_their_parents_group() {
        let mut router = Router::new();
        let (tx, _rx) = mpsc::channel(4);
        router.register("g".into(), &[(Platform::Sim, "general".into())], tx);
        let thread = InboundEvent::RoomSync {
            room: Room {
                parent: Some("general".into()),
                ..room("general:1.2", RoomKind::Thread)
            },
            members: vec![member("sam")],
        };
        assert_eq!(router.route(Platform::Sim, thread)[0].0, GroupId::new("g"));
        let orphan = InboundEvent::RoomSync {
            room: Room {
                parent: Some("elsewhere".into()),
                ..room("elsewhere:9", RoomKind::Thread)
            },
            members: vec![member("sam")],
        };
        assert!(router.route(Platform::Sim, orphan).is_empty());
        let later = InboundEvent::MessageDeleted {
            room: "general:1.2".into(),
            id: "m".into(),
        };
        assert_eq!(router.route(Platform::Sim, later).len(), 1);
    }

    #[test]
    fn action_results_follow_the_action_once() {
        let mut router = Router::new();
        let (tx, _rx) = mpsc::channel(4);
        router.register("g".into(), &[(Platform::Sim, "general".into())], tx);
        router.expect_result("g-a1".into(), "g".into());
        let result = InboundEvent::ActionResult {
            id: "g-a1".into(),
            ok: true,
            message: None,
            error: None,
        };
        assert_eq!(router.route(Platform::Sim, result.clone()).len(), 1);
        assert!(router.route(Platform::Sim, result).is_empty());
    }
}
