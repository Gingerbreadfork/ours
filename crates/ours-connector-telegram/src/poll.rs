//! Native polls: the limits Telegram sets, the request that creates one,
//! and how votes and results map back to option indexes. Lengths are in
//! UTF-16 code units, as Telegram counts them.

use crate::map::Target;
use crate::split::utf16_len;
use frankenstein::methods::SendPollParams;
use frankenstein::types::{InputPollOption, Poll};
use std::collections::BTreeSet;

/// Two options at least, as the engine's contract says, and the twelve
/// Telegram allows at most.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 12;

const MAX_QUESTION: usize = 300;
const MAX_OPTION: usize = 100;

/// The window Telegram accepts for closing a poll on its own, in seconds.
pub const MIN_OPEN_SECONDS: u32 = 5;
pub const MAX_OPEN_SECONDS: u32 = 2_628_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PollError {
    #[error("a poll needs at least 2 options, got {0}")]
    TooFewOptions(usize),
    #[error("Telegram allows at most 12 poll options, got {0}")]
    TooManyOptions(usize),
    #[error("the poll question is empty")]
    EmptyQuestion,
    #[error("the poll question is {0} characters, Telegram allows 300")]
    QuestionTooLong(usize),
    #[error("poll option {0} is empty")]
    EmptyOption(usize),
    #[error("poll option {0} is {1} characters, Telegram allows 100")]
    OptionTooLong(usize, usize),
}

/// Check a question and its options before anything is sent. Option
/// numbers in errors start at 1, as a person would count them.
pub fn validate(question: &str, options: &[String]) -> Result<(), PollError> {
    if options.len() < MIN_OPTIONS {
        return Err(PollError::TooFewOptions(options.len()));
    }
    if options.len() > MAX_OPTIONS {
        return Err(PollError::TooManyOptions(options.len()));
    }
    let question = question.trim();
    if question.is_empty() {
        return Err(PollError::EmptyQuestion);
    }
    if utf16_len(question) > MAX_QUESTION {
        return Err(PollError::QuestionTooLong(utf16_len(question)));
    }
    for (index, option) in options.iter().enumerate() {
        let option = option.trim();
        if option.is_empty() {
            return Err(PollError::EmptyOption(index + 1));
        }
        if utf16_len(option) > MAX_OPTION {
            return Err(PollError::OptionTooLong(index + 1, utf16_len(option)));
        }
    }
    Ok(())
}

/// How long Telegram should keep the poll open, when `hours` fits its
/// window. Otherwise the poll stays open until the engine ends it.
pub fn open_period(hours: u32) -> Option<u32> {
    let seconds = hours.checked_mul(3600)?;
    (MIN_OPEN_SECONDS..=MAX_OPEN_SECONDS)
        .contains(&seconds)
        .then_some(seconds)
}

/// The `sendPoll` request: named voters so votes carry people, plain-text
/// options in the order given.
pub fn params(
    target: Target,
    question: &str,
    options: &[String],
    multiple: bool,
    hours: u32,
) -> Result<SendPollParams, PollError> {
    validate(question, options)?;
    let options = options
        .iter()
        .map(|option| InputPollOption::builder().text(option.trim()).build())
        .collect();
    Ok(SendPollParams::builder()
        .chat_id(target.chat)
        .maybe_message_thread_id(target.topic)
        .question(question.trim())
        .options(options)
        .is_anonymous(false)
        .allows_multiple_answers(multiple)
        .maybe_open_period(open_period(hours))
        .build())
}

/// Vote counts in option order.
pub fn counts(poll: &Poll) -> Vec<u32> {
    poll.options.iter().map(|o| o.voter_count).collect()
}

/// The option indexes a voter chose, without any the poll does not have.
pub fn choice(option_ids: &[u8], options: usize) -> BTreeSet<u32> {
    option_ids
        .iter()
        .filter(|id| usize::from(**id) < options)
        .map(|id| u32::from(*id))
        .collect()
}

/// What changed between a voter's old and new choice: removals first, then
/// additions, each as `(option, added)`.
pub fn changes(before: &BTreeSet<u32>, after: &BTreeSet<u32>) -> Vec<(u32, bool)> {
    before
        .difference(after)
        .map(|o| (*o, false))
        .chain(after.difference(before).map(|o| (*o, true)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("option {i}")).collect()
    }

    fn target() -> Target {
        Target {
            chat: -1_001_234_567_890,
            topic: None,
        }
    }

    #[test]
    fn two_to_twelve_options_are_accepted() {
        for n in 2..=12 {
            assert_eq!(validate("when?", &options(n)), Ok(()), "{n} options");
        }
        assert_eq!(
            validate("when?", &options(1)),
            Err(PollError::TooFewOptions(1))
        );
        let error = validate("when?", &options(13)).unwrap_err();
        assert_eq!(error, PollError::TooManyOptions(13));
        assert_eq!(
            error.to_string(),
            "Telegram allows at most 12 poll options, got 13"
        );
    }

    #[test]
    fn labels_over_100_and_questions_over_300_are_refused() {
        let mut long = options(3);
        long[2] = "x".repeat(101);
        let error = validate("when?", &long).unwrap_err();
        assert_eq!(error, PollError::OptionTooLong(3, 101));
        assert_eq!(
            error.to_string(),
            "poll option 3 is 101 characters, Telegram allows 100"
        );
        long[2] = "x".repeat(100);
        assert_eq!(validate("when?", &long), Ok(()));

        assert_eq!(
            validate(&"q".repeat(301), &options(2)),
            Err(PollError::QuestionTooLong(301))
        );
        assert_eq!(validate(&"q".repeat(300), &options(2)), Ok(()));
        assert_eq!(validate("  ", &options(2)), Err(PollError::EmptyQuestion));

        let mut blank = options(2);
        blank[0] = " ".to_owned();
        assert_eq!(validate("when?", &blank), Err(PollError::EmptyOption(1)));
    }

    #[test]
    fn emoji_in_a_label_count_as_telegram_counts_them() {
        let mut set = options(2);
        set[0] = "🍕".repeat(50);
        assert_eq!(validate("when?", &set), Ok(()));
        set[0] = "🍕".repeat(51);
        assert_eq!(
            validate("when?", &set),
            Err(PollError::OptionTooLong(1, 102))
        );
    }

    #[test]
    fn hours_within_telegrams_window_become_an_open_period() {
        assert_eq!(open_period(1), Some(3600));
        assert_eq!(open_period(24), Some(86_400));
        assert_eq!(open_period(730), Some(2_628_000));
    }

    #[test]
    fn hours_outside_the_window_leave_the_poll_to_the_engine() {
        assert_eq!(open_period(0), None);
        assert_eq!(open_period(731), None);
        assert_eq!(open_period(768), None);
        assert_eq!(open_period(u32::MAX), None);
    }

    #[test]
    fn the_request_names_voters_and_keeps_option_order() {
        let sent = params(
            Target {
                chat: -10042,
                topic: Some(7),
            },
            " Which day? ",
            &[" Thursday ".to_owned(), "Friday".to_owned()],
            true,
            48,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&sent).unwrap(),
            json!({
                "chat_id": -10042,
                "message_thread_id": 7,
                "question": "Which day?",
                "options": [{ "text": "Thursday" }, { "text": "Friday" }],
                "is_anonymous": false,
                "allows_multiple_answers": true,
                "open_period": 172_800,
            })
        );
        let long = params(target(), "when?", &options(2), false, 1000).unwrap();
        assert_eq!(long.open_period, None);
        assert_eq!(long.close_date, None);
        assert_eq!(long.allows_multiple_answers, Some(false));
        assert_eq!(
            params(target(), "when?", &options(13), false, 24).unwrap_err(),
            PollError::TooManyOptions(13)
        );
    }

    #[test]
    fn counts_follow_option_order() {
        let poll: Poll = serde_json::from_value(json!({
            "id": "P1", "question": "when?", "total_voter_count": 5, "is_closed": true,
            "is_anonymous": false, "type": "regular",
            "options": [
                { "text": "Thu", "voter_count": 3 },
                { "text": "Fri", "voter_count": 0 },
                { "text": "Sat", "voter_count": 2 }
            ]
        }))
        .unwrap();
        assert_eq!(counts(&poll), vec![3, 0, 2]);
    }

    #[test]
    fn a_new_choice_is_removals_then_additions() {
        let set = |ids: &[u32]| ids.iter().copied().collect::<BTreeSet<u32>>();
        assert_eq!(changes(&set(&[]), &set(&[1])), vec![(1, true)]);
        assert_eq!(
            changes(&set(&[0, 2]), &set(&[2, 3])),
            vec![(0, false), (3, true)]
        );
        assert_eq!(changes(&set(&[1]), &set(&[])), vec![(1, false)]);
        assert!(changes(&set(&[1]), &set(&[1])).is_empty());
    }

    #[test]
    fn options_the_poll_lacks_are_dropped() {
        let chosen = choice(&[0, 3, 9], 4);
        assert_eq!(chosen.into_iter().collect::<Vec<_>>(), vec![0, 3]);
    }
}
