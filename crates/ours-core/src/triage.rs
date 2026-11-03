//! The rules pass of the triage cascade (DESIGN.md section 7): a first score
//! for every message from fixed word lists. It stands in when no model is
//! available and is what the small model refines. It never claims a
//! question is answerable from context; only the model decides that.

use crate::addressing::{after_address, is_addressed};
use crate::state::GroupState;
use ours_types::{Message, MessageKind, PlatformRoomId, PlatformUserId, Score, ScoreSource};
use std::collections::BTreeSet;

pub const ADDRESSED: f32 = 1.0;
pub const PLAN_FORMING: f32 = 0.7;
/// The plan-forming weight for threads started by someone who shushed the
/// agent this week.
pub const PLAN_FORMING_SHUSHED: f32 = 0.35;
pub const UNANSWERED_QUESTION: f32 = 0.6;
pub const FACT: f32 = 0.5;
pub const COMMITMENT: f32 = 0.5;
/// How far back a plan and its replies may be.
pub const PLAN_WINDOW_SECONDS: i64 = 10 * 60;

const QUESTION_WORDS: [&str; 17] = [
    "what", "when", "where", "who", "whom", "whose", "why", "how", "which", "what's", "when's",
    "where's", "who's", "how's", "whats", "wheres", "whos",
];

const QUESTION_OPENERS: [&str; 12] = [
    "anyone know",
    "anybody know",
    "does anyone",
    "does anybody",
    "has anyone",
    "did anyone",
    "is anyone",
    "can anyone",
    "could anyone",
    "do you know",
    "any idea",
    "any ideas",
];

/// Words that may come before a question without changing it: "hey, anyone know".
const LEAD_INS: [&str; 9] = ["hey", "hi", "ok", "okay", "so", "btw", "oh", "um", "guys"];

const SUGGESTIONS: [&str; 16] = [
    "let's",
    "lets",
    "how about",
    "what about",
    "shall we",
    "should we",
    "we could",
    "we should",
    "fancy",
    "up for",
    "anyone want",
    "who wants",
    "wanna",
    "why don't we",
    "why not",
    "keen for",
];

const PLAN_WORDS: [&str; 42] = [
    "dinner",
    "drinks",
    "drink",
    "lunch",
    "brunch",
    "breakfast",
    "coffee",
    "trip",
    "weekend",
    "tonight",
    "tomorrow",
    "this week",
    "next week",
    "meet",
    "meetup",
    "meet up",
    "book",
    "booking",
    "game",
    "match",
    "cinema",
    "movie",
    "film",
    "party",
    "bbq",
    "barbecue",
    "picnic",
    "holiday",
    "hike",
    "gig",
    "concert",
    "pub",
    "bar",
    "restaurant",
    "table",
    "tickets",
    "takeaway",
    "quiz",
    "karaoke",
    "festival",
    "camping",
    "walk",
];

const GREETINGS: [&str; 17] = [
    "hi",
    "hey",
    "heya",
    "hello",
    "hiya",
    "howdy",
    "yo",
    "sup",
    "wassup",
    "morning",
    "afternoon",
    "evening",
    "gm",
    "hallo",
    "hola",
    "greetings",
    "oi",
];

const GREETING_TIMES: [&str; 4] = ["morning", "afternoon", "evening", "night"];

const IMPERATIVES: [&str; 46] = [
    "remind",
    "tell",
    "add",
    "remove",
    "make",
    "set",
    "book",
    "send",
    "post",
    "show",
    "list",
    "find",
    "check",
    "cancel",
    "stop",
    "start",
    "schedule",
    "create",
    "delete",
    "forget",
    "note",
    "remember",
    "put",
    "give",
    "open",
    "close",
    "pin",
    "summarise",
    "summarize",
    "ping",
    "ask",
    "let",
    "undo",
    "mute",
    "track",
    "split",
    "move",
    "change",
    "update",
    "turn",
    "dial",
    "watch",
    "explain",
    "write",
    "draft",
    "please",
];

const POLITE_REQUESTS: [&str; 4] = ["can you", "could you", "would you", "will you"];

const FIRST_PERSON: [&str; 6] = ["i", "i'm", "i've", "im", "ive", "my"];

const DIETARY: [&str; 16] = [
    "vegetarian",
    "veggie",
    "vegan",
    "pescatarian",
    "gluten",
    "dairy",
    "lactose",
    "allergic",
    "allergy",
    "allergies",
    "coeliac",
    "celiac",
    "halal",
    "kosher",
    "teetotal",
    "intolerant",
];

const DURABLE: [&str; 16] = [
    "my new number",
    "my number is",
    "my new address",
    "my address is",
    "my new email",
    "my email is",
    "my birthday is",
    "my birthday's",
    "i moved to",
    "i've moved to",
    "i have moved to",
    "i'm moving to",
    "we moved to",
    "i live in",
    "i now live",
    "i'm based in",
];

const COMMITMENTS: [&str; 13] = [
    "i'll",
    "i will",
    "i can bring",
    "i can get",
    "i can book",
    "i can drive",
    "i can pick",
    "i can grab",
    "i can sort",
    "leave it with me",
    "i'm on it",
    "i've got it",
    "i've got this",
];

/// First-person ways of saying money was paid for the group.
const PAID_FOR: [&str; 12] = [
    "i got",
    "i've got",
    "i paid",
    "i've paid",
    "i bought",
    "i've bought",
    "i covered",
    "i grabbed",
    "i picked up",
    "i put in",
    "i paid for",
    "i shouted",
];

/// First-person ways of saying someone will be away.
const AWAY: [&str; 12] = [
    "i'm away",
    "i am away",
    "i'll be away",
    "i will be away",
    "i'm off from",
    "i'm on leave",
    "i'll be on leave",
    "i'm on holiday",
    "i'll be on holiday",
    "out of office",
    "i'm travelling",
    "i'll be travelling",
];

/// A message's words, lower-cased, with curly apostrophes straightened and
/// every other mark turned into a space.
struct Words {
    raw: String,
    tokens: Vec<String>,
    /// The tokens joined with single spaces and padded at both ends, so a
    /// phrase matches only on word boundaries.
    padded: String,
    /// `padded` without up to two lead-in words.
    opening: String,
}

impl Words {
    fn new(text: &str) -> Words {
        let raw = text
            .trim()
            .to_lowercase()
            .replace(['\u{2018}', '\u{2019}'], "'");
        let cleaned: String = raw
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        let tokens: Vec<String> = cleaned
            .split_whitespace()
            .map(|t| t.trim_matches('\'').to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let padded = format!(" {} ", tokens.join(" "));
        let skip = tokens
            .iter()
            .take(2)
            .take_while(|t| LEAD_INS.contains(&t.as_str()))
            .count()
            .min(tokens.len().saturating_sub(1));
        let opening = format!(" {} ", tokens[skip..].join(" "));
        Words {
            raw,
            tokens,
            padded,
            opening,
        }
    }

    fn first(&self) -> &str {
        self.tokens.first().map_or("", String::as_str)
    }

    fn opens_with(&self, phrase: &str) -> bool {
        self.opening.starts_with(&format!(" {phrase} "))
    }

    fn has(&self, phrase: &str) -> bool {
        self.padded.contains(&format!(" {phrase} "))
    }

    fn has_any(&self, phrases: &[&str]) -> bool {
        phrases.iter().any(|p| self.has(p))
    }

    fn starts_with(&self, phrase: &str) -> bool {
        self.padded.starts_with(&format!(" {phrase} "))
    }

    fn is_question(&self) -> bool {
        if self.raw.ends_with('?') {
            return true;
        }
        // "what a goal" is an exclamation, not a question.
        if self.opens_with("what a") || self.opens_with("what an") {
            return false;
        }
        QUESTION_WORDS.iter().any(|w| self.opens_with(w))
            || QUESTION_OPENERS.iter().any(|p| self.opens_with(p))
    }

    fn is_greeting(&self) -> bool {
        if self.tokens.is_empty() || self.tokens.len() > 4 || self.raw.ends_with('?') {
            return false;
        }
        let second = self.tokens.get(1).map_or("", String::as_str);
        GREETINGS.contains(&self.first())
            || (self.first() == "good" && GREETING_TIMES.contains(&second))
    }

    fn is_imperative(&self) -> bool {
        IMPERATIVES.contains(&self.first()) || POLITE_REQUESTS.iter().any(|p| self.starts_with(p))
    }

    fn is_plan(&self) -> bool {
        (self.is_question() || self.has_any(&SUGGESTIONS)) && self.has_any(&PLAN_WORDS)
    }

    fn is_fact(&self) -> bool {
        if self.is_question() {
            return false;
        }
        self.has_any(&DURABLE)
            || self.has_any(&AWAY)
            || (self.has_any(&FIRST_PERSON) && self.has_any(&DIETARY))
    }

    fn is_commitment(&self) -> bool {
        self.has_any(&COMMITMENTS) || self.is_money()
    }

    /// "I got the wine, 38": a payment with an amount.
    fn is_money(&self) -> bool {
        !self.is_question()
            && self.has_any(&PAID_FOR)
            && self
                .tokens
                .iter()
                .any(|t| t.chars().all(|c| c.is_ascii_digit()))
    }
}

/// The rules pass's score for `message` in `room`.
pub fn rules_score(state: &GroupState, room: &PlatformRoomId, message: &Message) -> Score {
    let addressed = is_addressed(state, message);
    let body = if addressed {
        after_address(state, &message.text)
    } else {
        &message.text
    };
    let words = Words::new(body);
    let human = state.is_human(room, &message.author);
    let fact = human && words.is_fact();
    let commitment = human && words.is_commitment();
    let kind = if words.is_question() {
        MessageKind::Question
    } else if addressed && words.is_imperative() {
        MessageKind::Instruction
    } else if words.is_greeting() {
        MessageKind::Greeting
    } else if fact || commitment {
        MessageKind::Statement
    } else {
        MessageKind::Other
    };
    let plan_forming = if human {
        plan_weight(state, room, message, &words)
    } else {
        0.0
    };
    let open_decision = if human {
        crate::decide::rules_signal(state, room, body)
    } else {
        0.0
    };
    Score {
        addressed: if addressed { ADDRESSED } else { 0.0 },
        open_decision,
        plan_forming,
        unanswered_question: if human && !addressed && kind == MessageKind::Question {
            UNANSWERED_QUESTION
        } else {
            0.0
        },
        fact: if fact { FACT } else { 0.0 },
        commitment: if commitment { COMMITMENT } else { 0.0 },
        banter: 0.0,
        kind,
        answerable_from_context: false,
        source: ScoreSource::Rules,
    }
}

/// The message that started a plan `message` belongs to: itself when it is
/// one, else the latest plan in the ten minutes before it.
pub(crate) fn plan_root<'a>(
    state: &'a GroupState,
    room: &PlatformRoomId,
    message: &Message,
) -> Option<&'a Message> {
    let r = state.rooms.get(room)?;
    let since = message.at.plus_seconds(-PLAN_WINDOW_SECONDS);
    let end = r
        .messages
        .iter()
        .position(|m| m.id == message.id)
        .map_or(r.messages.len(), |i| i + 1);
    r.messages
        .range(..end)
        .rev()
        .filter(|m| m.at >= since && state.is_human(room, &m.author))
        .find(|m| Words::new(&m.text).is_plan())
}

/// What a plan is about, from its first plan word: "Dinner", "Drinks".
pub(crate) fn plan_topic(text: &str) -> Option<String> {
    let words = Words::new(text);
    let word = PLAN_WORDS
        .iter()
        .filter(|w| {
            !matches!(
                **w,
                "tonight"
                    | "tomorrow"
                    | "this week"
                    | "next week"
                    | "weekend"
                    | "book"
                    | "booking"
                    | "meet"
                    | "table"
            )
        })
        .find(|w| words.has(w))?;
    Some(crate::proposal::capitalise(word))
}

/// The plan-forming weight: a question or suggestion with plan words in the
/// last ten minutes, this message included, with at least two people other
/// than its author posting in that time.
fn plan_weight(state: &GroupState, room: &PlatformRoomId, message: &Message, words: &Words) -> f32 {
    let since = message.at.plus_seconds(-PLAN_WINDOW_SECONDS);
    let earlier: Vec<&Message> = state
        .rooms
        .get(room)
        .map(|r| {
            r.messages
                .iter()
                .filter(|m| m.id != message.id && m.at >= since && m.at <= message.at)
                .filter(|m| state.is_human(room, &m.author))
                .collect()
        })
        .unwrap_or_default();
    let plan_author = if words.is_plan() {
        Some(&message.author)
    } else {
        earlier
            .iter()
            .rev()
            .find(|m| Words::new(&m.text).is_plan())
            .map(|m| &m.author)
    };
    let Some(plan_author) = plan_author else {
        return 0.0;
    };
    let others: BTreeSet<&PlatformUserId> = earlier
        .iter()
        .map(|m| &m.author)
        .chain(std::iter::once(&message.author))
        .filter(|a| *a != plan_author)
        .collect();
    if others.len() < 2 {
        return 0.0;
    }
    let shushed = state
        .shushed
        .get(plan_author)
        .is_some_and(|until| *until > message.at);
    if shushed {
        PLAN_FORMING_SHUSHED
    } else {
        PLAN_FORMING
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Me;
    use ours_types::{GroupId, Instant, Kind, Platform, PlatformMessageId, Room, RoomKind};

    const T0: i64 = 1_793_642_400_000;

    fn group() -> GroupState {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        state.agent_name = crate::sim::SIM_AGENT_NAME.to_owned();
        state.me.push(Me {
            platform: Platform::Sim,
            id: "bot".into(),
            name: "Ours".into(),
            capabilities: Default::default(),
        });
        let room = state.room_mut(&"r".into());
        room.info = Some(Room {
            platform: Platform::Sim,
            id: "r".into(),
            name: Some("chat".into()),
            kind: RoomKind::Channel,
            parent: None,
            container: None,
        });
        state
    }

    fn message(id: &str, author: &str, text: &str, seconds: i64) -> Message {
        Message {
            id: PlatformMessageId::new(id),
            room: "r".into(),
            author: author.into(),
            text: text.to_owned(),
            at: Instant::from_millis(T0 + seconds * 1000).unwrap(),
            reply_to: None,
            mentions: Vec::new(),
            mentions_me: false,
            attachments: Vec::new(),
            edited: false,
        }
    }

    fn score(text: &str) -> Score {
        let state = group();
        rules_score(&state, &"r".into(), &message("m", "sam", text, 0))
    }

    #[test]
    fn a_table_of_sentences() {
        use MessageKind::{Greeting, Instruction, Other, Question, Statement};
        // text, kind, addressed, unanswered question, fact, commitment
        let table: [(&str, MessageKind, f32, f32, f32, f32); 30] = [
            ("what time was it again", Question, 0.0, 0.6, 0.0, 0.0),
            ("is the bar open?", Question, 0.0, 0.6, 0.0, 0.0),
            (
                "anyone know the wifi password",
                Question,
                0.0,
                0.6,
                0.0,
                0.0,
            ),
            ("Does anyone have a charger", Question, 0.0, 0.6, 0.0, 0.0),
            ("who\u{2019}s driving", Question, 0.0, 0.6, 0.0, 0.0),
            ("what a goal", Other, 0.0, 0.0, 0.0, 0.0),
            ("that was never offside", Other, 0.0, 0.0, 0.0, 0.0),
            ("Q, when was that", Question, 1.0, 0.0, 0.0, 0.0),
            ("Q, remind me at 4", Instruction, 1.0, 0.0, 0.0, 0.0),
            (
                "Q, could you book a table for six",
                Instruction,
                1.0,
                0.0,
                0.0,
                0.0,
            ),
            ("Q, thanks", Other, 1.0, 0.0, 0.0, 0.0),
            ("Q, hi", Greeting, 1.0, 0.0, 0.0, 0.0),
            ("hello", Greeting, 0.0, 0.0, 0.0, 0.0),
            ("morning all", Greeting, 0.0, 0.0, 0.0, 0.0),
            ("Good evening everyone!", Greeting, 0.0, 0.0, 0.0, 0.0),
            (
                "hey, anyone know when the match starts",
                Question,
                0.0,
                0.6,
                0.0,
                0.0,
            ),
            ("I'm vegetarian now", Statement, 0.0, 0.0, 0.5, 0.0),
            ("I\u{2019}m off dairy now", Statement, 0.0, 0.0, 0.5, 0.0),
            (
                "my new number is 0412 555 019",
                Statement,
                0.0,
                0.0,
                0.5,
                0.0,
            ),
            ("I moved to Leith last month", Statement, 0.0, 0.0, 0.5, 0.0),
            ("I'm allergic to peanuts", Statement, 0.0, 0.0, 0.5, 0.0),
            ("is it vegan?", Question, 0.0, 0.6, 0.0, 0.0),
            ("I've gone vegan", Statement, 0.0, 0.0, 0.5, 0.0),
            ("I'll get the tickets", Statement, 0.0, 0.0, 0.0, 0.5),
            ("I will do the booking", Statement, 0.0, 0.0, 0.0, 0.5),
            ("I can drive on Saturday", Statement, 0.0, 0.0, 0.0, 0.5),
            ("leave it with me", Statement, 0.0, 0.0, 0.0, 0.5),
            ("I can't make it", Other, 0.0, 0.0, 0.0, 0.0),
            ("will I need a coat?", Question, 0.0, 0.6, 0.0, 0.0),
            ("", Other, 0.0, 0.0, 0.0, 0.0),
        ];
        for (text, kind, addressed, question, fact, commitment) in table {
            let s = score(text);
            assert_eq!(s.kind, kind, "{text}");
            assert_eq!(s.addressed, addressed, "{text}");
            assert_eq!(s.unanswered_question, question, "{text}");
            assert_eq!(s.fact, fact, "{text}");
            assert_eq!(s.commitment, commitment, "{text}");
            assert!(!s.answerable_from_context, "{text}");
            assert_eq!(s.source, ScoreSource::Rules);
        }
    }

    #[test]
    fn a_plan_forms_when_two_others_join_in() {
        let mut state = group();
        let room: PlatformRoomId = "r".into();
        let ask = message("m1", "sam", "dinner this week?", 0);
        assert_eq!(rules_score(&state, &room, &ask).plan_forming, 0.0);
        state.room_mut(&room).push(ask);
        let first = message("m2", "priya", "yes pls", 60);
        assert_eq!(rules_score(&state, &room, &first).plan_forming, 0.0);
        state.room_mut(&room).push(first);
        let second = message("m3", "jo", "thurs works", 120);
        assert_eq!(
            rules_score(&state, &room, &second).plan_forming,
            PLAN_FORMING
        );

        // The agent and the plan's author do not count as replies.
        let from_sam = message("m4", "sam", "great", 130);
        let mut quieter = group();
        quieter
            .room_mut(&room)
            .push(message("m1", "sam", "dinner this week?", 0));
        quieter
            .room_mut(&room)
            .push(message("m2", "bot", "Thursday?", 10));
        quieter
            .room_mut(&room)
            .push(message("m3", "priya", "yes", 20));
        assert_eq!(rules_score(&quieter, &room, &from_sam).plan_forming, 0.0);

        // Ten minutes later the plan has gone cold.
        let late = message("m5", "alex", "me too", 700);
        assert_eq!(rules_score(&state, &room, &late).plan_forming, 0.0);

        // A suggestion asked into a busy room is a plan at once.
        let mut busy = group();
        busy.room_mut(&room).push(message("m1", "priya", "ha", 0));
        busy.room_mut(&room).push(message("m2", "jo", "lol", 10));
        let suggest = message("m3", "sam", "let's do drinks friday", 20);
        assert_eq!(
            rules_score(&busy, &room, &suggest).plan_forming,
            PLAN_FORMING
        );
        let chat = message("m3", "sam", "the drinks were good", 20);
        assert_eq!(rules_score(&busy, &room, &chat).plan_forming, 0.0);
    }

    #[test]
    fn a_shush_lowers_that_members_plans_for_a_week() {
        let mut state = group();
        let room: PlatformRoomId = "r".into();
        state
            .room_mut(&room)
            .push(message("m1", "sam", "pub tonight?", 0));
        state
            .room_mut(&room)
            .push(message("m2", "priya", "maybe", 30));
        let reply = message("m3", "jo", "in", 60);
        let until = Instant::from_millis(T0 + 7 * 86_400_000).unwrap();
        state.shushed.insert("sam".into(), until);
        assert_eq!(
            rules_score(&state, &room, &reply).plan_forming,
            PLAN_FORMING_SHUSHED
        );
        state
            .shushed
            .insert("sam".into(), Instant::from_millis(T0).unwrap());
        assert_eq!(
            rules_score(&state, &room, &reply).plan_forming,
            PLAN_FORMING
        );
    }

    #[test]
    fn the_agent_and_bots_score_nothing_but_the_address() {
        let state = group();
        let own = rules_score(&state, &"r".into(), &message("m", "bot", "what time?", 0));
        assert_eq!(own.max(), 0.0);
        assert_eq!(own.kind, MessageKind::Question);
    }
}
