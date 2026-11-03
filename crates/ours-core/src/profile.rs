//! The settings a kind of group starts with (DESIGN.md sections 4, 7, 12
//! and 13). The engine reads only these, never the kind itself.

use ours_types::{Kind, PlatformRole};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Who is a lead without being named in the leads list (DESIGN.md section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeadsRule {
    /// Every member is a lead.
    Everyone,
    /// Platform owners and admins.
    Admins,
    /// Platform owners, admins and moderators.
    Moderators,
    /// Only the people in the leads list.
    Named,
}

impl LeadsRule {
    pub fn admits(self, role: PlatformRole) -> bool {
        use PlatformRole::{Admin, External, Moderator, Owner};
        match self {
            LeadsRule::Everyone => role != External,
            LeadsRule::Admins => matches!(role, Owner | Admin),
            LeadsRule::Moderators => matches!(role, Owner | Admin | Moderator),
            LeadsRule::Named => false,
        }
    }
}

/// What the agent does when it, not a lead, wants something to leave the
/// group (DESIGN.md section 13, "Unprompted outbound").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "UnpromptedOutbound")]
pub enum Outbound {
    /// Do it, with the hold.
    Do,
    /// DM a lead with a ✅ and do nothing until one comes.
    #[default]
    Propose,
    Never,
}

/// Local hours in the group's zone when nothing unprompted is sent. The
/// span may wrap midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuietHours {
    pub start: u8,
    pub end: u8,
}

impl QuietHours {
    /// Equal start and end means no quiet hours.
    pub const NONE: QuietHours = QuietHours { start: 0, end: 0 };

    pub fn is_none(self) -> bool {
        self.start == self.end
    }

    pub fn contains(self, hour: u8) -> bool {
        match self.start.cmp(&self.end) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => hour >= self.start && hour < self.end,
            std::cmp::Ordering::Greater => hour >= self.start || hour < self.end,
        }
    }
}

/// Local hours in the group's zone when unprompted messages may be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkingHours {
    pub start: u8,
    pub end: u8,
    pub weekdays_only: bool,
}

impl WorkingHours {
    /// `weekday` counts Monday as 1 and Sunday as 7.
    pub fn contains(self, weekday: u8, hour: u8) -> bool {
        if self.weekdays_only && weekday > 5 {
            return false;
        }
        hour >= self.start && hour < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Profile {
    pub leads: LeadsRule,
    /// Unprompted messages allowed in any rolling hour.
    pub budget_per_hour: u32,
    /// Unprompted messages allowed per calendar day in the group's zone.
    pub budget_per_day: u32,
    /// Attention score above which the agent speaks unprompted.
    pub speech_threshold: f64,
    /// How long a tier-1 action waits before it leaves the group.
    pub hold_window_seconds: u32,
    /// The persona dial, 0 to 3 (DESIGN.md section 16).
    pub dial: u8,
    pub quiet_hours: QuietHours,
    pub working_hours: Option<WorkingHours>,
    /// How long a question may go unanswered before the agent raises a hand.
    #[serde(default = "default_answer_window")]
    pub answer_window_seconds: u32,
    /// How long a new group is watched before the agent speaks unprompted.
    #[serde(default)]
    pub observe_seconds: u32,
    /// The scheduled jobs, once a lead has changed them; else the kind's.
    #[serde(default)]
    pub jobs: Option<Jobs>,
    /// Whether a tier-1 action nobody asked for may leave the group.
    #[serde(default)]
    pub unprompted_outbound: Outbound,
    /// The banter score at which it may chime in, at dial 2 and 3.
    #[serde(default = "default_chime_threshold")]
    pub chime_threshold: f32,
    /// Chime-ins allowed per local day; unset follows the dial.
    #[serde(default)]
    pub chimes_per_day: Option<u32>,
}

/// A local time of day in the group's zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClockTime {
    pub hour: u8,
    pub minute: u8,
}

/// The scheduled jobs a profile turns on (DESIGN.md section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Jobs {
    /// When the daily digest goes out; none for no digest.
    pub digest: Option<ClockTime>,
    /// The one line on a day with something on the calendar.
    pub day_of: bool,
}

impl Jobs {
    /// The digest in work profiles, at the last hour of the working day; the
    /// day-of summary in social ones.
    pub fn for_kind(kind: Kind, working: Option<WorkingHours>) -> Jobs {
        let digest = matches!(kind, Kind::Team | Kind::Project | Kind::Committee)
            .then(|| Jobs::default_digest(working));
        let day_of = matches!(
            kind,
            Kind::Friends | Kind::Household | Kind::Club | Kind::Cohort
        );
        Jobs { digest, day_of }
    }

    /// An hour before working hours end, or 17:00 without them.
    pub fn default_digest(working: Option<WorkingHours>) -> ClockTime {
        let hour = working.map_or(17, |w| w.end.saturating_sub(1).max(w.start));
        ClockTime { hour, minute: 0 }
    }
}

/// The answer window for profiles saved before it existed.
fn default_answer_window() -> u32 {
    ANSWER_WINDOW_SECONDS
}

fn default_chime_threshold() -> f32 {
    CHIME_THRESHOLD
}

pub const ANSWER_WINDOW_SECONDS: u32 = 20 * 60;
/// Banter at or above this may get a chime-in.
pub const CHIME_THRESHOLD: f32 = 0.7;
const DAY_SECONDS: u32 = 24 * 60 * 60;

impl Profile {
    pub fn for_kind(kind: Kind) -> Profile {
        let quiet_hours = QuietHours { start: 22, end: 8 };
        let office = Some(WorkingHours {
            start: 9,
            end: 18,
            weekdays_only: true,
        });
        let social = Profile {
            leads: LeadsRule::Everyone,
            budget_per_hour: 3,
            budget_per_day: 10,
            speech_threshold: 0.8,
            hold_window_seconds: 120,
            dial: 1,
            quiet_hours,
            working_hours: None,
            answer_window_seconds: ANSWER_WINDOW_SECONDS,
            observe_seconds: DAY_SECONDS,
            jobs: None,
            unprompted_outbound: Outbound::Do,
            chime_threshold: CHIME_THRESHOLD,
            chimes_per_day: None,
        };
        let work = Profile {
            leads: LeadsRule::Admins,
            budget_per_hour: 1,
            budget_per_day: 4,
            speech_threshold: 0.85,
            hold_window_seconds: 600,
            dial: 0,
            quiet_hours,
            working_hours: office,
            answer_window_seconds: 30 * 60,
            observe_seconds: 3 * DAY_SECONDS,
            jobs: None,
            unprompted_outbound: Outbound::Propose,
            chime_threshold: CHIME_THRESHOLD,
            chimes_per_day: None,
        };
        match kind {
            Kind::Friends | Kind::Household => social,
            Kind::Club | Kind::Cohort => Profile {
                leads: LeadsRule::Named,
                unprompted_outbound: Outbound::Propose,
                ..social
            },
            Kind::Team | Kind::Project => work,
            Kind::Committee => Profile {
                leads: LeadsRule::Named,
                budget_per_hour: 1,
                budget_per_day: 2,
                ..work
            },
            Kind::Community => Profile {
                leads: LeadsRule::Moderators,
                budget_per_hour: 2,
                budget_per_day: 8,
                speech_threshold: 0.8,
                hold_window_seconds: 300,
                dial: 1,
                quiet_hours,
                working_hours: None,
                answer_window_seconds: ANSWER_WINDOW_SECONDS,
                observe_seconds: DAY_SECONDS,
                jobs: None,
                unprompted_outbound: Outbound::Propose,
                chime_threshold: CHIME_THRESHOLD,
                chimes_per_day: None,
            },
        }
    }

    /// Chime-ins allowed per day: the profile's own number, else none at
    /// dial 0 and 1, three at dial 2 and six at dial 3.
    pub fn chimes_a_day(&self) -> u32 {
        self.chimes_per_day.unwrap_or(match self.dial {
            0 | 1 => 0,
            2 => 3,
            _ => 6,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_wrap_midnight() {
        let q = QuietHours { start: 22, end: 8 };
        assert!(q.contains(23));
        assert!(q.contains(0));
        assert!(q.contains(7));
        assert!(!q.contains(8));
        assert!(!q.contains(21));
    }

    #[test]
    fn defaults_follow_the_design() {
        let team = Profile::for_kind(Kind::Team);
        assert_eq!((team.budget_per_hour, team.budget_per_day), (1, 4));
        assert_eq!(team.hold_window_seconds, 600);
        assert_eq!(team.dial, 0);
        let friends = Profile::for_kind(Kind::Friends);
        assert_eq!((friends.budget_per_hour, friends.budget_per_day), (3, 10));
        assert_eq!(friends.leads, LeadsRule::Everyone);
        assert_eq!(Profile::for_kind(Kind::Community).hold_window_seconds, 300);
        assert_eq!(Profile::for_kind(Kind::Committee).budget_per_day, 2);
        assert_eq!(Profile::for_kind(Kind::Club).leads, LeadsRule::Named);
    }

    #[test]
    fn only_friends_and_households_send_unprompted() {
        for kind in [Kind::Friends, Kind::Household] {
            assert_eq!(Profile::for_kind(kind).unprompted_outbound, Outbound::Do);
        }
        for kind in [
            Kind::Team,
            Kind::Project,
            Kind::Committee,
            Kind::Community,
            Kind::Club,
            Kind::Cohort,
        ] {
            assert_eq!(
                Profile::for_kind(kind).unprompted_outbound,
                Outbound::Propose,
                "{kind:?}"
            );
        }
        assert_eq!(Outbound::default(), Outbound::Propose);
    }

    #[test]
    fn answer_windows_and_observe_periods() {
        for kind in [Kind::Friends, Kind::Household, Kind::Club, Kind::Cohort] {
            let p = Profile::for_kind(kind);
            assert_eq!((p.answer_window_seconds, p.observe_seconds), (1200, 86400));
        }
        for kind in [Kind::Team, Kind::Project, Kind::Committee] {
            let p = Profile::for_kind(kind);
            assert_eq!((p.answer_window_seconds, p.observe_seconds), (1800, 259200));
        }
        let community = Profile::for_kind(Kind::Community);
        assert_eq!(community.observe_seconds, 86400);
        assert!(QuietHours::NONE.is_none());
        assert!(!(0..24).any(|h| QuietHours::NONE.contains(h)));
    }

    #[test]
    fn chimes_follow_the_dial_unless_set() {
        let mut p = Profile::for_kind(Kind::Friends);
        assert_eq!(p.chime_threshold, CHIME_THRESHOLD);
        assert_eq!(p.chimes_per_day, None);
        let by_dial: Vec<u32> = (0..=3)
            .map(|dial| {
                p.dial = dial;
                p.chimes_a_day()
            })
            .collect();
        assert_eq!(by_dial, vec![0, 0, 3, 6]);
        p.chimes_per_day = Some(1);
        assert_eq!(p.chimes_a_day(), 1);
    }
}
