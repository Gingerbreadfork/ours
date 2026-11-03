//! The two patterns every profile keeps (DESIGN.md sections 5 and 12): the
//! hour each member usually posts and when each room is busy, as counts in
//! the group's zone, halved every seven days. They only time when the agent
//! speaks; they are never facts, never recalled and never reported.

use crate::state::GroupState;
use crate::zone;
use ours_types::{Instant, PlatformRoomId, PlatformUserId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Counts halve once per this many seconds.
pub const HALF_LIFE_SECONDS: i64 = 7 * 24 * 60 * 60;
/// Messages a member needs before they have a usual hour.
pub const USUAL_HOUR_MIN: u32 = 3;
/// Messages an hour of the week needs before the room counts as active then.
pub const ROOM_ACTIVE_MIN: u32 = 2;
const DAY_HOURS: usize = 24;
const WEEK_HOURS: usize = 7 * 24;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Patterns {
    /// When the counts were last halved; set by the first message counted.
    #[serde(default)]
    pub halved: Option<Instant>,
    /// Messages per hour of the day, 24 counts per member.
    #[serde(default)]
    pub members: BTreeMap<PlatformUserId, Vec<u32>>,
    /// Messages per hour of the week from Monday 00:00, 168 counts per room.
    #[serde(default)]
    pub rooms: BTreeMap<PlatformRoomId, Vec<u32>>,
}

impl Patterns {
    /// Halves every count once per half-life passed since the last halving.
    pub(crate) fn decay(&mut self, now: Instant) {
        let Some(last) = self.halved else {
            return;
        };
        let periods = now.seconds_since(last) / HALF_LIFE_SECONDS;
        if periods <= 0 {
            return;
        }
        let shift = u32::try_from(periods).unwrap_or(u32::MAX);
        for counts in self.members.values_mut().chain(self.rooms.values_mut()) {
            for n in counts.iter_mut() {
                *n = n.checked_shr(shift).unwrap_or(0);
            }
        }
        self.members.retain(|_, c| c.iter().any(|n| *n > 0));
        self.rooms.retain(|_, c| c.iter().any(|n| *n > 0));
        self.halved = Some(last.plus_seconds(periods * HALF_LIFE_SECONDS));
    }

    pub(crate) fn forget(&mut self, user: &PlatformUserId) {
        self.members.remove(user);
    }
}

/// Counts one message from `user` in `room`, sent at `at`. DMs count toward
/// the person's hour but not toward any room's week.
pub(crate) fn record(
    state: &mut GroupState,
    room: &PlatformRoomId,
    user: &PlatformUserId,
    at: Instant,
    now: Instant,
) {
    let local = zone::local(at, &state.timezone);
    let hour = usize::try_from(local.hour()).unwrap_or(0) % DAY_HOURS;
    let weekday = usize::try_from(local.weekday().to_monday_zero_offset()).unwrap_or(0) % 7;
    let in_dm = state.rooms.get(room).is_some_and(|r| r.is_dm());
    let patterns = &mut state.patterns;
    patterns.halved.get_or_insert(now);
    bump(
        patterns.members.entry(user.clone()).or_default(),
        DAY_HOURS,
        hour,
    );
    if !in_dm {
        bump(
            patterns.rooms.entry(room.clone()).or_default(),
            WEEK_HOURS,
            weekday * DAY_HOURS + hour,
        );
    }
}

fn bump(counts: &mut Vec<u32>, len: usize, at: usize) {
    counts.resize(len, 0);
    if let Some(n) = counts.get_mut(at) {
        *n = n.saturating_add(1);
    }
}

/// The hour of the day, in the group's zone, at which `user` posts most,
/// once there are enough messages to say.
pub fn usual_hour(state: &GroupState, user: &PlatformUserId) -> Option<u8> {
    let counts = state.patterns.members.get(user)?;
    if counts.iter().sum::<u32>() < USUAL_HOUR_MIN {
        return None;
    }
    let (hour, _) = counts.iter().enumerate().fold(
        (0, 0),
        |best, (h, &n)| if n > best.1 { (h, n) } else { best },
    );
    u8::try_from(hour).ok()
}

/// Whether `room` is usually busy at `hour` on `weekday`, counted from
/// Monday as 1, in the group's zone. Busy means at least two messages then
/// and at least a quarter of the room's busiest hour.
pub fn room_active(state: &GroupState, room: &PlatformRoomId, weekday: u8, hour: u8) -> bool {
    if !(1..=7).contains(&weekday) || usize::from(hour) >= DAY_HOURS {
        return false;
    }
    let Some(counts) = state.patterns.rooms.get(room) else {
        return false;
    };
    let at = usize::from(weekday - 1) * DAY_HOURS + usize::from(hour);
    let n = counts.get(at).copied().unwrap_or(0);
    let peak = counts.iter().copied().max().unwrap_or(0);
    n >= ROOM_ACTIVE_MIN && n.saturating_mul(4) >= peak
}
