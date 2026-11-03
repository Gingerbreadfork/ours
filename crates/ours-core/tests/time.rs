//! Time expressions read by rule (DESIGN.md section 12): a table of
//! phrases, crossed with every weekday and three zones, with the echo each
//! reading says back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::resolve_time_rules;
use ours_core::sim::instant;
use ours_core::time::{echo, wants_resolver};
use ours_types::{Instant, TimeMeaning};

const ZONES: [&str; 3] = ["Australia/Melbourne", "Europe/London", "America/New_York"];

/// The local date a reading lands on, as 2026-11-12.
fn date(meaning: &TimeMeaning, zone: &str) -> String {
    let at = match meaning {
        TimeMeaning::Instant { at, .. } => *at,
        TimeMeaning::Span { start, .. } => *start,
        other => panic!("not a single reading: {other:?}"),
    };
    at.0.to_zoned(jiff::tz::TimeZone::get(zone).unwrap())
        .date()
        .to_string()
}

/// 10:00 local on a day of the week of 2026-11-09, a Monday.
fn morning(day: i64, zone: &str) -> Instant {
    let date: jiff::civil::Date = format!("2026-11-{:02}", 9 + day).parse().unwrap();
    let ts = date
        .at(10, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::get(zone).unwrap())
        .unwrap()
        .timestamp();
    Instant(ts)
}

#[test]
fn weekdays_land_on_the_conventional_day_in_every_zone() {
    // Said on Monday 9th to Sunday 15th: the day each phrase means.
    let table: [(&str, [u8; 7]); 6] = [
        ("thurs", [12, 12, 12, 19, 19, 19, 19]),
        ("on tuesday", [10, 17, 17, 17, 17, 17, 17]),
        ("this friday", [13, 13, 13, 13, 13, 20, 20]),
        ("next friday", [20, 20, 20, 20, 20, 20, 20]),
        ("fri", [13, 13, 13, 13, 20, 20, 20]),
        ("tomorrow", [10, 11, 12, 13, 14, 15, 16]),
    ];
    for zone in ZONES {
        for (phrase, days) in table {
            for (weekday, day) in days.iter().enumerate() {
                let now = morning(weekday as i64, zone);
                let found = resolve_time_rules(phrase, zone, now)
                    .unwrap_or_else(|| panic!("{phrase} in {zone}"));
                let want = format!("2026-11-{day:02}");
                assert_eq!(
                    date(&found.meaning, zone),
                    want,
                    "{phrase} on day {weekday} in {zone}"
                );
                assert!(
                    matches!(
                        found.meaning,
                        TimeMeaning::Instant {
                            has_time: false,
                            ..
                        }
                    ),
                    "{phrase}"
                );
            }
        }
    }
}

#[test]
fn a_table_of_phrases_and_their_echoes() {
    // Monday 2026-11-09, 19:00 in Melbourne.
    let now = instant("2026-11-09T19:00:00+11:00");
    let zone = "Australia/Melbourne";
    let table: [(&str, &str); 34] = [
        ("thurs or fri", "Thursday the 12th"),
        ("dinner on tuesday?", "Tuesday the 10th"),
        ("next friday", "Friday the 20th"),
        ("today", "Monday the 9th"),
        ("tonight", "Monday the 9th"),
        ("tomorrow", "Tuesday the 10th"),
        ("the day after", "Wednesday the 11th"),
        ("the day after tomorrow", "Wednesday the 11th"),
        ("this weekend", "the weekend of the 14th"),
        ("next week", "the week of the 16th"),
        ("the week after next", "the week of the 23rd"),
        ("end of month", "Monday the 30th"),
        ("by the end of the month", "Monday the 30th"),
        ("in 3 days", "Thursday the 12th"),
        ("in two weeks", "Monday the 23rd"),
        ("in a fortnight", "Monday the 23rd"),
        ("14 March", "Sunday 14 March 2027"),
        ("March 14", "Sunday 14 March 2027"),
        ("14/3", "Sunday 14 March 2027"),
        ("2027-03-14", "Sunday 14 March 2027"),
        ("on the 14th", "Saturday the 14th"),
        ("7:30", "Monday the 9th at 7:30pm"),
        ("7.30pm", "Monday the 9th at 7:30pm"),
        ("at 7", "Tuesday the 10th at 7pm"),
        ("noon", "Tuesday the 10th at noon"),
        ("midnight", "Tuesday the 10th at midnight"),
        ("friday 7:30pm", "Friday the 13th at 7:30pm"),
        ("thursday at 7", "Thursday the 12th at 7pm"),
        ("7pm on friday", "Friday the 13th at 7pm"),
        ("tomorrow 6pm", "Tuesday the 10th at 6pm"),
        ("tonight at 8", "Monday the 9th at 8pm"),
        ("12 to 16 June", "12 to 16 June 2027"),
        ("fri to sun", "Friday the 13th to Sunday the 15th"),
        ("next month", "December"),
    ];
    for (text, want) in table {
        let found = resolve_time_rules(text, zone, now).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(found.echo, want, "{text}");
        assert_eq!(
            echo(&found.meaning, zone, now).as_deref(),
            Some(want),
            "{text}"
        );
    }
    for text in [
        "I sat down",
        "the sun is out",
        "I may come",
        "the football was great",
        "after the holidays",
        "when the exams finish",
        "8 people",
    ] {
        assert!(resolve_time_rules(text, zone, now).is_none(), "{text}");
    }
}

#[test]
fn ranges_and_times_carry_their_shape() {
    let now = instant("2026-11-09T19:00:00+11:00");
    let zone = "Australia/Melbourne";
    let week = resolve_time_rules("next week", zone, now).unwrap();
    assert_eq!(
        week.meaning,
        TimeMeaning::Span {
            start: instant("2026-11-16T00:00:00+11:00"),
            end: instant("2026-11-23T00:00:00+11:00"),
        }
    );
    let at = resolve_time_rules("friday 7:30pm", zone, now).unwrap();
    assert_eq!(
        at.meaning,
        TimeMeaning::Instant {
            at: instant("2026-11-13T19:30:00+11:00"),
            has_time: true,
        }
    );
    let trip = resolve_time_rules("cabin 12 to 16 june?", zone, now).unwrap();
    assert_eq!(trip.phrase, "12 to 16 june");
    assert_eq!(
        trip.meaning,
        TimeMeaning::Span {
            start: instant("2027-06-12T00:00:00+10:00"),
            end: instant("2027-06-17T00:00:00+10:00"),
        }
    );
}

#[test]
fn next_week_on_a_sunday_evening_has_two_readings() {
    let zone = "Europe/London";
    let evening = instant("2026-11-15T19:00:00Z");
    let found = resolve_time_rules("next week", zone, evening).unwrap();
    assert_eq!(
        found.meaning,
        TimeMeaning::Ambiguous {
            candidates: vec![
                instant("2026-11-16T00:00:00Z"),
                instant("2026-11-23T00:00:00Z")
            ],
        }
    );
    assert_eq!(found.echo, "the week of the 16th or the 23rd");
    // Earlier on Sunday, and on every other evening, it is the coming week.
    let afternoon = instant("2026-11-15T14:00:00Z");
    let found = resolve_time_rules("next week", zone, afternoon).unwrap();
    assert_eq!(found.echo, "the week of the 16th");
    let saturday = instant("2026-11-14T19:00:00Z");
    assert_eq!(
        resolve_time_rules("next week", zone, saturday)
            .unwrap()
            .echo,
        "the week of the 16th"
    );
}

#[test]
fn each_member_reads_in_their_own_zone() {
    // Thursday 07:00 in Melbourne is still Wednesday afternoon in New York.
    let now = instant("2026-11-11T20:00:00Z");
    let melbourne = resolve_time_rules("tomorrow", "Australia/Melbourne", now).unwrap();
    let new_york = resolve_time_rules("tomorrow", "America/New_York", now).unwrap();
    assert_eq!(melbourne.echo, "Friday the 13th");
    assert_eq!(new_york.echo, "Thursday the 12th");
    let melbourne = resolve_time_rules("thurs", "Australia/Melbourne", now).unwrap();
    let new_york = resolve_time_rules("thurs", "America/New_York", now).unwrap();
    assert_eq!(melbourne.echo, "Thursday the 19th");
    assert_eq!(new_york.echo, "Thursday the 12th");
    // 3/4 is the 3rd of April in London and March 4 in New York.
    let london = resolve_time_rules("3/4", "Europe/London", now).unwrap();
    let new_york = resolve_time_rules("3/4", "America/New_York", now).unwrap();
    assert_eq!(london.echo, "Saturday 3 April 2027");
    assert_eq!(new_york.echo, "Thursday 4 March 2027");
}

#[test]
fn words_the_rules_cannot_read_go_to_the_resolver() {
    for text in [
        "after the holidays",
        "when the exams finish",
        "the week after Priya is back",
        "before Christmas",
    ] {
        assert!(wants_resolver(text), "{text}");
    }
    for text in ["pizza", "Acme", "thai or indian"] {
        assert!(!wants_resolver(text), "{text}");
    }
}
