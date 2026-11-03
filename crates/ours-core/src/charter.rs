//! The charter (DESIGN.md section 18, "Compounding assets"): every setting
//! the group has, as Markdown a person can read. The binary writes it to a
//! file whenever it changes.

use crate::budget::Budget;
use crate::decide::{DEADLINE_SECONDS, SHORTEST_SECONDS, default_rule};
use crate::leads::listen_line;
use crate::observe::{Observe, span};
use crate::persona::Persona;
use crate::profile::LeadsRule;
use crate::reactions;
use crate::settings::{
    bits_line, clock, dial_name, digest_line, kind_name, kind_phrase, quiet_line, working_line,
};
use crate::state::GroupState;
use crate::zone::stamp;
use ours_types::{Instant, PlatformRoomId, PlatformUserId};
use std::collections::BTreeSet;

/// The group's charter as Markdown.
pub fn render_charter(state: &GroupState) -> String {
    let p = &state.profile;
    let tz = state.timezone.as_str();
    let model = if state.model_label.is_empty() {
        "not set".to_owned()
    } else {
        state.model_label.clone()
    };
    let mut lines = vec![
        format!("# Charter for group {}", state.id),
        String::new(),
        "## The group".to_owned(),
        String::new(),
        format!("- Kind: {}", kind_name(state.kind)),
        format!("- Agent name: {}", state.agent_name),
        format!("- Model: {model}"),
        format!("- Time zone: {tz}"),
        String::new(),
        "## Who it listens to".to_owned(),
        String::new(),
        format!("- Leads rule: {}", leads_rule(p.leads)),
        format!("- Named leads: {}", people(state, &state.leads)),
        format!("- Hosts: {}", people(state, &state.hosts)),
        String::new(),
        "## Voice".to_owned(),
        String::new(),
        format!("- Voice: {} (dial {})", voice_name(state), p.dial),
        format!("- Dial: {}, {}", p.dial, dial_name(p.dial)),
        format!("- Talks like: {}", state.persona.voice),
        format!("- Traits: {}", listed(&state.persona.traits)),
        format!(
            "- Never jokes about: {}",
            listed(&state.persona.never_jokes_about)
        ),
        format!("- Running bits: {}", listed(&state.persona.bits)),
    ];
    lines.extend(reaction_lines(state));
    lines.extend([
        String::new(),
        "## Speaking unprompted".to_owned(),
        String::new(),
        format!(
            "- Budget: {} an hour, {} a day",
            p.budget_per_hour, p.budget_per_day
        ),
        format!(
            "- Chime-ins: {} a day, on banter of {:.2} or more, at dial 2 and 3 only",
            p.chimes_a_day(),
            p.chime_threshold
        ),
    ]);
    if let Some(cut) = cut(&state.budget) {
        lines.push(cut);
    }
    lines.extend([
        format!("- Speech threshold: {:.2}", p.speech_threshold),
        format!(
            "- Quiet hours: {}",
            if p.quiet_hours.is_none() {
                "none".to_owned()
            } else {
                format!(
                    "{} to {}",
                    clock(p.quiet_hours.start),
                    clock(p.quiet_hours.end)
                )
            }
        ),
        format!(
            "- Working hours: {}",
            match p.working_hours {
                None => "none".to_owned(),
                Some(w) => format!(
                    "{} to {}, {}",
                    clock(w.start),
                    clock(w.end),
                    if w.weekdays_only {
                        "Monday to Friday"
                    } else {
                        "every day"
                    }
                ),
            }
        ),
        format!("- Answer window: {}", span_minutes(p.answer_window_seconds)),
        format!(
            "- Hold window for actions that leave the group: {}",
            span_minutes(p.hold_window_seconds)
        ),
        String::new(),
        "## Observe mode".to_owned(),
        String::new(),
        format!(
            "- A new group is watched for: {}",
            if p.observe_seconds == 0 {
                "no time at all".to_owned()
            } else {
                span(i64::from(p.observe_seconds))
            }
        ),
        format!("- Status: {}", observe_status(state, tz)),
        String::new(),
        "## Decisions".to_owned(),
        String::new(),
        format!("- Proposals close on: {}", default_rule(state).name()),
        format!(
            "- Deadline: {} hours, or half the time to the soonest option, never under {} hours",
            DEADLINE_SECONDS / 3600,
            SHORTEST_SECONDS / 3600
        ),
        String::new(),
        "## Scheduled".to_owned(),
        String::new(),
        format!("- {}", digest_line(state)),
        format!(
            "- Day-of summary: {}",
            if state.jobs().day_of { "on" } else { "off" }
        ),
        format!("- Tab currency: {}", state.currency),
        format!(
            "- Calendar feed: {}",
            state
                .feed_base
                .as_ref()
                .map_or_else(|| "off".to_owned(), |b| format!("{b}.ics"))
        ),
    ]);
    for rec in &state.recurrences {
        let paused = if rec.paused { ", paused" } else { "" };
        lines.push(format!(
            "- Repeats: {}{paused}",
            crate::recur::describe(state, rec)
        ));
    }
    lines.extend([String::new(), "## Retention".to_owned(), String::new()]);
    lines.extend(crate::retention::charter_lines(state));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The answer to "settings": six lines at most, for the room. Bits ride on
/// the dial's line.
pub fn settings_summary(state: &GroupState, room: &PlatformRoomId, now: Instant) -> String {
    let p = &state.profile;
    let model = if state.model_label.is_empty() {
        String::new()
    } else {
        format!(" on {}", state.model_label)
    };
    let left = state
        .budget
        .left_today(p.budget_per_day, now, &state.timezone);
    let observing = match &state.observe {
        Some(o) if o.until > now => format!(
            "Watching until {}; a lead says go to end it.",
            stamp(o.until, &state.timezone)
        ),
        Some(_) => "Done watching; waiting for a lead to say go.".to_owned(),
        None => "Speaking unprompted when it helps, within the budget.".to_owned(),
    };
    [
        format!(
            "{}; I'm {}{model}; times in {}.",
            capitalise(kind_phrase(state.kind)),
            state.agent_name,
            state.timezone
        ),
        listen_line(state, room),
        format!(
            "Unprompted: {} an hour, {} a day, {left} left today.",
            p.budget_per_hour, p.budget_per_day
        ),
        format!(
            "{} {}",
            quiet_line(p.quiet_hours),
            working_line(p.working_hours)
        ),
        dial_line(state),
        observing,
    ]
    .join("\n")
}

/// The dial, its chime-ins and the group's bits, on one line.
fn dial_line(state: &GroupState) -> String {
    let p = &state.profile;
    let mut line = format!("Dial at {}: {}", p.dial, dial_name(p.dial));
    let chimes = p.chimes_a_day();
    if p.dial >= crate::chime::CHIME_DIAL && chimes > 0 {
        line.push_str(&format!(", chiming in {chimes} times a day at most"));
    }
    line.push('.');
    if let Some(bits) = bits_line(state) {
        line.push(' ');
        line.push_str(&bits);
    }
    line.push_str(&format!(
        " Unanswered questions wait {}.",
        span_minutes(p.answer_window_seconds)
    ));
    line
}

/// "- Reactions here: 👍 approves, ...", one per platform that swaps any of
/// the agent's reactions for its own, named when the group has several.
fn reaction_lines(state: &GroupState) -> Vec<String> {
    let several = state.me.len() > 1;
    reactions::by_platform(state)
        .into_iter()
        .filter_map(|(platform, subs)| {
            let label = if several {
                format!("on {platform:?}")
            } else {
                "here".to_owned()
            };
            reactions::charter_line(&subs, &label)
        })
        .map(|line| format!("- {line}"))
        .collect()
}

/// The starter's name, "plain default", or "written by the host".
pub fn voice_name(state: &GroupState) -> String {
    match &state.persona_name {
        Some(name) => name.clone(),
        None => {
            let plain = Persona {
                bits: state.persona.bits.clone(),
                ..Persona::default()
            };
            if state.persona == plain {
                "plain default".to_owned()
            } else {
                "written by the host".to_owned()
            }
        }
    }
}

/// "a; b; c", or "none".
fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join("; ")
    }
}

fn leads_rule(rule: LeadsRule) -> &'static str {
    match rule {
        LeadsRule::Everyone => "everyone in the group",
        LeadsRule::Admins => "platform owners and admins, plus the named leads",
        LeadsRule::Moderators => "platform owners, admins and moderators, plus the named leads",
        LeadsRule::Named => "only the named leads",
    }
}

/// "Priya (`u1`), `u9`": display names where known, always with the id.
fn people(state: &GroupState, users: &BTreeSet<PlatformUserId>) -> String {
    if users.is_empty() {
        return "none".to_owned();
    }
    users
        .iter()
        .map(|u| {
            let name = state.name_of(u);
            if name == u.as_str() {
                format!("`{u}`")
            } else {
                format!("{name} (`{u}`)")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn cut(budget: &Budget) -> Option<String> {
    budget.cut.as_ref().map(|c| {
        format!(
            "- Shushed on {}: nothing unprompted for the rest of that day",
            c.day
        )
    })
}

fn observe_status(state: &GroupState, tz: &str) -> String {
    let counts = |o: &Observe| format!("{} would-have, {} 👍, {} 👎", o.would_have, o.up, o.down);
    match (&state.observe, &state.observed) {
        (Some(o), _) => format!(
            "watching since {}; leads hear what it saw at {}, and it keeps watching until a lead says go; so far {}",
            stamp(o.started, tz),
            stamp(o.until, tz),
            counts(o)
        ),
        (None, Some(o)) => format!(
            "ended; watched from {}, {}",
            stamp(o.started, tz),
            counts(o)
        ),
        (None, None) => "off".to_owned(),
    }
}

fn span_minutes(seconds: u32) -> String {
    let m = seconds / 60;
    match m {
        0 => format!("{seconds} seconds"),
        1 => "1 minute".to_owned(),
        _ if m.is_multiple_of(60) && m >= 120 => format!("{} hours", m / 60),
        _ => format!("{m} minutes"),
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
