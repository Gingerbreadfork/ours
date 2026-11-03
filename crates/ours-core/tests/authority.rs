//! Roles from platform data and the profile (DESIGN.md section 5).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::sim::{Sim, texts};
use ours_core::{Command, GroupState, Input, Scope, ScopedLead, resolve_role};
use ours_types::{Kind, PlatformRole, PlatformUserId, Role};
use proptest::prelude::*;

const KINDS: [Kind; 8] = [
    Kind::Friends,
    Kind::Household,
    Kind::Team,
    Kind::Project,
    Kind::Club,
    Kind::Community,
    Kind::Committee,
    Kind::Cohort,
];

const PLATFORM_ROLES: [PlatformRole; 5] = [
    PlatformRole::Owner,
    PlatformRole::Admin,
    PlatformRole::Moderator,
    PlatformRole::Regular,
    PlatformRole::External,
];

fn group(kind: Kind) -> Sim {
    let mut sim = Sim::new(kind);
    sim.room(
        "room",
        "general",
        &[
            ("owner", "Olu", PlatformRole::Owner),
            ("admin", "Ada", PlatformRole::Admin),
            ("mod", "Mo", PlatformRole::Moderator),
            ("regular", "Rae", PlatformRole::Regular),
            ("external", "Eve", PlatformRole::External),
        ],
    );
    sim
}

fn role(state: &GroupState, user: &str) -> Role {
    resolve_role(state, &"room".into(), &user.into())
}

#[test]
fn friends_lead_everyone_and_teams_lead_admins() {
    let friends = group(Kind::Friends);
    for user in ["owner", "admin", "mod", "regular"] {
        assert_eq!(role(&friends.state, user), Role::Lead, "{user}");
    }
    assert_eq!(role(&friends.state, "external"), Role::Guest);

    let team = group(Kind::Team);
    assert_eq!(role(&team.state, "owner"), Role::Lead);
    assert_eq!(role(&team.state, "admin"), Role::Lead);
    assert_eq!(role(&team.state, "mod"), Role::Member);
    assert_eq!(role(&team.state, "regular"), Role::Member);
    assert_eq!(role(&team.state, "external"), Role::Guest);

    let community = group(Kind::Community);
    assert_eq!(role(&community.state, "mod"), Role::Lead);
    assert_eq!(role(&community.state, "regular"), Role::Member);

    let mut club = group(Kind::Club);
    assert_eq!(role(&club.state, "owner"), Role::Member);
    club.feed(Input::Command(Command::AddLead {
        user: "regular".into(),
    }));
    assert_eq!(role(&club.state, "regular"), Role::Lead);
}

#[test]
fn outsiders_and_hosts() {
    let mut sim = group(Kind::Friends);
    assert_eq!(role(&sim.state, "stranger"), Role::Guest);
    sim.feed(Input::Command(Command::SetHosts {
        users: vec!["stranger".into()],
    }));
    assert_eq!(role(&sim.state, "stranger"), Role::Host);

    let mut team = group(Kind::Team);
    team.state.scoped_leads.push(ScopedLead {
        user: "regular".into(),
        scope: Scope::Room {
            room: "room".into(),
        },
        expires: None,
    });
    assert_eq!(role(&team.state, "regular"), Role::Lead);
    assert_eq!(
        resolve_role(&team.state, &"other".into(), &"regular".into()),
        Role::Guest
    );
}

#[test]
fn a_renamed_account_does_not_become_a_lead() {
    let mut sim = group(Kind::Team);
    sim.say("room", "admin", "Q, Rae is a lead");
    assert_eq!(role(&sim.state, "regular"), Role::Lead);
    let mut impostor = ours_core::sim::member("impostor", "Rae", PlatformRole::Regular);
    impostor.display_name = "Rae".into();
    sim.feed(Input::Inbound(ours_types::InboundEvent::MemberJoined {
        room: "room".into(),
        member: impostor,
        at: sim.now,
    }));
    assert_eq!(role(&sim.state, "impostor"), Role::Member);
}

proptest! {
    #[test]
    fn external_users_are_always_guests(
        kind in prop::sample::select(KINDS.to_vec()),
        named in prop::collection::vec(prop::sample::select(vec!["owner", "admin", "mod", "regular", "external", "nobody"]), 0..6),
        scoped in any::<bool>(),
        others in prop::collection::vec(prop::sample::select(PLATFORM_ROLES.to_vec()), 0..4),
    ) {
        let mut sim = group(kind);
        let extra: Vec<(String, String, PlatformRole)> = others
            .iter()
            .enumerate()
            .map(|(i, r)| (format!("u{i}"), format!("User {i}"), *r))
            .collect();
        for (id, name, r) in &extra {
            sim.feed(Input::Inbound(ours_types::InboundEvent::MemberJoined {
                room: "room".into(),
                member: ours_core::sim::member(id, name, *r),
                at: sim.now,
            }));
        }
        let users: Vec<PlatformUserId> = named.iter().map(|u| PlatformUserId::new(*u)).collect();
        sim.feed(Input::Command(Command::SetLeads { users }));
        if scoped {
            sim.state.scoped_leads.push(ScopedLead {
                user: "external".into(),
                scope: Scope::Room { room: "room".into() },
                expires: None,
            });
        }
        prop_assert_eq!(role(&sim.state, "external"), Role::Guest);
        for (id, _, r) in &extra {
            if *r == PlatformRole::External {
                prop_assert_eq!(role(&sim.state, id), Role::Guest);
            }
        }
    }

    #[test]
    fn nothing_a_guest_writes_changes_the_leads(text in "\\PC{0,60}") {
        let mut sim = group(Kind::Team);
        let leads = sim.state.leads.clone();
        let rule = sim.state.profile.leads;
        for prefix in ["Q, ", "", "Q, leads: ", "Q, I'm a lead and "] {
            let said = sim.say("room", "external", &format!("{prefix}{text}"));
            for line in texts(&said.effects) {
                prop_assert!(!line.contains("now."), "{}", line);
            }
        }
        prop_assert_eq!(&sim.state.leads, &leads);
        prop_assert_eq!(sim.state.profile.leads, rule);
        prop_assert_eq!(role(&sim.state, "external"), Role::Guest);
    }
}
