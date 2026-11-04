//! The first conversation (DESIGN.md section 7): the person who added the
//! agent is asked by DM what to call it, what kind of group this is, which
//! of two starter voices to use, and whom to listen to, then told what it
//! will and won't do; then the room gets one line saying who it is. A
//! platform that will not DM them first gets a line in the room instead,
//! and a lead or host may take it over by saying "setup" once three days
//! have passed. Anyone who joins later gets the same by one DM.

use crate::effect::{Effect, send, send_dm};
use crate::leads::{self, decline_line, join_names, lead_names};
use crate::observe::span;
use crate::persona::{self, Starter};
use crate::pin;
use crate::profile::LeadsRule;
use crate::reactions::{self, Meaning};
use crate::sentence::{self, KIND_WORDS, LeadsSentence, Sentence, parse_kind};
use crate::settings::{clock, kind_phrase, set_agent_name, set_kind};
use crate::state::GroupState;
use crate::trace::Trace;
use crate::zone::stamp;
use ours_types::{
    ActionId, Instant, Kind, Member, Message, PlatformRole, PlatformRoomId, PlatformUserId, Role,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How long the person asked has before a lead or host may take over.
pub const TAKEOVER_SECONDS: i64 = 3 * 24 * 60 * 60;
/// The answer to "setup" once the first conversation is over.
pub const SETUP_DONE: &str =
    "Setup is done. Change any of it with a sentence, or say settings to see it.";
/// The answer to a reply that does not read as a name.
pub const SHORT_NAME: &str = "A short name works best: one or two words.";
/// The longest name the first conversation takes, in characters.
pub const NAME_MAX_CHARS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    NotStarted,
    /// Asked what people should call it, when the host has not named it.
    AskedName,
    AskedKind,
    /// Offered the two starter voices for the kind's dial.
    AskedPersona,
    AskedLeads,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Setup {
    #[serde(default)]
    pub stage: Stage,
    /// The person being asked.
    #[serde(default)]
    pub with: Option<PlatformUserId>,
    /// The room the agent was added in, where the last message goes.
    #[serde(default)]
    pub room: Option<PlatformRoomId>,
    /// When the current question went out.
    #[serde(default)]
    pub asked_at: Option<Instant>,
    /// The DM carrying the first question, until the platform answers for it.
    #[serde(default)]
    pub asking: Option<ActionId>,
    /// The person the platform would not DM; their next DM gets the first
    /// question again.
    #[serde(default)]
    pub pending_retry: Option<PlatformUserId>,
    /// A reply to the name question did not read as a name and it was asked again.
    #[serde(default)]
    pub name_asked_again: bool,
    /// Someone gave a new name in the first conversation; a takeover keeps it.
    #[serde(default)]
    pub name_chosen: bool,
}

/// One line per kind, in the order of `Kind` and the numbers people reply with.
const KIND_LINES: [&str; 8] = [
    "Friends: plans, the tab, what was said.",
    "Household: bills, chores, the shared calendar.",
    "Team at work: commitments, decisions with a record, a digest.",
    "Project: decisions, commitments, briefing newcomers.",
    "Club or society: attendance, rotas, subs and kit money, events.",
    "Community: answers from memory, events, who is doing what.",
    "Committee: decisions with a record, who agreed to what.",
    "Class or cohort: deadlines, scheduling, answers.",
];

/// Sends the first message to a host in `room`, or else its owner, if
/// the conversation has not started.
pub(crate) fn start(
    state: &mut GroupState,
    room: &PlatformRoomId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if state.setup.stage != Stage::NotStarted {
        return;
    }
    let Some(person) = adder(state, room) else {
        return;
    };
    let trace = Trace::new(
        format!("I was added to {}", state.room_label(room)),
        "the first conversation starts with whoever added me",
        now,
    );
    begin(state, &person, room, trace, fx);
    let name = state.name_of(&person);
    fx.push(Effect::Notice(format!(
        "First conversation started with {name}."
    )));
}

/// Starts the first conversation afresh with `person` and DMs them the
/// first question. The name question is skipped when the host's config or
/// an earlier run of setup already chose the name.
fn begin(
    state: &mut GroupState,
    person: &PlatformUserId,
    room: &PlatformRoomId,
    trace: Trace,
    fx: &mut Vec<Effect>,
) {
    let name_chosen = state.setup.name_chosen;
    let stage = if state.name_from_host || name_chosen {
        Stage::AskedKind
    } else {
        Stage::AskedName
    };
    state.setup = Setup {
        stage,
        with: Some(person.clone()),
        room: Some(room.clone()),
        name_chosen,
        ..Setup::default()
    };
    ask_first(state, person, trace, fx);
}

/// Whether the conversation is still on the question sent by DM unprompted.
fn on_first_question(state: &GroupState) -> bool {
    matches!(state.setup.stage, Stage::AskedName | Stage::AskedKind)
}

/// DMs `person` the current question and waits to hear whether it got through.
fn ask_first(state: &mut GroupState, person: &PlatformUserId, trace: Trace, fx: &mut Vec<Effect>) {
    let now = trace.at;
    let text = current_question(state);
    let id = send_dm(state, fx, person, text, now, trace, false);
    state.setup.asked_at = Some(now);
    state.setup.asking = Some(id);
}

/// `opening`, then "What should people call me here? Say a name, or keep."
fn name_question(opening: &str) -> String {
    format!("{opening} What should people call me here? Say a name, or keep.")
}

/// "Hi, I'm Q."
fn greeting(state: &GroupState) -> String {
    format!("Hi, I'm {}.", state.agent_name)
}

/// `opening`, "What kind of group is this?" and the numbered kinds.
fn kind_question(opening: &str) -> String {
    let mut lines = vec![format!("{opening} What kind of group is this?")];
    lines.extend(
        KIND_LINES
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}. {line}", i + 1)),
    );
    lines.push("Reply with a number, or skip.".to_owned());
    lines.join("\n")
}

/// "Sam, open a chat with me and say hi so I can ask you a few things."
pub fn open_a_chat(name: &str) -> String {
    format!("{name}, open a chat with me and say hi so I can ask you a few things.")
}

/// The first question's DM got through.
pub(crate) fn on_delivered(state: &mut GroupState, id: &ActionId) {
    if state.setup.asking.as_ref() == Some(id) {
        state.setup.asking = None;
    }
}

/// The platform would not carry the first question, as Telegram refuses a
/// DM to anyone who has not opened the bot: ask in the room for a chat, and
/// ask again when they open one.
pub(crate) fn on_refused(
    state: &mut GroupState,
    id: &ActionId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if state.setup.asking.as_ref() != Some(id) {
        return;
    }
    state.setup.asking = None;
    let (Some(person), Some(room)) = (state.setup.with.clone(), state.setup.room.clone()) else {
        return;
    };
    if !on_first_question(state) {
        return;
    }
    state.setup.pending_retry = Some(person.clone());
    let name = state.display_name(&room, &person);
    let trace = Trace::new(
        format!("the platform would not let me DM {name}"),
        "the first conversation asks in the room for a chat it can reach",
        now,
    );
    send(state, fx, &room, open_a_chat(&name), None, now, trace);
    fx.push(Effect::Notice(format!(
        "First conversation: could not DM {name}; asked them in {} to open a chat.",
        state.room_label(&room)
    )));
}

/// `person` opened a chat after a refused DM: the first question again.
fn retry(state: &mut GroupState, person: &PlatformUserId, now: Instant, fx: &mut Vec<Effect>) {
    state.setup.pending_retry = None;
    let name = state.name_of(person);
    let trace = Trace::new(
        format!("{name} opened a chat with me"),
        "the first question goes again once a DM can reach them",
        now,
    );
    ask_first(state, person, trace, fx);
    fx.push(Effect::Notice(format!(
        "First conversation: asked {name} again now that they opened a chat."
    )));
}

/// "setup" by DM. The person being asked hears the current question again;
/// a lead or host takes the conversation over when nobody was asked or the
/// person asked has had three days. Returns false outside a DM, or for a
/// lead in none of the group's rooms.
pub(crate) fn on_setup_word(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    if !state.rooms.get(&message.room).is_some_and(|r| r.is_dm()) {
        return false;
    }
    let user = &message.author;
    let name = state.name_of(user);
    let trigger = format!("{name} said setup");
    let reply = |state: &mut GroupState, fx: &mut Vec<Effect>, text: String, rule: &str| {
        let trace = Trace::new(trigger.clone(), rule, now);
        send(state, fx, &message.room, text, None, now, trace);
    };
    if state.setup.stage == Stage::Done {
        reply(state, fx, SETUP_DONE.to_owned(), "setup runs once");
        return true;
    }
    if state.setup.with.as_ref() == Some(user) && state.setup.stage != Stage::NotStarted {
        state.setup.pending_retry = None;
        if on_first_question(state) {
            let trace = Trace::new(trigger, "setup asks the current question again", now);
            ask_first(state, user, trace, fx);
        } else {
            state.setup.asked_at = Some(now);
            let text = current_question(state);
            reply(state, fx, text, "setup asks the current question again");
        }
        return true;
    }
    let room = state
        .setup
        .room
        .clone()
        .or_else(|| group_room_of(state, user));
    if role < Role::Lead {
        let line = room.map_or_else(
            || format!("{}a lead.", leads::DECLINE_PREFIX),
            |r| decline_line(state, &r),
        );
        reply(
            state,
            fx,
            line,
            "the first conversation is for a lead or host",
        );
        return true;
    }
    let Some(room) = room else {
        return false;
    };
    let waiting = match (&state.setup.with, state.setup.asked_at) {
        (Some(asked), Some(at)) if state.setup.stage != Stage::NotStarted => {
            let until = at.plus_seconds(TAKEOVER_SECONDS);
            (now < until).then(|| (asked.clone(), until))
        }
        _ => None,
    };
    if let Some((asked, until)) = waiting {
        let text = format!(
            "I've asked {}; if they haven't answered by {}, say setup again and I'll ask you.",
            state.display_name(&room, &asked),
            stamp(until, &state.timezone)
        );
        reply(
            state,
            fx,
            text,
            "the person who added me has three days to answer",
        );
        return true;
    }
    let from = state.setup.with.as_ref().map(|u| state.name_of(u));
    let trace = Trace::new(
        trigger,
        "a lead or host takes the first conversation over when it stalls",
        now,
    );
    begin(state, user, &room, trace, fx);
    fx.push(Effect::Notice(match from {
        Some(from) => format!("First conversation taken over by {name} from {from}."),
        None => format!("First conversation started with {name}."),
    }));
    true
}

/// The question the first conversation is waiting on.
fn current_question(state: &GroupState) -> String {
    match state.setup.stage {
        Stage::AskedPersona => {
            let offered = offered(state);
            if offered.is_empty() {
                leads_question(state)
            } else {
                voices_question(&offered)
            }
        }
        Stage::AskedLeads => leads_question(state),
        Stage::AskedName => name_question(&greeting(state)),
        Stage::NotStarted | Stage::AskedKind | Stage::Done => kind_question(&greeting(state)),
    }
}

/// A room of the group's that `user` is in, for a setup started by DM.
fn group_room_of(state: &GroupState, user: &PlatformUserId) -> Option<PlatformRoomId> {
    state
        .rooms
        .iter()
        .find(|(_, r)| r.info.is_some() && !r.is_dm() && r.members.contains_key(user))
        .map(|(id, _)| id.clone())
}

/// A host who is in the room, or else the room's owner. Never a bot.
fn adder(state: &GroupState, room: &PlatformRoomId) -> Option<PlatformUserId> {
    let members = &state.rooms.get(room)?.members;
    let human = |m: &&Member| !m.is_bot && !state.is_me(&m.id);
    members
        .values()
        .filter(human)
        .find(|m| state.hosts.contains(&m.id))
        .or_else(|| {
            members
                .values()
                .filter(human)
                .find(|m| m.platform_role == PlatformRole::Owner)
        })
        .map(|m| m.id.clone())
}

/// Takes a DM reply as the answer to the current question. Returns false
/// when it is not one, so the message is handled as any other.
pub(crate) fn on_reply(
    state: &mut GroupState,
    message: &Message,
    body: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) -> bool {
    let in_dm = state.rooms.get(&message.room).is_some_and(|r| r.is_dm());
    if !in_dm || state.setup.with.as_ref() != Some(&message.author) {
        return false;
    }
    if on_first_question(state) && state.setup.pending_retry.as_ref() == Some(&message.author) {
        retry(state, &message.author, now, fx);
        return true;
    }
    if sentence::parse(body) == Some(Sentence::Setup) {
        return false;
    }
    let answer = body
        .trim()
        .to_lowercase()
        .replace(['\u{2018}', '\u{2019}'], "'")
        .trim_end_matches(['.', '!'])
        .trim()
        .to_owned();
    let trigger = format!(
        "{} answered in the first conversation",
        state.name_of(&message.author)
    );
    match state.setup.stage {
        Stage::AskedName => {
            if sentence::parse(body).is_some() {
                return false;
            }
            on_name(state, message, &trigger, now, fx);
            true
        }
        Stage::AskedKind => {
            if answer == "skip" {
                finish(state, message, true, now, fx);
                return true;
            }
            let Some(kind) = kind_answer(&answer) else {
                return false;
            };
            set_kind(state, kind);
            let offered = offered(state);
            let (text, rule) = if offered.is_empty() {
                state.setup.stage = Stage::AskedLeads;
                (
                    leads_question(state),
                    "the first conversation asks whom to listen to",
                )
            } else {
                state.setup.stage = Stage::AskedPersona;
                (
                    voices_question(&offered),
                    "the first conversation offers the two voices for the kind's dial",
                )
            };
            state.setup.asked_at = Some(now);
            let trace = Trace::new(trigger, rule, now);
            send(state, fx, &message.room, text, None, now, trace);
            let by = state.name_of(&message.author);
            fx.push(Effect::Notice(format!(
                "First conversation: set up as {} by {by}.",
                kind_phrase(kind)
            )));
            true
        }
        Stage::AskedPersona => {
            if answer == "skip" {
                finish(state, message, true, now, fx);
                return true;
            }
            let offered = offered(state);
            let pick = answer.strip_prefix("the ").unwrap_or(&answer);
            let chosen = match pick {
                "keep" | "keep it" | "keep that" | "neither" | "none" | "plain"
                | "plain default" | "default" => None,
                "1" | "2" => pick
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| offered.get(n - 1))
                    .map(|s| s.name.to_owned()),
                name => match persona::starter(name) {
                    Some(s) => Some(s.name.to_owned()),
                    None => return false,
                },
            };
            let question = leads_question(state);
            let text = match &chosen {
                Some(name) => {
                    let line = persona::use_starter(state, name);
                    let by = state.name_of(&message.author);
                    fx.push(Effect::Notice(format!(
                        "First conversation: {line} Chosen by {by}."
                    )));
                    format!("{line} {question}")
                }
                None => question,
            };
            state.setup.stage = Stage::AskedLeads;
            state.setup.asked_at = Some(now);
            let trace = Trace::new(
                trigger,
                "the first conversation asks whom to listen to",
                now,
            );
            send(state, fx, &message.room, text, None, now, trace);
            true
        }
        Stage::AskedLeads => {
            if answer == "skip" {
                finish(state, message, true, now, fx);
                return true;
            }
            let Some(room) = state.setup.room.clone() else {
                finish(state, message, false, now, fx);
                return true;
            };
            let sentence = match answer.as_str() {
                "keep" | "keep it" | "keep them" | "keep that" | "as is" => None,
                "everyone" | "everybody" | "all of us" => Some(LeadsSentence::Everyone),
                names => {
                    let names = names.strip_prefix("leads: ").unwrap_or(names);
                    let names: Vec<String> = names
                        .split(',')
                        .flat_map(|p| p.split(" and "))
                        .map(|n| n.trim().to_owned())
                        .filter(|n| !n.is_empty())
                        .collect();
                    if names.is_empty() {
                        return false;
                    }
                    Some(LeadsSentence::Set(names))
                }
            };
            if let Some(sentence) = sentence {
                let change = match leads::resolve_change(state, message, &room, sentence) {
                    Ok(change) => change,
                    Err(line) => {
                        let trace =
                            Trace::new(trigger, "names resolve to people in the room first", now);
                        send(state, fx, &message.room, line, None, now, trace);
                        return true;
                    }
                };
                let line = leads::apply(state, &room, &change);
                let by = state.name_of(&message.author);
                fx.push(Effect::Notice(format!(
                    "First conversation: {line} Set by {by}."
                )));
            }
            finish(state, message, false, now, fx);
            true
        }
        Stage::NotStarted | Stage::Done => false,
    }
}

/// The answer to the name question: a new name, keep the current one, or
/// ask again once with a hint.
fn on_name(
    state: &mut GroupState,
    message: &Message,
    trigger: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    // The raw text, since "Q is fine" would lose its name to addressing.
    let opening = match name_answer(&message.text, &state.agent_name) {
        NameAnswer::Name(name) => {
            set_agent_name(state, &name);
            state.setup.name_chosen = true;
            let by = state.name_of(&message.author);
            fx.push(Effect::Notice(format!(
                "First conversation: agent name set to {name} by {by}."
            )));
            format!("Thanks, {name} it is.")
        }
        NameAnswer::Keep => format!("Thanks, {} it is.", state.agent_name),
        NameAnswer::Unclear if !state.setup.name_asked_again => {
            state.setup.name_asked_again = true;
            state.setup.asked_at = Some(now);
            let text = name_question(SHORT_NAME);
            let trace = Trace::new(trigger, "a name is one or two short words", now);
            send(state, fx, &message.room, text, None, now, trace);
            return;
        }
        NameAnswer::Unclear => format!("I'll stay {} for now.", state.agent_name),
    };
    state.setup.stage = Stage::AskedKind;
    state.setup.asked_at = Some(now);
    let trace = Trace::new(
        trigger,
        "the first conversation asks what kind of group",
        now,
    );
    send(
        state,
        fx,
        &message.room,
        kind_question(&opening),
        None,
        now,
        trace,
    );
}

#[derive(Debug, PartialEq, Eq)]
enum NameAnswer {
    Name(String),
    Keep,
    Unclear,
}

/// Replies that keep the current name.
const KEEP_NAME: [&str; 25] = [
    "",
    "keep",
    "keep it",
    "keep that",
    "keep the name",
    "leave it",
    "skip",
    "default",
    "same",
    "as is",
    "no",
    "nope",
    "no thanks",
    "not really",
    "none",
    "nothing",
    "no change",
    "fine",
    "that's fine",
    "it's fine",
    "ok",
    "okay",
    "sure",
    "yes",
    "whatever",
];
/// Ways of putting a name before it: "call you Robin".
const NAME_BEFORE: [&str; 9] = [
    "i'll call you ",
    "we'll call you ",
    "let's call you ",
    "call you ",
    "call yourself ",
    "your name is ",
    "how about ",
    "you're ",
    "you are ",
];
/// Ways of putting a name after it: "Robin is fine".
const NAME_AFTER: [&str; 5] = [" is fine", " is good", " is ok", " works", " please"];

/// Reads a reply to the name question, given the name the agent has now.
fn name_answer(text: &str, current: &str) -> NameAnswer {
    let text = text.trim().replace(['\u{2018}', '\u{2019}'], "'");
    let mut name = text.trim_end_matches(['.', '!', '?']).trim();
    if KEEP_NAME.contains(&name.to_lowercase().as_str())
        || strip_prefix_ci(name, "keep ").is_some_and(|rest| rest.eq_ignore_ascii_case(current))
    {
        return NameAnswer::Keep;
    }
    if let Some(rest) = NAME_BEFORE.iter().find_map(|p| strip_prefix_ci(name, p)) {
        name = rest;
    }
    if let Some(rest) = NAME_AFTER.iter().find_map(|p| strip_suffix_ci(name, p)) {
        name = rest;
    }
    let name = name.trim_matches(|c: char| c == ',' || c == '"' || c == '\'' || c.is_whitespace());
    let words: Vec<&str> = name.split_whitespace().collect();
    let word_ok = |w: &&str| {
        w.chars().any(char::is_alphanumeric)
            && w.chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '\'')
    };
    let has_letter = words.iter().any(|w| w.chars().any(char::is_alphabetic));
    if !(1..=2).contains(&words.len()) || !words.iter().all(word_ok) || !has_letter {
        return NameAnswer::Unclear;
    }
    let name = words.join(" ");
    if name.chars().count() > NAME_MAX_CHARS {
        return NameAnswer::Unclear;
    }
    if name.eq_ignore_ascii_case(current) {
        return NameAnswer::Keep;
    }
    // Typed all in lower case, a name gets its capitals.
    if name.chars().any(char::is_uppercase) {
        NameAnswer::Name(name)
    } else {
        NameAnswer::Name(
            words
                .iter()
                .map(|w| capitalise(w))
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = text.len().checked_sub(suffix.len())?;
    let tail = text.get(cut..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &text[..cut])
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// A number from the list or a kind's name.
fn kind_answer(answer: &str) -> Option<Kind> {
    if let Ok(n) = answer.parse::<usize>() {
        return n
            .checked_sub(1)
            .and_then(|i| KIND_WORDS.get(i))
            .map(|(kind, _)| *kind);
    }
    parse_kind(answer)
}

/// The third message by DM and the one line to the room, then done.
fn finish(
    state: &mut GroupState,
    message: &Message,
    skipped: bool,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    state.setup.stage = Stage::Done;
    let text = if skipped {
        format!(
            "Skipped. I'll use the defaults for {}; change any of it with a sentence.",
            kind_phrase(state.kind)
        )
    } else {
        summary(state)
    };
    let trace = Trace::new(
        format!(
            "{} finished the first conversation",
            state.name_of(&message.author)
        ),
        "the first conversation ends with what I will and won't do",
        now,
    );
    send(state, fx, &message.room, text, None, now, trace);
    if let Some(room) = state.setup.room.clone() {
        let line = room_line(state, &room);
        let trace = Trace::new(
            "the first conversation finished",
            "the room hears once who I am, what I keep and who sees it",
            now,
        );
        send(state, fx, &room, line, None, now, trace);
    }
    fx.push(Effect::Notice("First conversation done.".to_owned()));
}

/// The two starters written for the group's dial and kind.
fn offered(state: &GroupState) -> Vec<Starter> {
    persona::offered(state.kind, state.profile.dial)
}

/// "Two voices to pick from: clerk, ...; host, .... Say one, or 'keep' for
/// the plain default."
fn voices_question(offered: &[Starter]) -> String {
    let voices: Vec<String> = offered
        .iter()
        .map(|s| format!("{}, {}", s.name, s.one_line))
        .collect();
    format!(
        "{} voices to pick from: {}. Say one, or 'keep' for the plain default.",
        if offered.len() == 2 { "Two" } else { "Some" },
        voices.join("; ")
    )
}

/// "Who should I take instructions from? Right now: ..."
fn leads_question(state: &GroupState) -> String {
    format!(
        "Who should I take instructions from? Right now: {}. Reply with names, 'everyone', or 'keep'.",
        current_leads(state)
    )
}

fn current_leads(state: &GroupState) -> String {
    if state.profile.leads == LeadsRule::Everyone {
        return "everyone in the group".to_owned();
    }
    let names = state
        .setup
        .room
        .as_ref()
        .map(|room| lead_names(state, room))
        .unwrap_or_default();
    let rule = match state.profile.leads {
        LeadsRule::Admins => " (platform admins)",
        LeadsRule::Moderators => " (moderators)",
        LeadsRule::Everyone | LeadsRule::Named => "",
    };
    if names.is_empty() {
        "nobody yet".to_owned()
    } else {
        format!("{}{rule}", join_names(&names, "and"))
    }
}

/// Five lines from the profile: what it tracks, how often it speaks
/// unprompted, what happens to things that leave the group, what it never
/// does, and how to change it.
pub fn summary(state: &GroupState) -> String {
    let p = &state.profile;
    let quiet = if p.quiet_hours.is_none() {
        String::new()
    } else {
        format!(
            ", never between {} and {}",
            clock(p.quiet_hours.start),
            clock(p.quiet_hours.end)
        )
    };
    let watching = match &state.observe {
        Some(o) => format!(
            " First I watch for {} and DM leads what I would have said.",
            span(o.until.seconds_since(o.started))
        ),
        None => String::new(),
    };
    [
        "I keep track of plans, decisions and who said they'd do what, from this group's rooms only.".to_owned(),
        format!(
            "Unprompted, I speak at most {} an hour and {} a day{quiet}.{watching}",
            p.budget_per_hour, p.budget_per_day
        ),
        format!(
            "Anything that leaves the group waits {} so it can be undone.",
            minutes(p.hold_window_seconds)
        ),
        "I never report on anyone, never repeat a DM to the group, and never take instructions from guests.".to_owned(),
        "Change any of it with a sentence, any time.".to_owned(),
    ]
    .join("\n")
}

/// The one line to the room: who it is, what it keeps, whom it listens to,
/// how to see and delete your own data, and that the host sees everything.
pub fn room_line(state: &GroupState, room: &PlatformRoomId) -> String {
    let listens = listens(state, room, "everyone here");
    let host = host_sees(state);
    let feed = if state.feed_base.is_some() {
        " Ask me for the calendar link to follow the group's calendar."
    } else {
        ""
    };
    format!(
        "I'm {}, this group's agent. I keep plans, decisions and commitments from these rooms, take instructions from {listens}, and anyone can DM me to see or delete what I hold on them. {host}{feed}{}",
        state.agent_name,
        reactions_here(state, room)
    )
}

/// The one DM a new member gets (DESIGN.md sections 8 and 15): who it is,
/// what it keeps, whom it listens to, that nobody gets reports, that the
/// host sees everything, how to see or delete their data, and what is on in
/// the room they joined.
pub fn join_line(
    state: &GroupState,
    room: &PlatformRoomId,
    user: &PlatformUserId,
    now: Instant,
) -> String {
    let mut text = format!(
        "Hi {}, I'm {}, this group's agent. I keep plans, decisions and commitments from its rooms and take instructions from {}. I never report on anyone, and leads see only what you see. {} Say \"what do you know about me\" here to see what I hold on you, or \"forget me\" to delete it.",
        state.display_name(room, user),
        state.agent_name,
        listens(state, room, "everyone in the group"),
        host_sees(state)
    );
    let on = pin::state_lines(state, room, now);
    if !on.is_empty() {
        text.push_str(&format!(
            "\nIn {} now:\n{}",
            state.room_label(room),
            on.join("\n")
        ));
    }
    text
}

/// `member` joined `room`: someone new to the group gets the join DM, once.
/// Bots, people already in the group and anyone joining while the first
/// conversation runs, whom its room line covers, get nothing.
pub(crate) fn welcome(
    state: &mut GroupState,
    room: &PlatformRoomId,
    member: &Member,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let user = &member.id;
    if member.is_bot
        || state.is_me(user)
        || state.rooms.get(room).is_none_or(|r| r.is_dm())
        || !state.welcomed.insert(user.clone())
    {
        return;
    }
    let setting_up = !matches!(state.setup.stage, Stage::NotStarted | Stage::Done);
    let known = state
        .rooms
        .iter()
        .filter(|(id, r)| *id != room && !r.is_dm())
        .any(|(_, r)| r.members.contains_key(user))
        || state
            .rooms
            .values()
            .filter(|r| !r.is_dm())
            .any(|r| r.messages.iter().any(|m| &m.author == user));
    if setting_up || known {
        return;
    }
    let name = state.display_name(room, user);
    let label = state.room_label(room);
    let trace = Trace::new(
        format!("{name} joined {label}"),
        "a new member gets one DM saying what I keep and who sees it",
        now,
    );
    let text = join_line(state, room, user, now);
    send_dm(state, fx, user, text, now, trace, false);
    fx.push(Effect::Notice(format!(
        "Sent {name} the join message on joining {label}."
    )));
}

/// Whom the agent takes instructions from, with `everyone` for a group
/// where every member is a lead.
fn listens(state: &GroupState, room: &PlatformRoomId, everyone: &str) -> String {
    if state.profile.leads == LeadsRule::Everyone {
        return everyone.to_owned();
    }
    let names = lead_names(state, room);
    if names.is_empty() {
        "the leads the host names".to_owned()
    } else {
        join_names(&names, "and")
    }
}

/// "Priya runs me and can see everything."
fn host_sees(state: &GroupState) -> String {
    let hosts: Vec<String> = state.hosts.iter().map(|u| state.name_of(u)).collect();
    match hosts.as_slice() {
        [] => "Whoever runs me can see everything.".to_owned(),
        [one] => format!("{one} runs me and can see everything."),
        many => format!("{} run me and can see everything.", join_names(many, "and")),
    }
}

/// " Here a lead's 👍 approves and 😴 stands me down for the day.", for a
/// platform that swaps either reaction; empty otherwise.
fn reactions_here(state: &GroupState, room: &PlatformRoomId) -> String {
    let swapped = |m: Meaning| {
        let emoji = reactions::emoji_for(state, room, m);
        (emoji != m.canonical()).then_some(emoji)
    };
    match (swapped(Meaning::Check), swapped(Meaning::Shush)) {
        (Some(check), Some(shush)) => {
            format!(" Here a lead's {check} approves and {shush} stands me down for the day.")
        }
        (Some(check), None) => format!(" Here a lead's {check} approves."),
        (None, Some(shush)) => format!(" Here {shush} stands me down for the day."),
        (None, None) => String::new(),
    }
}

fn minutes(seconds: u32) -> String {
    let m = (seconds / 60).max(1);
    if m == 1 {
        "a minute".to_owned()
    } else {
        format!("{m} minutes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_names_pick_a_kind() {
        assert_eq!(kind_answer("1"), Some(Kind::Friends));
        assert_eq!(kind_answer("3"), Some(Kind::Team));
        assert_eq!(kind_answer("8"), Some(Kind::Cohort));
        assert_eq!(kind_answer("9"), None);
        assert_eq!(kind_answer("0"), None);
        assert_eq!(kind_answer("a committee"), Some(Kind::Committee));
        assert_eq!(kind_answer("what?"), None);
        assert_eq!(KIND_LINES.len(), KIND_WORDS.len());
    }

    #[test]
    fn short_replies_read_as_names() {
        let name = |s: &str| NameAnswer::Name(s.to_owned());
        assert_eq!(name_answer("Robin", "Q"), name("Robin"));
        assert_eq!(name_answer("  robin.  ", "Q"), name("Robin"));
        assert_eq!(name_answer("call you Mx Pip", "Q"), name("Mx Pip"));
        assert_eq!(name_answer("Let's call you R2-D2!", "Q"), name("R2-D2"));
        assert_eq!(name_answer("O\u{2019}Brien is fine", "Q"), name("O'Brien"));
        assert_eq!(name_answer("\"Zoë\"", "Q"), name("Zoë"));
        assert_eq!(name_answer("deVries", "Q"), name("deVries"));
        for keep in [
            "keep",
            "Keep.",
            "Q",
            "q is fine",
            "call you Q",
            "skip",
            "",
            "no thanks",
        ] {
            assert_eq!(name_answer(keep, "Q"), NameAnswer::Keep, "{keep}");
        }
        for bad in [
            "what do you think suits you",
            "Robin!!!?? :)",
            "#1",
            "3",
            "---",
            "Bartholomew Montgomeryson",
        ] {
            assert_eq!(name_answer(bad, "Q"), NameAnswer::Unclear, "{bad}");
        }
    }

    #[test]
    fn keeping_means_the_current_name() {
        assert_eq!(name_answer("keep q", "Q"), NameAnswer::Keep);
        assert_eq!(name_answer("Noa", "Noa"), NameAnswer::Keep);
        assert_eq!(name_answer("keep Noa", "Noa"), NameAnswer::Keep);
        assert_eq!(name_answer("Q", "Noa"), NameAnswer::Name("Q".to_owned()));
    }

    #[test]
    fn the_shipped_first_question_names_noa() {
        let state = GroupState::new("g".into(), Kind::Friends);
        assert_eq!(
            name_question(&greeting(&state)),
            "Hi, I'm Noa. What should people call me here? Say a name, or keep."
        );
    }

    #[test]
    fn the_summary_is_five_lines() {
        let state = GroupState::new("g".into(), Kind::Team);
        let text = summary(&state);
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(text.contains("1 an hour and 4 a day, never between 22:00 and 08:00"));
        assert!(text.contains("waits 10 minutes"));
    }
}
