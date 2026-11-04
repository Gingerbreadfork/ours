//! The tab (DESIGN.md section 11): ledger lines from what the extractor
//! finds, splits, net balances, the fewest transfers that settle them, and
//! settle-up by DM with the creditor's own payment link. No money moves and
//! nothing here holds funds or card numbers.

use crate::action::{self, ActionClass, ActionRecord, Origin, Recording, Undo};
use crate::commitment::{event_of, person};
use crate::effect::{Effect, edit, queue, send, send_dm, send_dm_then};
use crate::extract::PendingExtract;
use crate::leads::join_names;
use crate::memory::{Place, Source, rooms_of};
use crate::observe;
use crate::proposal::{DecisionId, active_members, seq_id};
use crate::reactions::{self, Meaning};
use crate::settings::addressed_trigger;
use crate::state::{DmFollowUp, FollowUp, GroupState, PendingAction};
use crate::time::number_word;
use crate::trace::{Trace, quote};
use ours_types::{
    Instant, LedgerDraft, Message, PlatformMessageId, PlatformRoomId, PlatformUserId,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use tracing::debug;

seq_id!(
    /// A ledger line's id, minted from the group's sequence.
    LedgerId
);
seq_id!(
    /// A settle-up transfer's id, minted from the group's sequence.
    SettlementId
);

/// The reaction that takes the reactor out of a split.
pub const NOT_ME: &str = "🙅";
/// The reaction that counts the reactor twice.
pub const DOUBLE: &str = "✌️";
/// The most a single line may be, in cents.
const MAX_CENTS: i64 = 100_000_000;
/// The longest payment link kept.
const LINK_CHARS: usize = 300;
/// "not me" without a reply applies to the room's line from this recently.
const ADJUST_SECONDS: i64 = 24 * 60 * 60;

pub const NOBODY_OWES: &str = "Nobody owes anything.";
pub const EMPTY_TAB: &str = "The tab is empty.";
pub const LINK_SAVED: &str = "Saved. I'll only show it to people who owe you, by DM.";
pub const LINK_NOT_KEPT: &str =
    "I didn't keep that. DM it to me and I'll only show it to people who owe you.";
pub const NO_CARDS: &str = "I don't keep card numbers. A payment link or handle works.";

const TAB_RULE: &str = "money said in the group goes on the tab with one line";
const ADJUST_RULE: &str = "\"not me\" and \"double for me\" change the split";
const SETTLE_RULE: &str = "settle-up is by DM, with the creditor's own link, never in the room";

/// Who owes what share of a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Split {
    Equal {
        among: BTreeSet<PlatformUserId>,
    },
    Shares {
        shares: BTreeMap<PlatformUserId, u32>,
    },
    /// Amounts in cents; what they leave over is the payer's own.
    Exact {
        amounts: BTreeMap<PlatformUserId, i64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LineKind {
    #[default]
    Expense,
    /// A confirmed transfer between two members, which zeroes what it paid.
    Settlement { settlement: SettlementId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Voided {
    pub at: Instant,
    pub by: Option<PlatformUserId>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LedgerLine {
    pub id: LedgerId,
    pub payer: PlatformUserId,
    pub amount_cents: i64,
    /// An ISO 4217 code.
    pub currency: String,
    pub what: String,
    pub split: Split,
    /// The calendar entry it was for, by its decision.
    pub event: Option<DecisionId>,
    pub source: Source,
    pub created_at: Instant,
    pub voided: Option<Voided>,
    #[serde(default)]
    pub kind: LineKind,
    /// The agent's line announcing it, where 🙅 and ✌️ change the split.
    #[serde(default)]
    pub posted: Option<PlatformMessageId>,
}

impl LedgerLine {
    pub fn is_live(&self) -> bool {
        self.voided.is_none()
    }

    /// Whether it may be counted at `place`: lines from that room, and
    /// lines entered by DM, which are the group's.
    pub fn visible(&self, state: &GroupState, place: &Place) -> bool {
        let by_dm = state.rooms.get(&self.source.room).is_none_or(|r| r.is_dm());
        match place {
            Place::Room(room) => by_dm || &self.source.room == room,
            Place::Dm(user) => {
                by_dm || &self.payer == user || rooms_of(state, user).contains(&self.source.room)
            }
        }
    }
}

/// One payment that settles part of the tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Transfer {
    pub from: PlatformUserId,
    pub to: PlatformUserId,
    pub cents: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettlementStatus {
    /// The debtor has been sent the amount and the link.
    Asked,
    /// The debtor says they paid; the creditor is asked to confirm.
    Paid {
        at: Instant,
    },
    Confirmed {
        at: Instant,
    },
    Disputed {
        at: Instant,
        by: PlatformUserId,
    },
    /// A later settle-up replaced it before anyone paid.
    Superseded {
        at: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Settlement {
    pub id: SettlementId,
    pub from: PlatformUserId,
    pub to: PlatformUserId,
    pub cents: i64,
    pub currency: String,
    pub status: SettlementStatus,
    pub created_at: Instant,
    /// The debtor's DM.
    #[serde(default)]
    pub asked: Option<PlatformMessageId>,
    /// The creditor's DM.
    #[serde(default)]
    pub confirm: Option<PlatformMessageId>,
    /// The room whose lines it settles; none for lines entered by DM.
    #[serde(default)]
    pub room: Option<PlatformRoomId>,
}

/// What each person owes of a line, in cents, summing to its amount.
pub fn owed(line: &LedgerLine) -> BTreeMap<PlatformUserId, i64> {
    match &line.split {
        Split::Equal { among } => distribute(line.amount_cents, among.iter().map(|u| (u, 1))),
        Split::Shares { shares } => {
            distribute(line.amount_cents, shares.iter().map(|(u, s)| (u, *s)))
        }
        Split::Exact { amounts } => {
            let mut out = amounts.clone();
            let rest = line.amount_cents - amounts.values().sum::<i64>();
            if rest > 0 {
                *out.entry(line.payer.clone()).or_insert(0) += rest;
            }
            out
        }
    }
}

/// Splits `total` by weight; the cents left over go one each in id order.
fn distribute<'a>(
    total: i64,
    weights: impl Iterator<Item = (&'a PlatformUserId, u32)>,
) -> BTreeMap<PlatformUserId, i64> {
    let weights: Vec<(&PlatformUserId, i64)> = weights
        .filter(|(_, w)| *w > 0)
        .map(|(u, w)| (u, i64::from(w)))
        .collect();
    let sum: i64 = weights.iter().map(|(_, w)| w).sum();
    if sum == 0 {
        return BTreeMap::new();
    }
    let mut out: BTreeMap<PlatformUserId, i64> = weights
        .iter()
        .map(|(u, w)| ((*u).clone(), total * w / sum))
        .collect();
    let mut left = total - out.values().sum::<i64>();
    for (user, _) in &weights {
        if left <= 0 {
            break;
        }
        if let Some(v) = out.get_mut(*user) {
            *v += 1;
            left -= 1;
        }
    }
    out
}

/// Net balance per person in the group's currency: what they paid less what
/// they owe. Positive is owed money.
pub fn balances(state: &GroupState) -> BTreeMap<PlatformUserId, i64> {
    tally(state.ledger.iter(), &state.currency)
}

/// Net balances in `currency` from the lines `place` may count.
pub fn balances_at(
    state: &GroupState,
    place: &Place,
    currency: &str,
) -> BTreeMap<PlatformUserId, i64> {
    tally(
        state.ledger.iter().filter(|l| l.visible(state, place)),
        currency,
    )
}

fn tally<'a>(
    lines: impl Iterator<Item = &'a LedgerLine>,
    currency: &str,
) -> BTreeMap<PlatformUserId, i64> {
    let mut out: BTreeMap<PlatformUserId, i64> = BTreeMap::new();
    for line in lines.filter(|l| l.is_live() && l.currency == currency) {
        *out.entry(line.payer.clone()).or_insert(0) += line.amount_cents;
        for (user, cents) in owed(line) {
            *out.entry(user).or_insert(0) -= cents;
        }
    }
    out.retain(|_, v| *v != 0);
    out
}

/// The fewest transfers that settle the tab, largest debtor to largest
/// creditor first.
pub fn settle(state: &GroupState) -> Vec<Transfer> {
    transfers(balances(state))
}

fn transfers(mut balances: BTreeMap<PlatformUserId, i64>) -> Vec<Transfer> {
    let mut out = Vec::new();
    for _ in 0..(balances.len() * 2 + 1) {
        balances.retain(|_, v| *v != 0);
        let debtor = balances
            .iter()
            .filter(|(_, v)| **v < 0)
            .min_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
            .map(|(u, v)| (u.clone(), *v));
        let creditor = balances
            .iter()
            .filter(|(_, v)| **v > 0)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(u, v)| (u.clone(), *v));
        let (Some((from, owes)), Some((to, due))) = (debtor, creditor) else {
            break;
        };
        let cents = (-owes).min(due);
        *balances.entry(from.clone()).or_insert(0) += cents;
        *balances.entry(to.clone()).or_insert(0) -= cents;
        out.push(Transfer { from, to, cents });
    }
    out
}

/// "38", "71.40": whole amounts without cents.
pub fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.abs();
    if abs % 100 == 0 {
        format!("{sign}{}", abs / 100)
    } else {
        format!("{sign}{}.{:02}", abs / 100, abs % 100)
    }
}

/// "+30.40", "-7.60", always to the cent.
fn signed(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "+" };
    let abs = cents.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// "9.50 USD".
fn priced(cents: i64, currency: &str) -> String {
    let abs = cents.abs();
    format!("{}.{:02} {currency}", abs / 100, abs % 100)
}

/// A written amount in cents: "38", "$71.40", "20,".
pub fn cents_of(text: &str) -> Option<i64> {
    let t = text
        .trim()
        .trim_start_matches(['$', '€', '£', '¥'])
        .trim_end_matches([',', '.', '!', ';'])
        .replace(',', "");
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let value: f64 = t.parse().ok()?;
    cents_from(value)
}

fn cents_from(amount: f64) -> Option<i64> {
    if !amount.is_finite() || amount <= 0.0 {
        return None;
    }
    let cents = (amount * 100.0).round();
    if cents < 1.0 || cents > MAX_CENTS as f64 {
        return None;
    }
    Some(cents as i64)
}

/// "Priya 38 (wine) split five ways".
pub fn describe(state: &GroupState, line: &LedgerLine) -> String {
    let payer = state.name_of(&line.payer);
    let amount = if line.currency == state.currency {
        money(line.amount_cents)
    } else {
        format!("{} {}", money(line.amount_cents), line.currency)
    };
    let names = |users: Vec<&PlatformUserId>| -> String {
        let names: Vec<String> = users.into_iter().map(|u| state.name_of(u)).collect();
        join_names(&names, "and")
    };
    let split = match &line.split {
        Split::Equal { among } if among.len() == 1 => {
            format!("for {}", names(among.iter().collect()))
        }
        Split::Equal { among } => format!("split {} ways", ways(among.len())),
        Split::Shares { shares } => {
            let doubled: Vec<&PlatformUserId> = shares
                .iter()
                .filter(|(_, s)| **s > 1)
                .map(|(u, _)| u)
                .collect();
            if doubled.is_empty() {
                format!("split {} ways", ways(shares.len()))
            } else {
                format!(
                    "split {} ways, double for {}",
                    ways(shares.len()),
                    names(doubled)
                )
            }
        }
        Split::Exact { amounts } => {
            let parts: Vec<String> = amounts
                .iter()
                .map(|(u, c)| format!("{} owes {}", state.name_of(u), money(*c)))
                .collect();
            parts.join(", ")
        }
    };
    format!("{payer} {amount} ({}) {split}", line.what)
}

fn ways(n: usize) -> String {
    number_word(i64::try_from(n).unwrap_or(0))
}

/// Everyone a split names.
fn members(split: &Split) -> BTreeSet<PlatformUserId> {
    match split {
        Split::Equal { among } => among.clone(),
        Split::Shares { shares } => shares.keys().cloned().collect(),
        Split::Exact { amounts } => amounts.keys().cloned().collect(),
    }
}

/// Puts the lines the extractor found in one message on the tab and says
/// so in one line, or to leads while observing. Returns the new lines.
pub(crate) fn store_drafts(
    state: &mut GroupState,
    pending: &PendingExtract,
    drafts: Vec<LedgerDraft>,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> Vec<LedgerId> {
    let room = pending.room.clone();
    if state
        .ledger
        .iter()
        .any(|l| l.source.room == room && l.source.message == pending.message)
    {
        return Vec::new();
    }
    let mut added = Vec::new();
    for draft in drafts.into_iter().take(crate::memory::DRAFTS_KEPT) {
        let Some(line) = line_of(state, pending, draft, now) else {
            continue;
        };
        let described = describe(state, &line);
        fx.push(Effect::Notice(format!(
            "On the tab: {described}, from {}.",
            state.room_label(&room)
        )));
        let record = ActionRecord::new(
            state.mint_action(),
            ActionClass::LedgerLine,
            room.clone(),
            now,
            format!("the tab line {described}"),
            Undo::VoidLine {
                id: line.id.clone(),
            },
        )
        .from_origin(&Origin::asked(&pending.speaker, &pending.message));
        action::push(
            state,
            if state.is_observing() {
                record.hidden()
            } else {
                record
            },
        );
        added.push(line.id.clone());
        state.ledger.push(line);
    }
    if added.is_empty() {
        return added;
    }
    let parts: Vec<String> = added
        .iter()
        .filter_map(|id| state.ledger.iter().find(|l| &l.id == id))
        .map(|l| describe(state, l))
        .collect();
    let text = added_line(state, &room, &parts);
    let trigger = format!(
        "{} in {}: {}",
        state.display_name(&room, &pending.speaker),
        state.room_label(&room),
        quote(&pending.text, 80)
    );
    let trace = Trace::new(trigger, TAB_RULE, now);
    if state.is_observing() {
        let label = state.room_label(&room);
        let report = format!(
            "In {label} I would have said: {text}\n{}",
            reactions::feedback_line(state, &room)
        );
        observe::report(state, fx, &report, trace);
        return added;
    }
    queue(
        state,
        fx,
        PendingAction {
            room,
            text,
            reply_to: Some(pending.message.clone()),
            at: now,
            then: Some(FollowUp::TabLine {
                lines: added.clone(),
            }),
            trace: Some(trace),
            record: Recording::Unrecorded,
        },
    );
    added
}

/// "Added. Tab: Priya 38 (wine) split five ways.", pointing to the pinned
/// balances where the room has them.
fn added_line(state: &GroupState, room: &PlatformRoomId, parts: &[String]) -> String {
    let mut text = format!("Added. Tab: {}.", parts.join("; "));
    if state.pins.get(room).is_some_and(|p| p.message.is_some()) {
        text.push_str(" Running balances in the pinned message.");
    }
    text
}

/// One ledger line from a draft: the payer, the amount in cents, the
/// currency, and the split by what was said, the event, or the room.
fn line_of(
    state: &mut GroupState,
    pending: &PendingExtract,
    draft: LedgerDraft,
    now: Instant,
) -> Option<LedgerLine> {
    let room = &pending.room;
    let speaker = &pending.speaker;
    let payer = person(state, room, speaker, &draft.payer).unwrap_or_else(|| speaker.clone());
    let amount_cents = cents_from(draft.amount)?;
    let currency = draft
        .currency
        .as_deref()
        .map(|c| c.trim().to_uppercase())
        .filter(|c| c.len() == 3 && c.chars().all(|ch| ch.is_ascii_alphabetic()))
        .unwrap_or_else(|| state.currency.clone());
    let what = draft.what.trim().trim_end_matches('.').trim();
    let what = if what.is_empty() { "something" } else { what }.to_owned();
    let event = event_of(state, room, draft.event.as_deref(), None, now);
    let split = explicit(state, room, speaker, &payer, &pending.text, amount_cents)
        .or_else(|| named(state, room, speaker, &draft.participants))
        .or_else(|| attendees(state, event.as_ref()))
        .unwrap_or_else(|| everyone(state, room, speaker, &payer, now));
    if members(&split).iter().all(|u| *u == payer) {
        debug!("a line only the payer is in changes nothing");
        return None;
    }
    let id = state.mint_ledger();
    Some(LedgerLine {
        id,
        payer,
        amount_cents,
        currency,
        what,
        split,
        event,
        source: Source {
            room: room.clone(),
            message: pending.message.clone(),
            speaker: speaker.clone(),
            at: pending.at,
        },
        created_at: now,
        voided: None,
        kind: LineKind::Expense,
        posted: None,
    })
}

/// Shares said outright: "split it three ways between me, Sam and Jo",
/// "Sam owes 20 of it".
fn explicit(
    state: &GroupState,
    room: &PlatformRoomId,
    speaker: &PlatformUserId,
    payer: &PlatformUserId,
    text: &str,
    amount: i64,
) -> Option<Split> {
    let lower = text.to_lowercase().replace(['\u{2018}', '\u{2019}'], "'");
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut amounts: BTreeMap<PlatformUserId, i64> = BTreeMap::new();
    for i in 1..words.len().saturating_sub(1) {
        if words[i] != "owes" {
            continue;
        }
        let name = words[i - 1].trim_matches(|c: char| !c.is_alphanumeric());
        if let (Some(user), Some(cents)) =
            (person(state, room, speaker, name), cents_of(words[i + 1]))
            && user != *payer
        {
            amounts.insert(user, cents);
        }
    }
    if !amounts.is_empty() && amounts.values().sum::<i64>() <= amount {
        return Some(Split::Exact { amounts });
    }
    let at = words.iter().position(|w| *w == "split")?;
    let names_at = words
        .iter()
        .skip(at)
        .position(|w| matches!(*w, "between" | "with" | "among" | "amongst"))?
        + at
        + 1;
    let rest = words.get(names_at..)?.join(" ");
    let rest = rest.split(['.', '!', ';']).next().unwrap_or("");
    let among: BTreeSet<PlatformUserId> = rest
        .split(',')
        .flat_map(|p| p.split(" and "))
        .flat_map(|p| p.split(" & "))
        .map(|n| n.trim().trim_start_matches("and ").trim())
        .filter(|n| !n.is_empty())
        .filter_map(|n| person(state, room, speaker, n))
        .collect();
    (!among.is_empty()).then_some(Split::Equal { among })
}

/// Equal among the people the extractor named.
fn named(
    state: &GroupState,
    room: &PlatformRoomId,
    speaker: &PlatformUserId,
    names: &[String],
) -> Option<Split> {
    let among: BTreeSet<PlatformUserId> = names
        .iter()
        .filter_map(|n| person(state, room, speaker, n))
        .collect();
    (!among.is_empty()).then_some(Split::Equal { among })
}

/// Equal among those going to the event, when the calendar knows it.
fn attendees(state: &GroupState, event: Option<&DecisionId>) -> Option<Split> {
    let entry = state.calendar.iter().find(|e| Some(&e.decision) == event)?;
    (!entry.attendees.is_empty()).then(|| Split::Equal {
        among: entry.attendees.clone(),
    })
}

/// Equal among everyone active in the room, the payer included.
fn everyone(
    state: &GroupState,
    room: &PlatformRoomId,
    speaker: &PlatformUserId,
    payer: &PlatformUserId,
    now: Instant,
) -> Split {
    let in_dm = state.rooms.get(room).is_some_and(|r| r.is_dm());
    let mut among = if in_dm {
        rooms_of(state, speaker)
            .iter()
            .flat_map(|r| active_members(state, r, now))
            .collect()
    } else {
        active_members(state, room, now)
    };
    among.insert(payer.clone());
    Split::Equal { among }
}

/// The tab line's id came back: keep it and offer 🙅 and ✌️ on it.
pub(crate) fn line_posted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    lines: &[LedgerId],
    message: &PlatformMessageId,
    fx: &mut Vec<Effect>,
) {
    for line in state.ledger.iter_mut().filter(|l| lines.contains(&l.id)) {
        line.posted = Some(message.clone());
    }
    let records: Vec<_> = state
        .actions
        .iter()
        .filter(|r| matches!(&r.undo, Undo::VoidLine { id } if lines.contains(id)))
        .map(|r| r.id.clone())
        .collect();
    for record in records {
        action::attach(state, &record, message, false);
    }
    reactions::add(state, fx, room, message, Meaning::NotMe);
    reactions::add(state, fx, room, message, Meaning::Double);
}

/// A change to a split by word or reaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjust {
    NotMe,
    Double,
    /// A 🙅 taken off.
    BackIn,
    /// A ✌️ taken off.
    Single,
}

/// Applies an adjustment to a line's split. Returns whether it changed.
fn apply(line: &mut LedgerLine, user: &PlatformUserId, how: Adjust) -> bool {
    let split = std::mem::replace(
        &mut line.split,
        Split::Equal {
            among: BTreeSet::new(),
        },
    );
    let (split, changed) = match (split, how) {
        (Split::Equal { mut among }, Adjust::NotMe) => {
            let changed = among.remove(user);
            (Split::Equal { among }, changed)
        }
        (Split::Shares { mut shares }, Adjust::NotMe) => {
            let changed = shares.remove(user).is_some();
            (Split::Shares { shares }, changed)
        }
        (Split::Equal { mut among }, Adjust::BackIn) => {
            let changed = among.insert(user.clone());
            (Split::Equal { among }, changed)
        }
        (Split::Shares { mut shares }, Adjust::BackIn) => {
            let changed = !shares.contains_key(user);
            shares.entry(user.clone()).or_insert(1);
            (Split::Shares { shares }, changed)
        }
        (Split::Equal { among }, Adjust::Double) => {
            let mut shares: BTreeMap<PlatformUserId, u32> =
                among.into_iter().map(|u| (u, 1)).collect();
            shares.insert(user.clone(), 2);
            (Split::Shares { shares }, true)
        }
        (Split::Shares { mut shares }, Adjust::Double) => {
            let changed = shares.get(user) != Some(&2);
            shares.insert(user.clone(), 2);
            (Split::Shares { shares }, changed)
        }
        (Split::Shares { mut shares }, Adjust::Single) => {
            let changed = shares.get(user).is_some_and(|s| *s > 1);
            if changed {
                shares.insert(user.clone(), 1);
            }
            if shares.values().all(|s| *s == 1) {
                let among = shares.into_keys().collect();
                (Split::Equal { among }, changed)
            } else {
                (Split::Shares { shares }, changed)
            }
        }
        (other, _) => (other, false),
    };
    line.split = split;
    changed
}

/// The line a "not me" is about: the one replied to, else the room's latest.
fn line_for(state: &GroupState, message: &Message, now: Instant) -> Option<LedgerId> {
    let live = |l: &&LedgerLine| l.is_live() && l.kind == LineKind::Expense;
    if let Some(parent) = &message.reply_to
        && let Some(l) = state.ledger.iter().filter(live).find(|l| {
            l.source.room == message.room
                && (l.posted.as_ref() == Some(parent) || &l.source.message == parent)
        })
    {
        return Some(l.id.clone());
    }
    // Alone, it means the latest line, unless a ballot opened here since.
    let ballot = state
        .proposals
        .values()
        .filter(|p| p.is_open() && p.room == message.room)
        .map(|p| p.opened_at)
        .max();
    state
        .ledger
        .iter()
        .filter(live)
        .rev()
        .find(|l| l.source.room == message.room && now.seconds_since(l.created_at) < ADJUST_SECONDS)
        .filter(|l| ballot.is_none_or(|opened| l.created_at > opened))
        .map(|l| l.id.clone())
}

/// "not me" or "double for me" in reply to a tab line. Returns whether it
/// changed a line; a word that changes nothing, such as "not me" from
/// someone outside the split, is left for a ballot or the model.
pub(crate) fn adjust_words(
    state: &mut GroupState,
    message: &Message,
    how: Adjust,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let Some(id) = line_for(state, message, now) else {
        return false;
    };
    let Some(line) = state.ledger.iter_mut().find(|l| l.id == id) else {
        return false;
    };
    if !apply(line, &message.author, how) {
        return false;
    }
    let line = line.clone();
    let text = format!("Tab: {}.", describe(state, &line));
    let trace = Trace::new(addressed_trigger(state, message), ADJUST_RULE, now);
    send(
        state,
        fx,
        &message.room,
        text,
        Some(message.id.clone()),
        now,
        trace,
    );
    refresh_posted(state, &line, fx);
    true
}

/// A 🙅 or ✌️ on a tab line, put on or taken off. Returns whether the
/// message was a tab line.
pub(crate) fn on_reaction(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    added: bool,
    fx: &mut Vec<Effect>,
) -> bool {
    let not_me = reactions::means(state, room, emoji, Meaning::NotMe);
    let double = reactions::means(state, room, emoji, Meaning::Double);
    let how = match (not_me, double, added) {
        (true, _, true) => Adjust::NotMe,
        (true, _, false) => Adjust::BackIn,
        (_, true, true) => Adjust::Double,
        (_, true, false) => Adjust::Single,
        _ => return false,
    };
    if !state.is_human(room, user) {
        return false;
    }
    let mut changed = Vec::new();
    for line in state
        .ledger
        .iter_mut()
        .filter(|l| l.is_live() && l.posted.as_ref() == Some(message))
    {
        if apply(line, user, how) {
            changed.push(line.clone());
        }
    }
    if changed.is_empty() {
        return state
            .ledger
            .iter()
            .any(|l| l.posted.as_ref() == Some(message));
    }
    for line in &changed {
        fx.push(Effect::Notice(format!(
            "{} changed the split: {}.",
            state.name_of(user),
            describe(state, line)
        )));
    }
    if let Some(line) = changed.first() {
        refresh_posted(state, line, fx);
    }
    true
}

/// Edits the agent's tab line to the current split, where the platform can.
fn refresh_posted(state: &mut GroupState, line: &LedgerLine, fx: &mut Vec<Effect>) {
    let Some(posted) = line.posted.clone() else {
        return;
    };
    let room = line.source.room.clone();
    if !state.me_in(&room).is_some_and(|m| m.capabilities.edit) {
        return;
    }
    let parts: Vec<String> = state
        .ledger
        .iter()
        .filter(|l| l.posted.as_ref() == Some(&posted))
        .map(|l| describe(state, l))
        .collect();
    let text = added_line(state, &room, &parts);
    edit(state, fx, &room, &posted, text);
}

/// The net balances at `place`, with the asker's own first when `mine`.
pub(crate) fn tab_text(
    state: &GroupState,
    place: &Place,
    asker: Option<&PlatformUserId>,
) -> String {
    let currencies: BTreeSet<String> = state
        .ledger
        .iter()
        .filter(|l| l.is_live() && l.visible(state, place))
        .map(|l| l.currency.clone())
        .collect();
    if currencies.is_empty() {
        return EMPTY_TAB.to_owned();
    }
    let mut ordered: Vec<String> = vec![state.currency.clone()];
    ordered.extend(currencies.iter().filter(|c| **c != state.currency).cloned());
    let tabs: Vec<(&String, BTreeMap<PlatformUserId, i64>)> = ordered
        .iter()
        .filter(|c| currencies.contains(*c))
        .map(|c| (c, balances_at(state, place, c)))
        .collect();
    let mut lines = Vec::new();
    if let Some(user) = asker {
        lines.push(own_line(state, user, &tabs));
    }
    let mut any = false;
    for (currency, balances) in &tabs {
        if balances.is_empty() {
            continue;
        }
        any = true;
        lines.push(format!(
            "Tab ({currency}): {}.",
            balance_parts(state, balances).join(", ")
        ));
    }
    if !any {
        lines.push("Everyone's square.".to_owned());
    }
    lines.join("\n")
}

/// "You owe 7.60 and 12 EUR.": the asker's own balance in every currency,
/// the group's without a code.
fn own_line(
    state: &GroupState,
    user: &PlatformUserId,
    tabs: &[(&String, BTreeMap<PlatformUserId, i64>)],
) -> String {
    let mut owe = Vec::new();
    let mut owed = Vec::new();
    for (currency, balances) in tabs {
        let n = balances.get(user).copied().unwrap_or(0);
        let amount = if **currency == state.currency {
            money(n.abs())
        } else {
            format!("{} {currency}", money(n.abs()))
        };
        if n < 0 {
            owe.push(amount);
        } else if n > 0 {
            owed.push(amount);
        }
    }
    match (owe.is_empty(), owed.is_empty()) {
        (true, true) => "You're square.".to_owned(),
        (false, true) => format!("You owe {}.", join_names(&owe, "and")),
        (true, false) => format!("You're owed {}.", join_names(&owed, "and")),
        (false, false) => format!(
            "You owe {}, and you're owed {}.",
            join_names(&owe, "and"),
            join_names(&owed, "and")
        ),
    }
}

/// "Priya +30.40", "Sam -7.60": creditors first, then the largest debts.
pub(crate) fn balance_parts(
    state: &GroupState,
    balances: &BTreeMap<PlatformUserId, i64>,
) -> Vec<String> {
    let mut sorted: Vec<(&PlatformUserId, &i64)> = balances.iter().collect();
    sorted.sort_by(|a, b| {
        (*b.1 > 0)
            .cmp(&(*a.1 > 0))
            .then(b.1.abs().cmp(&a.1.abs()))
            .then(a.0.cmp(b.0))
    });
    sorted
        .into_iter()
        .map(|(u, c)| format!("{} {}", state.name_of(u), signed(*c)))
        .collect()
}

/// "what's the tab", "balances" and "what do I owe", where they were asked.
pub(crate) fn answer(
    state: &mut GroupState,
    message: &Message,
    mine: bool,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let place = Place::of(state, &message.room);
    let asker = mine.then_some(&message.author);
    let text = tab_text(state, &place, asker);
    let trace = Trace::new(
        addressed_trigger(state, message),
        "the tab is answered from the ledger, net per person",
        now,
    );
    send(
        state,
        fx,
        &message.room,
        text,
        Some(message.id.clone()),
        now,
        trace,
    );
}

/// Where a line's money is settled: the room it was said in, or none for a
/// line entered by DM, which every room counts.
fn ledger_of(state: &GroupState, line: &LedgerLine) -> Option<PlatformRoomId> {
    let by_dm = state.rooms.get(&line.source.room).is_none_or(|r| r.is_dm());
    (!by_dm).then(|| line.source.room.clone())
}

/// The ledgers a settle-up asked in `room` clears: the room's own and the
/// lines entered by DM, or from a DM every room the asker is in.
fn ledgers_for(
    state: &GroupState,
    room: &PlatformRoomId,
    asker: &PlatformUserId,
) -> Vec<Option<PlatformRoomId>> {
    let mut out: Vec<Option<PlatformRoomId>> = match Place::of(state, room) {
        Place::Room(room) => vec![Some(room)],
        Place::Dm(_) => rooms_of(state, asker).into_iter().map(Some).collect(),
    };
    out.push(None);
    out
}

/// "settle up": the fewest transfers for the tab where it was asked, each
/// debtor told by DM with the creditor's link, and the room told only how
/// many were sent. Each room's lines settle apart, so no room's balances
/// move for another's.
pub(crate) fn settle_up(
    state: &mut GroupState,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let ledgers = ledgers_for(state, &message.room, &message.author);
    for s in state.settlements.values_mut() {
        if s.status == SettlementStatus::Asked && ledgers.contains(&s.room) {
            s.status = SettlementStatus::Superseded { at: now };
        }
    }
    let mut owing: Vec<(Option<PlatformRoomId>, Transfer)> = Vec::new();
    for ledger in &ledgers {
        let lines = state
            .ledger
            .iter()
            .filter(|l| &ledger_of(state, l) == ledger);
        let mut balances = tally(lines, &state.currency);
        let paid = state.settlements.values().filter(|s| {
            &s.room == ledger
                && matches!(s.status, SettlementStatus::Paid { .. })
                && s.currency == state.currency
        });
        for s in paid {
            *balances.entry(s.from.clone()).or_insert(0) += s.cents;
            *balances.entry(s.to.clone()).or_insert(0) -= s.cents;
        }
        owing.extend(transfers(balances).into_iter().map(|t| (ledger.clone(), t)));
    }
    let trigger = addressed_trigger(state, message);
    if owing.is_empty() {
        let trace = Trace::new(trigger, SETTLE_RULE, now);
        let text = NOBODY_OWES.to_owned();
        send(
            state,
            fx,
            &message.room,
            text,
            Some(message.id.clone()),
            now,
            trace,
        );
        return;
    }
    let debtors: BTreeSet<PlatformUserId> = owing.iter().map(|(_, t)| t.from.clone()).collect();
    for (ledger, t) in owing {
        let id = state.mint_settlement();
        let settlement = Settlement {
            id: id.clone(),
            from: t.from.clone(),
            to: t.to.clone(),
            cents: t.cents,
            currency: state.currency.clone(),
            status: SettlementStatus::Asked,
            created_at: now,
            asked: None,
            confirm: None,
            room: ledger,
        };
        let text = ask_text(state, &settlement);
        state.settlements.insert(id.clone(), settlement);
        let trace = Trace::new(trigger.clone(), SETTLE_RULE, now);
        let then = DmFollowUp::SettleAsk { settlement: id };
        send_dm_then(state, fx, &t.from, text, now, trace, then);
    }
    let n = debtors.len();
    let text = format!(
        "Sent {} {} what they owe, by DM.",
        number_word(i64::try_from(n).unwrap_or(0)),
        if n == 1 { "person" } else { "people" }
    );
    let trace = Trace::new(trigger, SETTLE_RULE, now);
    send(
        state,
        fx,
        &message.room,
        text,
        Some(message.id.clone()),
        now,
        trace,
    );
    fx.push(Effect::Notice(format!(
        "Settle-up: {n} asked to pay, by DM."
    )));
}

/// "You owe Priya 9.50 USD for the tab. Priya's payment link: ..."
fn ask_text(state: &GroupState, s: &Settlement) -> String {
    let to = state.name_of(&s.to);
    let amount = priced(s.cents, &s.currency);
    let check = reactions::emoji_for_user(state, &s.from, Meaning::Check);
    match state.payment_links.get(&s.to) {
        Some(link) => format!(
            "You owe {to} {amount} for the tab. {to}'s payment link: {link}\nReact {check} here once you've paid."
        ),
        None => format!(
            "You owe {to} {amount} for the tab. Ask {to} for their details, then react {check} here once you've paid."
        ),
    }
}

/// Keeps a settle-up DM's id once it is known.
pub(crate) fn dm_posted(state: &mut GroupState, then: &DmFollowUp, message: PlatformMessageId) {
    match then {
        DmFollowUp::SettleAsk { settlement } => {
            if let Some(s) = state.settlements.get_mut(settlement) {
                s.asked = Some(message);
            }
        }
        DmFollowUp::SettleConfirm { settlement } => {
            if let Some(s) = state.settlements.get_mut(settlement) {
                s.confirm = Some(message);
            }
        }
        DmFollowUp::Due { .. } | DmFollowUp::Request { .. } => {}
    }
}

/// The debtor says they paid: ask the creditor to confirm.
fn mark_paid(state: &mut GroupState, id: &SettlementId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(s) = state.settlements.get_mut(id) else {
        return;
    };
    s.status = SettlementStatus::Paid { at: now };
    let s = s.clone();
    let from = state.name_of(&s.from);
    let check = reactions::emoji_for_user(state, &s.to, Meaning::Check);
    let cross = reactions::emoji_for_user(state, &s.to, Meaning::Cross);
    let text = format!(
        "{from} says they've paid you {} for the tab. React {check} to confirm, or {cross} if it hasn't arrived.",
        priced(s.cents, &s.currency)
    );
    let trace = Trace::new(
        format!("{from} marked a settle-up paid"),
        "the creditor confirms a payment with a reaction",
        now,
    );
    let then = DmFollowUp::SettleConfirm {
        settlement: id.clone(),
    };
    send_dm_then(state, fx, &s.to, text, now, trace, then);
}

/// The creditor confirmed: a ledger line of its own zeroes what was paid,
/// counted where the lines it settled are.
fn confirm(state: &mut GroupState, id: &SettlementId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(s) = state.settlements.get_mut(id) else {
        return;
    };
    s.status = SettlementStatus::Confirmed { at: now };
    let s = s.clone();
    let source_room = s.room.clone().unwrap_or_else(|| {
        state
            .dm_room_of(&s.to)
            .unwrap_or_else(|| PlatformRoomId::new(format!("dm-{}", s.to)))
    });
    let line_id = state.mint_ledger();
    state.ledger.push(LedgerLine {
        id: line_id,
        payer: s.from.clone(),
        amount_cents: s.cents,
        currency: s.currency.clone(),
        what: "settle-up".to_owned(),
        split: Split::Exact {
            amounts: BTreeMap::from([(s.to.clone(), s.cents)]),
        },
        event: None,
        source: Source {
            room: source_room,
            message: s
                .confirm
                .clone()
                .unwrap_or_else(|| PlatformMessageId::new("")),
            speaker: s.to.clone(),
            at: now,
        },
        created_at: now,
        voided: None,
        kind: LineKind::Settlement {
            settlement: id.clone(),
        },
        posted: None,
    });
    let to = state.name_of(&s.to);
    let text = format!(
        "{to} confirmed your {}; it's off the tab.",
        priced(s.cents, &s.currency)
    );
    let trace = Trace::new(
        format!("{to} confirmed a payment"),
        "a confirmed settlement goes on the tab as a line of its own",
        now,
    );
    send_dm(state, fx, &s.from, text, now, trace, false);
    fx.push(Effect::Notice(format!(
        "Settled: {} paid {to} {}.",
        state.name_of(&s.from),
        priced(s.cents, &s.currency)
    )));
}

/// Either side said no: both are told, and it stops there.
fn dispute(
    state: &mut GroupState,
    id: &SettlementId,
    by: &PlatformUserId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(s) = state.settlements.get_mut(id) else {
        return;
    };
    s.status = SettlementStatus::Disputed {
        at: now,
        by: by.clone(),
    };
    let s = s.clone();
    let (from, to) = (state.name_of(&s.from), state.name_of(&s.to));
    let text = format!(
        "The {} from {from} to {to} is disputed, so I've stopped there. Sort it out between you, then say settle up again.",
        priced(s.cents, &s.currency)
    );
    let trace = Trace::new(
        format!("{} disputed a settle-up", state.name_of(by)),
        "a dispute goes to the two people, not the group",
        now,
    );
    for user in [&s.from, &s.to] {
        send_dm(state, fx, user, text.clone(), now, trace.clone(), false);
    }
}

/// ✅ or ❌ on a settle-up DM in `room`. Returns whether it was one.
pub(crate) fn on_dm_reaction(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let found = state.settlements.values().find(|s| {
        (s.asked.as_ref() == Some(message) && &s.from == user)
            || (s.confirm.as_ref() == Some(message) && &s.to == user)
    });
    let Some(s) = found.cloned() else {
        return false;
    };
    let creditor = s.confirm.as_ref() == Some(message);
    let check = reactions::means(state, room, emoji, Meaning::Check);
    let cross = reactions::means(state, room, emoji, Meaning::Cross);
    match (&s.status, creditor) {
        (SettlementStatus::Asked, false) if check => mark_paid(state, &s.id, now, fx),
        (SettlementStatus::Paid { .. }, true) if check => confirm(state, &s.id, now, fx),
        (SettlementStatus::Asked | SettlementStatus::Paid { .. }, _) if cross => {
            dispute(state, &s.id, user, now, fx);
        }
        _ => {}
    }
    true
}

/// "paid" by DM from someone with a settle-up waiting. Returns whether it was.
pub(crate) fn paid_words(
    state: &mut GroupState,
    message: &Message,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let user = &message.author;
    let asked = state
        .settlements
        .values()
        .filter(|s| &s.from == user && s.status == SettlementStatus::Asked)
        .max_by_key(|s| (s.created_at, s.id.seq()))
        .cloned();
    let text = match asked {
        Some(s) => {
            mark_paid(state, &s.id, now, fx);
            format!("Thanks. I've asked {} to confirm.", state.name_of(&s.to))
        }
        None => {
            let waiting = state
                .settlements
                .values()
                .find(|s| &s.from == user && matches!(s.status, SettlementStatus::Paid { .. }))
                .map(|s| state.name_of(&s.to));
            match waiting {
                Some(to) => format!("Waiting on {to} to confirm."),
                None => return false,
            }
        }
    };
    let trace = Trace::new(addressed_trigger(state, message), SETTLE_RULE, now);
    send(
        state,
        fx,
        &message.room,
        text,
        Some(message.id.clone()),
        now,
        trace,
    );
    true
}

/// "my payment link is ...": kept when said by DM; in a room it is taken out
/// of memory and the person is asked to DM it.
pub(crate) fn payment_link(
    state: &mut GroupState,
    message: &Message,
    link: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let in_dm = state.rooms.get(&message.room).is_some_and(|r| r.is_dm());
    let link = link.trim();
    let text = if !in_dm {
        crate::rights::redact_message(state, fx, &message.room, &message.id);
        fx.push(Effect::Notice(format!(
            "Did not keep a payment link said in {}.",
            state.room_label(&message.room)
        )));
        LINK_NOT_KEPT
    } else if looks_like_card(link) {
        crate::rights::redact_message(state, fx, &message.room, &message.id);
        NO_CARDS
    } else {
        let kept: String = link.chars().take(LINK_CHARS).collect();
        state.payment_links.insert(message.author.clone(), kept);
        fx.push(Effect::Notice(format!(
            "{} set a payment link.",
            state.name_of(&message.author)
        )));
        LINK_SAVED
    };
    let trace = Trace::new(
        format!(
            "{} gave a payment link in {}",
            state.name_of(&message.author),
            state.room_label(&message.room)
        ),
        "payment links are kept only from a DM and shown only to debtors",
        now,
    );
    send(
        state,
        fx,
        &message.room,
        text,
        Some(message.id.clone()),
        now,
        trace,
    );
}

/// A run of 13 to 19 digits, which is a card number and never kept.
fn looks_like_card(text: &str) -> bool {
    let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    (13..=19).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
}

/// Whether `text` holds a card number anywhere: 13 to 19 digits that pass
/// the Luhn check and start as payment cards do, written whole or in groups
/// split by single spaces or dashes. Digits inside a link or a mention, or
/// run into letters, do not count.
pub fn has_card_number(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut digits: Vec<u32> = Vec::new();
        loop {
            while let Some(d) = chars.get(i).and_then(|c| c.to_digit(10)) {
                digits.push(d);
                i += 1;
            }
            let joined = matches!(chars.get(i), Some(' ' | '-'))
                && chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if !joined {
                break;
            }
            i += 1;
        }
        let before = start.checked_sub(1).and_then(|k| chars.get(k)).copied();
        let after = chars.get(i).copied();
        if card_edge(before)
            && card_edge(after)
            && (13..=19).contains(&digits.len())
            && digits.first().is_some_and(|d| (2..=6).contains(d))
            && luhn(&digits)
        {
            return true;
        }
    }
    false
}

/// What may sit next to a card number: nothing, space, or punctuation that
/// ends or wraps a phrase. A slash, `@` or `<` means a link or a mention.
fn card_edge(c: Option<char>) -> bool {
    c.is_none_or(|c| c.is_whitespace() || ",.:;!?()[]\"'*_`|~".contains(c))
}

fn luhn(digits: &[u32]) -> bool {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| match (i % 2 == 1, d * 2) {
            (true, twice) if twice > 9 => twice - 9,
            (true, twice) => twice,
            (false, _) => d,
        })
        .sum();
    sum.is_multiple_of(10)
}

const LINK_PREFIXES: [&str; 12] = [
    "my payment link is",
    "my payment link:",
    "my payment link",
    "my pay link is",
    "my paypal is",
    "my venmo is",
    "my payid is",
    "my paynow is",
    "my iban is",
    "my bank details are",
    "pay me at",
    "pay me via",
];

/// The link in "my payment link is https://...", as written.
pub fn link_in(text: &str) -> Option<String> {
    let t = text.trim();
    for prefix in LINK_PREFIXES {
        let Some(head) = t.get(..prefix.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(prefix) {
            continue;
        }
        let rest = t
            .get(prefix.len()..)
            .unwrap_or("")
            .trim_start_matches([' ', ':', ','])
            .trim();
        if !rest.is_empty() {
            return Some(rest.to_owned());
        }
    }
    None
}

/// Takes a line off the tab by undo. Returns whether it was live.
pub(crate) fn void(
    state: &mut GroupState,
    id: &LedgerId,
    by: &PlatformUserId,
    now: Instant,
) -> bool {
    let Some(line) = state.ledger.iter_mut().find(|l| &l.id == id && l.is_live()) else {
        return false;
    };
    line.voided = Some(Voided {
        at: now,
        by: Some(by.clone()),
        reason: crate::memory::UNDONE.to_owned(),
    });
    true
}

/// "forget me": their payment link goes, and settle-ups waiting on them
/// stop. Their lines stay, since others' shares are on them too.
pub(crate) fn forget(state: &mut GroupState, user: &PlatformUserId, now: Instant) {
    state.payment_links.remove(user);
    for s in state.settlements.values_mut() {
        if s.status == SettlementStatus::Asked && (&s.from == user || &s.to == user) {
            s.status = SettlementStatus::Superseded { at: now };
        }
    }
}

/// A deleted message takes the lines it put on the tab.
pub(crate) fn on_deleted(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    now: Instant,
) {
    for line in state
        .ledger
        .iter_mut()
        .filter(|l| l.is_live() && &l.source.room == room && &l.source.message == message)
    {
        line.voided = Some(Voided {
            at: now,
            by: None,
            reason: crate::memory::DELETED.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> PlatformUserId {
        PlatformUserId::new(id)
    }

    #[test]
    fn splits_sum_to_the_amount() {
        let among: BTreeSet<PlatformUserId> = ["a", "b", "c"].into_iter().map(user).collect();
        let owed = distribute(1000, among.iter().map(|u| (u, 1)));
        assert_eq!(owed.values().sum::<i64>(), 1000);
        assert_eq!(owed[&user("a")], 334);
        assert_eq!(owed[&user("c")], 333);
    }

    #[test]
    fn transfers_settle_every_balance() {
        let balances = BTreeMap::from([
            (user("priya"), 3040),
            (user("sam"), -760),
            (user("jo"), -760),
            (user("alex"), -1520),
        ]);
        let out = transfers(balances);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0],
            Transfer {
                from: user("alex"),
                to: user("priya"),
                cents: 1520
            }
        );
        assert_eq!(out.iter().map(|t| t.cents).sum::<i64>(), 3040);
    }

    #[test]
    fn amounts_read_and_print() {
        assert_eq!(cents_of("38"), Some(3800));
        assert_eq!(cents_of("$71.40"), Some(7140));
        assert_eq!(cents_of("20,"), Some(2000));
        assert_eq!(cents_of("tickets"), None);
        assert_eq!(money(3800), "38");
        assert_eq!(money(7140), "71.40");
        assert_eq!(signed(-760), "-7.60");
        assert!(looks_like_card("4111 1111 1111 1111"));
        assert!(!looks_like_card("https://monzo.me/priya"));
        for card in [
            "my card is 4111 1111 1111 1111",
            "4111-1111-1111-1111, exp 12/29",
            "use 5555555555554444.",
            "amex (3782 822463 10005) works",
        ] {
            assert!(has_card_number(card), "{card}");
        }
        for not in [
            "4111 1111 1111 1112",
            "call 1730000000000 ms",
            "<@412345678901234561> hi",
            "https://discord.com/channels/1/4111111111111111/2",
            "order A4111111111111111",
            "the print run is 500 copies",
        ] {
            assert!(!has_card_number(not), "{not}");
        }
        assert_eq!(
            link_in("My payment link is https://monzo.me/priya").as_deref(),
            Some("https://monzo.me/priya")
        );
        let canonical = |e, m| reactions::is_canonical(e, m);
        assert!(canonical("✅", Meaning::Check) && canonical("❌", Meaning::Cross));
        assert!(canonical("✌️", Meaning::Double) && canonical("🙅", Meaning::NotMe));
    }
}
