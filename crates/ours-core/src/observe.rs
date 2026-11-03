//! Observe mode (DESIGN.md section 18, "Proofs"). For its first days in a
//! group the agent runs everything and does nothing unprompted in a room.
//! Each thing it would have done goes to the leads and hosts by DM instead,
//! and their 👍 and 👎 on those DMs are counted. Addressed replies are never
//! affected.

use crate::authority::resolve_role;
use crate::effect::{Effect, send_dm};
use crate::reactions::{self, Meaning};
use crate::state::GroupState;
use crate::timers::{self, TimerPurpose};
use crate::trace::Trace;
use ours_types::{Instant, PlatformMessageId, PlatformRoomId, PlatformUserId, Role};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const DAY_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Observe {
    /// When leads and hosts hear what it saw.
    pub until: Instant,
    pub started: Instant,
    /// Times it would have done something unprompted.
    #[serde(default)]
    pub would_have: u32,
    #[serde(default)]
    pub up: u32,
    #[serde(default)]
    pub down: u32,
    /// "Would have" DMs still open for a 👍 or 👎, with whom each went to.
    #[serde(default)]
    pub targets: BTreeMap<PlatformMessageId, PlatformUserId>,
}

impl Observe {
    fn new(now: Instant, seconds: i64) -> Observe {
        Observe {
            until: now.plus_seconds(seconds),
            started: now,
            would_have: 0,
            up: 0,
            down: 0,
            targets: BTreeMap::new(),
        }
    }
}

/// Starts watching for `seconds`, as a new group does on its first sync.
pub(crate) fn start(state: &mut GroupState, seconds: u32, now: Instant, fx: &mut Vec<Effect>) {
    let seconds = i64::from(seconds);
    let observe = Observe::new(now, seconds);
    let until = observe.until;
    state.observe = Some(observe);
    timers::arm(state, fx, until, TimerPurpose::ObserveEnd);
    fx.push(Effect::Notice(format!(
        "Observing for {}: nothing unprompted until a lead or host says go.",
        span(seconds)
    )));
}

/// "observe" or "watch N more days": watch again, keeping the counts if
/// already watching.
pub(crate) fn watch(
    state: &mut GroupState,
    days: Option<u32>,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    let seconds = match days {
        Some(days) => i64::from(days) * DAY_SECONDS,
        None if state.profile.observe_seconds > 0 => i64::from(state.profile.observe_seconds),
        None => DAY_SECONDS,
    };
    timers::cancel_where(state, fx, |p| matches!(p, TimerPurpose::ObserveEnd));
    let until = now.plus_seconds(seconds);
    match state.observe.as_mut() {
        Some(observe) => observe.until = until,
        None => state.observe = Some(Observe::new(now, seconds)),
    }
    timers::arm(state, fx, until, TimerPurpose::ObserveEnd);
    format!(
        "Watching for {}; nothing unprompted until a lead says go.",
        span(seconds)
    )
}

/// "go": stop watching and start speaking unprompted.
pub(crate) fn go(state: &mut GroupState, _now: Instant, fx: &mut Vec<Effect>) -> String {
    let Some(observe) = state.observe.take() else {
        return "Not watching; I already speak up when it helps.".to_owned();
    };
    timers::cancel_where(state, fx, |p| matches!(p, TimerPurpose::ObserveEnd));
    let quiet: Vec<PlatformMessageId> = state
        .hands
        .iter()
        .filter(|(_, h)| h.observed)
        .map(|(id, _)| id.clone())
        .collect();
    for id in quiet {
        if let Some(hand) = state.hands.remove(&id) {
            timers::cancel(state, fx, &hand.timer);
        }
    }
    state.observed = Some(observe);
    "Done watching. I'll speak up when it helps, within the budget.".to_owned()
}

/// The period is over: tell leads and hosts once what it would have done.
/// It keeps watching until one of them says go.
pub(crate) fn end(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    let Some(observe) = &state.observe else {
        return;
    };
    let days = ((now.seconds_since(observe.started) + DAY_SECONDS / 2) / DAY_SECONDS).max(1);
    let (would_have, ups, downs) = (observe.would_have, observe.up, observe.down);
    let trace = Trace::new(
        "the end of the observe period",
        "observe mode asks once whether to start speaking",
        now,
    );
    let people = observers(state);
    for user in &people {
        let up = reactions::emoji_for_user(state, user, Meaning::ThumbsUp);
        let down = reactions::emoji_for_user(state, user, Meaning::ThumbsDown);
        let text = format!(
            "Watched for {days} {}: {would_have} {} I would have spoken, {ups} {up}, {downs} {down}. Say go and I'll start speaking unprompted, or say observe to watch longer.",
            plural(days, "day", "days"),
            plural(i64::from(would_have), "time", "times"),
        );
        send_dm(state, fx, user, text, now, trace.clone(), false);
    }
    fx.push(Effect::Notice(format!(
        "Observe period over; asked {} whether to go.",
        people.len()
    )));
}

/// DMs leads and hosts one "would have" line and counts it.
pub(crate) fn report(state: &mut GroupState, fx: &mut Vec<Effect>, text: &str, trace: Trace) {
    let now = trace.at;
    for user in observers(state) {
        send_dm(state, fx, &user, text, now, trace.clone(), true);
    }
    if let Some(observe) = state.observe.as_mut() {
        observe.would_have += 1;
    }
}

/// Remembers a "would have" DM as open for feedback once its id is known.
pub(crate) fn remember_target(
    state: &mut GroupState,
    message: PlatformMessageId,
    user: PlatformUserId,
) {
    if let Some(observe) = state.observe.as_mut() {
        observe.targets.insert(message, user);
    }
}

/// Counts a 👍 or 👎 from the person a "would have" DM went to. Returns
/// whether the reaction was feedback.
pub(crate) fn on_reaction(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    fx: &mut Vec<Effect>,
) -> bool {
    let up = reactions::means(state, room, emoji, Meaning::ThumbsUp);
    if !up && !reactions::means(state, room, emoji, Meaning::ThumbsDown) {
        return false;
    }
    let name = state.name_of(user);
    let Some(observe) = state.observe.as_mut().or(state.observed.as_mut()) else {
        return false;
    };
    if observe.targets.get(message) != Some(user) {
        return false;
    }
    observe.targets.remove(message);
    if up {
        observe.up += 1;
    } else {
        observe.down += 1;
    }
    let (ups, downs) = (observe.up, observe.down);
    let thumb = if up { "👍" } else { "👎" };
    fx.push(Effect::Notice(format!(
        "{name} gave a would-have line {thumb}; {ups} 👍 and {downs} 👎 so far."
    )));
    true
}

/// Leads and hosts, deduplicated, humans only.
pub fn observers(state: &GroupState) -> Vec<PlatformUserId> {
    let mut people = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for (id, room) in state.rooms.iter().filter(|(_, r)| !r.is_dm()) {
        for member in room.members.values() {
            seen.insert(&member.id);
            if member.is_bot || state.is_me(&member.id) {
                continue;
            }
            if resolve_role(state, id, &member.id) >= Role::Lead {
                people.insert(member.id.clone());
            }
        }
    }
    let unseen_named = state
        .leads
        .iter()
        .filter(|u| !seen.contains(u) && !state.is_me(u));
    people.extend(unseen_named.cloned());
    let hosts = state.hosts.iter().filter(|u| {
        !state.is_me(u)
            && !state
                .rooms
                .values()
                .any(|r| r.members.get(*u).is_some_and(|m| m.is_bot))
    });
    people.extend(hosts.cloned());
    people.into_iter().collect()
}

/// "1 day", "3 days", "12 hours".
pub fn span(seconds: i64) -> String {
    if seconds >= DAY_SECONDS && seconds % DAY_SECONDS == 0 {
        let days = seconds / DAY_SECONDS;
        format!("{days} {}", plural(days, "day", "days"))
    } else {
        let hours = (seconds / 3600).max(1);
        format!("{hours} {}", plural(hours, "hour", "hours"))
    }
}

fn plural(n: i64, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbs_and_spans() {
        let up = |e| reactions::is_canonical(e, Meaning::ThumbsUp);
        let down = |e| reactions::is_canonical(e, Meaning::ThumbsDown);
        assert!(up("👍") && up("👍🏽") && up(":+1:") && !up("👎"));
        assert!(down("👎") && !down("✅"));
        assert_eq!(span(86400), "1 day");
        assert_eq!(span(3 * 86400), "3 days");
        assert_eq!(span(7200), "2 hours");
    }
}
