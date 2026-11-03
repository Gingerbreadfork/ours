//! Sentences the engine handles by rule, without a model call: who it
//! listens to, changes to the leads list, undo, settings, running bits,
//! "shut up", "why", "settings", the memory rights, backfill, decisions, the
//! speaker's zone, commitments, the tab, the calendar link, recurrences and
//! what it sent. "be more sarcastic" and the like are left to the model.

use crate::action::ActionClass;
use crate::commitment::Ask;
use crate::proposal::split_options;
use crate::recur::RecurSentence;
use crate::settings::{MAX_DIAL, SettingChange};
use crate::zone::named_zone;
use ours_types::Kind;

/// What an addressed message says, when it is one of the ruled sentences.
/// Names are as written and are resolved against the room by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sentence {
    WhoDoYouListenTo,
    Leads(LeadsSentence),
    /// "undo", "undo the email", "cancel that".
    Undo(UndoWhat),
    /// "what did you send".
    WhatDidYouSend,
    Setting(SettingChange),
    /// "shut up", "quiet", "enough".
    Shush,
    /// "settings", "what are your settings".
    Settings,
    Why,
    /// "what do you know about me".
    WhatDoYouKnow,
    ForgetMe,
    /// "forget that", in reply to a message or alone.
    ForgetThat,
    /// "that's wrong", with whatever follows it as the right version.
    ThatsWrong {
        rest: String,
    },
    /// "remember that", with the fact when one follows.
    RememberThat {
        rest: String,
    },
    JustThisWeek,
    /// "backfill", "read the last 90 days", "catch up".
    Backfill,
    Decision(DecisionSentence),
    /// "my timezone is Europe/Lisbon", "I'm in Lisbon": the zone it names.
    MyZone {
        zone: String,
    },
    /// "what's outstanding", "what did Alex commit to", "what's due this week".
    Commitments(Ask),
    Tab(TabSentence),
    /// "calendar link".
    CalendarLink,
    /// "monthly dinner on the first Thursday", "pause the dinner".
    Recur(RecurSentence),
    /// "who can you be?": the starter personas.
    WhoCanYouBe,
    /// "setup": the first conversation, again or taken over.
    Setup,
}

/// What an undo names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoWhat {
    /// The latest action here, or the one replied to.
    Latest,
    Class(ActionClass),
    /// Something it could not place, such as "undo the monthly thing".
    Unclear,
}

/// Sentences about the tab (DESIGN.md section 11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabSentence {
    /// "what's the tab", "balances"; `mine` for "what do I owe".
    Balances {
        mine: bool,
    },
    SettleUp,
    /// "my payment link is ...", with the link as written.
    PaymentLink {
        link: String,
    },
    /// "paid", by DM.
    Paid,
    NotMe,
    Double,
}

/// Words a question starts with when it is a question, not a choice offered.
const ASKING: [&str; 20] = [
    "what", "when", "where", "who", "why", "how", "is", "are", "do", "does", "did", "can", "could",
    "will", "would", "should", "which", "was", "were", "has",
];

/// A question put to the agent that only offers short alternatives,
/// "thurs or fri for dinner?", read as a poll over them.
fn bare_alternatives(raw: &str) -> Option<DecisionSentence> {
    let body = raw.trim();
    let question = body.strip_suffix('?')?.trim();
    let lower = question.to_lowercase();
    if !lower.contains(" or ") {
        return None;
    }
    let first = lower.split_whitespace().next()?;
    if ASKING.contains(&first) {
        return None;
    }
    let (list, topic) = match lower.rsplit_once(" for ") {
        Some((list, topic)) if (1..=4).contains(&topic.split_whitespace().count()) => {
            (list.trim(), Some(topic.trim().to_owned()))
        }
        _ => (lower.as_str(), None),
    };
    let options = split_options(list);
    let short = |o: &String| (1..=3).contains(&o.split_whitespace().count());
    if options.len() < 2 || options.len() > 6 || !options.iter().all(short) {
        return None;
    }
    Some(DecisionSentence::Poll { topic, options })
}

/// Sentences about proposals and decisions (DESIGN.md section 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionSentence {
    /// "poll thursday vs friday", "poll where: thai or pizza".
    Poll {
        topic: Option<String>,
        options: Vec<String>,
    },
    /// "who's in for Saturday".
    WhoIsIn {
        when: String,
    },
    WhereAreWe,
    WhatsOpen,
    Revote,
    /// "go with Acme, the date matters".
    GoWith {
        choice: String,
        reason: Option<String>,
    },
    /// "lead decides on this one", "majority of voters".
    Rule {
        rule: RuleWord,
        this_one: bool,
    },
    /// "deadline tomorrow 6pm".
    Deadline {
        when: String,
    },
    /// "why did we pick Thursday", "what did we decide about the venue".
    Recall {
        query: String,
    },
}

/// The close rules a lead can name by sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleWord {
    LeadDecides,
    MajorityOfActive,
    MajorityOfVoters,
    Unanimous,
    WhoeverIsComing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeadsSentence {
    Add(Vec<String>),
    Remove(Vec<String>),
    Set(Vec<String>),
    Everyone,
}

/// Words that mean the speaker.
pub const SELF_WORDS: [&str; 3] = ["i", "me", "myself"];

const WHO_QUESTIONS: [&str; 8] = [
    "who do you listen to",
    "who you listen to",
    "who do you take instructions from",
    "who are the leads",
    "who are your leads",
    "who are leads",
    "who is a lead",
    "who's a lead",
];

const TRAILING: [&str; 6] = [
    " now",
    " too",
    " as well",
    " anymore",
    " any more",
    " please",
];

const LEADING: [&str; 4] = ["please ", "ok ", "okay ", "right "];

const SETUP: [&str; 6] = [
    "setup",
    "set up",
    "start setup",
    "run setup",
    "redo setup",
    "/setup",
];

const WHY: [&str; 9] = [
    "why",
    "why that",
    "why this",
    "why so",
    "why though",
    "why did you say that",
    "why did you do that",
    "why'd you say that",
    "explain that",
];

const SETTINGS_ASKS: [&str; 14] = [
    "settings",
    "your settings",
    "the settings",
    "what are your settings",
    "what are the settings",
    "what're your settings",
    "show settings",
    "show your settings",
    "show me your settings",
    "show the settings",
    "charter",
    "the charter",
    "show the charter",
    "what's the charter",
];

const SHUSH: [&str; 17] = [
    "shut up",
    "shut it",
    "quiet",
    "be quiet",
    "keep quiet",
    "enough",
    "that's enough",
    "enough already",
    "shush",
    "shh",
    "shhh",
    "hush",
    "stop talking",
    "pipe down",
    "zip it",
    "stfu",
    "give it a rest",
];

const GO: [&str; 6] = [
    "go",
    "go ahead",
    "go for it",
    "go live",
    "start talking",
    "start speaking",
];

const WATCH: [&str; 9] = [
    "observe",
    "keep observing",
    "observe longer",
    "observe more",
    "watch",
    "keep watching",
    "watch longer",
    "watch more",
    "watch some more",
];

const KNOW_ABOUT_ME: [&str; 14] = [
    "what do you know about me",
    "what do you remember about me",
    "what do you have on me",
    "what have you got on me",
    "what do you hold on me",
    "what do you hold about me",
    "what do you keep on me",
    "what do you store about me",
    "what do you know of me",
    "what you know about me",
    "show me what you know about me",
    "tell me what you know about me",
    "what's my data",
    "my data",
];

const FORGET_ME: [&str; 9] = [
    "forget me",
    "forget about me",
    "forget all about me",
    "forget everything about me",
    "forget everything you know about me",
    "delete me",
    "delete my data",
    "delete everything about me",
    "wipe my data",
];

const FORGET_THAT: [&str; 7] = [
    "forget that",
    "forget this",
    "forget that one",
    "drop that",
    "don't remember that",
    "do not remember that",
    "unremember that",
];

const THIS_WEEK: [&str; 8] = [
    "just for this week",
    "just this week",
    "only this week",
    "only for this week",
    "for this week only",
    "this week only",
    "just for the week",
    "only for the week",
];

const BACKFILL: [&str; 11] = [
    "backfill",
    "backfill this room",
    "backfill the room",
    "read the last 90 days",
    "read the last ninety days",
    "read the last 90 days of this room",
    "catch up",
    "catch up on this room",
    "read back",
    "read the history",
    "read the backlog",
];

/// Phrases that say the agent got something wrong; what follows is the fix.
const WRONG: [&str; 11] = [
    "that's wrong",
    "thats wrong",
    "that is wrong",
    "that's not right",
    "that is not right",
    "that isn't right",
    "that's not true",
    "that is not true",
    "that isn't true",
    "that's incorrect",
    "that is incorrect",
];

/// The same, only when they are the whole message.
const WRONG_ALONE: [&str; 5] = ["wrong", "incorrect", "not true", "not right", "nope, wrong"];

/// Longest first, so "remember that" wins over "remember".
const REMEMBER: [&str; 6] = [
    "keep in mind that",
    "make a note that",
    "remember that",
    "remember this",
    "note that",
    "remember",
];

/// After "remember", these start a question or a reminder, not a fact.
const NOT_A_FACT: [&str; 10] = [
    "when", "what", "where", "who", "how", "which", "why", "if", "whether", "to",
];

/// Words that may come before a memory phrase without changing it.
const MEMORY_LEAD_INS: [&str; 7] = ["no", "nope", "actually", "ok", "okay", "please", "hey"];

/// Kind names and the words people use for them, in the order of the
/// numbered list in the first conversation.
pub const KIND_WORDS: [(Kind, &[&str]); 8] = [
    (
        Kind::Friends,
        &[
            "friends",
            "friend group",
            "friends group",
            "group of friends",
            "mates",
        ],
    ),
    (
        Kind::Household,
        &[
            "household",
            "house",
            "family",
            "flat",
            "flatmates",
            "housemates",
        ],
    ),
    (Kind::Team, &["team", "work team", "team at work"]),
    (Kind::Project, &["project", "project team"]),
    (Kind::Club, &["club", "society", "club or society"]),
    (Kind::Community, &["community", "server"]),
    (Kind::Committee, &["committee", "board"]),
    (
        Kind::Cohort,
        &[
            "cohort",
            "class",
            "study group",
            "class or cohort",
            "course",
        ],
    ),
];

const KIND_PREFIXES: [&str; 8] = [
    "this is ",
    "this group is ",
    "we're ",
    "we are ",
    "kind:",
    "kind is ",
    "kind ",
    "set kind to ",
];

const REMOVE_SUFFIXES: [&str; 7] = [
    " is not a lead",
    " isn't a lead",
    " is no longer a lead",
    " are not leads",
    " aren't leads",
    " are no longer leads",
    " am not a lead",
];

const ADD_SUFFIXES: [&str; 5] = [
    " is a lead",
    " are leads",
    " am a lead",
    " is lead",
    "'s a lead",
];

/// Parses the text after the agent's name. Lenient about case, curly
/// apostrophes and trailing punctuation; anything else goes to the model.
pub fn parse(text: &str) -> Option<Sentence> {
    if let Some(change) = bit(text) {
        return Some(Sentence::Setting(change));
    }
    let t = normalise(text);
    if let Some(what) = undo(&t) {
        return Some(Sentence::Undo(what));
    }
    if SENT.contains(&t.trim_end_matches(" please")) {
        return Some(Sentence::WhatDidYouSend);
    }
    if WHY.contains(&t.as_str()) {
        return Some(Sentence::Why);
    }
    if WHO_CAN_YOU_BE.contains(&t.trim_end_matches(" please")) {
        return Some(Sentence::WhoCanYouBe);
    }
    if let Some(name) = persona(&t) {
        return Some(Sentence::Setting(SettingChange::UsePersona { name }));
    }
    if let Some(rest) = after_phrase(text, &WRONG) {
        return Some(Sentence::ThatsWrong { rest });
    }
    if WRONG_ALONE.contains(&t.as_str()) {
        return Some(Sentence::ThatsWrong {
            rest: String::new(),
        });
    }
    if !text.trim_end().ends_with('?')
        && let Some(rest) = after_phrase(text, &REMEMBER)
    {
        let first = rest.split_whitespace().next().unwrap_or("").to_lowercase();
        if !NOT_A_FACT.contains(&first.as_str()) {
            return Some(Sentence::RememberThat { rest });
        }
    }
    if let Some(link) = crate::tab::link_in(&straight(text)) {
        return Some(Sentence::Tab(TabSentence::PaymentLink { link }));
    }
    if WHO_QUESTIONS.iter().any(|q| t.contains(q)) {
        return Some(Sentence::WhoDoYouListenTo);
    }
    if let Some(s) = commitments_sentence(&t) {
        return Some(Sentence::Commitments(s));
    }
    if let Some(s) = tab_sentence(&t) {
        return Some(Sentence::Tab(s));
    }
    if CALENDAR.contains(&t.trim_end_matches(" please")) {
        return Some(Sentence::CalendarLink);
    }
    if let Some(s) = decision_sentence(text) {
        return Some(Sentence::Decision(s));
    }
    if let Some(zone) = my_zone(text) {
        return Some(Sentence::MyZone { zone });
    }
    if let Some(s) = crate::recur::parse(text) {
        return Some(Sentence::Recur(s));
    }
    if let Some(rest) = t
        .strip_prefix("leads:")
        .or_else(|| t.strip_prefix("leads are "))
    {
        let names = split_names(rest);
        return match names.as_slice() {
            [] => None,
            [one] if one == "everyone" || one == "everybody" => {
                Some(Sentence::Leads(LeadsSentence::Everyone))
            }
            _ => Some(Sentence::Leads(LeadsSentence::Set(names))),
        };
    }
    let mut t = t.as_str();
    while let Some(shorter) = TRAILING.iter().find_map(|s| t.strip_suffix(s)) {
        t = shorter;
    }
    while let Some(shorter) = LEADING.iter().find_map(|s| t.strip_prefix(s)) {
        t = shorter;
    }
    if SETTINGS_ASKS.contains(&t) {
        return Some(Sentence::Settings);
    }
    if SETUP.contains(&t) {
        return Some(Sentence::Setup);
    }
    if SHUSH.contains(&t) {
        return Some(Sentence::Shush);
    }
    if KNOW_ABOUT_ME.contains(&t) {
        return Some(Sentence::WhatDoYouKnow);
    }
    if FORGET_ME.contains(&t) {
        return Some(Sentence::ForgetMe);
    }
    if FORGET_THAT.contains(&t) {
        return Some(Sentence::ForgetThat);
    }
    if THIS_WEEK.contains(&t) {
        return Some(Sentence::JustThisWeek);
    }
    if BACKFILL.contains(&t) {
        return Some(Sentence::Backfill);
    }
    if let Some(change) = setting(t) {
        return Some(Sentence::Setting(change));
    }
    for suffix in REMOVE_SUFFIXES {
        if let Some(subject) = t.strip_suffix(suffix) {
            return non_empty(subject).map(|n| Sentence::Leads(LeadsSentence::Remove(n)));
        }
    }
    for suffix in ADD_SUFFIXES {
        if let Some(subject) = t.strip_suffix(suffix) {
            return non_empty(subject).map(|n| Sentence::Leads(LeadsSentence::Add(n)));
        }
    }
    if let Some(subject) = t
        .strip_prefix("make ")
        .and_then(|s| s.strip_suffix(" a lead"))
    {
        return non_empty(subject).map(|n| Sentence::Leads(LeadsSentence::Add(n)));
    }
    None
}

const WHO_CAN_YOU_BE: [&str; 12] = [
    "who can you be",
    "who else can you be",
    "who could you be",
    "what voices do you have",
    "which voices do you have",
    "what voices can you do",
    "what personas do you have",
    "which personas do you have",
    "what personas are there",
    "list the personas",
    "list your voices",
    "what can you sound like",
];

/// What comes before a starter's name: "be the clerk", "sound like the host".
const PERSONA_PREFIXES: [&str; 10] = [
    "be the ",
    "be a ",
    "be ",
    "sound like the ",
    "sound like a ",
    "sound like ",
    "switch to the ",
    "switch to ",
    "use the ",
    "use ",
];

/// A starter's name, or the plain default, after one of the frames that
/// pick a voice: "be the clerk", "sound like host", "use the keeper persona".
fn persona(t: &str) -> Option<String> {
    let rest = PERSONA_PREFIXES.iter().find_map(|p| t.strip_prefix(p))?;
    let name = [" persona", " voice"]
        .iter()
        .find_map(|s| rest.strip_suffix(s))
        .unwrap_or(rest)
        .trim();
    let known = crate::persona::starter(name).is_some() || crate::persona::PLAIN.contains(&name);
    known.then(|| name.to_owned())
}

/// "legal hold on", "hold everything", "stop holding everything".
fn legal_hold(t: &str) -> Option<SettingChange> {
    match t {
        "legal hold on" | "legal hold" | "turn on legal hold" | "turn legal hold on"
        | "start a legal hold" | "start legal hold" | "hold everything" | "keep everything" => {
            Some(SettingChange::LegalHold { on: true })
        }
        "legal hold off"
        | "turn off legal hold"
        | "turn legal hold off"
        | "end the legal hold"
        | "end legal hold"
        | "lift the legal hold"
        | "lift legal hold"
        | "stop holding everything" => Some(SettingChange::LegalHold { on: false }),
        _ => None,
    }
}

/// "you have a bit: the negroni", with the bit as written.
const NEW_BIT: [&str; 6] = [
    "you have a new bit",
    "you've got a new bit",
    "you have a bit",
    "you've got a bit",
    "here's a bit",
    "new bit",
];

/// "drop the bit about the negroni", with the words that name it.
const DROP_BIT: [&str; 5] = [
    "drop the bit about",
    "forget the bit about",
    "lose the bit about",
    "retire the bit about",
    "no more bit about",
];

/// "drop the bit: the negroni".
const DROP_BIT_MARKED: [&str; 2] = ["drop the bit", "drop bit"];

/// Trimmed from the ends of a bit's words.
const BIT_TRIM: [char; 5] = ['"', '\'', '\u{201c}', '\u{201d}', ' '];

/// A new running bit or one to drop, keeping the words as written.
fn bit(text: &str) -> Option<SettingChange> {
    let raw = straight(text);
    if let Some(rest) = after_marker(&raw, &NEW_BIT) {
        let text = rest.trim_matches(BIT_TRIM).trim();
        return (!text.is_empty()).then(|| SettingChange::AddBit {
            text: text.to_owned(),
        });
    }
    let rest = after_prefix(&raw, &DROP_BIT).or_else(|| after_marker(&raw, &DROP_BIT_MARKED))?;
    let words = rest.trim_matches(BIT_TRIM).trim();
    (!words.is_empty()).then(|| SettingChange::DropBit {
        words: words.to_owned(),
    })
}

/// The text after one of `prefixes` when a colon, comma, dash or opening
/// quote comes next, so "you have a bit of a cheek" is not a bit.
fn after_marker<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for p in prefixes {
        let Some(head) = text.get(..p.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(p) {
            continue;
        }
        let rest = text.get(p.len()..).unwrap_or("").trim_start();
        if let Some(after) = rest.strip_prefix([':', ',', '-']) {
            return Some(after.trim());
        }
        if rest.starts_with(['"', '\u{201c}']) {
            return Some(rest);
        }
    }
    None
}

/// Words for "it" after undo or cancel.
const IT: [&str; 10] = [
    "",
    "that",
    "it",
    "this",
    "that one",
    "this one",
    "the last one",
    "last one",
    "the last thing",
    "that last one",
];

/// What people call each kind of action, after "undo the".
const CLASS_WORDS: [(&str, ActionClass); 30] = [
    ("email", ActionClass::Mail),
    ("emails", ActionClass::Mail),
    ("mail", ActionClass::Mail),
    ("booking", ActionClass::Mail),
    ("reservation", ActionClass::Mail),
    ("cancellation", ActionClass::Mail),
    ("poll", ActionClass::Poll),
    ("vote", ActionClass::Poll),
    ("ballot", ActionClass::Poll),
    ("proposal", ActionClass::Poll),
    ("message", ActionClass::RoomMessage),
    ("post", ActionClass::RoomMessage),
    ("reply", ActionClass::RoomMessage),
    ("dm", ActionClass::Dm),
    ("reaction", ActionClass::Reaction),
    ("emoji", ActionClass::Reaction),
    ("fact", ActionClass::Fact),
    ("facts", ActionClass::Fact),
    ("note", ActionClass::Fact),
    ("commitment", ActionClass::Commitment),
    ("promise", ActionClass::Commitment),
    ("tab", ActionClass::LedgerLine),
    ("expense", ActionClass::LedgerLine),
    ("tab line", ActionClass::LedgerLine),
    ("calendar", ActionClass::CalendarEntry),
    ("calendar entry", ActionClass::CalendarEntry),
    ("calendar hold", ActionClass::CalendarEntry),
    ("hold", ActionClass::CalendarEntry),
    ("event", ActionClass::CalendarEntry),
    ("pin", ActionClass::Pin),
];

const SENT: [&str; 10] = [
    "what did you send",
    "what have you sent",
    "what did you email",
    "what have you emailed",
    "what emails did you send",
    "what did you send out",
    "what's been sent",
    "what has been sent",
    "what's going out",
    "what are you sending",
];

/// "undo", "undo the email", "undo, I'll add a line first", "cancel that",
/// "don't send it". Plain "cancel" takes only these objects, so "cancel the
/// monthly dinner" stays with recurrences.
fn undo(t: &str) -> Option<UndoWhat> {
    if matches!(
        t,
        "don't send it"
            | "don't send that"
            | "do not send it"
            | "do not send that"
            | "dont send it"
    ) {
        return Some(UndoWhat::Class(ActionClass::Mail));
    }
    let (word, rest) = t.split_once(' ').unwrap_or((t, ""));
    let cancel = match word.trim_end_matches([',', ':', ';', '-']) {
        "undo" => false,
        "cancel" => true,
        _ => return None,
    };
    if word.ends_with([',', ':', ';', '-']) {
        return Some(UndoWhat::Latest);
    }
    let mut rest = rest.trim();
    for tail in [" please", " pls", " then", " now", " thanks"] {
        rest = rest.strip_suffix(tail).unwrap_or(rest);
    }
    if IT.contains(&rest) {
        return Some(UndoWhat::Latest);
    }
    let object = ["the ", "my ", "your ", "that ", "this ", "last ", "latest "]
        .iter()
        .fold(rest, |r, p| r.strip_prefix(p).unwrap_or(r));
    let words: Vec<&str> = object.split_whitespace().collect();
    let class = [
        words.iter().take(2).copied().collect::<Vec<_>>().join(" "),
        words.first().copied().unwrap_or("").to_owned(),
    ]
    .into_iter()
    .find_map(|key| CLASS_WORDS.iter().find(|(w, _)| *w == key).map(|(_, c)| *c));
    match (class, cancel) {
        (Some(class), _) => Some(UndoWhat::Class(class)),
        (None, true) => None,
        (None, false) => Some(UndoWhat::Unclear),
    }
}

const OUTSTANDING: [&str; 15] = [
    "what's outstanding",
    "whats outstanding",
    "what is outstanding",
    "what's still outstanding",
    "what is still outstanding",
    "outstanding",
    "anything outstanding",
    "is anything outstanding",
    "what's on the list",
    "commitments",
    "open commitments",
    "who's doing what",
    "whos doing what",
    "who is doing what",
    "what are the commitments",
];

const DUE_THIS_WEEK: [&str; 8] = [
    "what's due this week",
    "whats due this week",
    "what is due this week",
    "due this week",
    "anything due this week",
    "is anything due this week",
    "what's due",
    "what is due",
];

const MY_COMMITMENTS: [&str; 8] = [
    "my commitments",
    "what did i commit to",
    "what have i committed to",
    "what am i committed to",
    "what do i have on",
    "what have i got on",
    "what did i promise",
    "what's on my list",
];

/// "what did Alex commit to", "what has Sam committed to".
const COMMIT_FRAMES: [(&str, &str); 6] = [
    ("what did ", " commit to"),
    ("what did ", " promise"),
    ("what has ", " committed to"),
    ("what's ", " committed to"),
    ("what is ", " committed to"),
    ("what's on ", "'s list"),
];

fn commitments_sentence(t: &str) -> Option<Ask> {
    if OUTSTANDING.contains(&t) {
        return Some(Ask::Outstanding);
    }
    if DUE_THIS_WEEK.contains(&t) {
        return Some(Ask::DueThisWeek);
    }
    if MY_COMMITMENTS.contains(&t) {
        return Some(Ask::By {
            name: "i".to_owned(),
        });
    }
    for (head, tail) in COMMIT_FRAMES {
        let Some(name) = t.strip_prefix(head).and_then(|r| {
            r.strip_suffix(tail)
                .or_else(|| r.strip_suffix(&format!("{tail} do")))
        }) else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && name != "we" && name.split_whitespace().count() <= 3 {
            return Some(Ask::By {
                name: name.to_owned(),
            });
        }
    }
    None
}

const TAB: [&str; 16] = [
    "what's the tab",
    "whats the tab",
    "what is the tab",
    "the tab",
    "tab",
    "show the tab",
    "show me the tab",
    "balances",
    "the balances",
    "show balances",
    "what are the balances",
    "who owes what",
    "who owes who",
    "who owes whom",
    "what's owed",
    "how's the tab",
];

const MY_TAB: [&str; 9] = [
    "what do i owe",
    "how much do i owe",
    "what i owe",
    "do i owe anything",
    "do i owe anyone",
    "what am i owed",
    "how much am i owed",
    "what's my balance",
    "my balance",
];

const SETTLE: [&str; 7] = [
    "settle up",
    "let's settle up",
    "lets settle up",
    "settle the tab",
    "time to settle up",
    "settle",
    "settle up please",
];

const PAID: [&str; 11] = [
    "paid",
    "i paid",
    "i've paid",
    "i have paid",
    "paid it",
    "i paid it",
    "paid you",
    "sent the money",
    "i sent the money",
    "all paid",
    "done, paid",
];

const NOT_ME: [&str; 6] = [
    "not me",
    "not me please",
    "leave me out",
    "count me out",
    "take me off",
    "i wasn't there",
];

const DOUBLE: [&str; 7] = [
    "double for me",
    "double me",
    "count me twice",
    "i had double",
    "i'll take double",
    "me double",
    "double for me please",
];

fn tab_sentence(t: &str) -> Option<TabSentence> {
    if TAB.contains(&t) {
        return Some(TabSentence::Balances { mine: false });
    }
    if MY_TAB.contains(&t) {
        return Some(TabSentence::Balances { mine: true });
    }
    if SETTLE.contains(&t) {
        return Some(TabSentence::SettleUp);
    }
    if PAID.contains(&t) {
        return Some(TabSentence::Paid);
    }
    if NOT_ME.contains(&t) {
        return Some(TabSentence::NotMe);
    }
    if DOUBLE.contains(&t) {
        return Some(TabSentence::Double);
    }
    None
}

const CALENDAR: [&str; 15] = [
    "calendar link",
    "the calendar link",
    "calendar feed",
    "the calendar feed",
    "calendar",
    "the calendar",
    "ics",
    "ics link",
    "ical",
    "ical link",
    "calendar url",
    "link to the calendar",
    "subscribe to the calendar",
    "what's the calendar link",
    "send me the calendar link",
];

/// "digest at 4pm", "no digest", "digest on", "day-of summary off".
fn job_setting(t: &str) -> Option<SettingChange> {
    let t = t.replace("day of", "day-of");
    match t.as_str() {
        "no digest"
        | "digest off"
        | "turn off the digest"
        | "stop the digest"
        | "no more digests"
        | "no digests" => return Some(SettingChange::Digest { on: false }),
        "digest on" | "turn on the digest" | "start the digest" | "digest please"
        | "digests on" => {
            return Some(SettingChange::Digest { on: true });
        }
        "day-of summary on"
        | "day-of summaries on"
        | "day-of on"
        | "turn on the day-of summary"
        | "day-of summary please" => return Some(SettingChange::DayOf { on: true }),
        "day-of summary off"
        | "day-of summaries off"
        | "day-of off"
        | "no day-of summary"
        | "no day-of summaries"
        | "turn off the day-of summary" => return Some(SettingChange::DayOf { on: false }),
        _ => {}
    }
    let rest = [
        "digest at ",
        "send the digest at ",
        "move the digest to ",
        "the digest at ",
        "digest time ",
    ]
    .iter()
    .find_map(|p| t.strip_prefix(p))?;
    let (hour, minute) = clock_minute(rest)?;
    Some(SettingChange::DigestAt { hour, minute })
}

/// "4pm", "4:30pm", "16:30", "4" read as the afternoon for a digest.
fn clock_minute(text: &str) -> Option<(u8, u8)> {
    let t = text.trim().replace(' ', "");
    let minute = t
        .split_once([':', '.'])
        .map(|(_, m)| m.trim_end_matches(|c: char| c.is_ascii_alphabetic()))
        .filter(|m| !m.is_empty())
        .map(|m| m.parse::<u8>().ok().filter(|m| *m < 60))
        .unwrap_or(Some(0))?;
    let c = clock(&t)?;
    let hour = if c.exact || c.hour >= 12 || c.hour == 0 {
        c.hour
    } else if (1..=7).contains(&c.hour) {
        c.hour + 12
    } else {
        c.hour
    };
    Some((hour, minute))
}

const WHERE_ARE_WE: [&str; 17] = [
    "where are we",
    "where are we at",
    "where are we on this",
    "where are we with this",
    "where do we stand",
    "tally",
    "the tally",
    "what's the tally",
    "whats the tally",
    "how's the vote going",
    "how is the vote going",
    "how's the poll going",
    "how is the poll going",
    "how's the vote",
    "how's the poll",
    "status",
    "what's the status",
];

const WHATS_OPEN: [&str; 11] = [
    "what's open",
    "what is open",
    "whats open",
    "what's still open",
    "what is still open",
    "anything open",
    "is anything open",
    "open decisions",
    "what's undecided",
    "what's pending",
    "what's open here",
];

const REVOTE: [&str; 10] = [
    "re-vote",
    "revote",
    "re vote",
    "vote again",
    "let's vote again",
    "redo the vote",
    "redo the poll",
    "re-run the poll",
    "rerun the poll",
    "do the vote again",
];

const PUT_TO_A_VOTE: [&str; 8] = [
    "put it to a vote",
    "put it to the vote",
    "put this to a vote",
    "let's put it to a vote",
    "let's vote",
    "lets vote",
    "let's vote on it",
    "take a vote",
];

const RULES: [(&str, RuleWord); 22] = [
    ("lead decides", RuleWord::LeadDecides),
    ("a lead decides", RuleWord::LeadDecides),
    ("the lead decides", RuleWord::LeadDecides),
    ("leads decide", RuleWord::LeadDecides),
    ("lead's call", RuleWord::LeadDecides),
    ("majority of voters", RuleWord::MajorityOfVoters),
    ("majority of the voters", RuleWord::MajorityOfVoters),
    ("majority of those who voted", RuleWord::MajorityOfVoters),
    ("majority of votes", RuleWord::MajorityOfVoters),
    ("most votes wins", RuleWord::MajorityOfVoters),
    ("majority of active members", RuleWord::MajorityOfActive),
    ("majority of members", RuleWord::MajorityOfActive),
    ("majority", RuleWord::MajorityOfActive),
    ("majority rules", RuleWord::MajorityOfActive),
    ("unanimous", RuleWord::Unanimous),
    ("it has to be unanimous", RuleWord::Unanimous),
    ("must be unanimous", RuleWord::Unanimous),
    ("everyone has to agree", RuleWord::Unanimous),
    ("whoever is coming decides", RuleWord::WhoeverIsComing),
    ("whoever's coming decides", RuleWord::WhoeverIsComing),
    ("those coming decide", RuleWord::WhoeverIsComing),
    ("whoever comes decides", RuleWord::WhoeverIsComing),
];

const THIS_ONE: [&str; 6] = [
    " on this one",
    " for this one",
    " this one",
    " this time",
    " on this",
    " for this",
];

const POLL: [&str; 6] = [
    "quick poll",
    "start a poll",
    "make a poll",
    "run a poll",
    "do a poll",
    "poll",
];

const WHO_IS_IN: [&str; 8] = [
    "who's in for",
    "who is in for",
    "whos in for",
    "who's up for",
    "who is up for",
    "who's coming to",
    "who's coming on",
    "who's free on",
];

const GO_WITH: [&str; 9] = [
    "let's go with",
    "lets go with",
    "we'll go with",
    "we're going with",
    "we go with",
    "go with",
    "decision is",
    "decide on",
    "pick",
];

const DEADLINE: [&str; 13] = [
    "set the deadline to",
    "set the deadline for",
    "set deadline to",
    "move the deadline to",
    "make the deadline",
    "deadline is",
    "deadline:",
    "deadline",
    "close it",
    "close the poll",
    "close voting",
    "voting closes",
    "poll closes",
];

const RECALL: [&str; 19] = [
    "why did we pick",
    "why did we choose",
    "why did we go with",
    "why did we decide on",
    "why did we decide",
    "why did we settle on",
    "why did we book",
    "why are we going with",
    "what did we decide about",
    "what did we decide on",
    "what did we decide for",
    "what was decided about",
    "what was decided on",
    "what did we pick for",
    "what did we choose for",
    "what did we go with for",
    "what did we settle on for",
    "what did we settle on",
    "when did we decide",
];

const RECALL_ALONE: [&str; 5] = [
    "what did we decide",
    "what was decided",
    "what did we pick",
    "what did we go with",
    "what did we choose",
];

const ZONE_PREFIXES: [&str; 14] = [
    "my timezone is",
    "my time zone is",
    "my tz is",
    "my zone is",
    "set my timezone to",
    "set my time zone to",
    "i'm in",
    "i am in",
    "i'm based in",
    "i am based in",
    "i'm now in",
    "i am now in",
    "i'm on",
    "i am on",
];

const ZONE_TRAILING: [&str; 11] = [
    " this month",
    " this week",
    " for now",
    " now",
    " at the moment",
    " for a while",
    " this year",
    " for the month",
    " for a bit",
    " these days",
    " time",
];

/// The text after one of `prefixes`, matched without regard to ASCII case
/// and only on a word boundary.
fn after_prefix<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for p in prefixes {
        let Some(head) = text.get(..p.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(p) {
            continue;
        }
        let rest = text.get(p.len()..).unwrap_or("");
        if p.ends_with(':') || rest.is_empty() || rest.starts_with([' ', ':', ',']) {
            return Some(rest.trim_start_matches([' ', ':', ',']).trim());
        }
    }
    None
}

/// The text with curly apostrophes straightened and the ends trimmed.
fn straight(text: &str) -> String {
    text.replace(['\u{2018}', '\u{2019}'], "'")
        .trim()
        .trim_end_matches(['.', '!'])
        .trim()
        .to_owned()
}

/// A sentence about proposals or decisions.
fn decision_sentence(text: &str) -> Option<DecisionSentence> {
    let raw = straight(text);
    let t = normalise(text);
    if WHERE_ARE_WE.contains(&t.as_str()) {
        return Some(DecisionSentence::WhereAreWe);
    }
    if WHATS_OPEN.contains(&t.as_str()) {
        return Some(DecisionSentence::WhatsOpen);
    }
    if REVOTE.contains(&t.as_str()) {
        return Some(DecisionSentence::Revote);
    }
    if PUT_TO_A_VOTE.contains(&t.as_str()) {
        return Some(DecisionSentence::Rule {
            rule: RuleWord::MajorityOfVoters,
            this_one: true,
        });
    }
    if RECALL_ALONE.contains(&t.as_str()) {
        return Some(DecisionSentence::Recall {
            query: String::new(),
        });
    }
    if let Some(rest) = after_prefix(&raw, &RECALL) {
        return Some(DecisionSentence::Recall {
            query: rest.trim_end_matches('?').trim().to_owned(),
        });
    }
    let mut rule_text = t.as_str();
    let mut this_one = false;
    while let Some(shorter) = THIS_ONE.iter().find_map(|s| rule_text.strip_suffix(s)) {
        rule_text = shorter;
        this_one = true;
    }
    if let Some((_, rule)) = RULES.iter().find(|(words, _)| *words == rule_text) {
        return Some(DecisionSentence::Rule {
            rule: *rule,
            this_one,
        });
    }
    if let Some(rest) = after_prefix(&raw, &WHO_IS_IN) {
        let when = rest.trim_end_matches('?').trim();
        return (!when.is_empty()).then(|| DecisionSentence::WhoIsIn {
            when: when.to_owned(),
        });
    }
    if let Some(rest) = after_prefix(&raw, &POLL) {
        let rest = ["on ", "for ", "about "]
            .iter()
            .find_map(|p| rest.strip_prefix(p))
            .unwrap_or(rest);
        let (topic, list) = match rest.split_once(':') {
            Some((topic, list))
                if !topic.trim().is_empty() && topic.split_whitespace().count() <= 6 =>
            {
                (Some(topic.trim().to_owned()), list)
            }
            _ => (None, rest.trim_start_matches(':')),
        };
        return Some(DecisionSentence::Poll {
            topic,
            options: split_options(list.trim().trim_end_matches('?')),
        });
    }
    // "thurs or fri for dinner?" asked of the agent is a poll without the word.
    if let Some(poll) = bare_alternatives(&raw) {
        return Some(poll);
    }
    if let Some(rest) = after_prefix(&raw, &GO_WITH) {
        let (choice, reason) = match [",", " because ", " since ", " as "]
            .iter()
            .find_map(|sep| rest.split_once(sep))
        {
            Some((choice, reason)) => (choice, Some(reason.trim().to_owned())),
            None => (rest, None),
        };
        let choice = choice.trim();
        return (!choice.is_empty()).then(|| DecisionSentence::GoWith {
            choice: choice.to_owned(),
            reason: reason.filter(|r| !r.is_empty()),
        });
    }
    if let Some(rest) = after_prefix(&raw, &DEADLINE) {
        let when = rest.trim_end_matches('?').trim();
        return (!when.is_empty()).then(|| DecisionSentence::Deadline {
            when: when.to_owned(),
        });
    }
    None
}

/// The zone a speaker says they are in: "my timezone is Europe/Lisbon",
/// "I'm in Lisbon this month".
fn my_zone(text: &str) -> Option<String> {
    let raw = straight(text);
    let mut rest = after_prefix(&raw, &ZONE_PREFIXES)?.to_lowercase();
    while let Some(shorter) = ZONE_TRAILING
        .iter()
        .find_map(|s| rest.strip_suffix(s).map(str::to_owned))
    {
        rest = shorter;
    }
    named_zone(&rest)
}

/// A settings sentence, already normalised.
fn setting(t: &str) -> Option<SettingChange> {
    if let Some(change) = job_setting(t) {
        return Some(change);
    }
    if let Some(change) = legal_hold(t) {
        return Some(change);
    }
    if GO.contains(&t) {
        return Some(SettingChange::Go);
    }
    if WATCH.contains(&t) {
        return Some(SettingChange::Observe { days: None });
    }
    if let Some(days) = watch_days(t) {
        return Some(SettingChange::Observe { days: Some(days) });
    }
    if matches!(
        t,
        "no quiet hours" | "quiet hours off" | "turn off quiet hours" | "no quiet time"
    ) {
        return Some(SettingChange::NoQuietHours);
    }
    if matches!(
        t,
        "no working hours" | "working hours off" | "turn off working hours" | "no work hours"
    ) {
        return Some(SettingChange::NoWorkingHours);
    }
    if let Some(rest) = ["quiet after ", "quiet from ", "quiet at "]
        .iter()
        .find_map(|p| t.strip_prefix(p))
    {
        if let Some((from, to)) = range(rest) {
            return Some(SettingChange::QuietHours {
                start: from.quiet_start(),
                end: to.quiet_end(),
            });
        }
        let start = clock(rest)?;
        return Some(SettingChange::QuietFrom {
            start: start.quiet_start(),
        });
    }
    if let Some(rest) = [
        "quiet hours are ",
        "quiet hours ",
        "quiet time ",
        "quiet between ",
    ]
    .iter()
    .find_map(|p| t.strip_prefix(p))
    {
        let (from, to) = range(rest)?;
        return Some(SettingChange::QuietHours {
            start: from.quiet_start(),
            end: to.quiet_end(),
        });
    }
    if let Some(rest) = [
        "working hours are ",
        "working hours ",
        "work hours ",
        "office hours ",
    ]
    .iter()
    .find_map(|p| t.strip_prefix(p))
    {
        let (from, to) = range(rest)?;
        let start = from.working_start();
        return Some(SettingChange::WorkingHours {
            start,
            end: to.working_end(start),
        });
    }
    if let Some(change) = dial(t) {
        return Some(change);
    }
    KIND_PREFIXES
        .iter()
        .find_map(|p| t.strip_prefix(p))
        .and_then(parse_kind)
        .map(|kind| SettingChange::Kind { kind })
}

fn dial(t: &str) -> Option<SettingChange> {
    match t {
        "dial it down" | "dial down" | "dial it down a notch" | "tone it down" | "turn it down" => {
            return Some(SettingChange::DialBy { delta: -1 });
        }
        "dial it up" | "dial up" | "dial it up a notch" | "turn it up" => {
            return Some(SettingChange::DialBy { delta: 1 });
        }
        _ => {}
    }
    let rest = [
        "dial to ",
        "dial it to ",
        "set the dial to ",
        "set dial to ",
        "dial ",
    ]
    .iter()
    .find_map(|p| t.strip_prefix(p))?;
    let dial: u8 = rest.trim().parse().ok()?;
    (dial <= MAX_DIAL).then_some(SettingChange::DialTo { dial })
}

/// "watch for 3 more days", "observe 2 days", "watch another day".
fn watch_days(t: &str) -> Option<u32> {
    let rest = ["keep watching ", "keep observing ", "watch ", "observe "]
        .iter()
        .find_map(|p| t.strip_prefix(p))?;
    let rest = rest.strip_prefix("for ").unwrap_or(rest);
    let words: Vec<&str> = rest.split_whitespace().collect();
    let (count, unit) = match words.as_slice() {
        [n, unit] | [n, "more", unit] => (*n, *unit),
        _ => return None,
    };
    let n: u32 = match count {
        "a" | "an" | "one" | "another" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        other => other.parse().ok()?,
    };
    let days = match unit {
        "day" | "days" => n,
        "week" | "weeks" => n.checked_mul(7)?,
        _ => return None,
    };
    (1..=60).contains(&days).then_some(days)
}

/// A kind of group from its name or a common word for it: "a team",
/// "flatmates", "club or society". Expects lower case.
pub fn parse_kind(text: &str) -> Option<Kind> {
    let t = text.trim().trim_end_matches(['.', '!', '?']);
    let t = ["a ", "an ", "the ", "just "]
        .iter()
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(t);
    let named = |t: &str| {
        KIND_WORDS
            .iter()
            .find(|(_, words)| words.contains(&t))
            .map(|(kind, _)| *kind)
    };
    named(t).or_else(|| t.strip_suffix(" group").and_then(named))
}

/// An hour as written: "10", "10pm", "22:00", "7.30am", "midnight".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clock {
    hour: u8,
    /// Already on the 24-hour clock: am or pm was given, or the hour says so.
    exact: bool,
}

impl Clock {
    /// "quiet after 10" means 22:00 and "quiet after 1" means 01:00.
    fn quiet_start(self) -> u8 {
        match (self.exact, self.hour) {
            (true, h) => h,
            (false, 12) => 0,
            (false, h @ 6..=11) => h + 12,
            (false, h) => h,
        }
    }

    fn quiet_end(self) -> u8 {
        self.hour
    }

    fn working_start(self) -> u8 {
        self.hour
    }

    /// "9 to 6" ends at 18:00.
    fn working_end(self, start: u8) -> u8 {
        if !self.exact && self.hour <= start && self.hour < 12 {
            self.hour + 12
        } else {
            self.hour
        }
    }
}

fn clock(text: &str) -> Option<Clock> {
    let t = text.trim().replace(' ', "");
    match t.as_str() {
        "midnight" => {
            return Some(Clock {
                hour: 0,
                exact: true,
            });
        }
        "noon" | "midday" => {
            return Some(Clock {
                hour: 12,
                exact: true,
            });
        }
        _ => {}
    }
    let (digits, pm) = if let Some(d) = t.strip_suffix("pm").or_else(|| t.strip_suffix("p.m")) {
        (d, Some(true))
    } else if let Some(d) = t.strip_suffix("am").or_else(|| t.strip_suffix("a.m")) {
        (d, Some(false))
    } else {
        (t.as_str(), None)
    };
    let hour_text = digits.split([':', '.']).next().unwrap_or(digits);
    let hour: u8 = hour_text.parse().ok()?;
    match pm {
        Some(pm) if (1..=12).contains(&hour) => Some(Clock {
            hour: match (pm, hour) {
                (false, 12) => 0,
                (true, 12) => 12,
                (true, h) => h + 12,
                (false, h) => h,
            },
            exact: true,
        }),
        Some(_) => None,
        None if hour < 24 => Some(Clock {
            hour,
            exact: hour == 0 || hour > 12 || hour_text.starts_with('0'),
        }),
        None => None,
    }
}

/// "22 to 8", "from 10pm until 7am", "9-6", "10 and 7".
fn range(text: &str) -> Option<(Clock, Clock)> {
    let t = text.trim();
    let t = ["from ", "between "]
        .iter()
        .find_map(|p| t.strip_prefix(p))
        .unwrap_or(t);
    let (from, to) = [" to ", " until ", " till ", " and ", "-"]
        .iter()
        .find_map(|sep| t.split_once(sep))?;
    Some((clock(from)?, clock(to)?))
}

/// When `text` opens with one of `phrases`, after at most one lead-in word,
/// whatever follows it, in its original case with separators trimmed.
fn after_phrase(text: &str, phrases: &[&str]) -> Option<String> {
    let words: Vec<(usize, &str)> = text
        .split_whitespace()
        .map(|w| (w.as_ptr() as usize - text.as_ptr() as usize, w))
        .collect();
    let lead_in = words
        .first()
        .is_some_and(|(_, w)| MEMORY_LEAD_INS.contains(&clean_word(w).as_str()));
    for skip in [0, usize::from(lead_in)] {
        for phrase in phrases {
            let want: Vec<&str> = phrase.split(' ').collect();
            let Some(have) = words.get(skip..skip + want.len()) else {
                continue;
            };
            if have
                .iter()
                .zip(&want)
                .all(|((_, got), wanted)| clean_word(got) == *wanted)
            {
                let (start, last) = have[have.len() - 1];
                let rest = &text[start + last.len()..];
                return Some(
                    rest.trim_start_matches(|c: char| {
                        matches!(c, ',' | ':' | ';' | '-' | '.' | '!') || c.is_whitespace()
                    })
                    .trim_end()
                    .to_owned(),
                );
            }
        }
    }
    None
}

/// A word lower-cased, apostrophes straightened, edge punctuation dropped.
fn clean_word(word: &str) -> String {
    word.to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .trim_matches(|c: char| matches!(c, ',' | ':' | ';' | '.' | '!' | '-'))
        .to_owned()
}

fn non_empty(subject: &str) -> Option<Vec<String>> {
    let names = split_names(subject);
    (!names.is_empty()).then_some(names)
}

fn normalise(text: &str) -> String {
    let lower = text.to_lowercase().replace(['\u{2018}', '\u{2019}'], "'");
    let collapsed = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_end_matches(['.', '!', '?', ' ']);
    trimmed.replace("i'm ", "i am ")
}

/// Splits "sam, jo and alex" into its names.
fn split_names(list: &str) -> Vec<String> {
    list.split(',')
        .flat_map(|part| part.split(" and "))
        .flat_map(|part| part.split(" & "))
        .map(|name| name.trim().trim_start_matches("and ").trim())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(names: &[&str]) -> Option<Sentence> {
        Some(Sentence::Leads(LeadsSentence::Add(
            names.iter().map(|n| n.to_string()).collect(),
        )))
    }

    #[test]
    fn lead_sentences_parse_leniently() {
        assert_eq!(parse("Sam is a lead."), add(&["sam"]));
        assert_eq!(parse("Sam and Jo are leads"), add(&["sam", "jo"]));
        assert_eq!(parse("I\u{2019}m a lead now"), add(&["i"]));
        assert_eq!(parse("make Priya a lead"), add(&["priya"]));
        assert_eq!(parse("sam's a lead too!"), add(&["sam"]));
        assert_eq!(
            parse("Sam is not a lead anymore"),
            Some(Sentence::Leads(LeadsSentence::Remove(vec!["sam".into()])))
        );
        assert_eq!(
            parse("leads: Priya, Sam and Jo"),
            Some(Sentence::Leads(LeadsSentence::Set(vec![
                "priya".into(),
                "sam".into(),
                "jo".into()
            ])))
        );
        assert_eq!(
            parse("leads: everyone"),
            Some(Sentence::Leads(LeadsSentence::Everyone))
        );
    }

    #[test]
    fn questions_and_undo() {
        assert_eq!(
            parse("who do you listen to?"),
            Some(Sentence::WhoDoYouListenTo)
        );
        assert_eq!(
            parse("So who are the leads"),
            Some(Sentence::WhoDoYouListenTo)
        );
        assert_eq!(parse("undo"), Some(Sentence::Undo(UndoWhat::Latest)));
        assert_eq!(
            parse("Undo the booking"),
            Some(Sentence::Undo(UndoWhat::Class(ActionClass::Mail)))
        );
    }

    #[test]
    fn undo_names_what_it_takes_back() {
        let class = |c| Some(Sentence::Undo(UndoWhat::Class(c)));
        let latest = Some(Sentence::Undo(UndoWhat::Latest));
        assert_eq!(parse("undo that"), latest);
        assert_eq!(parse("cancel that"), latest);
        assert_eq!(parse("Cancel it please"), latest);
        assert_eq!(parse("undo, I'll add a line first"), latest);
        assert_eq!(parse("undo the email"), class(ActionClass::Mail));
        assert_eq!(
            parse("undo the email to bai tong"),
            class(ActionClass::Mail)
        );
        assert_eq!(parse("cancel the booking"), class(ActionClass::Mail));
        assert_eq!(parse("don't send it"), class(ActionClass::Mail));
        assert_eq!(parse("undo the poll"), class(ActionClass::Poll));
        assert_eq!(
            parse("undo my last message"),
            class(ActionClass::RoomMessage)
        );
        assert_eq!(
            parse("undo the calendar entry"),
            class(ActionClass::CalendarEntry)
        );
        assert_eq!(parse("undo the tab line"), class(ActionClass::LedgerLine));
        assert_eq!(parse("undo the fact"), class(ActionClass::Fact));
        assert_eq!(
            parse("undo the monthly thing"),
            Some(Sentence::Undo(UndoWhat::Unclear))
        );
        assert_eq!(parse("what did you send?"), Some(Sentence::WhatDidYouSend));
        assert_eq!(parse("what's been sent"), Some(Sentence::WhatDidYouSend));
        for text in ["cancel the monthly dinner", "undone yet?", "cancelled?"] {
            assert!(!matches!(parse(text), Some(Sentence::Undo(_))), "{text}");
        }
    }

    fn setting(text: &str) -> Option<SettingChange> {
        match parse(text) {
            Some(Sentence::Setting(change)) => Some(change),
            _ => None,
        }
    }

    #[test]
    fn setting_sentences_parse_leniently() {
        use SettingChange::{
            DialBy, DialTo, Go, NoQuietHours, NoWorkingHours, Observe, QuietFrom, QuietHours,
            WorkingHours,
        };
        let kind = |kind: Kind| SettingChange::Kind { kind };
        let table: [(&str, SettingChange); 34] = [
            ("quiet after 10", QuietFrom { start: 22 }),
            ("quiet after 10pm.", QuietFrom { start: 22 }),
            ("Quiet after 11 please", QuietFrom { start: 23 }),
            ("quiet after 1", QuietFrom { start: 1 }),
            ("quiet after midnight", QuietFrom { start: 0 }),
            ("quiet after 21:30", QuietFrom { start: 21 }),
            ("quiet hours 22 to 8", QuietHours { start: 22, end: 8 }),
            ("quiet hours 10pm to 7am", QuietHours { start: 22, end: 7 }),
            ("quiet hours 10 to 8", QuietHours { start: 22, end: 8 }),
            (
                "quiet hours from 23:00 until 06:00",
                QuietHours { start: 23, end: 6 },
            ),
            ("quiet hours 22-7", QuietHours { start: 22, end: 7 }),
            ("quiet from 9 to 7", QuietHours { start: 21, end: 7 }),
            ("no quiet hours", NoQuietHours),
            ("working hours 9 to 6", WorkingHours { start: 9, end: 18 }),
            (
                "working hours 8am to 4pm",
                WorkingHours { start: 8, end: 16 },
            ),
            ("working hours 9 to 17", WorkingHours { start: 9, end: 17 }),
            ("office hours 10-4", WorkingHours { start: 10, end: 16 }),
            ("no working hours", NoWorkingHours),
            ("dial it down", DialBy { delta: -1 }),
            ("Dial it up!", DialBy { delta: 1 }),
            ("dial 2", DialTo { dial: 2 }),
            ("set the dial to 0", DialTo { dial: 0 }),
            ("this is a team", kind(Kind::Team)),
            ("we\u{2019}re a household", kind(Kind::Household)),
            ("kind: club", kind(Kind::Club)),
            ("we're friends", kind(Kind::Friends)),
            ("this is a study group", kind(Kind::Cohort)),
            ("go", Go),
            ("go ahead!", Go),
            ("observe", Observe { days: None }),
            ("keep watching", Observe { days: None }),
            ("watch for 3 more days", Observe { days: Some(3) }),
            ("watch 2 more days", Observe { days: Some(2) }),
            ("observe for a week", Observe { days: Some(7) }),
        ];
        for (text, change) in table {
            assert_eq!(setting(text), Some(change), "{text}");
        }
        for text in [
            "quiet after dinner",
            "dial 7",
            "this is a disaster",
            "go home",
            "watch the match",
            "working hours",
        ] {
            assert_eq!(setting(text), None, "{text}");
        }
    }

    #[test]
    fn voices_and_the_legal_hold() {
        let voice = |name: &str| {
            Some(Sentence::Setting(SettingChange::UsePersona {
                name: name.to_owned(),
            }))
        };
        assert_eq!(parse("be the clerk"), voice("clerk"));
        assert_eq!(parse("Be the Host."), voice("host"));
        assert_eq!(parse("sound like keeper"), voice("keeper"));
        assert_eq!(parse("sound like the regular"), voice("regular"));
        assert_eq!(parse("use the mate persona"), voice("mate"));
        assert_eq!(parse("switch to the wag voice"), voice("wag"));
        assert_eq!(parse("use the plain default"), voice("plain default"));
        for text in [
            "be more sarcastic",
            "be quiet",
            "be a mate and remind me",
            "use the other room",
        ] {
            assert_ne!(
                parse(text)
                    .map(|s| matches!(s, Sentence::Setting(SettingChange::UsePersona { .. }))),
                Some(true),
                "{text}"
            );
        }
        for text in [
            "who can you be?",
            "Who else can you be",
            "what voices do you have",
        ] {
            assert_eq!(parse(text), Some(Sentence::WhoCanYouBe), "{text}");
        }
        let hold = |on: bool| Some(Sentence::Setting(SettingChange::LegalHold { on }));
        assert_eq!(parse("legal hold on"), hold(true));
        assert_eq!(parse("Hold everything."), hold(true));
        assert_eq!(parse("legal hold off please"), hold(false));
        assert_eq!(parse("stop holding everything"), hold(false));
        assert_eq!(parse("hold everything until friday"), None);
    }

    #[test]
    fn shush_settings_and_why() {
        for text in [
            "shut up",
            "Quiet!",
            "enough",
            "ok enough",
            "be quiet please",
            "shush",
        ] {
            assert_eq!(parse(text), Some(Sentence::Shush), "{text}");
        }
        for text in [
            "settings",
            "What are your settings?",
            "show me your settings",
        ] {
            assert_eq!(parse(text), Some(Sentence::Settings), "{text}");
        }
        for text in ["why", "Why?", "why did you say that"] {
            assert_eq!(parse(text), Some(Sentence::Why), "{text}");
        }
        for text in [
            "why is the sky blue",
            "enough about the football",
            "quiet in here",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn bits_parse_and_personality_requests_do_not() {
        let add = |text: &str| {
            Some(Sentence::Setting(SettingChange::AddBit {
                text: text.to_owned(),
            }))
        };
        let drop = |words: &str| {
            Some(Sentence::Setting(SettingChange::DropBit {
                words: words.to_owned(),
            }))
        };
        assert_eq!(parse("you have a bit: Sam's parking"), add("Sam's parking"));
        assert_eq!(
            parse("You\u{2019}ve got a new bit, \"the negroni was a war crime\"."),
            add("the negroni was a war crime")
        );
        assert_eq!(parse("drop the bit about the negroni"), drop("the negroni"));
        assert_eq!(
            parse("Drop the bit about Sam's parking!"),
            drop("Sam's parking")
        );
        assert_eq!(parse("drop the bit: parking"), drop("parking"));
        assert_eq!(
            parse("new bit \u{201c}Jo's sourdough\u{201d}"),
            add("Jo's sourdough")
        );
        for text in [
            "you have a bit",
            "drop the bit",
            "be more sarcastic",
            "be more fun",
            "you have a bit of a cheek",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn memory_sentences_parse_leniently() {
        let wrong = |rest: &str| {
            Some(Sentence::ThatsWrong {
                rest: rest.to_owned(),
            })
        };
        let remember = |rest: &str| {
            Some(Sentence::RememberThat {
                rest: rest.to_owned(),
            })
        };
        assert_eq!(
            parse("What do you know about me?"),
            Some(Sentence::WhatDoYouKnow)
        );
        assert_eq!(parse("forget me please"), Some(Sentence::ForgetMe));
        assert_eq!(parse("Forget that."), Some(Sentence::ForgetThat));
        assert_eq!(parse("just for this week"), Some(Sentence::JustThisWeek));
        assert_eq!(parse("catch up"), Some(Sentence::Backfill));
        assert_eq!(parse("read the last 90 days"), Some(Sentence::Backfill));
        assert_eq!(parse("that's wrong"), wrong(""));
        assert_eq!(parse("That\u{2019}s wrong."), wrong(""));
        assert_eq!(
            parse("no, that's wrong: Priya is Vegan now"),
            wrong("Priya is Vegan now")
        );
        assert_eq!(parse("wrong"), wrong(""));
        assert_eq!(parse("remember that"), remember(""));
        assert_eq!(parse("Remember that Sam drives."), remember("Sam drives."));
        assert_eq!(
            parse("remember: the code is 4471"),
            remember("the code is 4471")
        );
        for text in [
            "remember that bar in March?",
            "remember when we went to Hobson's",
            "remember to book the table",
            "wrong channel",
            "forget it",
            "what do you know about Sam",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn kinds_by_name_and_word() {
        assert_eq!(parse_kind("a team"), Some(Kind::Team));
        assert_eq!(parse_kind("flatmates"), Some(Kind::Household));
        assert_eq!(parse_kind("club or society"), Some(Kind::Club));
        assert_eq!(parse_kind("friend group"), Some(Kind::Friends));
        assert_eq!(parse_kind("committee group"), Some(Kind::Committee));
        assert_eq!(parse_kind("pirates"), None);
    }

    #[test]
    fn a_bare_choice_put_to_the_agent_is_a_poll() {
        assert_eq!(
            decision("thurs or fri for dinner?"),
            Some(DecisionSentence::Poll {
                topic: Some("dinner".to_owned()),
                options: vec!["thurs".to_owned(), "fri".to_owned()],
            })
        );
        assert_eq!(
            decision("Thai or pizza?"),
            Some(DecisionSentence::Poll {
                topic: None,
                options: vec!["thai".to_owned(), "pizza".to_owned()],
            })
        );
        assert!(!matches!(
            decision("do you know if sam or alex is coming?"),
            Some(DecisionSentence::Poll { .. })
        ));
        assert!(!matches!(
            decision("thurs or fri"),
            Some(DecisionSentence::Poll { .. })
        ));
    }

    fn decision(text: &str) -> Option<DecisionSentence> {
        match parse(text) {
            Some(Sentence::Decision(s)) => Some(s),
            _ => None,
        }
    }

    #[test]
    fn decision_sentences_parse_leniently() {
        assert_eq!(
            decision("poll thursday vs friday"),
            Some(DecisionSentence::Poll {
                topic: None,
                options: vec!["thursday".into(), "friday".into()]
            })
        );
        assert_eq!(
            decision("poll: pizza, thai, or indian"),
            Some(DecisionSentence::Poll {
                topic: None,
                options: vec!["pizza".into(), "thai".into(), "indian".into()]
            })
        );
        assert_eq!(
            decision("Poll where: Bai Tong or Thai Palace?"),
            Some(DecisionSentence::Poll {
                topic: Some("where".into()),
                options: vec!["Bai Tong".into(), "Thai Palace".into()]
            })
        );
        assert_eq!(
            decision("who\u{2019}s in for Saturday?"),
            Some(DecisionSentence::WhoIsIn {
                when: "Saturday".into()
            })
        );
        assert_eq!(
            decision("where are we?"),
            Some(DecisionSentence::WhereAreWe)
        );
        assert_eq!(decision("What's open"), Some(DecisionSentence::WhatsOpen));
        assert_eq!(decision("re-vote"), Some(DecisionSentence::Revote));
        assert_eq!(
            decision("go with Acme, the date matters"),
            Some(DecisionSentence::GoWith {
                choice: "Acme".into(),
                reason: Some("the date matters".into())
            })
        );
        assert_eq!(
            decision("lead decides on this one"),
            Some(DecisionSentence::Rule {
                rule: RuleWord::LeadDecides,
                this_one: true
            })
        );
        assert_eq!(
            decision("majority of voters"),
            Some(DecisionSentence::Rule {
                rule: RuleWord::MajorityOfVoters,
                this_one: false
            })
        );
        assert_eq!(
            decision("put it to a vote"),
            Some(DecisionSentence::Rule {
                rule: RuleWord::MajorityOfVoters,
                this_one: true
            })
        );
        assert_eq!(
            decision("deadline tomorrow 6pm"),
            Some(DecisionSentence::Deadline {
                when: "tomorrow 6pm".into()
            })
        );
        assert_eq!(
            decision("why did we pick Thursday?"),
            Some(DecisionSentence::Recall {
                query: "Thursday".into()
            })
        );
        assert_eq!(
            decision("what did we decide about the venue"),
            Some(DecisionSentence::Recall {
                query: "the venue".into()
            })
        );
        assert_eq!(
            parse("my timezone is europe/lisbon"),
            Some(Sentence::MyZone {
                zone: "Europe/Lisbon".into()
            })
        );
        assert_eq!(
            parse("I\u{2019}m in Lisbon this month"),
            Some(Sentence::MyZone {
                zone: "Europe/Lisbon".into()
            })
        );
        for text in [
            "polls close at 8",
            "go",
            "I'm in",
            "I'm on it",
            "deadlines suck",
        ] {
            assert_eq!(decision(text), None, "{text}");
        }
        assert_eq!(parse("I'm on it"), None);
        assert_eq!(parse("go"), Some(Sentence::Setting(SettingChange::Go)));
    }

    #[test]
    fn promise_tab_and_schedule_sentences_parse() {
        use crate::commitment::Ask;
        let ask = |text: &str| match parse(text) {
            Some(Sentence::Commitments(a)) => Some(a),
            _ => None,
        };
        assert_eq!(ask("What's outstanding?"), Some(Ask::Outstanding));
        assert_eq!(ask("what's due this week"), Some(Ask::DueThisWeek));
        assert_eq!(
            ask("what did Alex commit to?"),
            Some(Ask::By {
                name: "alex".into()
            })
        );
        assert_eq!(
            ask("what have I committed to"),
            Some(Ask::By { name: "i".into() })
        );
        let tab = |text: &str| match parse(text) {
            Some(Sentence::Tab(t)) => Some(t),
            _ => None,
        };
        assert_eq!(
            tab("what do I owe?"),
            Some(TabSentence::Balances { mine: true })
        );
        assert_eq!(
            tab("what's the tab"),
            Some(TabSentence::Balances { mine: false })
        );
        assert_eq!(tab("balances"), Some(TabSentence::Balances { mine: false }));
        assert_eq!(tab("settle up"), Some(TabSentence::SettleUp));
        assert_eq!(tab("paid"), Some(TabSentence::Paid));
        assert_eq!(tab("not me"), Some(TabSentence::NotMe));
        assert_eq!(tab("double for me"), Some(TabSentence::Double));
        assert_eq!(
            tab("My payment link is https://monzo.me/Priya"),
            Some(TabSentence::PaymentLink {
                link: "https://monzo.me/Priya".into()
            })
        );
        assert_eq!(parse("calendar link"), Some(Sentence::CalendarLink));
        for (text, change) in [
            (
                "digest at 4pm",
                SettingChange::DigestAt {
                    hour: 16,
                    minute: 0,
                },
            ),
            (
                "digest at 4:30",
                SettingChange::DigestAt {
                    hour: 16,
                    minute: 30,
                },
            ),
            ("no digest", SettingChange::Digest { on: false }),
            ("digest on", SettingChange::Digest { on: true }),
            ("day-of summary off", SettingChange::DayOf { on: false }),
            ("day of summary on", SettingChange::DayOf { on: true }),
        ] {
            assert_eq!(setting(text), Some(change), "{text}");
        }
        assert!(matches!(
            parse("monthly dinner on the first Thursday"),
            Some(Sentence::Recur(RecurSentence::New(_)))
        ));
        assert!(matches!(
            parse("pause the dinner"),
            Some(Sentence::Recur(RecurSentence::Pause { .. }))
        ));
        for text in [
            "I'm in",
            "thurs or fri?",
            "what did we decide",
            "not really",
        ] {
            assert!(
                !matches!(
                    parse(text),
                    Some(Sentence::Tab(_) | Sentence::Commitments(_) | Sentence::Recur(_))
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn everything_else_goes_to_the_model() {
        for text in [
            "when was that bar",
            "is Sam a lead?",
            "undone yet?",
            "leads:",
            "",
            "lead the way",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }
}
