//! The host's export of a group (DESIGN.md sections 9 and 15), for a data
//! request or a move: JSON that reads back through serde, and Markdown a
//! person can read. It holds what the group keeps in every scope, labelled.
//! Messages themselves stay in the group's journal.

use crate::action::{ActionRecord, ActionStatus, tier_name};
use crate::authority::{ScopedLead, group_role};
use crate::charter::render_charter;
use crate::commitment::{Commitment, CommitmentStatus};
use crate::decision::{Decision, Entry, render_decisions};
use crate::memory::{self, Fact, FactScope, link, source_label};
use crate::persona::{Persona, role_name};
use crate::profile::{Jobs, Profile};
use crate::proposal::{CloseRule, Proposal};
use crate::recur::Recurrence;
use crate::retention::retention_days;
use crate::settings::{dial_name, kind_name};
use crate::state::{Deferral, GroupState};
use crate::tab::{LedgerLine, Settlement, balances, describe, money};
use crate::zone::stamp;
use ours_types::{
    GroupId, Instant, Kind, Platform, PlatformMessageId, PlatformRoomId, PlatformUserId, Role,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::warn;

/// Bumped when a field changes meaning or goes away.
pub const EXPORT_FORMAT: u32 = 1;

/// One export, in both forms.
#[derive(Debug, Clone, PartialEq)]
pub struct Export {
    pub json: serde_json::Value,
    pub markdown: String,
}

/// Everything the JSON half carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportData {
    pub format: u32,
    pub exported_at: Instant,
    pub group: GroupId,
    pub agent_name: String,
    pub kind: Kind,
    pub persona_name: Option<String>,
    pub persona: Persona,
    pub profile: Profile,
    /// The scheduled jobs in effect, the kind's when nobody changed them.
    pub jobs: Jobs,
    pub timezone: String,
    pub currency: String,
    pub model_label: String,
    /// How new proposals close, when the group chose; else the kind's rule.
    pub decision_rule: Option<CloseRule>,
    pub legal_hold: bool,
    pub legal_hold_since: Option<Instant>,
    /// Days the action log is kept when there is no legal hold.
    pub retention_days: u32,
    pub rooms: Vec<ExportRoom>,
    pub members: Vec<ExportMember>,
    pub leads: Vec<PlatformUserId>,
    pub hosts: Vec<PlatformUserId>,
    pub scoped_leads: Vec<ScopedLead>,
    pub facts: Vec<ExportFact>,
    pub proposals: Vec<Proposal>,
    pub decisions: Vec<Decision>,
    pub calendar: Vec<Entry>,
    pub commitments: Vec<Commitment>,
    pub ledger: Vec<LedgerLine>,
    /// Net balance per member in the tab's currency, in cents.
    pub balances: BTreeMap<PlatformUserId, i64>,
    pub settlements: Vec<Settlement>,
    pub payment_links: BTreeMap<PlatformUserId, String>,
    pub recurrences: Vec<Recurrence>,
    pub actions: Vec<ActionRecord>,
    /// Requests waiting for a lead's ✅, by the agent's message that asks.
    pub deferrals: BTreeMap<PlatformMessageId, Deferral>,
    /// The charter as Markdown.
    pub charter: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportRoom {
    pub id: PlatformRoomId,
    pub name: String,
    pub dm: bool,
    pub platform: Option<Platform>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportMember {
    pub id: PlatformUserId,
    pub name: String,
    /// The best role they hold anywhere in the group.
    pub role: Role,
    pub named_lead: bool,
    pub host: bool,
    /// The rooms other than DMs they are in.
    pub rooms: Vec<PlatformRoomId>,
    /// Their own zone when known; otherwise the group's applies.
    pub zone: Option<String>,
    pub bot: bool,
    /// Whether the hour they usually reply is kept, for timing only. The
    /// hour itself is never reported (DESIGN.md section 5).
    pub reply_hour_kept: bool,
}

/// A fact with who may hear it and where it stands, in words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportFact {
    /// "said in the group", "said by Sam about themselves", "private to Sam".
    pub scope: String,
    /// "lasting", "until 2026-11-14", "expired 2026-11-04", "forgotten 2026-11-03 (forget me)".
    pub status: String,
    pub fact: Fact,
}

/// Everything `state` holds, as JSON and as Markdown.
pub fn render_export(state: &GroupState, now: Instant) -> Export {
    let data = export_data(state, now);
    let markdown = markdown(state, &data, now);
    let json = serde_json::to_value(&data).unwrap_or_else(|error| {
        warn!(%error, "export did not serialise");
        serde_json::Value::Null
    });
    Export { json, markdown }
}

/// The JSON half as a typed value.
pub fn export_data(state: &GroupState, now: Instant) -> ExportData {
    let mut facts: Vec<&Fact> = state.facts.values().collect();
    facts.sort_by(|a, b| a.first_seen.cmp(&b.first_seen).then(a.id.cmp(&b.id)));
    let mut decisions: Vec<Decision> = state.decisions.values().cloned().collect();
    decisions.sort_by(|a, b| a.closed_at.cmp(&b.closed_at).then(a.id.cmp(&b.id)));
    ExportData {
        format: EXPORT_FORMAT,
        exported_at: now,
        group: state.id.clone(),
        agent_name: state.agent_name.clone(),
        kind: state.kind,
        persona_name: state.persona_name.clone(),
        persona: state.persona.clone(),
        profile: state.profile.clone(),
        jobs: state.jobs(),
        timezone: state.timezone.clone(),
        currency: state.currency.clone(),
        model_label: state.model_label.clone(),
        decision_rule: state.decision_rule.clone(),
        legal_hold: state.legal_hold,
        legal_hold_since: state.legal_hold_since,
        retention_days: retention_days(state.kind),
        rooms: rooms(state),
        members: members(state),
        leads: state.leads.iter().cloned().collect(),
        hosts: state.hosts.iter().cloned().collect(),
        scoped_leads: state.scoped_leads.clone(),
        facts: facts
            .into_iter()
            .map(|f| ExportFact {
                scope: scope_label(state, f),
                status: status_label(state, f, now),
                fact: f.clone(),
            })
            .collect(),
        proposals: state.proposals.values().cloned().collect(),
        decisions,
        calendar: state.calendar.clone(),
        commitments: state.commitments.values().cloned().collect(),
        ledger: state.ledger.clone(),
        balances: balances(state),
        settlements: state.settlements.values().cloned().collect(),
        payment_links: state.payment_links.clone(),
        recurrences: state.recurrences.clone(),
        actions: state.actions.iter().cloned().collect(),
        deferrals: state.deferrals.clone(),
        charter: render_charter(state),
    }
}

fn rooms(state: &GroupState) -> Vec<ExportRoom> {
    state
        .rooms
        .iter()
        .map(|(id, r)| ExportRoom {
            id: id.clone(),
            name: r.name(id),
            dm: r.is_dm(),
            platform: r.info.as_ref().map(|i| i.platform),
        })
        .collect()
}

/// Everyone any room knows, the agent's own accounts left out.
fn members(state: &GroupState) -> Vec<ExportMember> {
    let mut seen: BTreeMap<PlatformUserId, ExportMember> = BTreeMap::new();
    for (room_id, room) in &state.rooms {
        for m in room.members.values().filter(|m| !state.is_me(&m.id)) {
            let entry = seen.entry(m.id.clone()).or_insert_with(|| ExportMember {
                id: m.id.clone(),
                name: m.display_name.clone(),
                role: group_role(state, &m.id),
                named_lead: state.leads.contains(&m.id),
                host: state.hosts.contains(&m.id),
                rooms: Vec::new(),
                zone: state.zones.get(&m.id).map(|z| z.name.clone()),
                bot: m.is_bot,
                reply_hour_kept: state.patterns.members.contains_key(&m.id),
            });
            if !room.is_dm() {
                entry.rooms.push(room_id.clone());
            }
        }
    }
    seen.into_values().collect()
}

fn scope_label(state: &GroupState, fact: &Fact) -> String {
    match &fact.scope {
        FactScope::Group => "said in the group".to_owned(),
        FactScope::SelfStated(user) => {
            format!("said by {} about themselves", state.name_of(user))
        }
        FactScope::Private(user) => format!("private to {}", state.name_of(user)),
    }
}

fn status_label(state: &GroupState, fact: &Fact, now: Instant) -> String {
    let tz = state.timezone.as_str();
    if let Some(r) = &fact.retracted {
        let what = if fact.is_forgotten() {
            "forgotten"
        } else {
            "retracted"
        };
        return format!("{what} {} ({})", memory::day(r.at, tz), r.reason);
    }
    match fact.expires {
        None => "lasting".to_owned(),
        Some(at) if at <= now => format!("expired {}", memory::last_day(at, tz)),
        Some(at) => format!("until {}", memory::last_day(at, tz)),
    }
}

fn markdown(state: &GroupState, data: &ExportData, now: Instant) -> String {
    let tz = state.timezone.as_str();
    let mut out = vec![
        format!("# Export of group {}", state.id),
        String::new(),
        format!(
            "Exported {} ({tz}). Everything this group's agent holds: people, facts in every \
             scope, decisions, commitments, the tab, the action log and the charter. Messages \
             themselves are not part of it; they stay in the group's journal.",
            stamp(now, tz)
        ),
        String::new(),
    ];
    section(&mut out, "The group", group_lines(state, data));
    section(&mut out, "Rooms", room_lines(data));
    section(&mut out, "People", people_lines(state, data));
    section(&mut out, "Leads and hosts", lead_lines(state, data));
    out.push("## Facts".to_owned());
    out.push(String::new());
    fact_lines(state, data, &mut out);
    out.push("## Decisions".to_owned());
    out.push(String::new());
    out.extend(demoted(&render_decisions(state)));
    section(&mut out, "Commitments", commitment_lines(state, data));
    section(&mut out, "The tab", tab_lines(state, data));
    section(&mut out, "Action log", action_lines(state, data));
    section(&mut out, "Waiting for a lead", deferral_lines(state, data));
    out.push("## Charter".to_owned());
    out.push(String::new());
    out.extend(demoted(&data.charter));
    let mut text = out.join("\n").trim_end().to_owned();
    text.push('\n');
    text
}

fn section(out: &mut Vec<String>, title: &str, lines: Vec<String>) {
    out.push(format!("## {title}"));
    out.push(String::new());
    if lines.is_empty() {
        out.push("- none".to_owned());
    } else {
        out.extend(lines);
    }
    out.push(String::new());
}

/// Another Markdown document under this one: its title dropped and every
/// heading one level down.
fn demoted(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| l.starts_with("# "))
        .skip_while(|l| l.is_empty())
        .map(|l| {
            if l.starts_with('#') {
                format!("#{l}")
            } else {
                l.to_owned()
            }
        })
        .chain(std::iter::once(String::new()))
        .collect()
}

fn group_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    let voice = data.persona_name.as_deref().unwrap_or("plain default");
    let hold = match (data.legal_hold, data.legal_hold_since) {
        (true, Some(since)) => format!(
            "on since {}; nothing expires",
            memory::day(since, &state.timezone)
        ),
        (true, None) => "on; nothing expires".to_owned(),
        (false, _) => "off".to_owned(),
    };
    vec![
        format!("- Kind: {}", kind_name(data.kind)),
        format!("- Agent name: {}", data.agent_name),
        format!(
            "- Voice: {voice} (dial {}, {})",
            data.profile.dial,
            dial_name(data.profile.dial)
        ),
        format!("- Talks like: {}", data.persona.voice),
        format!("- Time zone: {}", data.timezone),
        format!("- Tab currency: {}", data.currency),
        format!(
            "- Model: {}",
            if data.model_label.is_empty() {
                "not set"
            } else {
                &data.model_label
            }
        ),
        format!(
            "- Retention: action log kept {} days; perishable facts dropped once they expire",
            data.retention_days
        ),
        format!("- Legal hold: {hold}"),
    ]
}

fn room_lines(data: &ExportData) -> Vec<String> {
    data.rooms
        .iter()
        .map(|r| {
            if r.dm {
                format!("- A DM (`{}`)", r.id)
            } else {
                format!("- #{} (`{}`)", r.name, r.id)
            }
        })
        .collect()
}

fn people_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    data.members
        .iter()
        .map(|m| {
            let rooms: Vec<String> = m.rooms.iter().map(|r| state.room_label(r)).collect();
            let mut line = format!("- {} (`{}`): {}", m.name, m.id, role_name(m.role));
            if m.bot {
                line.push_str(", a bot");
            }
            if !rooms.is_empty() {
                line.push_str(&format!(", in {}", rooms.join(", ")));
            }
            line.push_str(&format!(
                "; zone {}",
                m.zone.as_deref().unwrap_or("the group's")
            ));
            if m.reply_hour_kept {
                line.push_str("; reply hour kept for timing only");
            }
            line
        })
        .collect()
}

fn names(state: &GroupState, users: &[PlatformUserId]) -> String {
    if users.is_empty() {
        return "none".to_owned();
    }
    users
        .iter()
        .map(|u| format!("{} (`{u}`)", state.name_of(u)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn lead_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    let mut lines = vec![
        format!("- Named leads: {}", names(state, &data.leads)),
        format!("- Hosts: {}", names(state, &data.hosts)),
    ];
    for s in &data.scoped_leads {
        let over = match &s.scope {
            crate::authority::Scope::Room { room } => state.room_label(room),
            crate::authority::Scope::Event { name } => name.clone(),
            crate::authority::Scope::Proposal { id } => format!("proposal {id}"),
        };
        let until = s.expires.map_or_else(String::new, |e| {
            format!(" until {}", stamp(e, &state.timezone))
        });
        lines.push(format!(
            "- Lead over {over}: {}{until}",
            state.name_of(&s.user)
        ));
    }
    lines
}

fn fact_lines(state: &GroupState, data: &ExportData, out: &mut Vec<String>) {
    let titles = [
        "Said in the group",
        "Said by members about themselves",
        "Private, said in a DM",
    ];
    let bucket = |scope: &FactScope| match scope {
        FactScope::Group => 0,
        FactScope::SelfStated(_) => 1,
        FactScope::Private(_) => 2,
    };
    for (i, title) in titles.into_iter().enumerate() {
        let lines: Vec<String> = data
            .facts
            .iter()
            .filter(|f| !f.fact.is_forgotten() && bucket(&f.fact.scope) == i)
            .map(|f| fact_line(state, f))
            .collect();
        out.push(format!("### {title}"));
        out.push(String::new());
        if lines.is_empty() {
            out.push("- none".to_owned());
        } else {
            out.extend(lines);
        }
        out.push(String::new());
    }
    let removed: Vec<String> = data
        .facts
        .iter()
        .filter(|f| f.fact.is_forgotten())
        .map(|f| {
            format!(
                "- `{}`: {}, first seen {}; its words are gone",
                f.fact.id,
                f.status,
                memory::day(f.fact.first_seen, &state.timezone)
            )
        })
        .collect();
    out.push("### Removed".to_owned());
    out.push(String::new());
    if removed.is_empty() {
        out.push("- none".to_owned());
    } else {
        out.extend(removed);
    }
    out.push(String::new());
}

fn fact_line(state: &GroupState, f: &ExportFact) -> String {
    let sources: Vec<String> = f
        .fact
        .sources
        .iter()
        .map(|s| {
            let at = link(state, s).unwrap_or_else(|| format!("msg {}", s.message));
            format!("{}, {at}", source_label(state, s))
        })
        .collect();
    let from = if sources.is_empty() {
        String::new()
    } else {
        format!(" From {}.", sources.join("; "))
    };
    format!(
        "- {}. {}; {}.{from}",
        f.fact.said(),
        capitalised(&f.scope),
        f.status
    )
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn commitment_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    let tz = state.timezone.as_str();
    data.commitments
        .iter()
        .map(|c| {
            let due = match c.due {
                Some(d) if c.due_has_time => format!(", due {}", stamp(d, tz)),
                Some(d) => format!(", due {}", memory::last_day(d, tz)),
                None => String::new(),
            };
            let status = match &c.status {
                CommitmentStatus::Open => "open".to_owned(),
                CommitmentStatus::Done { at } => format!("done {}", memory::day(*at, tz)),
                CommitmentStatus::Moved { to, .. } => format!("moved to {}", stamp(*to, tz)),
                CommitmentStatus::Dropped { at, .. } => {
                    format!("dropped {}", memory::day(*at, tz))
                }
            };
            format!(
                "- {}: {}{due}, {status}. From {}.",
                state.name_of(&c.who),
                c.what,
                source_label(state, &c.source)
            )
        })
        .collect()
}

fn tab_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    let tz = state.timezone.as_str();
    let mut lines: Vec<String> = data
        .ledger
        .iter()
        .map(|l| {
            let voided = if l.voided.is_some() { ", voided" } else { "" };
            format!(
                "- {}, {}{voided}",
                describe(state, l),
                memory::day(l.created_at, tz)
            )
        })
        .collect();
    let owed: Vec<String> = data
        .balances
        .iter()
        .filter(|(_, c)| **c != 0)
        .map(|(u, c)| {
            let sign = if *c > 0 { "+" } else { "" };
            format!("{} {sign}{}", state.name_of(u), money(*c))
        })
        .collect();
    if !owed.is_empty() {
        lines.push(format!(
            "- Balances in {}: {}",
            data.currency,
            owed.join(", ")
        ));
    }
    for user in data.payment_links.keys() {
        lines.push(format!(
            "- {} gave a payment link, shown only to people who owe them",
            state.name_of(user)
        ));
    }
    lines
}

fn action_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    let tz = state.timezone.as_str();
    data.actions
        .iter()
        .map(|r| {
            let status = match &r.status {
                ActionStatus::Done => "done".to_owned(),
                ActionStatus::Held { until } => format!("held until {}", stamp(*until, tz)),
                ActionStatus::Sending { .. } => "sending".to_owned(),
                ActionStatus::Sent { at, .. } => format!("sent {}", stamp(*at, tz)),
                ActionStatus::Undone { at, by } => match by {
                    Some(by) => format!("undone {} by {}", stamp(*at, tz), state.name_of(by)),
                    None => format!("undone {}", stamp(*at, tz)),
                },
                ActionStatus::Failed { error } => format!("failed: {error}"),
                ActionStatus::Declined => "declined".to_owned(),
            };
            let mut line = format!(
                "- {} in {}: {}, {}, tier {}",
                stamp(r.at, tz),
                state.room_label(&r.room),
                r.summary,
                status,
                tier_name(r.tier)
            );
            if let Some(by) = &r.requested_by {
                line.push_str(&format!(", asked by {}", state.name_of(by)));
            }
            if let Some(by) = &r.approved_by {
                line.push_str(&format!(", approved by {}", state.name_of(by)));
            }
            if let Some(outcome) = &r.outcome {
                line.push_str(&format!(", outcome: {outcome}"));
            }
            line
        })
        .collect()
}

fn deferral_lines(state: &GroupState, data: &ExportData) -> Vec<String> {
    data.deferrals
        .values()
        .map(|d| {
            format!(
                "- {} asked in {}: {}; lapses {}",
                state.name_of(&d.asked_by),
                state.room_label(&d.room),
                crate::trace::quote(&d.request_text, 80),
                stamp(d.expires, &state.timezone)
            )
        })
        .collect()
}
