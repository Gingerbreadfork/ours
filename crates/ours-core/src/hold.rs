//! The hold queue (DESIGN.md section 13, tier 1). An action that leaves the
//! group waits out the profile's window so "undo" can still stop it, then
//! goes out as `Effect::Outside`. An irreversible action instead waits for
//! a ✅ from the one person whose call it is, and lapses after a day.

use crate::action::{self, ActionStatus, Origin, Recording};
use crate::authority::resolve_role;
use crate::effect::{Effect, send_dm_as};
use crate::state::GroupState;
use crate::timers::{self, TimerPurpose};
use crate::trace::Trace;
use ours_types::{
    ActionId, Instant, OutsideAction, PlatformMessageId, PlatformRoomId, PlatformUserId, Role,
    Tier, TimerId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// How long a request for a ✅ on an irreversible action stays open.
pub const ASK_SECONDS: i64 = 24 * 60 * 60;

/// An action waiting to leave the group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hold {
    pub action: OutsideAction,
    pub release_at: Instant,
    pub requested_by: Option<PlatformUserId>,
    pub approved_by: Option<PlatformUserId>,
    pub room: PlatformRoomId,
    /// The line in the room that says it is waiting.
    pub notice: Option<PlatformMessageId>,
    /// For an irreversible action, the one person whose ✅ releases it.
    #[serde(default)]
    pub ask: Option<PlatformUserId>,
    /// The DM that asked them.
    #[serde(default)]
    pub asked: Option<PlatformMessageId>,
    pub timer: TimerId,
}

/// How an action of some tier proceeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Now,
    Hold {
        until: Instant,
    },
    /// Ask this person by DM; it lapses at `until`.
    Ask {
        who: PlatformUserId,
        until: Instant,
    },
    /// Irreversible, and nobody here may say yes to it.
    Nobody,
}

/// Free actions go now, windowed ones wait out the profile's window, and an
/// irreversible one asks the lead who approved or gave the instruction, or
/// else the host.
pub fn route(
    state: &GroupState,
    room: &PlatformRoomId,
    tier: Tier,
    origin: &Origin,
    now: Instant,
) -> Route {
    match tier {
        Tier::Free => Route::Now,
        Tier::Windowed => Route::Hold {
            until: now.plus_seconds(i64::from(state.profile.hold_window_seconds)),
        },
        Tier::Irreversible => {
            let lead = |user: &Option<PlatformUserId>| {
                user.clone()
                    .filter(|u| resolve_role(state, room, u) >= Role::Lead)
            };
            let who = lead(&origin.approved_by)
                .or_else(|| lead(&origin.requested_by))
                .or_else(|| state.hosts.iter().next().cloned());
            match who {
                Some(who) => Route::Ask {
                    who,
                    until: now.plus_seconds(ASK_SECONDS),
                },
                None => Route::Nobody,
            }
        }
    }
}

/// What to place in the queue.
pub(crate) struct Placing<'a> {
    pub id: &'a ActionId,
    pub outside: OutsideAction,
    pub origin: &'a Origin,
    pub room: &'a PlatformRoomId,
    /// The DM line for an irreversible action.
    pub ask_text: String,
}

/// Holds, asks for, or releases an action as its route says.
pub(crate) fn place(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    placing: Placing<'_>,
    route: &Route,
    now: Instant,
    trace: Trace,
) {
    let (until, ask) = match route {
        Route::Now => {
            mark_sending(state, placing.id, now);
            fx.push(Effect::Outside {
                action: placing.id.clone(),
                outside: placing.outside,
            });
            return;
        }
        Route::Nobody => return,
        Route::Hold { until } => (*until, None),
        Route::Ask { who, until } => (*until, Some(who.clone())),
    };
    let purpose = TimerPurpose::Release {
        action: placing.id.clone(),
    };
    let timer = timers::arm(state, fx, until, purpose);
    state.holds.insert(
        placing.id.clone(),
        Hold {
            action: placing.outside,
            release_at: until,
            requested_by: placing.origin.requested_by.clone(),
            approved_by: placing.origin.approved_by.clone(),
            room: placing.room.clone(),
            notice: None,
            ask: ask.clone(),
            asked: None,
            timer,
        },
    );
    if let Some(who) = ask {
        let record = Recording::Line {
            action: placing.id.clone(),
        };
        send_dm_as(state, fx, &who, placing.ask_text, now, trace, record);
    }
}

fn mark_sending(state: &mut GroupState, id: &ActionId, now: Instant) {
    if let Some(record) = action::find_mut(state, id) {
        record.status = ActionStatus::Sending { since: now };
    }
}

/// Sends a held action now. Returns whether one was held.
pub(crate) fn release(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    id: &ActionId,
    now: Instant,
) -> bool {
    let Some(hold) = state.holds.remove(id) else {
        return false;
    };
    timers::cancel(state, fx, &hold.timer);
    mark_sending(state, id, now);
    let summary = action::find(state, id).map_or_else(|| id.to_string(), |r| r.summary.clone());
    fx.push(Effect::Outside {
        action: id.clone(),
        outside: hold.action,
    });
    fx.push(Effect::Notice(format!("Released {summary}.")));
    true
}

/// Takes a held action out of the queue before it goes.
pub(crate) fn cancel(state: &mut GroupState, fx: &mut Vec<Effect>, id: &ActionId) -> Option<Hold> {
    let hold = state.holds.remove(id)?;
    timers::cancel(state, fx, &hold.timer);
    Some(hold)
}

/// The window is over: release it, or let an unanswered ask lapse.
pub(crate) fn on_timer(state: &mut GroupState, fx: &mut Vec<Effect>, id: &ActionId, now: Instant) {
    let Some(hold) = state.holds.get(id) else {
        debug!(action = %id, "nothing held under this action");
        return;
    };
    if hold.ask.is_none() {
        release(state, fx, id, now);
        return;
    }
    state.holds.remove(id);
    let summary = match action::find_mut(state, id) {
        Some(record) => {
            record.status = ActionStatus::Declined;
            record.outcome = Some("lapsed without a ✅".to_owned());
            record.summary.clone()
        }
        None => id.to_string(),
    };
    fx.push(Effect::Notice(format!("Lapsed without a ✅: {summary}.")));
}

/// A ✅ on the DM that asked for one, from the person asked. Returns
/// whether it released something.
pub(crate) fn on_dm_check(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    now: Instant,
) -> bool {
    let found = state
        .holds
        .iter()
        .find(|(_, h)| h.asked.as_ref() == Some(message) && h.ask.as_ref() == Some(user))
        .map(|(id, _)| id.clone());
    match found {
        Some(id) => release(state, fx, &id, now),
        None => false,
    }
}

/// "10m", "2m", "1h", "45s".
pub fn window(seconds: i64) -> String {
    match seconds {
        s if s < 60 => format!("{}s", s.max(0)),
        s if s < 3600 => format!("{}m", (s + 59) / 60),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s => format!("{}h{}m", s / 3600, (s % 3600 + 59) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_types::{GroupId, Kind};

    #[test]
    fn windows_read_short() {
        assert_eq!(window(600), "10m");
        assert_eq!(window(120), "2m");
        assert_eq!(window(300), "5m");
        assert_eq!(window(45), "45s");
        assert_eq!(window(3600), "1h");
        assert_eq!(window(5400), "1h30m");
    }

    #[test]
    fn an_irreversible_action_waits_for_one_persons_check_or_lapses() {
        use crate::action::{ActionClass, ActionRecord, Undo, attach, find, push};
        use ours_types::{OutboundAction, OutgoingMail};
        let mut state = GroupState::new(GroupId::new("g"), Kind::Team);
        state.hosts.insert("host".into());
        let room = PlatformRoomId::new("r");
        let now = Instant::UNIX_EPOCH;
        let origin = Origin::default();
        let route = route(&state, &room, Tier::Irreversible, &origin, now);
        let mut fx = Vec::new();
        for n in ["a1", "a2"] {
            let id = ActionId::new(n);
            let record = ActionRecord::new(
                id.clone(),
                ActionClass::Mail,
                room.clone(),
                now,
                "the deposit",
                Undo::CancelHold,
            );
            push(&mut state, record);
            let outside = OutsideAction::SendMail(OutgoingMail {
                to: "x@y.com".into(),
                subject: "s".into(),
                body: "b".into(),
                in_reply_to: None,
            });
            let placing = Placing {
                id: &id,
                outside,
                origin: &origin,
                room: &room,
                ask_text: "React ✅ and I will.".into(),
            };
            place(
                &mut state,
                &mut fx,
                placing,
                &route,
                now,
                Trace::new("t", "r", now),
            );
        }
        let asked = fx
            .iter()
            .filter(|e| {
                matches!(e, Effect::Send(o) if matches!(&o.action,
                    OutboundAction::SendDm { user, .. } if user.as_str() == "host"))
            })
            .count();
        assert_eq!(asked, 2);
        assert!(!fx.iter().any(|e| matches!(e, Effect::Outside { .. })));

        let first = ActionId::new("a1");
        attach(&mut state, &first, &"dm1".into(), true);
        assert_eq!(state.holds[&first].asked, Some("dm1".into()));
        let mut fx = Vec::new();
        assert!(!on_dm_check(
            &mut state,
            &mut fx,
            &"dm1".into(),
            &"other".into(),
            now
        ));
        assert!(on_dm_check(
            &mut state,
            &mut fx,
            &"dm1".into(),
            &"host".into(),
            now
        ));
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Outside { action, .. } if action == &first))
        );

        let second = ActionId::new("a2");
        on_timer(&mut state, &mut fx, &second, now.plus_seconds(ASK_SECONDS));
        assert_eq!(
            find(&state, &second).unwrap().status,
            ActionStatus::Declined
        );
        assert!(state.holds.is_empty());
    }

    #[test]
    fn each_tier_routes_its_own_way() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Team);
        let now = Instant::UNIX_EPOCH;
        let room = PlatformRoomId::new("r");
        let origin = Origin::default();
        assert_eq!(route(&state, &room, Tier::Free, &origin, now), Route::Now);
        assert_eq!(
            route(&state, &room, Tier::Windowed, &origin, now),
            Route::Hold {
                until: now.plus_seconds(600)
            }
        );
        // With nobody who may say yes, an irreversible action cannot go.
        assert_eq!(
            route(&state, &room, Tier::Irreversible, &origin, now),
            Route::Nobody
        );
        state.hosts.insert("host".into());
        assert_eq!(
            route(&state, &room, Tier::Irreversible, &origin, now),
            Route::Ask {
                who: "host".into(),
                until: now.plus_seconds(ASK_SECONDS)
            }
        );
    }
}
