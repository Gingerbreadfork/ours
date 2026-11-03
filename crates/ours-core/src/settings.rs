//! Settings by sentence (DESIGN.md section 8, "Control"): quiet hours,
//! working hours, the dial, the kind, the persona's running bits, and the
//! end or start of observe mode. A lead's sentence applies at once, a
//! member's waits for a lead's ✅, and a guest's is declined.

use crate::action::Recording;
use crate::authority::{ActionKind, Disposition, dispose};
use crate::effect::{Effect, send, send_as};
use crate::leads::{self, decline_line};
use crate::observe;
use crate::persona::BITS_KEPT;
use crate::profile::{ClockTime, Jobs, Profile, QuietHours, WorkingHours};
use crate::state::{DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, GroupState};
use crate::timers::{self, TimerPurpose};
use crate::trace::{Trace, quote};
use ours_types::{Instant, Kind, Message, Role};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The highest dial setting.
pub const MAX_DIAL: u8 = 3;
/// Quiet hours end here when a sentence gives only the start.
const DEFAULT_QUIET_END: u8 = 8;

/// A change to the group's settings, as a sentence or the admin page asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingChange {
    /// Quiet from `start` until the current end, or 08:00 when there was none.
    QuietFrom {
        start: u8,
    },
    QuietHours {
        start: u8,
        end: u8,
    },
    NoQuietHours,
    WorkingHours {
        start: u8,
        end: u8,
    },
    NoWorkingHours,
    /// Turn the dial up or down by `delta`, within 0 to 3.
    DialBy {
        delta: i8,
    },
    DialTo {
        dial: u8,
    },
    /// Reset the profile to this kind's defaults.
    Kind {
        kind: Kind,
    },
    /// End observe mode and start speaking unprompted.
    Go,
    /// Watch again for `days`, or the profile's period when not given.
    Observe {
        days: Option<u32>,
    },
    /// The digest at this local time, which also turns it on.
    DigestAt {
        hour: u8,
        minute: u8,
    },
    /// The digest on at its usual time, or off.
    Digest {
        on: bool,
    },
    /// The day-of summary on or off.
    DayOf {
        on: bool,
    },
    /// A running bit the group gave the agent.
    AddBit {
        text: String,
    },
    /// Drops every bit that mentions these words.
    DropBit {
        words: String,
    },
    /// One of the starter personas by name, or "plain" for the default.
    UsePersona {
        name: String,
    },
    /// Turn expiry and retention off, or back on.
    LegalHold {
        on: bool,
    },
}

/// Why a setting's line is not undone.
pub const SETTING_NOTE: &str = "a setting changes back by saying it the other way";

/// Handles a setting sentence from `message`'s author.
pub(crate) fn handle(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    change: SettingChange,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    let reply_to = Some(message.id.clone());
    let trigger = addressed_trigger(state, message);
    match dispose(role, ActionKind::ChangeSettings) {
        Disposition::Do => {
            let line = apply(state, &change, now, fx);
            let trace = Trace::new(trigger, "a lead's or host's setting applies at once", now);
            let note = Recording::Note {
                why: SETTING_NOTE.to_owned(),
            };
            send_as(state, fx, room, line.clone(), reply_to, now, trace, note);
            let by = state.display_name(room, &message.author);
            fx.push(Effect::Notice(format!("Setting: {line} Changed by {by}.")));
        }
        Disposition::Defer => {
            let deferral = Deferral {
                room: room.clone(),
                asked_by: message.author.clone(),
                request: message.id.clone(),
                request_text: message.text.clone(),
                change: DeferredChange::Setting { change },
                expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
            };
            let trace = Trace::new(trigger, "a member's setting waits for a lead's ✅", now);
            leads::open_deferral(state, fx, deferral, now, trace);
        }
        Disposition::Decline => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
    }
}

/// Applies a change and returns the one line that announces it.
pub(crate) fn apply(
    state: &mut GroupState,
    change: &SettingChange,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    let profile = &mut state.profile;
    match change {
        SettingChange::QuietFrom { start } => {
            let end = if profile.quiet_hours.is_none() {
                DEFAULT_QUIET_END
            } else {
                profile.quiet_hours.end
            };
            profile.quiet_hours = QuietHours {
                start: hour(*start),
                end,
            };
            quiet_line(profile.quiet_hours)
        }
        SettingChange::QuietHours { start, end } => {
            profile.quiet_hours = QuietHours {
                start: hour(*start),
                end: hour(*end),
            };
            quiet_line(profile.quiet_hours)
        }
        SettingChange::NoQuietHours => {
            profile.quiet_hours = QuietHours::NONE;
            quiet_line(profile.quiet_hours)
        }
        SettingChange::WorkingHours { start, end } => {
            let weekdays_only = profile.working_hours.is_none_or(|w| w.weekdays_only);
            profile.working_hours = Some(WorkingHours {
                start: hour(*start),
                end: hour(*end),
                weekdays_only,
            });
            working_line(profile.working_hours)
        }
        SettingChange::NoWorkingHours => {
            profile.working_hours = None;
            working_line(None)
        }
        SettingChange::DialBy { delta } => {
            let dial = i16::from(profile.dial) + i16::from(*delta);
            profile.dial = u8::try_from(dial.clamp(0, i16::from(MAX_DIAL))).unwrap_or(0);
            dial_line(profile.dial)
        }
        SettingChange::DialTo { dial } => {
            profile.dial = (*dial).min(MAX_DIAL);
            dial_line(profile.dial)
        }
        SettingChange::Kind { kind } => {
            set_kind(state, *kind);
            format!("Set up as {}, with its defaults.", kind_phrase(*kind))
        }
        SettingChange::Go => observe::go(state, now, fx),
        SettingChange::Observe { days } => observe::watch(state, *days, now, fx),
        SettingChange::DigestAt { hour, minute } => {
            let at = ClockTime {
                hour: hour % 24,
                minute: minute % 60,
            };
            set_jobs(state, fx, |jobs| jobs.digest = Some(at));
            digest_line(state)
        }
        SettingChange::Digest { on } => {
            let usual = Jobs::default_digest(state.profile.working_hours);
            let on = *on;
            set_jobs(state, fx, |jobs| {
                jobs.digest = if on {
                    Some(jobs.digest.unwrap_or(usual))
                } else {
                    None
                };
            });
            digest_line(state)
        }
        SettingChange::DayOf { on } => {
            let on = *on;
            set_jobs(state, fx, |jobs| jobs.day_of = on);
            if on {
                "Day-of summary on.".to_owned()
            } else {
                "Day-of summary off.".to_owned()
            }
        }
        SettingChange::AddBit { text } => add_bit(state, text),
        SettingChange::DropBit { words } => drop_bit(state, words),
        SettingChange::UsePersona { name } => crate::persona::use_starter(state, name),
        SettingChange::LegalHold { on } => crate::retention::set_legal_hold(state, *on, now, fx),
    }
}

/// Keeps a new bit, dropping the oldest past `BITS_KEPT`.
fn add_bit(state: &mut GroupState, text: &str) -> String {
    let text = text.trim();
    let bits = &mut state.persona.bits;
    if bits.iter().any(|b| b.eq_ignore_ascii_case(text)) {
        return format!("I already have that one: {}.", quote(text, 80));
    }
    bits.push(text.to_owned());
    let dropped = (bits.len() > BITS_KEPT).then(|| bits.remove(0));
    let mut line = format!(
        "New bit: {}. I'll bring it out when it fits, once a day at most.",
        quote(text, 80)
    );
    if let Some(old) = dropped {
        line.push_str(&format!(" Retired the oldest, {}.", quote(&old, 40)));
    }
    line
}

/// Drops the bits that mention `words`, ignoring case.
fn drop_bit(state: &mut GroupState, words: &str) -> String {
    let needle = words.trim().to_lowercase();
    let bits = &mut state.persona.bits;
    let before = bits.len();
    bits.retain(|b| needle.is_empty() || !b.to_lowercase().contains(&needle));
    match before - bits.len() {
        0 => format!("I don't have a bit about {}.", words.trim()),
        1 => format!("Dropped the bit about {}.", words.trim()),
        n => format!("Dropped {n} bits about {}.", words.trim()),
    }
}

/// "Bits: "the negroni", "Sam's parking"." or nothing when there are none.
pub fn bits_line(state: &GroupState) -> Option<String> {
    let bits = &state.persona.bits;
    if bits.is_empty() {
        return None;
    }
    let quoted: Vec<String> = bits.iter().map(|b| quote(b, 40)).collect();
    Some(format!("Bits: {}.", quoted.join(", ")))
}

/// Changes the profile's jobs and drops the digest timers so they are
/// armed again at the new time.
fn set_jobs(state: &mut GroupState, fx: &mut Vec<Effect>, change: impl FnOnce(&mut Jobs)) {
    let mut jobs = state.jobs();
    change(&mut jobs);
    state.profile.jobs = Some(jobs);
    timers::cancel_where(state, fx, |p| matches!(p, TimerPurpose::Digest { .. }));
    if !jobs.day_of {
        timers::cancel_where(state, fx, |p| matches!(p, TimerPurpose::DayOf { .. }));
    }
}

/// "Digest at 16:00 each working day.", or "No digest."
pub fn digest_line(state: &GroupState) -> String {
    let days = if state.profile.working_hours.is_some_and(|w| w.weekdays_only) {
        "each working day"
    } else {
        "each day"
    };
    match state.jobs().digest {
        Some(at) => format!("Digest at {:02}:{:02} {days}.", at.hour, at.minute),
        None => "No digest.".to_owned(),
    }
}

/// Changes the kind and resets the profile to its defaults. The agent's
/// name, leads, hosts and observe state are kept.
pub(crate) fn set_kind(state: &mut GroupState, kind: Kind) {
    state.kind = kind;
    state.profile = Profile::for_kind(kind);
}

/// Renames the agent, as the config or the first conversation asks, and
/// says what happened. An empty name is ignored.
pub(crate) fn set_agent_name(state: &mut GroupState, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Ignored an empty agent name.".to_owned()
    } else {
        state.agent_name = name.to_owned();
        format!("Agent name set to {name}.")
    }
}

/// "Sam in #dinner: "Q, quiet after 10"", for traces of addressed messages.
pub(crate) fn addressed_trigger(state: &GroupState, message: &Message) -> String {
    format!(
        "{} in {}: {}",
        state.display_name(&message.room, &message.author),
        state.room_label(&message.room),
        quote(&message.text, 80)
    )
}

fn hour(h: u8) -> u8 {
    h % 24
}

pub fn clock(h: u8) -> String {
    format!("{:02}:00", h % 24)
}

pub fn quiet_line(q: QuietHours) -> String {
    if q.is_none() {
        "No quiet hours.".to_owned()
    } else {
        format!("Quiet from {} to {}.", clock(q.start), clock(q.end))
    }
}

pub fn working_line(w: Option<WorkingHours>) -> String {
    match w {
        None => "No working hours.".to_owned(),
        Some(w) => format!(
            "Working hours {} to {}, {}.",
            clock(w.start),
            clock(w.end),
            if w.weekdays_only {
                "Monday to Friday"
            } else {
                "every day"
            }
        ),
    }
}

pub fn dial_name(dial: u8) -> &'static str {
    match dial {
        0 => "concierge",
        1 => "dry and brief",
        2 => "a mate",
        _ => "chaos",
    }
}

fn dial_line(dial: u8) -> String {
    format!("Dial at {dial}: {}.", dial_name(dial))
}

pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Friends => "friends",
        Kind::Household => "household",
        Kind::Team => "team",
        Kind::Project => "project",
        Kind::Club => "club",
        Kind::Community => "community",
        Kind::Committee => "committee",
        Kind::Cohort => "cohort",
    }
}

pub fn kind_phrase(kind: Kind) -> &'static str {
    match kind {
        Kind::Friends => "a friends group",
        Kind::Household => "a household",
        Kind::Team => "a team",
        Kind::Project => "a project",
        Kind::Club => "a club",
        Kind::Community => "a community",
        Kind::Committee => "a committee",
        Kind::Cohort => "a cohort",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ours_types::GroupId;

    #[test]
    fn changes_apply_within_bounds() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let now = Instant::UNIX_EPOCH;
        let mut fx = Vec::new();
        let mut run = |state: &mut GroupState, c: SettingChange| apply(state, &c, now, &mut fx);

        assert_eq!(
            run(&mut state, SettingChange::NoQuietHours),
            "No quiet hours."
        );
        assert_eq!(
            run(&mut state, SettingChange::QuietFrom { start: 23 }),
            "Quiet from 23:00 to 08:00."
        );
        assert_eq!(
            run(&mut state, SettingChange::QuietFrom { start: 22 }),
            "Quiet from 22:00 to 08:00."
        );
        assert_eq!(
            run(&mut state, SettingChange::DialBy { delta: -5 }),
            "Dial at 0: concierge."
        );
        assert_eq!(
            run(&mut state, SettingChange::DialTo { dial: 9 }),
            "Dial at 3: chaos."
        );
        assert_eq!(
            run(
                &mut state,
                SettingChange::WorkingHours { start: 9, end: 18 }
            ),
            "Working hours 09:00 to 18:00, Monday to Friday."
        );
        state.agent_name = "Robin".into();
        state.leads.insert("sam".into());
        run(&mut state, SettingChange::Kind { kind: Kind::Team });
        assert_eq!(state.kind, Kind::Team);
        assert_eq!(state.profile, Profile::for_kind(Kind::Team));
        assert_eq!(state.agent_name, "Robin");
        assert!(state.leads.contains(&"sam".into()));
    }

    #[test]
    fn bits_are_kept_listed_and_dropped() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        let now = Instant::UNIX_EPOCH;
        let mut fx = Vec::new();
        let mut run = |state: &mut GroupState, c: SettingChange| apply(state, &c, now, &mut fx);
        let add = |text: &str| SettingChange::AddBit { text: text.into() };
        assert_eq!(bits_line(&state), None);
        assert_eq!(
            run(&mut state, add("the negroni")),
            "New bit: \"the negroni\". I'll bring it out when it fits, once a day at most."
        );
        assert!(run(&mut state, add("The Negroni")).starts_with("I already have that one"));
        run(&mut state, add("Sam's parking"));
        assert_eq!(
            bits_line(&state).as_deref(),
            Some("Bits: \"the negroni\", \"Sam's parking\".")
        );
        let drop = |words: &str| SettingChange::DropBit {
            words: words.into(),
        };
        assert_eq!(
            run(&mut state, drop("NEGRONI")),
            "Dropped the bit about NEGRONI."
        );
        assert_eq!(
            run(&mut state, drop("the cat")),
            "I don't have a bit about the cat."
        );
        assert_eq!(state.persona.bits, vec!["Sam's parking".to_owned()]);
        for n in 0..BITS_KEPT {
            run(&mut state, add(&format!("bit {n}")));
        }
        assert_eq!(state.persona.bits.len(), BITS_KEPT);
        assert_eq!(state.persona.bits[0], "bit 0");
    }
}
