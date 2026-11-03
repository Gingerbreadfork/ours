//! Proposals (DESIGN.md section 10): what is being decided, the options,
//! the ballot, the close rule and the votes, and how a line of chat names
//! one of the options.

use crate::authority::resolve_role;
use crate::state::GroupState;
use crate::time::{self, resolve_time_rules, wants_resolver, weekday_word};
use ours_types::{
    Instant, Kind, PlatformMessageId, PlatformRoomId, PlatformUserId, Role, TimeMeaning,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A member counts as active for this long after they last posted or reacted.
pub const ACTIVE_SECONDS: i64 = 30 * 24 * 60 * 60;
/// The most options a ballot carries.
pub const MAX_OPTIONS: usize = 10;
/// Number emojis for reaction ballots, in option order.
pub const NUMBERS: [&str; 10] = ["1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣", "6️⃣", "7️⃣", "8️⃣", "9️⃣", "🔟"];
/// The ballot emoji for a single-option attendance poll.
pub const IN: &str = "👍";

macro_rules! seq_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> $name {
                $name(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The sequence number the id ends with, so ids sort in the
            /// order they were minted.
            pub fn seq(&self) -> u64 {
                let digits = self.0.len() - self.0.trim_end_matches(|c: char| c.is_ascii_digit()).len();
                self.0[self.0.len() - digits..].parse().unwrap_or(0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pub(crate) use seq_id;

seq_id!(
    /// A proposal's id, minted from the group's sequence.
    ProposalId
);
seq_id!(
    /// A decision record's id, minted from the group's sequence.
    DecisionId
);

/// How a proposal closes (DESIGN.md section 10, "Close").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloseRule {
    /// More than half of the active members chose one option.
    MajorityOfActive,
    /// The most votes among those who voted.
    MajorityOfVoters,
    /// Every active member chose the same option.
    Unanimous,
    /// No ballot; views are kept and a lead's choice closes it.
    LeadDecides,
    /// Only these people's votes count.
    NamedDeciders(BTreeSet<PlatformUserId>),
    /// Those who say they are coming decide.
    WhoeverIsComing,
}

impl CloseRule {
    /// The rule a kind of group starts with.
    pub fn for_kind(kind: Kind) -> CloseRule {
        match kind {
            Kind::Team | Kind::Project | Kind::Committee => CloseRule::LeadDecides,
            Kind::Friends | Kind::Household | Kind::Club | Kind::Cohort | Kind::Community => {
                CloseRule::MajorityOfActive
            }
        }
    }

    pub fn needs_ballot(&self) -> bool {
        *self != CloseRule::LeadDecides
    }

    pub fn name(&self) -> &'static str {
        match self {
            CloseRule::MajorityOfActive => "majority of active members",
            CloseRule::MajorityOfVoters => "majority of those who voted",
            CloseRule::Unanimous => "unanimous",
            CloseRule::LeadDecides => "lead decides",
            CloseRule::NamedDeciders(_) => "named deciders",
            CloseRule::WhoeverIsComing => "whoever is coming decides",
        }
    }
}

/// How people vote on a proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ballot {
    /// The agent's own message with one emoji per option; the id is known
    /// once the send comes back.
    Reactions {
        message: Option<PlatformMessageId>,
        emojis: Vec<String>,
    },
    /// A native poll on platforms that have them.
    Native { message: Option<PlatformMessageId> },
    /// No ballot: a lead decides.
    None,
}

impl Ballot {
    pub fn message(&self) -> Option<&PlatformMessageId> {
        match self {
            Ballot::Reactions { message, .. } | Ballot::Native { message } => message.as_ref(),
            Ballot::None => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Open,
    Closed(DecisionId),
    /// Closed without a decision: nobody voted, or no agreement.
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    /// Pick one of the options.
    Choice,
    /// One option; a vote means "I'm in".
    Attendance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProposalOption {
    /// As written, first letter capitalised: "Thursday", "Acme".
    pub label: String,
    /// What the option means in time, when it names one.
    pub when: Option<TimeMeaning>,
    /// The date said back, as resolved in the speaker's zone.
    #[serde(default)]
    pub echo: Option<String>,
    pub votes: BTreeSet<PlatformUserId>,
    /// Messages that named this option, with who said them.
    pub views: Vec<(PlatformUserId, PlatformMessageId)>,
}

impl ProposalOption {
    pub fn new(label: String, when: Option<TimeMeaning>, echo: Option<String>) -> ProposalOption {
        ProposalOption {
            label,
            when,
            echo,
            votes: BTreeSet::new(),
            views: Vec::new(),
        }
    }

    /// The option as a ballot shows it: the date when it has one.
    pub fn display(&self) -> &str {
        self.echo.as_deref().unwrap_or(&self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Proposal {
    pub id: ProposalId,
    pub room: PlatformRoomId,
    /// The message that started it; the ballot and the outcome reply to it.
    pub thread_root: PlatformMessageId,
    /// "Dinner: Thursday or Friday?"
    pub question: String,
    /// What it is about, when known: "Dinner", "Print run".
    #[serde(default)]
    pub topic: Option<String>,
    pub options: Vec<ProposalOption>,
    pub opened_at: Instant,
    pub deadline: Instant,
    pub rule: CloseRule,
    pub organiser: Option<PlatformUserId>,
    pub ballot: Ballot,
    /// Nudges sent, per member; never more than two.
    pub nudged: BTreeMap<PlatformUserId, u8>,
    pub status: ProposalStatus,
    pub kind: ProposalKind,
    /// Members who said they don't mind, with the message that said so.
    #[serde(default)]
    pub abstained: BTreeMap<PlatformUserId, PlatformMessageId>,
    /// Lines about the proposal that named no single option.
    #[serde(default)]
    pub remarks: Vec<(PlatformUserId, PlatformMessageId)>,
    /// The words of every view and remark, kept for the record.
    #[serde(default)]
    pub quotes: BTreeMap<PlatformMessageId, String>,
    /// Ballot reactions each member has on, oldest first; the last is the vote.
    #[serde(default)]
    pub reacted: BTreeMap<PlatformUserId, Vec<usize>>,
    /// Deadlines passed with nothing to close on.
    #[serde(default)]
    pub lapsed: u8,
    /// The outcome line, held until the other proposal in the thread closes.
    #[serde(default)]
    pub held: Option<Held>,
    /// The decision this one votes on again.
    #[serde(default)]
    pub reopens: Option<DecisionId>,
}

impl Proposal {
    pub fn is_open(&self) -> bool {
        self.status == ProposalStatus::Open
    }

    /// The topic, or the options when there is none.
    pub fn title(&self) -> String {
        self.topic.clone().unwrap_or_else(|| self.options_or())
    }

    /// "Thursday or Friday", "Pizza, Thai or Indian".
    pub fn options_or(&self) -> String {
        or_list(self.options.iter().map(|o| o.label.clone()).collect())
    }

    /// The option each member currently votes for.
    pub fn vote_of(&self, user: &PlatformUserId) -> Option<usize> {
        self.options.iter().position(|o| o.votes.contains(user))
    }

    pub fn voters(&self) -> BTreeSet<PlatformUserId> {
        self.options
            .iter()
            .flat_map(|o| o.votes.iter().cloned())
            .collect()
    }

    pub fn counts(&self) -> Vec<u32> {
        self.options
            .iter()
            .map(|o| u32::try_from(o.votes.len()).unwrap_or(u32::MAX))
            .collect()
    }

    /// Moves `user`'s vote to `option`, or removes it.
    pub(crate) fn set_vote(&mut self, user: &PlatformUserId, option: Option<usize>) {
        for o in &mut self.options {
            o.votes.remove(user);
        }
        if let Some(o) = option.and_then(|i| self.options.get_mut(i)) {
            o.votes.insert(user.clone());
            self.abstained.remove(user);
        }
    }

    /// Records that `user` named `option` in `message`, once.
    pub(crate) fn add_view(
        &mut self,
        option: usize,
        user: &PlatformUserId,
        message: &PlatformMessageId,
        text: &str,
    ) {
        if let Some(o) = self.options.get_mut(option)
            && !o.views.iter().any(|(_, m)| m == message)
        {
            o.views.push((user.clone(), message.clone()));
        }
        self.quote(message, text);
    }

    pub(crate) fn add_remark(
        &mut self,
        user: &PlatformUserId,
        message: &PlatformMessageId,
        text: &str,
    ) {
        if !self.remarks.iter().any(|(_, m)| m == message) {
            self.remarks.push((user.clone(), message.clone()));
        }
        self.quote(message, text);
    }

    fn quote(&mut self, message: &PlatformMessageId, text: &str) {
        self.quotes
            .entry(message.clone())
            .or_insert_with(|| crate::trace::shorten(text, 200));
    }
}

/// An outcome as one line on its own, as a part of a combined line, and
/// the sentence that follows either, such as "Holding Thursday the 12th.".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Held {
    pub full: String,
    pub part: String,
    pub tail: Option<String>,
}

/// "A", "A or B", "A, B or C".
pub fn or_list(items: Vec<String>) -> String {
    match items.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// The question a ballot asks: "Dinner: Thursday or Friday?".
pub fn question(topic: Option<&str>, labels: &[String]) -> String {
    let options = or_list(labels.to_vec());
    match topic {
        Some(topic) => format!("{topic}: {options}?"),
        None => format!("{options}?"),
    }
}

/// The ballot option a reaction is for, by number or as the 👍 of an
/// attendance poll.
pub fn emoji_option(emoji: &str, kind: ProposalKind) -> Option<usize> {
    let e = emoji.trim_matches(':').replace('\u{fe0f}', "");
    if kind == ProposalKind::Attendance {
        return crate::reactions::is_canonical(&e, crate::reactions::Meaning::In).then_some(0);
    }
    if let Some(i) = NUMBERS.iter().position(|n| n.replace('\u{fe0f}', "") == e) {
        return Some(i);
    }
    const NAMES: [&str; 10] = [
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "keycap_ten",
    ];
    NAMES.iter().position(|n| *n == e)
}

/// Members of `room` who may vote and have posted or reacted in the last
/// 30 days. Guests, bots and the agent never count.
pub fn active_members(
    state: &GroupState,
    room: &PlatformRoomId,
    now: Instant,
) -> BTreeSet<PlatformUserId> {
    let Some(r) = state.rooms.get(room) else {
        return BTreeSet::new();
    };
    r.members
        .values()
        .filter(|m| !m.is_bot && !state.is_me(&m.id))
        .filter(|m| resolve_role(state, room, &m.id) >= Role::Member)
        .filter(|m| {
            state
                .last_seen
                .get(&m.id)
                .is_some_and(|at| now.seconds_since(*at) < ACTIVE_SECONDS)
        })
        .map(|m| m.id.clone())
        .collect()
}

/// What a line of chat says about a proposal's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    /// Names exactly one option; `clear` when it plainly picks it.
    One {
        index: usize,
        clear: bool,
    },
    /// Names more than one option.
    Several,
    /// "whatever works": no preference.
    Abstain,
    /// Names one option and says no to it: "I can't do Thursday".
    Against {
        index: usize,
    },
    Nothing,
}

const ABSTAIN: [&str; 24] = [
    "whatever works",
    "whatever suits",
    "whatever",
    "either",
    "either works",
    "either is fine",
    "either way",
    "both work",
    "both are fine",
    "both fine",
    "both good",
    "don't mind",
    "dont mind",
    "no preference",
    "no pref",
    "easy either way",
    "happy with either",
    "happy with any",
    "anything works",
    "any works",
    "i'm easy",
    "im easy",
    "up to you",
    "dealer's choice",
];

const NEGATIONS: [&str; 18] = [
    "can't", "cant", "cannot", "won't", "wont", "not", "no", "never", "busy", "unable", "don't",
    "dont", "isn't", "isnt", "doesn't", "doesnt", "couldn't", "away",
];

/// Words that show a short line picks the option it names.
const PICKS: [&str; 22] = [
    "works", "work", "for me", "please", "pls", "plz", "prefer", "go with", "vote", "it is",
    "suits", "best", "better", "good", "fine", "keen", "let's do", "lets do", "pick", "choose",
    "+1", "agree",
];

const IN_WORDS: [&str; 10] = [
    "in",
    "i'm in",
    "im in",
    "count me in",
    "me",
    "me too",
    "yes",
    "yep",
    "yeah",
    "i'll be there",
];

const OUT_WORDS: [&str; 7] = [
    "out",
    "i'm out",
    "im out",
    "not me",
    "can't make it",
    "cant make it",
    "can't",
];

/// Lower-cased words, apostrophes straightened, other marks dropped.
pub(crate) fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' || c == '+' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.trim_matches('\'').to_owned())
        .filter(|w| !w.is_empty())
        .collect()
}

const LABEL_STOP: [&str; 10] = ["the", "a", "an", "at", "on", "in", "of", "for", "to", "and"];

/// The phrases that name an option: its label's words, and for a weekday
/// its short forms, also when only a time follows it ("Thursday 7:30pm").
/// Short forms that are everyday words count only in short lines.
fn option_phrases(label: &str, short: bool) -> Vec<String> {
    let label = words(label);
    let mut phrases = Vec::new();
    let core: Vec<&str> = label
        .iter()
        .map(String::as_str)
        .filter(|w| !LABEL_STOP.contains(w))
        .collect();
    if !core.is_empty() {
        phrases.push(core.join(" "));
    }
    let day = match core.as_slice() {
        [one] => Some(*one),
        [first, rest @ ..]
            if rest
                .iter()
                .all(|w| w.chars().any(|c| c.is_ascii_digit()) || matches!(*w, "am" | "pm")) =>
        {
            Some(*first)
        }
        _ => None,
    };
    if let Some(day) = day
        && let Some((weekday, _)) = weekday_word(day)
    {
        for w in [
            "monday",
            "mon",
            "tuesday",
            "tue",
            "tues",
            "wednesday",
            "wed",
            "weds",
            "thursday",
            "thu",
            "thur",
            "thurs",
            "friday",
            "fri",
            "saturday",
            "sat",
            "sunday",
            "sun",
        ] {
            if let Some((day, risky)) = weekday_word(w)
                && day == weekday
                && (!risky || short)
            {
                phrases.push(w.to_owned());
            }
        }
    }
    phrases.sort();
    phrases.dedup();
    phrases
}

/// What `text` says about the proposal's options.
pub fn named(text: &str, proposal: &Proposal) -> Named {
    let labels: Vec<&str> = proposal.options.iter().map(|o| o.label.as_str()).collect();
    named_labels(text, &labels, proposal.kind)
}

/// What `text` says about options with these labels.
pub fn named_labels(text: &str, labels: &[&str], kind: ProposalKind) -> Named {
    let toks = words(text);
    if toks.is_empty() {
        return Named::Nothing;
    }
    let padded = format!(" {} ", toks.join(" "));
    let has = |p: &str| padded.contains(&format!(" {p} "));
    let negated = NEGATIONS.iter().any(|n| has(n));
    if kind == ProposalKind::Attendance {
        let line = toks.join(" ");
        if toks.len() <= 4 && OUT_WORDS.iter().any(|w| has(w)) {
            return Named::Against { index: 0 };
        }
        if toks.len() <= 4 && !negated && IN_WORDS.contains(&line.as_str()) {
            return Named::One {
                index: 0,
                clear: true,
            };
        }
    }
    let short = toks.len() <= 4;
    let mut hits: Vec<usize> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        if option_phrases(label, short).iter().any(|p| has(p)) {
            hits.push(i);
        }
    }
    if hits.is_empty() && short {
        let numbers: Vec<usize> = toks
            .iter()
            .filter_map(|t| t.trim_start_matches('#').parse::<usize>().ok())
            .filter(|n| (1..=labels.len()).contains(n))
            .collect();
        let rest_ok = toks.iter().all(|t| {
            t.trim_start_matches('#').parse::<usize>().is_ok()
                || matches!(
                    t.as_str(),
                    "option" | "number" | "no" | "for" | "me" | "please" | "pls" | "vote"
                )
        });
        if let [n] = numbers.as_slice()
            && rest_ok
        {
            return Named::One {
                index: n - 1,
                clear: true,
            };
        }
    }
    match hits.as_slice() {
        [] if ABSTAIN.iter().any(|a| has(a)) => Named::Abstain,
        [] => Named::Nothing,
        [index] if negated => Named::Against { index: *index },
        [index] => {
            let starts = option_phrases(labels[*index], short)
                .iter()
                .any(|p| padded.starts_with(&format!(" {p} ")));
            let clear = toks.len() <= 6 || starts || PICKS.iter().any(|p| has(p));
            Named::One {
                index: *index,
                clear,
            }
        }
        _ => Named::Several,
    }
}

/// What follows the option a line starts with, as the reason it gives:
/// "Acme, the date matters more than 8%" gives "the date matters more than 8%".
pub fn reason_after(text: &str, label: &str) -> Option<String> {
    let start = *find_ignoring_case(text, label).first()?;
    let rest = text.get(start + label.len()..)?;
    let rest = rest
        .trim_start_matches(|c: char| {
            matches!(c, ',' | ':' | ';' | '-' | '.' | '!') || c.is_whitespace()
        })
        .trim_end_matches(['.', '!'])
        .trim();
    let rest = ["because ", "since ", "as "]
        .iter()
        .find_map(|p| rest.strip_prefix(p))
        .unwrap_or(rest);
    (rest.split_whitespace().count() >= 2).then(|| rest.to_owned())
}

/// One option as offered in chat or in a poll sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub label: String,
    pub when: Option<TimeMeaning>,
    pub echo: Option<String>,
    /// Carries a time the rules could not read; ask the resolver.
    pub unresolved: bool,
}

/// An option from the words someone wrote: a time when the rules read the
/// whole of it as one.
pub fn offered(text: &str, zone: &str, now: Instant) -> Option<Offered> {
    let text = text
        .trim()
        .trim_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ':' | ';' | '"'))
        .trim();
    let text = ["or ", "and "]
        .iter()
        .find_map(|p| text.strip_prefix(p))
        .unwrap_or(text)
        .trim();
    if text.is_empty() {
        return None;
    }
    let toks = time::tokens(text);
    if let Some(found) = resolve_time_rules(text, zone, now)
        && found.phrase.split(' ').count() == toks.len()
    {
        let label = match toks.as_slice() {
            [one] => weekday_word(one)
                .map(|(w, _)| time::weekday_name(w).to_owned())
                .unwrap_or_else(|| capitalise(text)),
            _ => capitalise(text),
        };
        return Some(Offered {
            label,
            when: Some(found.meaning),
            echo: Some(found.echo),
            unresolved: false,
        });
    }
    Some(Offered {
        label: capitalise(text),
        when: None,
        echo: None,
        unresolved: wants_resolver(text),
    })
}

/// Splits a poll sentence's options: "thursday vs friday", "pizza, thai,
/// or indian".
pub fn split_options(text: &str) -> Vec<String> {
    let mut items = vec![text.to_owned()];
    for sep in [" vs. ", " vs ", " versus ", " or ", ",", " / "] {
        items = items
            .iter()
            .flat_map(|i| {
                let mut out = Vec::new();
                let mut last = 0;
                for at in find_ignoring_case(i, sep) {
                    if at < last {
                        continue;
                    }
                    out.push(i.get(last..at).unwrap_or("").to_owned());
                    last = at + sep.len();
                }
                out.push(i.get(last..).unwrap_or("").to_owned());
                out
            })
            .collect();
    }
    items
        .into_iter()
        .map(|i| i.trim().to_owned())
        .filter(|i| !i.is_empty() && !i.eq_ignore_ascii_case("or"))
        .collect()
}

/// Byte offsets where `needle` starts in `hay`, ignoring ASCII case.
fn find_ignoring_case(hay: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    hay.char_indices()
        .map(|(at, _)| at)
        .filter(|at| {
            hay.get(*at..at + needle.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(needle))
        })
        .collect()
}

pub fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Words that end an option on its left: "are we going with Acme".
const LEFT_STOPS: [&str; 30] = [
    "with", "do", "go", "going", "at", "on", "is", "be", "either", "we", "should", "shall", "like",
    "want", "prefer", "choose", "pick", "between", "about", "maybe", "perhaps", "so", "then",
    "for", "to", "it", "it's", "fancy", "rather", "how",
];
/// Words that end an option on its right: "Beacon for the print run".
const RIGHT_STOPS: [&str; 14] = [
    "for", "at", "then", "works", "work", "is", "are", "would", "will", "this", "next", "and",
    "with", "instead",
];

/// Alternatives offered in one line, "thurs or fri" or "are we going with
/// Acme or Beacon for the print run", with the topic after "for the".
pub fn alternatives(
    text: &str,
    zone: &str,
    now: Instant,
) -> Option<(Vec<Offered>, Option<String>)> {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let bare = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
            .to_lowercase()
    };
    let ends = |w: &str| w.ends_with([',', '?', '.', '!', ';', ':']);
    let sep = |w: &str| matches!(bare(w).as_str(), "or" | "vs" | "versus");
    let first_sep = raw.iter().position(|w| sep(w))?;
    if first_sep == 0 {
        return None;
    }
    // Items to the left, nearest first: up to three words, stopping at a
    // lead-in word or after a comma.
    let mut left: Vec<Vec<&str>> = Vec::new();
    let mut k = first_sep;
    loop {
        let mut item = Vec::new();
        while k > 0 && item.len() < 3 {
            let w = raw[k - 1];
            if LEFT_STOPS.contains(&bare(w).as_str()) || (ends(w) && !item.is_empty()) {
                break;
            }
            item.insert(0, w);
            k -= 1;
        }
        if item.is_empty() {
            break;
        }
        left.insert(0, item);
        if k > 0 && ends(raw[k - 1]) && !raw[k - 1].ends_with(['?', '.', '!']) {
            continue;
        }
        break;
    }
    let mut items: Vec<Vec<&str>> = left;
    let mut k = first_sep + 1;
    let mut after = None;
    while k < raw.len() {
        let mut item = Vec::new();
        while k < raw.len() && item.len() < 3 {
            let w = raw[k];
            if !item.is_empty() && RIGHT_STOPS.contains(&bare(w).as_str()) {
                break;
            }
            item.push(w);
            k += 1;
            if ends(w) {
                break;
            }
        }
        if item.is_empty() {
            break;
        }
        let closed = item.last().is_some_and(|w| w.ends_with(['?', '.', '!']));
        items.push(item);
        if k < raw.len() && sep(raw[k]) && !closed {
            k += 1;
            continue;
        }
        after = Some(k);
        break;
    }
    if items.len() < 2 || items.len() > MAX_OPTIONS {
        return None;
    }
    let mut options: Vec<Offered> = Vec::new();
    let first_right = items.get(1)?;
    let timed = offered(&first_right.join(" "), zone, now).is_some_and(|o| o.when.is_some());
    for (n, item) in items.iter().enumerate() {
        let candidate = if timed && n == 0 {
            // The longest tail of the left item that reads as a time.
            (0..item.len())
                .map(|s| item[s..].join(" "))
                .filter_map(|t| offered(&t, zone, now))
                .find(|o| o.when.is_some())?
        } else if timed {
            (1..=item.len())
                .rev()
                .map(|e| item[..e].join(" "))
                .filter_map(|t| offered(&t, zone, now))
                .find(|o| o.when.is_some())?
        } else {
            offered(&item.join(" "), zone, now)?
        };
        if options
            .iter()
            .any(|o| o.label.eq_ignore_ascii_case(&candidate.label))
        {
            return None;
        }
        options.push(candidate);
    }
    let topic = after.and_then(|k| topic_after(&raw[k..]));
    Some((options, topic))
}

/// "for the print run" gives "Print run".
fn topic_after(rest: &[&str]) -> Option<String> {
    let first = rest.first()?;
    if !first.eq_ignore_ascii_case("for") {
        return None;
    }
    let mut words: Vec<String> = rest[1..]
        .iter()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_owned()
        })
        .collect();
    if words
        .first()
        .is_some_and(|w| matches!(w.to_lowercase().as_str(), "the" | "a" | "an" | "our"))
    {
        words.remove(0);
    }
    let words: Vec<String> = words
        .into_iter()
        .take(3)
        .filter(|w| !w.is_empty())
        .collect();
    (!words.is_empty()).then(|| capitalise(&words.join(" ").to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(labels: &[&str]) -> Proposal {
        Proposal {
            id: ProposalId::new("p"),
            room: "r".into(),
            thread_root: "m".into(),
            question: String::new(),
            topic: None,
            options: labels
                .iter()
                .map(|l| ProposalOption::new((*l).to_owned(), None, None))
                .collect(),
            opened_at: Instant::UNIX_EPOCH,
            deadline: Instant::UNIX_EPOCH,
            rule: CloseRule::MajorityOfActive,
            organiser: None,
            ballot: Ballot::None,
            nudged: BTreeMap::new(),
            status: ProposalStatus::Open,
            kind: ProposalKind::Choice,
            abstained: BTreeMap::new(),
            remarks: Vec::new(),
            quotes: BTreeMap::new(),
            reacted: BTreeMap::new(),
            lapsed: 0,
            held: None,
            reopens: None,
        }
    }

    #[test]
    fn lines_name_options() {
        let p = proposal(&["Thursday", "Friday"]);
        let one = |i| Named::One {
            index: i,
            clear: true,
        };
        assert_eq!(named("thursday works", &p), one(0));
        assert_eq!(named("Friday for me", &p), one(1));
        assert_eq!(named("2", &p), one(1));
        assert_eq!(named("thurs", &p), one(0));
        assert_eq!(
            named("I can't do Thursday", &p),
            Named::Against { index: 0 }
        );
        assert_eq!(named("whatever works for everyone", &p), Named::Abstain);
        assert_eq!(named("thursday or friday, both fine", &p), Named::Several);
        assert_eq!(named("the football was great", &p), Named::Nothing);
        assert_eq!(named("I sat down", &p), Named::Nothing);
        let firms = proposal(&["Acme", "Beacon"]);
        assert_eq!(named("Acme, the date matters more than 8%", &firms), one(0));
        assert_eq!(
            named(
                "Beacon were cheaper but Acme hit the date last time",
                &firms
            ),
            Named::Several
        );
        assert_eq!(
            reason_after("Acme, the date matters more than 8%", "Acme").as_deref(),
            Some("the date matters more than 8%")
        );
    }

    #[test]
    fn poll_options_split() {
        assert_eq!(
            split_options("thursday vs friday"),
            vec!["thursday", "friday"]
        );
        assert_eq!(
            split_options("pizza, thai, or indian"),
            vec!["pizza", "thai", "indian"]
        );
        assert_eq!(split_options("A or B or C"), vec!["A", "B", "C"]);
    }

    #[test]
    fn alternatives_come_out_of_a_line() {
        let now = Instant("2026-11-09T08:00:00Z".parse().unwrap());
        let zone = "Australia/Melbourne";
        let (options, topic) = alternatives("thurs or fri", zone, now).unwrap();
        let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["Thursday", "Friday"]);
        assert!(options.iter().all(|o| o.when.is_some()));
        assert!(topic.is_none());
        let (options, topic) = alternatives(
            "are we going with Acme or Beacon for the print run?",
            zone,
            now,
        )
        .unwrap();
        let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["Acme", "Beacon"]);
        assert_eq!(topic.as_deref(), Some("Print run"));
        let (options, _) = alternatives("pizza, thai or indian?", zone, now).unwrap();
        assert_eq!(options.len(), 3);
        assert!(alternatives("Beacon were cheaper but Acme hit the date", zone, now).is_none());
        assert!(alternatives("dinner this week?", zone, now).is_none());
    }
}
