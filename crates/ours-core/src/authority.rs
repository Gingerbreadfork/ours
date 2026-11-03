//! Who the speaker is to the agent and what that lets a plan do (DESIGN.md
//! section 5). Computed before the model is called, enforced after; the
//! model never decides who is a lead.

use crate::plan::PlannedAction;
use crate::state::GroupState;
use ours_types::{Instant, PlatformRole, PlatformRoomId, PlatformUserId, Role};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The kinds of thing a request or a plan can do.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Say,
    React,
    ChangeLeads,
    ChangeSettings,
    /// Forget or change facts that are not the asker's own, or read history.
    ChangeMemory,
    /// Send an email from the group's address.
    Mail,
    /// Take back an action the asker neither asked for nor is the subject of.
    Undo,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ActionSet(BTreeSet<ActionKind>);

impl ActionSet {
    pub fn empty() -> ActionSet {
        ActionSet::default()
    }

    pub fn of(kinds: &[ActionKind]) -> ActionSet {
        ActionSet(kinds.iter().copied().collect())
    }

    pub fn allows(&self, kind: ActionKind) -> bool {
        self.0.contains(&kind)
    }

    /// Whether everything in `other` is allowed here.
    pub fn covers(&self, other: &ActionSet) -> bool {
        other.0.is_subset(&self.0)
    }
}

/// A lead over one slice of the group (DESIGN.md section 5). Only room
/// scopes grant anything yet; events and proposals arrive with decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScopedLead {
    pub user: PlatformUserId,
    pub scope: Scope,
    pub expires: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Scope {
    Room { room: PlatformRoomId },
    Event { name: String },
    Proposal { id: String },
}

impl ScopedLead {
    fn leads_room(&self, user: &PlatformUserId, room: &PlatformRoomId) -> bool {
        &self.user == user && matches!(&self.scope, Scope::Room { room: r } if r == room)
    }
}

/// The speaker's role in `room`. External accounts and people who are not in
/// the room are guests whatever any list says; only hosts outrank that. In a
/// DM the role is the best one the person holds in the group's other rooms,
/// since a DM's own membership says nothing about the group.
pub fn resolve_role(state: &GroupState, room: &PlatformRoomId, user: &PlatformUserId) -> Role {
    if state.hosts.contains(user) {
        return Role::Host;
    }
    if state.rooms.get(room).is_some_and(|r| r.is_dm()) {
        return role_outside_dms(state, user);
    }
    let Some(member) = state.rooms.get(room).and_then(|r| r.members.get(user)) else {
        return Role::Guest;
    };
    if member.platform_role == PlatformRole::External {
        return Role::Guest;
    }
    let named = state.leads.contains(user);
    let scoped = state.scoped_leads.iter().any(|s| s.leads_room(user, room));
    if named || scoped || state.profile.leads.admits(member.platform_role) {
        Role::Lead
    } else {
        Role::Member
    }
}

/// The best role `user` holds anywhere in the group, as a DM would see it.
pub fn group_role(state: &GroupState, user: &PlatformUserId) -> Role {
    if state.hosts.contains(user) {
        return Role::Host;
    }
    role_outside_dms(state, user)
}

fn role_outside_dms(state: &GroupState, user: &PlatformUserId) -> Role {
    let mut external = false;
    let mut best = Role::Guest;
    for (id, room) in state.rooms.iter().filter(|(_, r)| !r.is_dm()) {
        if let Some(member) = room.members.get(user) {
            external |= member.platform_role == PlatformRole::External;
            best = best.max(resolve_role(state, id, user));
        }
    }
    if !external && state.leads.contains(user) {
        best = best.max(Role::Lead);
    }
    best
}

/// What a request from someone with `role` may do directly.
pub fn permitted(role: Role) -> ActionSet {
    use ActionKind::{ChangeLeads, ChangeMemory, ChangeSettings, Mail, React, Say, Undo};
    match role {
        Role::Guest | Role::Member => ActionSet::of(&[Say, React]),
        Role::Lead | Role::Host => ActionSet::of(&[
            Say,
            React,
            ChangeLeads,
            ChangeSettings,
            ChangeMemory,
            Mail,
            Undo,
        ]),
    }
}

/// What happens to a request (DESIGN.md section 5, "Requests").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Do,
    /// Waits for a lead's ✅.
    Defer,
    /// Guests are answered, never obeyed.
    Decline,
}

pub fn dispose(role: Role, kind: ActionKind) -> Disposition {
    if permitted(role).allows(kind) {
        Disposition::Do
    } else if role == Role::Guest {
        Disposition::Decline
    } else {
        Disposition::Defer
    }
}

/// A plan's actions split by whether the asker may have them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilteredPlan {
    pub allowed: Vec<PlannedAction>,
    pub deferred: Vec<PlannedAction>,
}

/// Splits planned actions against `allowed`. Anything outside it becomes a
/// deferral rather than an error, so injected text cannot escalate.
pub fn filter_actions(actions: Vec<PlannedAction>, allowed: &ActionSet) -> FilteredPlan {
    let mut out = FilteredPlan::default();
    for action in actions {
        match action.kind() {
            None => {}
            Some(kind) if allowed.allows(kind) => out.allowed.push(action),
            Some(_) => out.deferred.push(action),
        }
    }
    out
}

impl PlannedAction {
    pub fn kind(&self) -> Option<ActionKind> {
        match self {
            PlannedAction::Say { .. } => Some(ActionKind::Say),
            PlannedAction::React { .. } => Some(ActionKind::React),
            PlannedAction::Mail { .. } => Some(ActionKind::Mail),
            PlannedAction::Nothing => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_outside_the_set_are_deferred() {
        let plan = vec![
            PlannedAction::Say { text: "hi".into() },
            PlannedAction::React {
                emoji: "👍".into()
            },
            PlannedAction::Nothing,
        ];
        let out = filter_actions(plan, &ActionSet::of(&[ActionKind::React]));
        assert_eq!(
            out.allowed,
            vec![PlannedAction::React {
                emoji: "👍".into()
            }]
        );
        assert_eq!(out.deferred, vec![PlannedAction::Say { text: "hi".into() }]);

        let none = filter_actions(
            vec![PlannedAction::Say { text: "x".into() }],
            &ActionSet::empty(),
        );
        assert!(none.allowed.is_empty());
        assert_eq!(none.deferred.len(), 1);
    }

    #[test]
    fn requests_are_done_deferred_or_declined_by_role() {
        assert_eq!(
            dispose(Role::Lead, ActionKind::ChangeLeads),
            Disposition::Do
        );
        assert_eq!(
            dispose(Role::Host, ActionKind::ChangeLeads),
            Disposition::Do
        );
        assert_eq!(
            dispose(Role::Member, ActionKind::ChangeLeads),
            Disposition::Defer
        );
        assert_eq!(
            dispose(Role::Guest, ActionKind::ChangeLeads),
            Disposition::Decline
        );
        assert_eq!(dispose(Role::Guest, ActionKind::Say), Disposition::Do);
        assert_eq!(dispose(Role::Lead, ActionKind::Mail), Disposition::Do);
        assert_eq!(dispose(Role::Member, ActionKind::Mail), Disposition::Defer);
        assert_eq!(dispose(Role::Guest, ActionKind::Mail), Disposition::Decline);
    }
}
