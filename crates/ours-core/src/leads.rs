//! Leads by sentence (DESIGN.md section 5): "who do you listen to", "Sam is
//! a lead", "leads: a, b", "Sam is not a lead". Names resolve to platform
//! ids before anything changes, so a renamed account never becomes a lead.

use crate::action::Recording;
use crate::authority::{ActionKind, Disposition, dispose, resolve_role};
use crate::effect::{Effect, queue, send, send_as, send_dm_then};
use crate::profile::LeadsRule;
use crate::reactions::{self, Meaning};
use crate::sentence::{LeadsSentence, SELF_WORDS};
use crate::settings::addressed_trigger;
use crate::state::{
    DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, DmFollowUp, FollowUp, GroupState, LeadChange,
    PendingAction,
};
use crate::trace::Trace;
use ours_types::{Instant, Member, Message, PlatformRole, PlatformRoomId, PlatformUserId, Role};
use std::collections::BTreeSet;

/// The reply that opens a deferral, where any emoji is allowed.
pub const DEFERRAL_REPLY: &str = "Happy to. Needs a lead's ✅.";
/// Why a change to the leads is not undone.
pub const LEADS_NOTE: &str = "say it the other way, like Sam is not a lead";

/// Handles a leads sentence from `message`'s author: done for a lead or
/// host, deferred for a member, declined for a guest.
pub(crate) fn handle(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    sentence: LeadsSentence,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    let reply_to = Some(message.id.clone());
    let trigger = addressed_trigger(state, message);
    let deferred = match dispose(role, ActionKind::ChangeLeads) {
        Disposition::Do => false,
        Disposition::Defer => true,
        Disposition::Decline => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            send(state, fx, room, line, reply_to, now, trace);
            return;
        }
    };
    let change = match resolve_change(state, message, room, sentence) {
        Ok(change) => change,
        Err(line) => {
            let trace = Trace::new(trigger, "names resolve to people in the room first", now);
            send(state, fx, room, line, reply_to, now, trace);
            return;
        }
    };
    if deferred {
        let deferral = Deferral {
            room: room.clone(),
            asked_by: message.author.clone(),
            request: message.id.clone(),
            request_text: message.text.clone(),
            change: DeferredChange::Leads { change },
            expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
        };
        let trace = Trace::new(
            trigger,
            "a member's change to the leads waits for a lead's ✅",
            now,
        );
        open_deferral(state, fx, deferral, now, trace);
        return;
    }
    let line = apply(state, room, &change);
    let trace = Trace::new(trigger, "a lead's change to the leads applies at once", now);
    let note = Recording::Note {
        why: LEADS_NOTE.to_owned(),
    };
    send_as(state, fx, room, line.clone(), reply_to, now, trace, note);
    let by = state.display_name(room, &message.author);
    fx.push(Effect::Notice(format!("Leads: {line} Changed by {by}.")));
}

/// Replies "Needs a lead's ✅" to the request and opens the deferral once
/// the reply's id is known.
pub(crate) fn open_deferral(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    deferral: Deferral,
    now: Instant,
    trace: Trace,
) {
    let text = deferral_reply(state, &deferral.room);
    let pending = PendingAction {
        room: deferral.room.clone(),
        text,
        reply_to: Some(deferral.request.clone()),
        at: now,
        then: Some(FollowUp::OpenDeferral { deferral }),
        trace: Some(trace),
        record: Recording::Own,
    };
    queue(state, fx, pending);
}

/// "Happy to. Needs a lead's ✅.", with the approving reaction for `room`.
pub fn deferral_reply(state: &GroupState, room: &PlatformRoomId) -> String {
    let check = reactions::emoji_for(state, room, Meaning::Check);
    format!("Happy to. Needs a lead's {check}.")
}

/// Applies a lead change and returns the one line that announces it.
pub(crate) fn apply(state: &mut GroupState, room: &PlatformRoomId, change: &LeadChange) -> String {
    match change {
        LeadChange::Add { users } => {
            let (already, added): (Vec<_>, Vec<_>) = users
                .iter()
                .cloned()
                .partition(|u| resolve_role(state, room, u) >= Role::Lead);
            state.leads.extend(added.iter().cloned());
            if added.is_empty() {
                let verb = if already.len() == 1 {
                    "is already a lead"
                } else {
                    "are already leads"
                };
                format!("{} {verb}.", names(state, room, &already))
            } else {
                format!(
                    "{} {} now.",
                    names(state, room, &added),
                    be_a_lead(added.len())
                )
            }
        }
        LeadChange::Remove { users } => {
            let was: Vec<bool> = users
                .iter()
                .map(|u| resolve_role(state, room, u) >= Role::Lead)
                .collect();
            for user in users {
                state.leads.remove(user);
            }
            let mut removed = Vec::new();
            let mut still = Vec::new();
            let mut never = Vec::new();
            for (user, was_lead) in users.iter().zip(was) {
                if resolve_role(state, room, user) >= Role::Lead {
                    still.push(user.clone());
                } else if was_lead {
                    removed.push(user.clone());
                } else {
                    never.push(user.clone());
                }
            }
            let mut parts = Vec::new();
            if !removed.is_empty() {
                let verb = if removed.len() == 1 {
                    "is no longer a lead"
                } else {
                    "are no longer leads"
                };
                parts.push(format!("{} {verb}.", names(state, room, &removed)));
            }
            if !never.is_empty() {
                let verb = if never.len() == 1 {
                    "wasn't a lead"
                } else {
                    "weren't leads"
                };
                parts.push(format!("{} {verb}.", names(state, room, &never)));
            }
            if !still.is_empty() {
                let verb = if still.len() == 1 {
                    "still leads"
                } else {
                    "still lead"
                };
                parts.push(format!(
                    "{} {verb} by this group's default; to change that, give me the list as \"leads: names\".",
                    names(state, room, &still)
                ));
            }
            parts.join(" ")
        }
        LeadChange::Set { users } => {
            state.leads = users.iter().cloned().collect();
            state.profile.leads = LeadsRule::Named;
            format!("Leads are now {}.", names(state, room, users))
        }
        LeadChange::Everyone => {
            state.profile.leads = LeadsRule::Everyone;
            "Everyone is a lead now.".to_owned()
        }
    }
}

/// The answer to "who do you listen to".
pub(crate) fn listen_line(state: &GroupState, room: &PlatformRoomId) -> String {
    if state.profile.leads == LeadsRule::Everyone {
        return "Everyone in the group.".to_owned();
    }
    let leads = lead_names(state, room);
    if leads.is_empty() {
        "Nobody yet. The host can name the leads.".to_owned()
    } else {
        format!("I take instructions from {}.", join_names(&leads, "and"))
    }
}

/// How every line declining a guest's instruction begins.
pub const DECLINE_PREFIX: &str = "That's one for ";

/// One line declining a guest's instruction, naming who could ask instead.
pub(crate) fn decline_line(state: &GroupState, room: &PlatformRoomId) -> String {
    let leads = lead_names(state, room);
    if state.profile.leads != LeadsRule::Everyone && (1..=3).contains(&leads.len()) {
        format!("{DECLINE_PREFIX}{}.", join_names(&leads, "or"))
    } else {
        format!("{DECLINE_PREFIX}a lead.")
    }
}

/// A request only a lead may make: declined in the room, flagged to each
/// lead and host by DM, and run as the lead's own if one of them reacts ✅.
pub(crate) fn flag_request(
    state: &mut GroupState,
    message: &Message,
    text: &str,
    reason: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &message.room;
    let trigger = addressed_trigger(state, message);
    let line = decline_line(state, room);
    let trace = Trace::new(trigger.clone(), reason, now);
    send(state, fx, room, line, Some(message.id.clone()), now, trace);
    let guest = state.display_name(room, &message.author);
    let label = state.room_label(room);
    let text = text.trim().trim_end_matches('.');
    let check = reactions::emoji_for(state, room, Meaning::Check);
    let dm = format!("{guest} in {label} asked: {text}. React {check} and I'll treat it as yours.");
    let deferral = Deferral {
        room: room.clone(),
        asked_by: message.author.clone(),
        request: message.id.clone(),
        request_text: message.text.clone(),
        change: DeferredChange::Request {
            text: text.to_owned(),
            room: room.clone(),
            from: message.author.clone(),
        },
        expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
    };
    for lead in dm_targets(state, room) {
        let trace = Trace::new(
            trigger.clone(),
            format!("{reason}; flagged to the leads"),
            now,
        );
        let then = DmFollowUp::Request {
            deferral: deferral.clone(),
        };
        send_dm_then(state, fx, &lead, dm.clone(), now, trace, then);
    }
}

/// The humans who lead `room` or host the group, sorted.
fn dm_targets(state: &GroupState, room: &PlatformRoomId) -> Vec<PlatformUserId> {
    let mut ids: BTreeSet<PlatformUserId> = state.hosts.iter().cloned().collect();
    ids.extend(lead_ids(state, room));
    ids.into_iter()
        .filter(|u| state.is_human(room, u) && resolve_role(state, room, u) >= Role::Lead)
        .collect()
}

/// Ids of everyone who leads in `room`.
fn lead_ids(state: &GroupState, room: &PlatformRoomId) -> BTreeSet<PlatformUserId> {
    let members = state.rooms.get(room).map(|r| &r.members);
    let external = |u: &PlatformUserId| {
        members
            .and_then(|m| m.get(u))
            .is_some_and(|m| m.platform_role == PlatformRole::External)
    };
    let mut ids: BTreeSet<PlatformUserId> = state
        .leads
        .iter()
        .filter(|u| !external(u))
        .cloned()
        .collect();
    if let Some(members) = members {
        ids.extend(
            members
                .values()
                .filter(|m| !m.is_bot && !state.is_me(&m.id))
                .filter(|m| resolve_role(state, room, &m.id) == Role::Lead)
                .map(|m| m.id.clone()),
        );
    }
    ids
}

/// Display names of everyone who leads in `room`, sorted.
pub(crate) fn lead_names(state: &GroupState, room: &PlatformRoomId) -> Vec<String> {
    let mut names: Vec<String> = lead_ids(state, room)
        .iter()
        .map(|u| state.display_name(room, u))
        .collect();
    names.sort();
    names
}

/// "Sam", "Sam and Jo", "Sam, Jo and Alex".
pub fn join_names(names: &[String], conjunction: &str) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} {conjunction} {last}", init.join(", ")),
    }
}

fn names(state: &GroupState, room: &PlatformRoomId, users: &[PlatformUserId]) -> String {
    let names: Vec<String> = users.iter().map(|u| state.display_name(room, u)).collect();
    join_names(&names, "and")
}

fn be_a_lead(n: usize) -> &'static str {
    if n == 1 { "is a lead" } else { "are leads" }
}

/// Resolves a leads sentence to a change, naming people as `room` knows them.
pub(crate) fn resolve_change(
    state: &GroupState,
    message: &Message,
    room: &PlatformRoomId,
    sentence: LeadsSentence,
) -> Result<LeadChange, String> {
    Ok(match sentence {
        LeadsSentence::Everyone => LeadChange::Everyone,
        LeadsSentence::Add(names) => LeadChange::Add {
            users: people(state, message, room, &names, true)?,
        },
        LeadsSentence::Remove(names) => LeadChange::Remove {
            users: people(state, message, room, &names, false)?,
        },
        LeadsSentence::Set(names) => LeadChange::Set {
            users: people(state, message, room, &names, true)?,
        },
    })
}

/// Resolves written names to platform ids, or the one line saying why not.
fn people(
    state: &GroupState,
    message: &Message,
    room: &PlatformRoomId,
    names: &[String],
    granting: bool,
) -> Result<Vec<PlatformUserId>, String> {
    let mut users = Vec::new();
    for name in names {
        let user = match find_person(state, message, room, name) {
            Found::One(user) => user,
            Found::Nobody => return Err(format!("I can't find \"{name}\" in this room.")),
            Found::Several(options) => {
                return Err(format!(
                    "\"{name}\" could be {}; use a mention.",
                    join_names(&options, "or")
                ));
            }
        };
        let member = state.rooms.get(room).and_then(|r| r.members.get(&user));
        if granting && member.is_some_and(|m| m.platform_role == PlatformRole::External) {
            let who = state.display_name(room, &user);
            return Err(format!("{who} is a guest here, so can't be a lead."));
        }
        if !users.contains(&user) {
            users.push(user);
        }
    }
    Ok(users)
}

enum Found {
    One(PlatformUserId),
    Nobody,
    Several(Vec<String>),
}

/// Finds a person by a written name: the speaker for "I" or "me", a platform
/// mention whose id appears in the name, then an exact display name in the
/// room, then a unique prefix of one.
fn find_person(state: &GroupState, message: &Message, room: &PlatformRoomId, name: &str) -> Found {
    if SELF_WORDS.contains(&name) {
        return Found::One(message.author.clone());
    }
    let mentioned = message
        .mentions
        .iter()
        .filter(|u| !state.is_me(u))
        .find(|u| name.contains(&u.as_str().to_lowercase()));
    if let Some(user) = mentioned {
        return Found::One(user.clone());
    }
    let Some(room) = state.rooms.get(room) else {
        return Found::Nobody;
    };
    let name = name.trim_start_matches('@');
    let people: Vec<&Member> = room
        .members
        .values()
        .filter(|m| !m.is_bot && !state.is_me(&m.id))
        .collect();
    let exact: Vec<&Member> = people
        .iter()
        .copied()
        .filter(|m| m.display_name.to_lowercase() == name)
        .collect();
    let candidates = if exact.is_empty() {
        people
            .into_iter()
            .filter(|m| m.display_name.to_lowercase().starts_with(name))
            .collect()
    } else {
        exact
    };
    match candidates.as_slice() {
        [] => Found::Nobody,
        [one] => Found::One(one.id.clone()),
        several => Found::Several(several.iter().map(|m| m.display_name.clone()).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_join_like_a_sentence() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(join_names(&n(&[]), "and"), "");
        assert_eq!(join_names(&n(&["Sam"]), "and"), "Sam");
        assert_eq!(join_names(&n(&["Priya", "Sam"]), "or"), "Priya or Sam");
        assert_eq!(join_names(&n(&["A", "B", "C"]), "and"), "A, B and C");
    }
}
