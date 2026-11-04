//! The scenario format (benchmark/FORMAT.md): members, timed turns and what
//! a good group agent does after each one, in plain JSON.

use crate::bench::BenchError;
use ours_types::{Instant, Kind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The clock a scenario starts on when it names none: 2026-11-02 18:00 UTC, a Monday.
pub const DEFAULT_START: &str = "2026-11-02T18:00:00Z";
/// How long the clock runs after the last turn when a scenario sets no end.
pub const DEFAULT_TAIL_SECONDS: i64 = 60 * 60;
/// The room a turn is in when it names none.
pub const GENERAL: &str = "general";
/// The room name for a turn sent to the agent in a DM.
pub const DM: &str = "dm";
/// What `"to"` says to react to the agent's latest message in the room.
pub const AGENT: &str = "agent";
/// Room past the end for timers the engine arms during a run.
const HEADROOM_SECONDS: i64 = 400 * 24 * 60 * 60;

/// Every action name a scenario may expect or forbid.
pub const ACTIONS: [&str; 11] = [
    "open_poll",
    "record_decision",
    "store_fact",
    "store_commitment",
    "ledger_line",
    "hold_mail",
    "send_mail",
    "undo",
    "defer_to_lead",
    "decline",
    "change_setting",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub kind: Kind,
    pub description: String,
    /// Where it comes from, such as a section of DESIGN.md.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default = "default_agent_name")]
    pub agent_name: String,
    /// RFC 3339; the instant `t` counts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// The group's home zone, an IANA name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Seconds from start when the run stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    pub members: Vec<ScenarioMember>,
    /// Messages from before the start, for an agent that reads history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryLine>,
    pub turns: Vec<Turn>,
    /// Keyed by turn index or an inclusive range such as "0-39".
    pub expect: BTreeMap<String, Expectation>,
}

fn default_agent_name() -> String {
    "Noa".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioMember {
    pub id: String,
    pub name: String,
    pub role: RoleHint,
    /// Channels this member is in; every channel when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rooms: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleHint {
    Lead,
    Member,
    Guest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryLine {
    /// Seconds before the start.
    pub ago: i64,
    pub from: String,
    pub text: String,
    #[serde(default = "general")]
    pub room: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    /// Seconds from the start.
    pub t: i64,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default = "general")]
    pub room: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<TurnReaction>,
}

fn general() -> String {
    GENERAL.to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnReaction {
    pub to: Target,
    pub emoji: String,
}

/// A turn's message by index, or the agent's latest message in the room.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Target {
    Turn(usize),
    Named(String),
}

impl Target {
    pub fn is_agent(&self) -> bool {
        matches!(self, Target::Named(name) if name == AGENT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    Must,
    May,
    MustNot,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    /// A message in the turn's room, or a DM back for a turn sent in a DM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speak: Option<Need>,
    /// A reaction on someone's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub react: Option<Need>,
    /// A DM to anyone, other than the reply to a DM turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dm: Option<Need>,
    /// Each entry must appear in what it said; "a|b" accepts either.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_must_mention: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_must_not_mention: Vec<String>,
    /// One action or a list; "a|b" accepts either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Actions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_must_not: Vec<String>,
    /// Why, for the reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Actions {
    One(String),
    Many(Vec<String>),
}

impl Actions {
    pub fn names(&self) -> Vec<&str> {
        match self {
            Actions::One(one) => vec![one.as_str()],
            Actions::Many(many) => many.iter().map(String::as_str).collect(),
        }
    }
}

/// Alternatives in a content or action entry, trimmed.
pub fn alternatives(entry: &str) -> impl Iterator<Item = &str> {
    entry.split('|').map(str::trim).filter(|a| !a.is_empty())
}

impl Scenario {
    pub fn from_json(text: &str) -> Result<Scenario, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The instant `t = 0` stands for.
    pub fn start_instant(&self) -> Result<Instant, BenchError> {
        let text = self.start.as_deref().unwrap_or(DEFAULT_START);
        text.parse::<jiff::Timestamp>()
            .map(Instant)
            .map_err(|e| self.invalid(format!("start {text:?} is not RFC 3339: {e}")))
    }

    /// Seconds from start when the run stops.
    pub fn end_seconds(&self) -> i64 {
        let last = self.turns.last().map_or(0, |t| t.t);
        self.end
            .unwrap_or(last.saturating_add(DEFAULT_TAIL_SECONDS))
            .max(last)
    }

    pub fn member(&self, id: &str) -> Option<&ScenarioMember> {
        self.members.iter().find(|m| m.id == id)
    }

    /// Channels named anywhere, "general" first.
    pub fn channels(&self) -> Vec<String> {
        let mut rooms = vec![GENERAL.to_owned()];
        let named = self
            .turns
            .iter()
            .map(|t| t.room.as_str())
            .chain(self.history.iter().map(|h| h.room.as_str()))
            .chain(
                self.members
                    .iter()
                    .flat_map(|m| m.rooms.iter().flatten().map(String::as_str)),
            );
        for room in named {
            if is_channel(room) && !rooms.iter().any(|r| r == room) {
                rooms.push(room.to_owned());
            }
        }
        rooms
    }

    /// Whether `member` is in the channel `room`.
    pub fn in_channel(&self, member: &ScenarioMember, room: &str) -> bool {
        member
            .rooms
            .as_ref()
            .is_none_or(|rooms| rooms.iter().any(|r| r == room))
    }

    /// Whether turn `index` speaks to the agent: a DM, its name first, an
    /// @-mention of it, or a reaction on its message.
    pub fn addresses_agent(&self, index: usize) -> bool {
        let Some(turn) = self.turns.get(index) else {
            return false;
        };
        if turn.room == DM {
            return true;
        }
        if let Some(reaction) = &turn.reaction {
            return reaction.to.is_agent();
        }
        let text = turn.text.as_deref().unwrap_or("").trim_start();
        let text = text.strip_prefix('@').unwrap_or(text);
        let name = self.agent_name.to_lowercase();
        let lower = text.to_lowercase();
        lower.strip_prefix(&name).is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|c| c == ',' || c == ':' || c.is_whitespace())
        })
    }

    /// The expectation for each turn, by index.
    pub fn expectations(&self) -> Result<Vec<Option<Expectation>>, BenchError> {
        let mut out: Vec<Option<Expectation>> = vec![None; self.turns.len()];
        for (key, expectation) in &self.expect {
            let (first, last) = parse_key(key).ok_or_else(|| {
                self.invalid(format!(
                    "expect key {key:?} is not an index or a range like 0-3"
                ))
            })?;
            if last >= self.turns.len() {
                return Err(self.invalid(format!(
                    "expect key {key:?} names turn {last}, but there are {} turns",
                    self.turns.len()
                )));
            }
            for slot in &mut out[first..=last] {
                if slot.is_some() {
                    return Err(self.invalid(format!("expect key {key:?} overlaps another")));
                }
                *slot = Some(expectation.clone());
            }
        }
        Ok(out)
    }

    /// Checks everything the format promises, so a scenario that loads can be run.
    pub fn validate(&self) -> Result<(), BenchError> {
        if self.name.trim().is_empty() {
            return Err(self.invalid("the name is empty".to_owned()));
        }
        let start = self.start_instant()?;
        if let Some(zone) = &self.timezone {
            jiff::tz::TimeZone::get(zone)
                .map_err(|e| self.invalid(format!("timezone {zone:?}: {e}")))?;
        }
        if self.members.is_empty() {
            return Err(self.invalid("there are no members".to_owned()));
        }
        for (i, m) in self.members.iter().enumerate() {
            if self.members[..i].iter().any(|o| o.id == m.id) {
                return Err(self.invalid(format!("member id {:?} is used twice", m.id)));
            }
            if m.id == crate::bench::engine::AGENT_ID {
                return Err(self.invalid(format!("member id {:?} is the agent's", m.id)));
            }
        }
        for line in &self.history {
            if self.member(&line.from).is_none() {
                return Err(self.invalid(format!("history names unknown member {:?}", line.from)));
            }
            if line.ago <= 0 {
                return Err(self.invalid("history lines are before the start".to_owned()));
            }
            if offset(start, -line.ago).is_none() {
                return Err(self.invalid(format!(
                    "history line {} seconds ago is before the clock's range",
                    line.ago
                )));
            }
            if !is_channel(&line.room) {
                return Err(self.invalid(format!("history room {:?} is not a channel", line.room)));
            }
        }
        let mut last_t = 0;
        for (i, turn) in self.turns.iter().enumerate() {
            let at = |problem: String| self.invalid(format!("turn {i}: {problem}"));
            let member = self
                .member(&turn.from)
                .ok_or_else(|| at(format!("unknown member {:?}", turn.from)))?;
            if turn.t < last_t || turn.t < 0 {
                return Err(at("t goes backwards".to_owned()));
            }
            last_t = turn.t;
            if turn.room.is_empty() {
                return Err(at("the room is empty".to_owned()));
            }
            if is_channel(&turn.room) && !self.in_channel(member, &turn.room) {
                return Err(at(format!("{} is not in {}", turn.from, turn.room)));
            }
            match (&turn.text, &turn.reaction) {
                (Some(_), None) => {}
                (None, Some(reaction)) => match &reaction.to {
                    Target::Turn(to) => {
                        let target = self.turns.get(*to).filter(|_| *to < i).ok_or_else(|| {
                            at(format!("reacts to turn {to}, which is not earlier"))
                        })?;
                        if target.text.is_none() {
                            return Err(at(format!("reacts to turn {to}, which is a reaction")));
                        }
                        if target.room != turn.room {
                            return Err(at(format!("reacts to turn {to} in another room")));
                        }
                    }
                    Target::Named(name) if name == AGENT => {}
                    Target::Named(name) => {
                        return Err(at(format!(
                            "reaction target {name:?} is not a turn index or \"agent\""
                        )));
                    }
                },
                _ => return Err(at("a turn has text or a reaction, not both".to_owned())),
            }
        }
        if let Some(end) = self.end
            && end < last_t
        {
            return Err(self.invalid("end is before the last turn".to_owned()));
        }
        // Every turn falls at or before the end, so checking the end covers them.
        let end = self.end_seconds();
        if offset(start, end.saturating_add(HEADROOM_SECONDS)).is_none() {
            return Err(self.invalid(format!(
                "the run ends {end} seconds after the start, past the clock's range"
            )));
        }
        for (i, expectation) in self.expectations()?.iter().enumerate() {
            let Some(e) = expectation else { continue };
            let named = e
                .action
                .iter()
                .flat_map(Actions::names)
                .chain(e.action_must_not.iter().map(String::as_str))
                .flat_map(alternatives);
            for name in named {
                if !ACTIONS.contains(&name) {
                    return Err(self.invalid(format!("turn {i}: unknown action {name:?}")));
                }
            }
        }
        Ok(())
    }

    fn invalid(&self, problem: String) -> BenchError {
        BenchError::Invalid {
            scenario: self.name.clone(),
            problem,
        }
    }
}

/// `start` moved by `seconds`, or None when that falls outside the clock's range.
fn offset(start: Instant, seconds: i64) -> Option<Instant> {
    start
        .0
        .checked_add(jiff::SignedDuration::from_secs(seconds))
        .ok()
        .map(Instant)
}

/// Whether a turn's room is a channel rather than a DM or a thread.
pub fn is_channel(room: &str) -> bool {
    room != DM && !room.starts_with("thread:")
}

fn parse_key(key: &str) -> Option<(usize, usize)> {
    let key = key.trim();
    match key.split_once('-') {
        Some((a, b)) => {
            let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
            (a <= b).then_some((a, b))
        }
        None => key.parse().ok().map(|n| (n, n)),
    }
}

/// Reads one scenario file and checks it.
pub fn load_scenario(path: &Path) -> Result<Scenario, BenchError> {
    let text = std::fs::read_to_string(path).map_err(|source| BenchError::Read {
        path: path.to_owned(),
        source,
    })?;
    let scenario = Scenario::from_json(&text).map_err(|source| BenchError::Parse {
        path: path.to_owned(),
        what: "scenario",
        source,
    })?;
    scenario.validate()?;
    Ok(scenario)
}

/// Every `*.json` scenario in `dir`, in file name order.
pub fn load_scenarios(dir: &Path) -> Result<Vec<Scenario>, BenchError> {
    let mut paths = json_files(dir)?;
    paths.sort();
    let scenarios = paths
        .iter()
        .map(|p| load_scenario(p))
        .collect::<Result<Vec<_>, _>>()?;
    for (i, s) in scenarios.iter().enumerate() {
        if scenarios[..i].iter().any(|o| o.name == s.name) {
            return Err(BenchError::Invalid {
                scenario: s.name.clone(),
                problem: "two scenario files share this name".to_owned(),
            });
        }
    }
    Ok(scenarios)
}

pub(crate) fn json_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, BenchError> {
    let read = |source| BenchError::Read {
        path: dir.to_owned(),
        source,
    };
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(read)? {
        let path = entry.map_err(read)?.path();
        if path.extension().is_some_and(|e| e == "json") {
            paths.push(path);
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Scenario {
        Scenario::from_json(
            r#"{
                "name": "tiny", "kind": "friends", "description": "two lines",
                "members": [
                    { "id": "sam", "name": "Sam", "role": "lead" },
                    { "id": "jo", "name": "Jo", "role": "member" }
                ],
                "turns": [
                    { "t": 0, "from": "sam", "text": "Noa, hi" },
                    { "t": 5, "from": "jo", "reaction": { "to": 0, "emoji": "👍" } },
                    { "t": 9, "from": "jo", "reaction": { "to": "agent", "emoji": "🤫" } }
                ],
                "expect": { "0": { "speak": "must" }, "1-2": { "speak": "must_not" } }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn ranges_and_indexes_resolve_per_turn() {
        let s = tiny();
        s.validate().unwrap();
        let e = s.expectations().unwrap();
        assert_eq!(e[0].as_ref().unwrap().speak, Some(Need::Must));
        assert_eq!(e[2].as_ref().unwrap().speak, Some(Need::MustNot));
        assert!(s.addresses_agent(0));
        assert!(!s.addresses_agent(1));
        assert!(s.addresses_agent(2));
        assert_eq!(s.end_seconds(), 9 + DEFAULT_TAIL_SECONDS);
    }

    #[test]
    fn bad_scenarios_are_refused() {
        let mut s = tiny();
        s.expect.insert("2".into(), Expectation::default());
        assert!(s.validate().is_err(), "overlap");

        let mut s = tiny();
        s.expect.insert("7".into(), Expectation::default());
        assert!(s.validate().is_err(), "out of range");

        let mut s = tiny();
        s.turns[1].reaction = Some(TurnReaction {
            to: Target::Turn(2),
            emoji: "x".into(),
        });
        assert!(s.validate().is_err(), "reaction to a later turn");

        let mut s = tiny();
        s.expect.get_mut("0").unwrap().action = Some(Actions::One("teleport".into()));
        assert!(s.validate().is_err(), "unknown action");
    }

    #[test]
    fn times_past_the_clock_are_refused_not_run() {
        let mut s = tiny();
        s.turns[2].t = i64::MAX;
        assert!(s.validate().is_err(), "turn past the clock");

        let mut s = tiny();
        s.end = Some(i64::MAX);
        assert!(s.validate().is_err(), "end past the clock");

        let mut s = tiny();
        s.start = Some("9999-12-01T00:00:00Z".into());
        assert!(s.validate().is_err(), "no room after the end");

        let mut s = tiny();
        s.history.push(HistoryLine {
            ago: i64::MAX,
            from: "sam".into(),
            text: "long ago".into(),
            room: GENERAL.into(),
        });
        assert!(s.validate().is_err(), "history before the clock");

        let mut s = tiny();
        s.end = Some(30 * 24 * 60 * 60);
        s.validate().expect("a month is fine");
    }

    #[test]
    fn the_name_must_stand_alone_to_address() {
        let mut s = tiny();
        s.turns[0].text = Some("noa-final tickets anyone?".into());
        assert!(!s.addresses_agent(0));
        s.turns[0].text = Some("@noa what's up".into());
        assert!(s.addresses_agent(0));
    }
}
