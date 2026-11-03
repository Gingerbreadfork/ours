//! Retention and legal hold (DESIGN.md section 9, "Retention"). A daily
//! sweep drops action records older than the kind keeps them and scrubs
//! facts past their expiry. A legal hold turns the sweep off.

use crate::action::{ActionRecord, ActionStatus};
use crate::effect::Effect;
use crate::memory::{self, DAY_SECONDS, EXPIRED, FactId};
use crate::state::GroupState;
use crate::time::{at_on, date_of};
use crate::timers::{self, TimerPurpose};
use ours_types::{ActionId, Instant, Kind, TimerId};
use std::collections::BTreeSet;
use tracing::info;

/// The local hour the sweep runs, inside the default quiet hours.
pub const SWEEP_HOUR: i8 = 4;

/// Days the action log is kept: a year at work, ninety days elsewhere.
pub fn retention_days(kind: Kind) -> u32 {
    match kind {
        Kind::Team | Kind::Project | Kind::Committee => 365,
        Kind::Friends | Kind::Household | Kind::Club | Kind::Cohort | Kind::Community => 90,
    }
}

fn kept_seconds(state: &GroupState) -> i64 {
    i64::from(retention_days(state.kind)) * DAY_SECONDS
}

/// A record still waiting to leave the group is kept however old it is.
fn settled(state: &GroupState, record: &ActionRecord) -> bool {
    !matches!(
        record.status,
        ActionStatus::Held { .. } | ActionStatus::Sending { .. }
    ) && !state.holds.contains_key(&record.id)
}

/// When the sweep next has something to do: the oldest record passing the
/// retention period, or the first live fact reaching its expiry.
fn next_due(state: &GroupState) -> Option<Instant> {
    let kept = kept_seconds(state);
    let record = state
        .actions
        .iter()
        .filter(|r| settled(state, r))
        .map(|r| r.at.plus_seconds(kept))
        .min();
    let fact = state
        .facts
        .values()
        .filter(|f| f.retracted.is_none())
        .filter_map(|f| f.expires)
        .min();
    record.into_iter().chain(fact).min()
}

/// The first sweep time after `at` in the group's zone.
fn sweep_after(state: &GroupState, at: Instant) -> Option<Instant> {
    let tz = state.timezone.as_str();
    let mut day = date_of(at, tz);
    for _ in 0..3 {
        if let Some(t) = at_on(day, SWEEP_HOUR, 0, tz)
            && t > at
        {
            return Some(t);
        }
        day = day.tomorrow().ok()?;
    }
    None
}

fn armed(state: &GroupState) -> Option<(TimerId, Instant)> {
    state
        .timers
        .iter()
        .find(|(_, t)| t.purpose == TimerPurpose::Retention)
        .map(|(id, t)| (id.clone(), t.at))
}

/// Arms the sweep for the first night something is due, and takes it down
/// under a legal hold. Runs at the end of every step.
pub(crate) fn reconcile(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    let armed = armed(state);
    if state.legal_hold {
        if let Some((id, _)) = armed {
            timers::cancel(state, fx, &id);
        }
        return;
    }
    let Some(want) = next_due(state).and_then(|due| sweep_after(state, due.max(now))) else {
        return;
    };
    match armed {
        Some((_, at)) if at <= want => {}
        Some((id, _)) => {
            timers::cancel(state, fx, &id);
            timers::arm(state, fx, want, TimerPurpose::Retention);
        }
        None => {
            timers::arm(state, fx, want, TimerPurpose::Retention);
        }
    }
}

/// The sweep: drops records past the retention period and scrubs expired
/// facts, saying how many in a notice. Nothing goes under a legal hold.
pub(crate) fn on_timer(state: &mut GroupState, now: Instant, fx: &mut Vec<Effect>) {
    if state.legal_hold {
        return;
    }
    let cutoff = now.plus_seconds(-kept_seconds(state));
    let old: BTreeSet<ActionId> = state
        .actions
        .iter()
        .filter(|r| r.at <= cutoff && settled(state, r))
        .map(|r| r.id.clone())
        .collect();
    state.actions.retain(|r| !old.contains(&r.id));
    let records = old.len();
    let expired: Vec<FactId> = state
        .facts
        .values()
        .filter(|f| f.retracted.is_none() && f.is_expired(now))
        .map(|f| f.id.clone())
        .collect();
    let facts = memory::retract(state, &expired, EXPIRED, None, now);
    report(state, fx, records, facts);
}

fn report(state: &GroupState, fx: &mut Vec<Effect>, records: usize, facts: usize) {
    let days = retention_days(state.kind);
    let mut parts = Vec::new();
    if records > 0 {
        let noun = if records == 1 { "record" } else { "records" };
        parts.push(format!("{records} action {noun} older than {days} days"));
    }
    if facts > 0 {
        let noun = if facts == 1 { "fact" } else { "facts" };
        parts.push(format!("{facts} expired {noun}"));
    }
    if parts.is_empty() {
        return;
    }
    info!(records, facts, days, "retention sweep");
    fx.push(Effect::Notice(format!(
        "Retention: dropped {}.",
        parts.join(" and ")
    )));
}

/// Turns the legal hold on or off and returns the line that says so.
pub(crate) fn set_legal_hold(
    state: &mut GroupState,
    on: bool,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> String {
    if on {
        let since = *state.legal_hold_since.get_or_insert(now);
        state.legal_hold = true;
        timers::cancel_where(state, fx, |p| *p == TimerPurpose::Retention);
        format!(
            "Legal hold on since {}: nothing expires and nothing is dropped until a lead says legal hold off.",
            memory::day(since, &state.timezone)
        )
    } else {
        state.legal_hold = false;
        state.legal_hold_since = None;
        format!(
            "Legal hold off. The action log is kept {} days again and expired facts are dropped.",
            retention_days(state.kind)
        )
    }
}

/// The charter's lines on retention and legal hold.
pub fn charter_lines(state: &GroupState) -> Vec<String> {
    let hold = match (state.legal_hold, state.legal_hold_since) {
        (true, Some(since)) => format!(
            "- Legal hold: on since {}; nothing expires",
            memory::day(since, &state.timezone)
        ),
        (true, None) => "- Legal hold: on; nothing expires".to_owned(),
        (false, _) => "- Legal hold: off".to_owned(),
    };
    vec![
        format!("- Action log kept: {} days", retention_days(state.kind)),
        "- Perishable facts: dropped once they expire".to_owned(),
        hold,
    ]
}
