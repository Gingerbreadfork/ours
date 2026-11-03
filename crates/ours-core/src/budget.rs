//! The interjection budget, the day's chime-ins and the right moment to
//! speak unprompted (DESIGN.md sections 7, 12 and 16). Answers to direct
//! questions never count.

use crate::state::GroupState;
use crate::zone;
use ours_types::Instant;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const HOUR_SECONDS: i64 = 60 * 60;
/// Long enough to cover any calendar day in any zone.
const KEPT_SECONDS: i64 = 48 * HOUR_SECONDS;

/// When the agent spoke unprompted, oldest first, and any cut to today's
/// allowance.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Budget {
    pub spent: VecDeque<Instant>,
    #[serde(default)]
    pub cut: Option<BudgetCut>,
    /// When it chimed in, oldest first. Each also counts in `spent`.
    #[serde(default)]
    pub chimes: VecDeque<Instant>,
}

/// Today's allowance after a 🤫 or a "shut up". It lapses at local midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BudgetCut {
    /// The local date in the group's zone, as 2026-11-02.
    pub day: String,
    pub allowance: u32,
}

impl Budget {
    /// Unprompted messages in the rolling hour before `now`.
    pub fn this_hour(&self, now: Instant) -> u32 {
        let n = self
            .spent
            .iter()
            .filter(|t| (0..HOUR_SECONDS).contains(&now.seconds_since(**t)))
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// Unprompted messages on `now`'s calendar day in the named zone.
    pub fn today(&self, now: Instant, zone_name: &str) -> u32 {
        on_the_day(&self.spent, now, zone_name)
    }

    /// Chime-ins on `now`'s calendar day in the named zone.
    pub fn chimes_today(&self, now: Instant, zone_name: &str) -> u32 {
        on_the_day(&self.chimes, now, zone_name)
    }

    /// Whether a 🤫 or a "shut up" cut today's budget, which also ends
    /// chime-ins until local midnight.
    pub fn shushed_today(&self, now: Instant, zone_name: &str) -> bool {
        self.cut
            .as_ref()
            .is_some_and(|cut| cut.day == local_day(now, zone_name))
    }

    /// What may be spent on `now`'s local day, with any cut applied.
    pub fn allowance_today(&self, per_day: u32, now: Instant, zone_name: &str) -> u32 {
        match &self.cut {
            Some(cut) if cut.day == local_day(now, zone_name) => cut.allowance.min(per_day),
            _ => per_day,
        }
    }

    pub fn left_today(&self, per_day: u32, now: Instant, zone_name: &str) -> u32 {
        self.allowance_today(per_day, now, zone_name)
            .saturating_sub(self.today(now, zone_name))
    }

    pub(crate) fn prune(&mut self, now: Instant) {
        self.spent.retain(|t| now.seconds_since(*t) < KEPT_SECONDS);
        self.chimes.retain(|t| now.seconds_since(*t) < KEPT_SECONDS);
    }
}

fn on_the_day(times: &VecDeque<Instant>, now: Instant, zone_name: &str) -> u32 {
    let tz = zone::or_utc(zone_name);
    let today = now.0.to_zoned(tz.clone()).date();
    let n = times
        .iter()
        .filter(|t| t.0.to_zoned(tz.clone()).date() == today)
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn local_day(now: Instant, zone_name: &str) -> String {
    zone::local(now, zone_name).date().to_string()
}

/// Whether an unprompted message may go out now: outside quiet hours, inside
/// working hours where the profile has them, and within both budgets.
pub fn can_speak_unprompted(state: &GroupState, now: Instant) -> bool {
    let profile = &state.profile;
    let local = zone::local(now, &state.timezone);
    let hour = u8::try_from(local.hour()).unwrap_or(0);
    if profile.quiet_hours.contains(hour) {
        return false;
    }
    if let Some(hours) = profile.working_hours {
        let weekday = u8::try_from(local.weekday().to_monday_one_offset()).unwrap_or(1);
        if !hours.contains(weekday, hour) {
            return false;
        }
    }
    state.budget.this_hour(now) < profile.budget_per_hour
        && state
            .budget
            .left_today(profile.budget_per_day, now, &state.timezone)
            > 0
}

/// Counts an unprompted room action against the budget.
pub fn record_unprompted(state: &mut GroupState, now: Instant) {
    state.budget.spent.push_back(now);
}

/// Counts a chime-in against the budget and the day's chimes.
pub fn record_chime(state: &mut GroupState, now: Instant) {
    record_unprompted(state, now);
    state.budget.chimes.push_back(now);
}

/// Ends unprompted speech and chime-ins until local midnight (DESIGN.md
/// section 16): today's allowance becomes what was already spent.
pub fn stop_today(state: &mut GroupState, now: Instant) {
    let zone_name = state.timezone.clone();
    let budget = &mut state.budget;
    budget.cut = Some(BudgetCut {
        day: local_day(now, &zone_name),
        allowance: budget.today(now, &zone_name),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_types::{GroupId, Kind};

    fn at(rfc3339: &str) -> Instant {
        Instant(rfc3339.parse().unwrap())
    }

    #[test]
    fn friends_get_three_an_hour() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let noon = at("2026-11-02T12:00:00Z");
        for _ in 0..3 {
            assert!(can_speak_unprompted(&state, noon));
            record_unprompted(&mut state, noon);
        }
        assert!(!can_speak_unprompted(&state, noon));
        assert!(can_speak_unprompted(
            &state,
            noon.plus_seconds(HOUR_SECONDS)
        ));
    }

    #[test]
    fn the_day_budget_holds_across_hours() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let start = at("2026-11-02T09:00:00Z");
        for h in 0..10 {
            let t = start.plus_seconds(h * HOUR_SECONDS / 4);
            record_unprompted(&mut state, t);
        }
        assert_eq!(state.budget.today(start, "UTC"), 10);
        assert!(!can_speak_unprompted(&state, at("2026-11-02T20:00:00Z")));
        assert!(can_speak_unprompted(&state, at("2026-11-03T09:00:00Z")));
    }

    #[test]
    fn quiet_hours_are_in_the_groups_zone() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let t = at("2026-11-02T23:30:00Z");
        assert!(!can_speak_unprompted(&state, t));
        state.timezone = "America/New_York".into();
        assert!(can_speak_unprompted(&state, t));
    }

    #[test]
    fn a_shush_stops_it_until_midnight() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let noon = at("2026-11-02T12:00:00Z");
        for _ in 0..2 {
            record_unprompted(&mut state, noon.plus_seconds(-3 * HOUR_SECONDS));
        }
        assert_eq!(state.budget.left_today(10, noon, "UTC"), 8);
        stop_today(&mut state, noon);
        assert_eq!(state.budget.left_today(10, noon, "UTC"), 0);
        assert_eq!(state.budget.allowance_today(10, noon, "UTC"), 2);
        assert!(!can_speak_unprompted(&state, noon));
        assert!(!can_speak_unprompted(&state, at("2026-11-02T23:59:00Z")));
        let tomorrow = at("2026-11-03T12:00:00Z");
        assert_eq!(state.budget.left_today(10, tomorrow, "UTC"), 10);
        assert!(can_speak_unprompted(&state, tomorrow));
    }

    #[test]
    fn chimes_count_per_day_and_stop_after_a_shush() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let noon = at("2026-11-02T12:00:00Z");
        record_chime(&mut state, noon);
        record_chime(&mut state, noon);
        assert_eq!(state.budget.chimes_today(noon, "UTC"), 2);
        assert_eq!(state.budget.today(noon, "UTC"), 2);
        assert!(!state.budget.shushed_today(noon, "UTC"));
        stop_today(&mut state, noon);
        assert!(state.budget.shushed_today(noon, "UTC"));
        let tomorrow = at("2026-11-03T12:00:00Z");
        assert!(!state.budget.shushed_today(tomorrow, "UTC"));
        assert_eq!(state.budget.chimes_today(tomorrow, "UTC"), 0);
        state.budget.prune(at("2026-11-05T12:00:00Z"));
        assert!(state.budget.chimes.is_empty());
    }

    #[test]
    fn work_profiles_keep_working_hours() {
        let state = GroupState::new(GroupId::new("g"), Kind::Team);
        assert!(can_speak_unprompted(&state, at("2026-11-02T10:00:00Z")));
        assert!(!can_speak_unprompted(&state, at("2026-11-02T19:00:00Z")));
        assert!(!can_speak_unprompted(&state, at("2026-11-07T10:00:00Z")));
    }
}
