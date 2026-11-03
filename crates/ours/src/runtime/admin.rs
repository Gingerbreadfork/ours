//! The binary's side of the admin page: what the page may read and the
//! channel the engine loop announces on.

use crate::runtime::group::WipeRequest;
use async_trait::async_trait;
use ours_admin::{
    AdminEvent, AdminSource, ComplianceReport, FactCounts, GroupFacts, GroupSummary, HostFacts,
};
use ours_core::Input;
use ours_store::{EventLog, StoreDir};
use ours_types::GroupId;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, mpsc, oneshot};

const RECENT_KEPT: usize = 500;
const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Default)]
struct GroupView {
    summary: Option<GroupSummary>,
    state: Option<serde_json::Value>,
    charter: Option<String>,
    decision_log: Option<String>,
    actions: Vec<serde_json::Value>,
    feed_secret: Option<String>,
    ics_group: Option<String>,
    ics_members: HashMap<String, String>,
    recent: VecDeque<AdminEvent>,
    /// The running group's queue, for commands the page may send.
    commands: Option<mpsc::Sender<Input>>,
    wipes: Option<mpsc::Sender<WipeRequest>>,
}

/// Shared between the engine loops, which write, and the admin server,
/// which reads. Clone it to hand the server its copy.
#[derive(Debug, Clone)]
pub struct AdminHub {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    groups: RwLock<HashMap<GroupId, GroupView>>,
    catalogue: RwLock<Vec<serde_json::Value>>,
    sender: broadcast::Sender<AdminEvent>,
    host: RwLock<Option<HostFacts>>,
    store: RwLock<Option<Arc<StoreDir>>>,
}

impl Default for AdminHub {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminHub {
    pub fn new() -> Self {
        AdminHub {
            inner: Arc::new(Inner {
                groups: RwLock::new(HashMap::new()),
                catalogue: RwLock::new(Vec::new()),
                sender: broadcast::channel(CHANNEL_CAPACITY).0,
                host: RwLock::new(None),
                store: RwLock::new(None),
            }),
        }
    }

    /// The queues a running group answers on: commands from the page and
    /// wipe requests.
    pub fn set_controls(
        &self,
        group: &GroupId,
        commands: mpsc::Sender<Input>,
        wipes: mpsc::Sender<WipeRequest>,
    ) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        let view = groups.entry(group.clone()).or_default();
        view.commands = Some(commands);
        view.wipes = Some(wipes);
    }

    /// Forgets everything the page knew about a wiped group except its queues.
    pub fn clear_group(&self, group: &GroupId) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        if let Some(view) = groups.get_mut(group) {
            let commands = view.commands.take();
            let wipes = view.wipes.take();
            *view = GroupView {
                commands,
                wipes,
                ..GroupView::default()
            };
        }
    }

    fn commands_of(&self, group: &GroupId) -> Option<mpsc::Sender<Input>> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        groups.get(group).and_then(|v| v.commands.clone())
    }

    fn wipes_of(&self, group: &GroupId) -> Option<mpsc::Sender<WipeRequest>> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        groups.get(group).and_then(|v| v.wipes.clone())
    }

    /// Sends a command into a running group; false when it is not running.
    pub async fn command(&self, group: &GroupId, command: ours_core::Command) -> bool {
        match self.commands_of(group) {
            Some(tx) => tx.send(Input::Command(command)).await.is_ok(),
            None => false,
        }
    }

    /// What the compliance page says about this host.
    pub fn set_host_facts(&self, facts: HostFacts) {
        *self.inner.host.write().unwrap_or_else(|e| e.into_inner()) = Some(facts);
    }

    /// The store, for journal counts and file sizes on the compliance page.
    pub fn set_store(&self, store: Arc<StoreDir>) {
        *self.inner.store.write().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    fn group_facts(&self, summary: &GroupSummary, state: &serde_json::Value) -> GroupFacts {
        let store = self
            .inner
            .store
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let path = store.as_ref().and_then(|s| s.group_path(&summary.id).ok());
        let file_bytes = path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
        let stats = store
            .as_ref()
            .and_then(|s| s.open_group(&summary.id).ok())
            .and_then(|s| s.stats().ok());
        let count = |key: &str| {
            state.get(key).map_or(0, |v| match v {
                serde_json::Value::Object(o) => o.len(),
                serde_json::Value::Array(a) => a.len(),
                _ => 0,
            }) as u32
        };
        let mut facts = FactCounts::default();
        if let Some(all) = state.get("facts").and_then(|v| v.as_object()) {
            for fact in all.values() {
                let scope = fact.get("scope");
                let limited = fact
                    .get("rooms")
                    .and_then(|r| r.as_array())
                    .is_some_and(|r| !r.is_empty());
                match scope {
                    Some(serde_json::Value::String(s)) if s == "group" => {
                        if limited {
                            facts.room += 1
                        } else {
                            facts.group += 1
                        }
                    }
                    Some(serde_json::Value::Object(o)) if o.contains_key("private") => {
                        facts.private += 1
                    }
                    Some(serde_json::Value::Object(o)) if o.contains_key("self_stated") => {
                        facts.self_stated += 1
                    }
                    _ => facts.group += 1,
                }
            }
        }
        let members: std::collections::BTreeSet<&str> = state
            .get("rooms")
            .and_then(|r| r.as_object())
            .map(|rooms| {
                rooms
                    .values()
                    .filter_map(|r| r.get("members").and_then(|m| m.as_object()))
                    .flat_map(|m| m.keys().map(String::as_str))
                    .collect()
            })
            .unwrap_or_default();
        let kind = state
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("friends")
            .to_owned();
        let parsed: Option<ours_types::Kind> =
            serde_json::from_value(serde_json::Value::String(kind.clone())).ok();
        GroupFacts {
            id: summary.id.clone(),
            name: summary.name.clone(),
            kind,
            file: path.map(|p| p.display().to_string()).unwrap_or_default(),
            file_bytes,
            retention_days: parsed.map_or(90, crate::runtime::retention_days),
            legal_hold: state
                .get("legal_hold")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            members: members.len() as u32,
            leads: count("leads"),
            facts,
            decisions: count("decisions"),
            commitments: count("commitments"),
            ledger_lines: count("ledger"),
            actions: count("actions"),
            journal_records: stats.as_ref().map_or(0, |s| s.records),
            redacted_records: stats.as_ref().map_or(0, |s| s.redacted),
            snapshots: stats.as_ref().map_or(0, |s| s.snapshots as u32),
        }
    }

    pub fn set_summary(&self, summary: GroupSummary) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        let id = summary.id.clone();
        groups.entry(id).or_default().summary = Some(summary);
    }

    pub fn set_charter(&self, group: &GroupId, charter: String) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        groups.entry(group.clone()).or_default().charter = Some(charter);
    }

    pub fn set_feed_secret(&self, group: &GroupId, secret: String) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        groups.entry(group.clone()).or_default().feed_secret = Some(secret);
    }

    /// The calendar texts the ICS routes serve: the group's and one per member.
    pub fn set_calendars(
        &self,
        group: &GroupId,
        group_ics: String,
        members: HashMap<String, String>,
    ) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        let view = groups.entry(group.clone()).or_default();
        view.ics_group = Some(group_ics);
        view.ics_members = members;
    }

    /// Constant-time check of a feed secret, so a probe learns nothing from timing.
    fn feed_secret_matches(view: &GroupView, given: &str) -> bool {
        let Some(expected) = view.feed_secret.as_deref() else {
            return false;
        };
        let (a, b) = (expected.as_bytes(), given.as_bytes());
        let mut diff = u8::from(a.len() != b.len());
        for (i, x) in a.iter().enumerate() {
            diff |= x ^ b.get(i).copied().unwrap_or(0);
        }
        diff == 0
    }

    /// The engine's action records, newest first, with names resolved.
    pub fn set_actions(&self, group: &GroupId, actions: Vec<serde_json::Value>) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        groups.entry(group.clone()).or_default().actions = actions;
    }

    pub fn set_catalogue(&self, catalogue: Vec<serde_json::Value>) {
        *self
            .inner
            .catalogue
            .write()
            .unwrap_or_else(|e| e.into_inner()) = catalogue;
    }

    pub fn set_decision_log(&self, group: &GroupId, log: String) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        groups.entry(group.clone()).or_default().decision_log = Some(log);
    }

    pub fn set_state(&self, group: &GroupId, state: serde_json::Value) {
        let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
        groups.entry(group.clone()).or_default().state = Some(state);
    }

    /// Records the event for the page's backlog and announces it. Call this
    /// only after the input or effect it describes has been persisted.
    pub fn publish(&self, event: AdminEvent) {
        {
            let mut groups = self.inner.groups.write().unwrap_or_else(|e| e.into_inner());
            let view = groups.entry(event.group.clone()).or_default();
            view.recent.push_back(event.clone());
            while view.recent.len() > RECENT_KEPT {
                view.recent.pop_front();
            }
        }
        let _ = self.inner.sender.send(event);
    }
}

/// The engine's facts from a state snapshot, each source given a link where
/// the room's platform has one.
fn facts_with_links(state: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(facts) = state.get("facts").and_then(|f| f.as_object()) else {
        return Vec::new();
    };
    let rooms = state.get("rooms").and_then(|r| r.as_object());
    facts
        .values()
        .cloned()
        .map(|mut fact| {
            if let Some(sources) = fact.get_mut("sources").and_then(|s| s.as_array_mut()) {
                for source in sources {
                    let link = source
                        .get("room")
                        .and_then(|r| r.as_str())
                        .and_then(|room_id| {
                            let info = rooms?.get(room_id)?.get("info")?;
                            let platform: ours_types::Platform =
                                serde_json::from_value(info.get("platform")?.clone()).ok()?;
                            let container = info.get("container").and_then(|c| c.as_str());
                            let message = source.get("message")?.as_str()?;
                            ours_types::message_link(
                                platform,
                                container,
                                &ours_types::PlatformRoomId::new(room_id),
                                &ours_types::PlatformMessageId::new(message),
                            )
                        });
                    if let (Some(link), Some(obj)) = (link, source.as_object_mut()) {
                        obj.insert("link".to_owned(), serde_json::Value::String(link));
                    }
                }
            }
            fact
        })
        .collect()
}

/// Open proposals, closed decisions and calendar entries from a state
/// snapshot. Maps are flattened to their values; a proposal counts as open
/// unless its status says otherwise.
fn decisions_view(state: &serde_json::Value) -> serde_json::Value {
    fn values(value: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
        match value {
            Some(serde_json::Value::Object(map)) => map.values().cloned().collect(),
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        }
    }
    let open: Vec<serde_json::Value> = values(state.get("proposals"))
        .into_iter()
        .filter(|p| {
            p.get("status")
                .map(|s| s.as_str() == Some("open") || s.get("open").is_some())
                .unwrap_or(true)
        })
        .collect();
    let mut closed = values(state.get("decisions"));
    closed.reverse();
    let upcoming = values(state.get("calendar"));
    serde_json::json!({ "open": open, "closed": closed, "upcoming": upcoming })
}

/// Commitments, ledger lines and balances from a state snapshot, with user
/// ids resolved to display names where a room knows them. Balances come
/// from the state's own `balances` when present.
fn ledger_view(state: &serde_json::Value) -> serde_json::Value {
    fn values(value: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
        match value {
            Some(serde_json::Value::Object(map)) => map.values().cloned().collect(),
            Some(serde_json::Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        }
    }
    let name_of = |id: &str| -> String {
        state
            .get("rooms")
            .and_then(|r| r.as_object())
            .and_then(|rooms| {
                rooms.values().find_map(|room| {
                    room.get("members")?
                        .get(id)?
                        .get("display_name")?
                        .as_str()
                        .map(str::to_owned)
                })
            })
            .unwrap_or_else(|| id.to_owned())
    };
    let mut commitments = values(state.get("commitments"));
    for c in &mut commitments {
        if let Some(who) = c.get("who").and_then(|w| w.as_str()).map(str::to_owned) {
            c["who_name"] = serde_json::Value::String(name_of(&who));
        }
    }
    let mut lines = values(state.get("ledger"));
    lines.reverse();
    for l in &mut lines {
        if let Some(payer) = l.get("payer").and_then(|w| w.as_str()).map(str::to_owned) {
            l["payer_name"] = serde_json::Value::String(name_of(&payer));
        }
    }
    let balances: Vec<serde_json::Value> = match state.get("balances") {
        Some(serde_json::Value::Object(map)) => map
            .iter()
            .map(|(user, cents)| serde_json::json!({ "user": user, "name": name_of(user), "cents": cents }))
            .collect(),
        Some(serde_json::Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let currency = state.get("currency").and_then(|c| c.as_str()).unwrap_or("");
    serde_json::json!({ "commitments": commitments, "lines": lines, "balances": balances, "currency": currency })
}

impl AdminHub {
    /// Asks the group's runner to wipe and waits for the fresh one.
    async fn request_wipe(&self, group: &GroupId) -> Result<(), String> {
        let Some(tx) = self.wipes_of(group) else {
            return Err("that group is not running".to_owned());
        };
        let (done, finished) = oneshot::channel();
        tx.send(done)
            .await
            .map_err(|_| "the group stopped before it could be wiped".to_owned())?;
        match tokio::time::timeout(std::time::Duration::from_secs(60), finished).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("the wipe was abandoned".to_owned()),
            Err(_) => Err("the wipe did not finish in time".to_owned()),
        }
    }
}

#[async_trait]
impl AdminSource for AdminHub {
    async fn wipe(&self, group: &GroupId) -> Result<(), String> {
        self.request_wipe(group).await
    }

    async fn export(&self, group: &GroupId) -> Option<ours_admin::Export> {
        let state: ours_core::GroupState = serde_json::from_value(self.state(group).await?).ok()?;
        let now = ours_types::Instant::from(jiff::Timestamp::now());
        let export = ours_core::render_export(&state, now);
        Some(ours_admin::Export {
            json: serde_json::to_string_pretty(&export.json).ok()?,
            markdown: export.markdown,
        })
    }

    async fn set_legal_hold(&self, group: &GroupId, on: bool) -> bool {
        self.command(group, ours_core::Command::SetLegalHold { on })
            .await
    }

    async fn compliance(&self) -> ComplianceReport {
        let host = self
            .inner
            .host
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        let mut groups = Vec::new();
        for summary in self.groups().await {
            let state = self
                .state(&summary.id)
                .await
                .unwrap_or(serde_json::Value::Null);
            groups.push(self.group_facts(&summary, &state));
        }
        ComplianceReport { host, groups }
    }

    async fn persona(&self, group: &GroupId) -> Option<serde_json::Value> {
        let state = self.state(group).await?;
        let mut persona = state.get("persona")?.clone();
        if let Some(object) = persona.as_object_mut() {
            if let Some(name) = state.get("persona_name").cloned() {
                object.insert("name".to_owned(), name);
            }
            if let Some(dial) = state.pointer("/profile/dial").cloned() {
                object.insert("dial".to_owned(), dial);
            }
        }
        Some(persona)
    }

    async fn profile(&self, group: &GroupId) -> Option<serde_json::Value> {
        let state = self.state(group).await?;
        let mut profile = state.get("profile")?.clone();
        if let Some(object) = profile.as_object_mut() {
            if let Some(kind) = state.get("kind").and_then(|k| k.as_str()) {
                let kind: Option<ours_types::Kind> =
                    serde_json::from_value(serde_json::Value::String(kind.to_owned())).ok();
                if let Some(kind) = kind {
                    object.insert(
                        "retention_days".to_owned(),
                        serde_json::json!(crate::runtime::retention_days(kind)),
                    );
                }
            }
            if let Some(hold) = state.get("legal_hold").cloned() {
                object.insert("legal_hold".to_owned(), hold);
            }
        }
        Some(profile)
    }

    async fn groups(&self) -> Vec<GroupSummary> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<GroupSummary> =
            groups.values().filter_map(|v| v.summary.clone()).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    async fn state(&self, group: &GroupId) -> Option<serde_json::Value> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        groups.get(group).and_then(|v| v.state.clone())
    }

    async fn charter(&self, group: &GroupId) -> Option<String> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        groups.get(group).and_then(|v| v.charter.clone())
    }

    async fn decisions(&self, group: &GroupId) -> serde_json::Value {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        let Some(state) = groups.get(group).and_then(|v| v.state.as_ref()) else {
            return serde_json::json!({ "open": [], "closed": [], "upcoming": [] });
        };
        decisions_view(state)
    }

    async fn decision_log(&self, group: &GroupId) -> Option<String> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        groups.get(group).and_then(|v| v.decision_log.clone())
    }

    async fn actions(&self, group: &GroupId) -> Vec<serde_json::Value> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        groups
            .get(group)
            .map(|v| v.actions.clone())
            .unwrap_or_default()
    }

    async fn catalogue(&self) -> Vec<serde_json::Value> {
        self.inner
            .catalogue
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    async fn ledger(&self, group: &GroupId) -> serde_json::Value {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        let Some(state) = groups.get(group).and_then(|v| v.state.as_ref()) else {
            return serde_json::json!({ "commitments": [], "lines": [], "balances": [], "currency": "" });
        };
        ledger_view(state)
    }

    async fn ics(&self, group: &GroupId, secret: &str, member: Option<&str>) -> Option<String> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        let view = groups.get(group)?;
        match member {
            // A member's link carries a token derived from the group secret
            // and their id, so one member's link opens nobody else's feed.
            Some(user) => {
                let expected = view
                    .feed_secret
                    .as_deref()
                    .map(|s| member_feed_token(s, user))?;
                constant_time_eq(&expected, secret).then(|| view.ics_members.get(user).cloned())?
            }
            None => {
                if !Self::feed_secret_matches(view, secret) {
                    return None;
                }
                view.ics_group.clone()
            }
        }
    }

    async fn facts(&self, group: &GroupId) -> Vec<serde_json::Value> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        let Some(state) = groups.get(group).and_then(|v| v.state.as_ref()) else {
            return Vec::new();
        };
        facts_with_links(state)
    }

    async fn recent(&self, group: &GroupId, limit: usize) -> Vec<AdminEvent> {
        let groups = self.inner.groups.read().unwrap_or_else(|e| e.into_inner());
        let Some(view) = groups.get(group) else {
            return Vec::new();
        };
        let skip = view.recent.len().saturating_sub(limit);
        view.recent.iter().skip(skip).cloned().collect()
    }

    fn subscribe(&self) -> broadcast::Receiver<AdminEvent> {
        self.inner.sender.subscribe()
    }
}

use ours_core::member_feed_token;

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod feed_tests {
    use super::*;

    #[test]
    fn member_tokens_are_stable_and_differ_per_member() {
        let sam = member_feed_token("s3cret", "sam");
        assert_eq!(sam.len(), 16);
        assert_eq!(sam, member_feed_token("s3cret", "sam"));
        assert_ne!(sam, member_feed_token("s3cret", "priya"));
        assert_ne!(sam, member_feed_token("other", "sam"));
        assert!(constant_time_eq(&sam, &sam.clone()));
        assert!(!constant_time_eq(&sam, "s3cret"));
    }
}
