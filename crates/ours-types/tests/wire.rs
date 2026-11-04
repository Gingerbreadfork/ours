//! The journal stores `InboundEvent`s as JSON, so their wire form is fixed by
//! the fixtures here. A change that fails this test needs a migration for
//! existing journals, not a new fixture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_types::{
    Attachment, Capabilities, InboundEvent, Instant, Member, Message, Platform, PlatformRole, Room,
    RoomKind,
};
use std::collections::BTreeMap;

const FIXTURES: &str = include_str!("fixtures/inbound_events.json");

fn at(text: &str) -> Instant {
    Instant(text.parse().unwrap())
}

/// The fixture name for each variant. The match has no wildcard, so a new
/// variant does not compile until it has a fixture.
fn name(event: &InboundEvent) -> &'static str {
    match event {
        InboundEvent::Ready { .. } => "ready",
        InboundEvent::RoomSync { .. } => "room_sync",
        InboundEvent::Message(_) => "message",
        InboundEvent::MessageEdited(_) => "message_edited",
        InboundEvent::MessageDeleted { .. } => "message_deleted",
        InboundEvent::ReactionAdded { .. } => "reaction_added",
        InboundEvent::ReactionRemoved { .. } => "reaction_removed",
        InboundEvent::MemberJoined { .. } => "member_joined",
        InboundEvent::MemberLeft { .. } => "member_left",
        InboundEvent::MemberUpdated { .. } => "member_updated",
        InboundEvent::ActionResult { .. } => "action_result",
        InboundEvent::PollVote { .. } => "poll_vote",
        InboundEvent::PollEnded { .. } => "poll_ended",
        InboundEvent::History { .. } => "history",
        InboundEvent::Disconnected { .. } => "disconnected",
    }
}

fn message(id: &str, text: &str, when: &str) -> Message {
    Message {
        id: id.into(),
        room: "C1".into(),
        author: "U2".into(),
        text: text.to_owned(),
        at: at(when),
        reply_to: None,
        mentions: Vec::new(),
        mentions_me: false,
        attachments: Vec::new(),
        edited: false,
    }
}

fn member(id: &str, name: &str, role: PlatformRole, is_bot: bool) -> Member {
    Member {
        id: id.into(),
        display_name: name.to_owned(),
        platform_role: role,
        is_bot,
        timezone: None,
    }
}

fn events() -> Vec<InboundEvent> {
    vec![
        InboundEvent::Ready {
            platform: Platform::Discord,
            me: "U1".into(),
            me_name: "Noa".to_owned(),
            capabilities: Capabilities {
                reactions: true,
                threads: true,
                polls: true,
                dms: true,
                edit: true,
                delete: true,
                pin: true,
                history: true,
                timezones: false,
                external_flag: false,
                typing: true,
                reaction_set: Some(vec!["👍".to_owned(), "✅".to_owned()]),
            },
        },
        InboundEvent::RoomSync {
            room: Room {
                platform: Platform::Slack,
                id: "C1".into(),
                name: Some("general".to_owned()),
                kind: RoomKind::Thread,
                parent: Some("C0".into()),
                container: Some("T1".to_owned()),
            },
            members: vec![Member {
                timezone: Some("Europe/Lisbon".to_owned()),
                ..member("U2", "Sam", PlatformRole::External, false)
            }],
        },
        InboundEvent::Message(Message {
            reply_to: Some("M0".into()),
            mentions: vec!["U1".into()],
            mentions_me: true,
            attachments: vec![Attachment {
                kind: "image".to_owned(),
                url: Some("https://example.com/a.png".to_owned()),
                name: Some("a.png".to_owned()),
            }],
            ..message("M1", "hello", "2026-11-05T10:00:00Z")
        }),
        InboundEvent::MessageEdited(Message {
            edited: true,
            ..message("M1", "hello again", "2026-11-05T10:00:00Z")
        }),
        InboundEvent::MessageDeleted {
            room: "C1".into(),
            id: "M1".into(),
        },
        InboundEvent::ReactionAdded {
            room: "C1".into(),
            message: "M1".into(),
            user: "U2".into(),
            emoji: "✅".to_owned(),
            at: at("2026-11-05T10:01:00Z"),
        },
        InboundEvent::ReactionRemoved {
            room: "C1".into(),
            message: "M1".into(),
            user: "U2".into(),
            emoji: "✅".to_owned(),
            at: at("2026-11-05T10:02:00Z"),
        },
        InboundEvent::MemberJoined {
            room: "C1".into(),
            member: member("U3", "Jo", PlatformRole::Admin, false),
            at: at("2026-11-05T10:03:00Z"),
        },
        InboundEvent::MemberLeft {
            room: "C1".into(),
            user: "U3".into(),
            at: at("2026-11-05T10:04:00Z"),
        },
        InboundEvent::MemberUpdated {
            room: "C1".into(),
            member: member("U3", "Jo B", PlatformRole::Regular, true),
        },
        InboundEvent::ActionResult {
            id: "A1".into(),
            ok: false,
            message: Some("M2".into()),
            error: Some("rate limited".to_owned()),
        },
        InboundEvent::PollVote {
            room: "C1".into(),
            message: "M3".into(),
            user: "U2".into(),
            option: 1,
            added: true,
            at: at("2026-11-05T10:05:00Z"),
        },
        InboundEvent::PollEnded {
            room: "C1".into(),
            message: "M3".into(),
            counts: vec![2, 0, 1],
        },
        InboundEvent::History {
            room: "C1".into(),
            messages: vec![message("M0", "earlier", "2026-11-04T09:00:00Z")],
        },
        InboundEvent::Disconnected {
            platform: Platform::Telegram,
            reason: "token revoked".to_owned(),
        },
    ]
}

#[test]
fn every_inbound_event_matches_its_fixture() {
    let fixtures: BTreeMap<String, serde_json::Value> = serde_json::from_str(FIXTURES).unwrap();
    let events = events();
    let names: Vec<&str> = events.iter().map(name).collect();
    let mut expected: Vec<&str> = fixtures.keys().map(String::as_str).collect();
    expected.sort_unstable();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, expected, "one fixture per variant");

    for event in events {
        let fixture = &fixtures[name(&event)];
        assert_eq!(
            &serde_json::to_value(&event).unwrap(),
            fixture,
            "{} writes differently",
            name(&event)
        );
        let read: InboundEvent = serde_json::from_value(fixture.clone()).unwrap();
        assert_eq!(read, event, "{} reads differently", name(&event));
    }
}

#[test]
fn fields_added_later_default_when_an_old_record_lacks_them() {
    let old = serde_json::json!({
        "type": "ready",
        "platform": "slack",
        "me": "U1",
        "me_name": "Noa"
    });
    let read: InboundEvent = serde_json::from_value(old).unwrap();
    let InboundEvent::Ready { capabilities, .. } = read else {
        panic!("not a ready event");
    };
    assert_eq!(capabilities, Capabilities::default());
}
