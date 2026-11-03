//! Every setting has a sentence, the charter shows them all, and "why"
//! explains any message (DESIGN.md sections 8 and 18).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::attention::SHUSH;
use ours_core::leads::DEFERRAL_REPLY;
use ours_core::sim::{ClassifierScript, Sim, messages, model_requests, notices, texts};
use ours_core::step::{CHECK, NOTHING_SAID};
use ours_core::{
    Command, DeferredChange, GroupState, Input, LeadsRule, Persona, QuietHours, SettingChange,
    WorkingHours, render_charter,
};
use ours_types::{Kind, PlatformRole};

/// Priya is a workspace admin, Sam a member, Dana an external guest. The
/// dial starts at 1 so it can go either way.
fn work() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("sam", "Sam", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    let mut profile = sim.state.profile.clone();
    profile.dial = 1;
    sim.feed(Input::Command(Command::SetProfile { profile }));
    sim
}

type Check = fn(&GroupState) -> bool;

const CASES: [(&str, &str, Check); 10] = [
    ("quiet after 11", "Quiet from 23:00 to 08:00.", |s| {
        s.profile.quiet_hours == QuietHours { start: 23, end: 8 }
    }),
    ("quiet hours 21 to 7", "Quiet from 21:00 to 07:00.", |s| {
        s.profile.quiet_hours == QuietHours { start: 21, end: 7 }
    }),
    ("no quiet hours", "No quiet hours.", |s| {
        s.profile.quiet_hours.is_none()
    }),
    (
        "working hours 8 to 4",
        "Working hours 08:00 to 16:00, Monday to Friday.",
        |s| {
            s.profile.working_hours
                == Some(WorkingHours {
                    start: 8,
                    end: 16,
                    weekdays_only: true,
                })
        },
    ),
    ("no working hours", "No working hours.", |s| {
        s.profile.working_hours.is_none()
    }),
    ("dial it up", "Dial at 2: a mate.", |s| s.profile.dial == 2),
    ("dial it down", "Dial at 0: concierge.", |s| {
        s.profile.dial == 0
    }),
    ("dial 2", "Dial at 2: a mate.", |s| s.profile.dial == 2),
    (
        "we're a committee",
        "Set up as a committee, with its defaults.",
        |s| s.kind == Kind::Committee && s.profile.budget_per_day == 2,
    ),
    ("kind: club", "Set up as a club, with its defaults.", |s| {
        s.kind == Kind::Club
    }),
];

#[test]
fn each_sentence_changes_its_setting_for_a_lead() {
    for (sentence, line, changed) in CASES {
        let mut sim = work();
        assert!(!changed(&sim.state), "{sentence} already holds");
        let said = sim.say("launch", "priya", &format!("Q, {sentence}"));
        assert_eq!(messages(&said.effects), vec![(line, Some(&said.id))]);
        assert!(model_requests(&said.effects).is_empty(), "{sentence}");
        let notice = notices(&said.effects).join(" ");
        assert!(
            notice.contains(line) && notice.contains("Changed by Priya"),
            "{notice}"
        );
        assert!(changed(&sim.state), "{sentence}");
    }
}

#[test]
fn each_sentence_waits_for_a_leads_check_from_a_member() {
    for (sentence, line, changed) in CASES {
        let mut sim = work();
        let asked = sim.say("launch", "sam", &format!("Q, {sentence}"));
        assert_eq!(
            messages(&asked.effects),
            vec![(DEFERRAL_REPLY, Some(&asked.id))],
            "{sentence}"
        );
        assert!(!changed(&sim.state), "{sentence}");
        let deferral = sim.state.deferrals.values().next().unwrap();
        assert!(matches!(deferral.change, DeferredChange::Setting { .. }));

        let reply = sim.last_sent_message().unwrap();
        assert!(sim.react("launch", &reply, "sam", CHECK).is_empty());
        let approved = sim.react("launch", &reply, "priya", CHECK);
        assert_eq!(
            texts(&approved),
            vec![format!("{line} Asked by Sam, approved by Priya.")]
        );
        assert!(changed(&sim.state), "{sentence}");
    }
}

#[test]
fn a_guest_is_declined_in_one_line() {
    for (sentence, _, changed) in CASES {
        let mut sim = work();
        let said = sim.say("launch", "dana", &format!("Q, {sentence}"));
        assert_eq!(texts(&said.effects), vec!["That's one for Priya."]);
        assert!(!changed(&sim.state), "{sentence}");
        assert!(sim.state.deferrals.is_empty());
    }
}

#[test]
fn a_new_kind_keeps_the_name_the_leads_the_hosts_and_the_watch() {
    let mut sim = Sim::observing(Kind::Friends);
    sim.feed(Input::Command(Command::SetAgentName {
        name: "Robin".into(),
        by_host: false,
    }));
    sim.feed(Input::Command(Command::SetHosts {
        users: vec!["olu".into()],
    }));
    sim.room("chat", "chat", &[("sam", "Sam", PlatformRole::Regular)]);
    sim.feed(Input::Command(Command::AddLead { user: "sam".into() }));
    let watching = sim.state.observe.clone();
    sim.say("chat", "sam", "Robin, this is a team");
    assert_eq!(sim.state.kind, Kind::Team);
    assert_eq!(sim.state.agent_name, "Robin");
    assert!(sim.state.leads.contains(&"sam".into()));
    assert!(sim.state.hosts.contains(&"olu".into()));
    assert_eq!(sim.state.observe, watching);
}

#[test]
fn unknown_sentences_still_go_to_the_model() {
    let mut sim = work();
    sim.model_says("Sure.");
    let said = sim.say("launch", "priya", "Q, quiet in the library please");
    assert_eq!(model_requests(&said.effects).len(), 1);
}

#[test]
fn settings_are_six_lines_at_most_in_the_room() {
    let mut sim = work();
    sim.feed(Input::Command(Command::SetModelLabel {
        label: "gpt-oss-20b on the host".into(),
    }));
    let said = sim.say("launch", "dana", "Q, what are your settings?");
    let text = texts(&said.effects)[0];
    assert!(text.lines().count() <= 6, "{text}");
    assert_eq!(
        text,
        "A team; I'm Q on gpt-oss-20b on the host; times in UTC.\n\
         I take instructions from Priya.\n\
         Unprompted: 1 an hour, 4 a day, 4 left today.\n\
         Quiet from 22:00 to 08:00. Working hours 09:00 to 18:00, Monday to Friday.\n\
         Dial at 1: dry and brief. Unanswered questions wait 30 minutes.\n\
         Speaking unprompted when it helps, within the budget."
    );
}

#[test]
fn the_charter_renders_every_setting() {
    let mut sim = Sim::observing(Kind::Team);
    for command in [
        Command::SetAgentName {
            name: "Robin".into(),
            by_host: false,
        },
        Command::SetTimezone {
            zone: "Europe/London".into(),
        },
        Command::SetModelLabel {
            label: "gpt-oss-20b on the host".into(),
        },
        Command::SetHosts {
            users: vec!["olu".into()],
        },
        Command::AddLead { user: "jo".into() },
        Command::AddLead {
            user: "ghost".into(),
        },
        Command::SetPersona {
            persona: Persona {
                voice: "Warm, brief, a little dry.".into(),
                ..Persona::default()
            },
        },
    ] {
        sim.feed(Input::Command(command));
    }
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("jo", "Jo", PlatformRole::Regular),
            ("olu", "Olu", PlatformRole::Regular),
        ],
    );
    for sentence in [
        "Robin, quiet hours 23 to 7",
        "Robin, working hours 8 to 5",
        "Robin, dial 2",
    ] {
        sim.say("launch", "priya", sentence);
    }
    sim.feed(Input::Command(Command::ChangeSetting {
        change: SettingChange::NoQuietHours,
    }));
    sim.say("launch", "priya", "Robin, quiet hours 23 to 7");
    sim.model_says("Noted.");
    sim.say("launch", "jo", "Robin, noted?");
    let answer = sim.last_sent_message().unwrap();
    sim.react("launch", &answer, "jo", SHUSH);

    let charter = render_charter(&sim.state);
    for expected in [
        "# Charter for group group",
        "- Kind: team",
        "- Agent name: Robin",
        "- Model: gpt-oss-20b on the host",
        "- Time zone: Europe/London",
        "- Leads rule: platform owners and admins, plus the named leads",
        "- Named leads: `ghost`, Jo (`jo`)",
        "- Hosts: Olu (`olu`)",
        "- Voice: written by the host (dial 2)",
        "- Dial: 2, a mate",
        "- Talks like: Warm, brief, a little dry.",
        "- Budget: 1 an hour, 4 a day",
        "- Shushed on 2026-11-02: nothing unprompted for the rest of that day",
        "- Speech threshold: 0.85",
        "- Quiet hours: 23:00 to 07:00",
        "- Working hours: 08:00 to 17:00, Monday to Friday",
        "- Answer window: 30 minutes",
        "- Hold window for actions that leave the group: 10 minutes",
        "- A new group is watched for: 3 days",
        "- Status: watching since 2026-11-02 18:00; leads hear what it saw at 2026-11-05 18:00",
        "## Retention",
        "- Action log kept: 365 days",
        "- Perishable facts: dropped once they expire",
        "- Legal hold: off",
    ] {
        assert!(
            charter.contains(expected),
            "missing {expected:?} in\n{charter}"
        );
    }
    assert!(!charter.contains('\u{2014}'));

    sim.say("launch", "priya", "Robin, go");
    sim.say("launch", "priya", "Robin, we're friends");
    let charter = render_charter(&sim.state);
    assert!(charter.contains("- Kind: friends"));
    assert!(charter.contains("- Leads rule: everyone in the group"));
    assert!(
        charter
            .contains("- Status: ended; watched from 2026-11-02 18:00, 0 would-have, 0 👍, 0 👎")
    );
    assert_eq!(sim.state.profile.leads, LeadsRule::Everyone);
}

#[test]
fn why_explains_a_reply_without_the_model() {
    let mut sim = work();
    let empty = sim.say("launch", "sam", "Q, why");
    assert_eq!(texts(&empty.effects), vec![NOTHING_SAID]);

    sim.model_says("Thursday at 2.");
    sim.say("launch", "sam", "Q, when's the review?");
    let answer = sim.last_sent_message().unwrap();
    let why = sim.reply("launch", "priya", "why?", &answer);
    assert!(model_requests(&why.effects).is_empty());
    let expected = "Trigger: Sam in #launch: \"Q, when's the review?\"\n\
                    Rule: addressed messages are always answered; the model wrote the reply\n\
                    Score: 1.00 on addressed, from the rules pass.";
    assert_eq!(messages(&why.effects), vec![(expected, Some(&why.id))]);

    // Alone, "why" means the agent's last message in the room.
    sim.model_says("Noted.");
    sim.say("launch", "sam", "Q, noted?");
    let alone = sim.say("launch", "priya", "Q, why");
    let text = texts(&alone.effects)[0];
    assert!(
        text.starts_with("Trigger: Sam in #launch: \"Q, noted?\""),
        "{text}"
    );
    assert!(text.lines().count() <= 3);

    // Asked again, it explains its own explanation.
    let again = sim.say("launch", "priya", "Q, why");
    let text = texts(&again.effects)[0];
    assert!(text.contains("\"why\" reads the record"), "{text}");
}

#[test]
fn why_explains_an_unprompted_answer_and_a_setting() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    sim.classify(ClassifierScript::Answerable);
    let asked = sim.say("dinner", "sam", "what time was it again?");
    sim.advance(20 * 60);
    sim.model_says("Half seven.");
    sim.react("dinner", &asked.id, "jo", "👀");
    let answer = sim.last_sent_message().unwrap();
    let why = sim.reply("dinner", "jo", "why", &answer);
    let text = texts(&why.effects)[0];
    assert_eq!(
        text,
        "Trigger: Sam asked in #dinner and nobody answered: \"what time was it again?\"\n\
         Rule: reaction first: a 🙋 after the answer window, an answer after a reaction or the window again\n\
         Score: 0.60 on unanswered question, from the small model sim."
    );

    sim.say("dinner", "jo", "Q, quiet after 11");
    let setting = sim.last_sent_message().unwrap();
    let why = sim.reply("dinner", "sam", "why", &setting);
    assert_eq!(
        texts(&why.effects)[0],
        "Trigger: Jo in #dinner: \"Q, quiet after 11\"\n\
         Rule: a lead's or host's setting applies at once"
    );
}
