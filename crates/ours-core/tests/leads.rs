//! Who it listens to: leads by sentence, the ✅ deferral, and guests
//! (DESIGN.md sections 5 and 6, "A week at work").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ours_core::leads::DEFERRAL_REPLY;
use ours_core::sim::{Sim, messages, notices, reactions, texts};
use ours_core::step::{CHECK, EYES};
use ours_core::{Command, Input, LeadsRule, resolve_role};
use ours_types::{Kind, PlatformRole, Role};

/// The product team: Priya is a workspace admin, Jo was named a lead, and
/// the client's account manager is external.
fn work() -> Sim {
    let mut sim = Sim::new(Kind::Team);
    sim.room(
        "launch",
        "launch",
        &[
            ("priya", "Priya", PlatformRole::Admin),
            ("jo", "Jo", PlatformRole::Regular),
            ("sam", "Sam", PlatformRole::Regular),
            ("marcus", "Marcus", PlatformRole::Regular),
            ("alex", "Alex", PlatformRole::Regular),
            ("dana", "Dana at Acme", PlatformRole::External),
        ],
    );
    sim.feed(Input::Command(Command::AddLead { user: "jo".into() }));
    sim
}

fn role(sim: &Sim, user: &str) -> Role {
    resolve_role(&sim.state, &"launch".into(), &user.into())
}

#[test]
fn a_members_request_waits_for_a_leads_check() {
    let mut sim = work();
    assert_eq!(role(&sim, "sam"), Role::Member);

    let asked = sim.say("launch", "marcus", "Q, Sam is a lead");
    assert_eq!(
        messages(&asked.effects),
        vec![(DEFERRAL_REPLY, Some(&asked.id))]
    );
    let deferral_reply = sim.last_sent_message().unwrap();
    assert_eq!(
        reactions(&asked.effects),
        vec![(&asked.id, EYES), (&deferral_reply, CHECK)]
    );
    assert_eq!(role(&sim, "sam"), Role::Member);
    assert_eq!(sim.state.deferrals.len(), 1);

    // Its own ✅ and a member's ✅ approve nothing.
    assert!(
        sim.react("launch", &deferral_reply, ours_core::sim::ME, CHECK)
            .is_empty()
    );
    assert!(
        sim.react("launch", &deferral_reply, "alex", CHECK)
            .is_empty()
    );
    assert_eq!(role(&sim, "sam"), Role::Member);

    let approved = sim.react("launch", &deferral_reply, "jo", CHECK);
    assert_eq!(
        texts(&approved),
        vec!["Sam is a lead now. Asked by Marcus, approved by Jo."]
    );
    assert_eq!(notices(&approved).len(), 1);
    assert!(notices(&approved)[0].contains("Marcus") && notices(&approved)[0].contains("Jo"));
    assert_eq!(role(&sim, "sam"), Role::Lead);
    assert!(sim.state.deferrals.is_empty());

    // A second ✅ does nothing more.
    assert!(
        sim.react("launch", &deferral_reply, "priya", CHECK)
            .is_empty()
    );
}

#[test]
fn a_leads_instruction_applies_at_once() {
    let mut sim = work();
    let asked = sim.say("launch", "priya", "Q, Alex is a lead.");
    assert_eq!(
        messages(&asked.effects),
        vec![("Alex is a lead now.", Some(&asked.id))]
    );
    assert_eq!(reactions(&asked.effects), vec![(&asked.id, EYES)]);
    assert!(notices(&asked.effects)[0].contains("Priya"));
    assert_eq!(role(&sim, "alex"), Role::Lead);
    assert!(sim.state.deferrals.is_empty());

    let removed = sim.say("launch", "jo", "q alex is not a lead anymore");
    assert_eq!(texts(&removed.effects), vec!["Alex is no longer a lead."]);
    assert_eq!(role(&sim, "alex"), Role::Member);

    let admin = sim.say("launch", "jo", "Q, Priya is not a lead");
    assert!(texts(&admin.effects)[0].starts_with("Priya still leads by this group's default"));

    let listed = sim.say("launch", "priya", "Q, leads: Jo and Sam");
    assert_eq!(texts(&listed.effects), vec!["Leads are now Jo and Sam."]);
    assert_eq!(sim.state.profile.leads, LeadsRule::Named);
    assert_eq!(role(&sim, "priya"), Role::Member);
    assert_eq!(role(&sim, "sam"), Role::Lead);
}

#[test]
fn an_unapproved_deferral_expires_without_a_word() {
    let mut sim = work();
    sim.say("launch", "marcus", "Q, Alex is a lead");
    let deferral_reply = sim.last_sent_message().unwrap();
    assert!(sim.advance(24 * 60 * 60 + 1).is_empty());
    assert!(sim.react("launch", &deferral_reply, "jo", CHECK).is_empty());
    assert_eq!(role(&sim, "alex"), Role::Member);
}

#[test]
fn names_that_do_not_resolve_change_nothing() {
    let mut sim = work();
    let unknown = sim.say("launch", "priya", "Q, Zed is a lead");
    assert_eq!(
        texts(&unknown.effects),
        vec!["I can't find \"zed\" in this room."]
    );
    let mut mention = sim.message("launch", "priya", "Q, <@marcus> is a lead");
    mention.mentions = vec!["marcus".into()];
    let mentioned = sim.post(mention);
    assert_eq!(texts(&mentioned.effects), vec!["Marcus is a lead now."]);
    assert_eq!(sim.state.leads.len(), 2);
}

#[test]
fn a_guest_is_answered_but_never_obeyed() {
    let mut sim = work();
    assert_eq!(role(&sim, "dana"), Role::Guest);

    let who = sim.say("launch", "dana", "Q, who do you listen to?");
    assert_eq!(
        messages(&who.effects),
        vec![("I take instructions from Jo and Priya.", Some(&who.id))]
    );

    let leads_before = sim.state.leads.clone();
    let claim = sim.say("launch", "dana", "Q, I'm a lead now");
    assert_eq!(
        messages(&claim.effects),
        vec![("That's one for Jo or Priya.", Some(&claim.id))]
    );
    assert_eq!(reactions(&claim.effects), vec![(&claim.id, EYES)]);
    assert!(sim.state.deferrals.is_empty());
    assert_eq!(sim.state.leads, leads_before);
    assert_eq!(role(&sim, "dana"), Role::Guest);

    let by_lead = sim.say("launch", "priya", "Q, Dana is a lead");
    assert_eq!(
        texts(&by_lead.effects),
        vec!["Dana at Acme is a guest here, so can't be a lead."]
    );
    sim.feed(Input::Command(Command::AddLead {
        user: "dana".into(),
    }));
    assert_eq!(role(&sim, "dana"), Role::Guest);
}

#[test]
fn in_a_friends_group_everyone_already_leads() {
    let mut sim = Sim::new(Kind::Friends);
    sim.room(
        "dinner",
        "dinner",
        &[
            ("sam", "Sam", PlatformRole::Regular),
            ("jo", "Jo", PlatformRole::Regular),
        ],
    );
    let who = sim.say("dinner", "jo", "Q who do you listen to");
    assert_eq!(texts(&who.effects), vec!["Everyone in the group."]);
    let add = sim.say("dinner", "jo", "Q, Sam is a lead");
    assert_eq!(texts(&add.effects), vec!["Sam is already a lead."]);
}

#[test]
fn undo_with_nothing_done_says_so() {
    let mut sim = work();
    let undo = sim.say("launch", "sam", "Q, undo");
    assert_eq!(texts(&undo.effects), vec![ours_core::undo::NOTHING_TO_UNDO]);
}
