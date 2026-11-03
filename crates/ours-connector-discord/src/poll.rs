//! Native polls: the limits Discord sets, the body that creates one, and the
//! mapping from Discord's answer ids and results back to option indexes.
//!
//! twilight's `Poll` has no `duration`, only the `expiry` Discord reports
//! back, so the create body is built here and sent as the raw message JSON.

use serde_json::{Value, json};
use twilight_model::poll::{Poll, PollLayoutType};

/// Discord allows two to ten answers.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 10;

/// How long a poll may stay open, in hours: one hour to 32 days.
pub const MIN_HOURS: u32 = 1;
pub const MAX_HOURS: u32 = 768;

/// Text limits in characters.
const MAX_QUESTION: usize = 300;
const MAX_ANSWER: usize = 55;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PollError {
    #[error("a poll needs at least 2 options, got {0}")]
    TooFewOptions(usize),
    #[error("a poll takes at most 10 options, got {0}")]
    TooManyOptions(usize),
    #[error("the poll question is empty")]
    EmptyQuestion,
    #[error("the poll question is {0} characters, Discord allows 300")]
    QuestionTooLong(usize),
    #[error("poll option {0} is empty")]
    EmptyOption(usize),
    #[error("poll option {0} is {1} characters, Discord allows 55")]
    OptionTooLong(usize, usize),
}

/// `hours` held to what Discord accepts.
pub fn clamp_hours(hours: u32) -> u32 {
    let clamped = hours.clamp(MIN_HOURS, MAX_HOURS);
    if clamped != hours {
        tracing::warn!(requested = hours, used = clamped, "poll duration clamped");
    }
    clamped
}

/// Check a question and its options before anything is sent. Option numbers
/// in errors start at 1, as a person would count them.
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
    let length = question.chars().count();
    if length > MAX_QUESTION {
        return Err(PollError::QuestionTooLong(length));
    }
    for (index, option) in options.iter().enumerate() {
        let option = option.trim();
        if option.is_empty() {
            return Err(PollError::EmptyOption(index + 1));
        }
        let length = option.chars().count();
        if length > MAX_ANSWER {
            return Err(PollError::OptionTooLong(index + 1, length));
        }
    }
    Ok(())
}

/// The JSON body of a message that carries only a poll: plain-text answers
/// in the order given, the default layout, and no mentions parsed.
pub fn create_body(
    question: &str,
    options: &[String],
    multiple: bool,
    hours: u32,
) -> Result<Value, PollError> {
    validate(question, options)?;
    let answers: Vec<Value> = options
        .iter()
        .map(|option| json!({ "poll_media": { "text": option.trim() } }))
        .collect();
    Ok(json!({
        "poll": {
            "question": { "text": question.trim() },
            "answers": answers,
            "allow_multiselect": multiple,
            "duration": clamp_hours(hours),
            "layout_type": u8::from(PollLayoutType::Default),
        },
        "allowed_mentions": { "parse": [] },
    }))
}

/// Discord numbers answers from 1; the engine counts options from 0.
pub fn option_index(answer_id: u8) -> Option<u32> {
    answer_id.checked_sub(1).map(u32::from)
}

/// Vote counts in option order, once Discord has counted the votes
/// precisely. `None` while the poll is open or carries no results.
pub fn final_counts(poll: &Poll) -> Option<Vec<u32>> {
    let results = poll.results.as_ref().filter(|r| r.is_finalized)?;
    let mut counts = vec![0u32; poll.answers.len()];
    for tally in &results.answer_counts {
        let slot = option_index(tally.id).and_then(|i| counts.get_mut(usize::try_from(i).ok()?));
        if let Some(slot) = slot {
            *slot = u32::try_from(tally.count).unwrap_or(u32::MAX);
        }
    }
    Some(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("option {i}")).collect()
    }

    fn poll(answers: u8, results: Option<(bool, &[(u8, u64)])>) -> Poll {
        let answers: Vec<_> = (1..=answers)
            .map(|id| json!({ "answer_id": id, "poll_media": { "text": format!("option {id}") } }))
            .collect();
        let results = results.map(|(is_finalized, counts)| {
            let counts: Vec<_> = counts
                .iter()
                .map(|(id, count)| json!({ "id": id, "count": count, "me_voted": false }))
                .collect();
            json!({ "answer_counts": counts, "is_finalized": is_finalized })
        });
        serde_json::from_value(json!({
            "answers": answers, "allow_multiselect": false, "expiry": "2026-11-04T10:00:00.000000+00:00",
            "layout_type": 1, "question": { "text": "when?" }, "results": results
        }))
        .unwrap()
    }

    #[test]
    fn answer_ids_start_at_one_and_options_at_zero() {
        assert_eq!(option_index(1), Some(0));
        assert_eq!(option_index(2), Some(1));
        assert_eq!(option_index(10), Some(9));
        assert_eq!(option_index(255), Some(254));
        assert_eq!(option_index(0), None);
    }

    #[test]
    fn hours_are_held_between_one_hour_and_32_days() {
        assert_eq!(clamp_hours(0), 1);
        assert_eq!(clamp_hours(1), 1);
        assert_eq!(clamp_hours(24), 24);
        assert_eq!(clamp_hours(768), 768);
        assert_eq!(clamp_hours(769), 768);
        assert_eq!(clamp_hours(u32::MAX), 768);
    }

    #[test]
    fn two_to_ten_options_are_accepted() {
        for n in 2..=10 {
            assert_eq!(validate("when?", &options(n)), Ok(()), "{n} options");
        }
    }

    #[test]
    fn fewer_than_two_options_fail() {
        assert_eq!(
            validate("when?", &options(1)),
            Err(PollError::TooFewOptions(1))
        );
        assert_eq!(validate("when?", &[]), Err(PollError::TooFewOptions(0)));
    }

    #[test]
    fn more_than_ten_options_fail() {
        let error = validate("when?", &options(11)).unwrap_err();
        assert_eq!(error, PollError::TooManyOptions(11));
        assert_eq!(error.to_string(), "a poll takes at most 10 options, got 11");
    }

    #[test]
    fn empty_and_oversized_text_fail() {
        assert_eq!(validate("  ", &options(2)), Err(PollError::EmptyQuestion));
        assert_eq!(
            validate(&"q".repeat(301), &options(2)),
            Err(PollError::QuestionTooLong(301))
        );
        assert_eq!(validate(&"q".repeat(300), &options(2)), Ok(()));

        let mut blank = options(3);
        blank[1] = " ".to_owned();
        assert_eq!(validate("when?", &blank), Err(PollError::EmptyOption(2)));

        let mut long = options(3);
        long[2] = "x".repeat(56);
        assert_eq!(
            validate("when?", &long),
            Err(PollError::OptionTooLong(3, 56))
        );
        long[2] = "x".repeat(55);
        assert_eq!(validate("when?", &long), Ok(()));
    }

    #[test]
    fn text_limits_count_characters_not_bytes() {
        let wide = "é".repeat(55);
        let mut set = options(2);
        set[0] = wide;
        assert_eq!(validate("when?", &set), Ok(()));
    }

    #[test]
    fn the_body_keeps_option_order_and_sends_plain_text() {
        let body = create_body(
            " Which day? ",
            &[
                "Thursday".to_owned(),
                "Friday".to_owned(),
                "Either".to_owned(),
            ],
            true,
            48,
        )
        .unwrap();
        assert_eq!(
            body,
            json!({
                "poll": {
                    "question": { "text": "Which day?" },
                    "answers": [
                        { "poll_media": { "text": "Thursday" } },
                        { "poll_media": { "text": "Friday" } },
                        { "poll_media": { "text": "Either" } },
                    ],
                    "allow_multiselect": true,
                    "duration": 48,
                    "layout_type": 1,
                },
                "allowed_mentions": { "parse": [] },
            })
        );
    }

    #[test]
    fn the_body_clamps_the_duration_and_rejects_bad_option_counts() {
        let long = create_body("when?", &options(2), false, 10_000).unwrap();
        assert_eq!(long["poll"]["duration"], 768);
        assert_eq!(long["poll"]["allow_multiselect"], false);
        let short = create_body("when?", &options(2), false, 0).unwrap();
        assert_eq!(short["poll"]["duration"], 1);

        assert_eq!(
            create_body("when?", &options(11), false, 24),
            Err(PollError::TooManyOptions(11))
        );
        assert_eq!(
            create_body("when?", &options(1), false, 24),
            Err(PollError::TooFewOptions(1))
        );
    }

    #[test]
    fn an_open_poll_has_no_final_counts() {
        assert_eq!(final_counts(&poll(2, None)), None);
        assert_eq!(
            final_counts(&poll(2, Some((false, &[(1, 3), (2, 1)])))),
            None
        );
    }

    #[test]
    fn final_counts_follow_option_order() {
        let counts = final_counts(&poll(3, Some((true, &[(3, 7), (1, 2), (2, 5)]))));
        assert_eq!(counts, Some(vec![2, 5, 7]));
    }

    #[test]
    fn answers_nobody_picked_count_zero() {
        let counts = final_counts(&poll(4, Some((true, &[(2, 6)]))));
        assert_eq!(counts, Some(vec![0, 6, 0, 0]));
        assert_eq!(
            final_counts(&poll(3, Some((true, &[])))),
            Some(vec![0, 0, 0])
        );
    }

    #[test]
    fn tallies_for_unknown_answers_are_ignored() {
        let counts = final_counts(&poll(2, Some((true, &[(1, 1), (3, 9), (0, 4)]))));
        assert_eq!(counts, Some(vec![1, 0]));
    }

    #[test]
    fn huge_counts_saturate() {
        let counts = final_counts(&poll(2, Some((true, &[(1, u64::MAX)]))));
        assert_eq!(counts, Some(vec![u32::MAX, 0]));
    }
}
