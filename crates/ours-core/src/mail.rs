//! The group's address (DESIGN.md sections 13, 15 and 18). Mail a lead
//! asked for is checked, held and sent through the integration; undoing a
//! sent one sends a cancellation; and mail that comes back is a guest's
//! words, relayed in one line and remembered, never obeyed.

use crate::action::{
    self, ActionClass, ActionRecord, ActionStatus, Origin, Recording, Undo, quoted, tier_of,
};
use crate::effect::{Effect, edit, send_as};
use crate::extract::{ExtractWhy, PendingExtract};
use crate::hold::{self, Placing, Route, window};
use crate::memory::{self, Place, Viewer};
use crate::plan::PlannedAction;
use crate::reactions::{self, Meaning};
use crate::state::GroupState;
use crate::trace::{Trace, shorten};
use crate::zone;
use ours_types::{
    ActionId, ContextLine, ExtractRequest, InboundMail, Instant, Message, OutgoingMail,
    OutsideAction, OutsideResult, PlatformMessageId, PlatformRoomId, PlatformUserId,
};
use tracing::debug;

/// What undo says about a held mail.
pub const NOT_SENT: &str = "Not sent.";
pub const NOTHING_SENT: &str = "I haven't emailed anyone from here.";
/// Message ids of mail start with this, so nothing links them to a platform.
pub const MAIL_PREFIX: &str = "mail:";
/// Mails one plan may hold.
const MAILS_PER_PLAN: usize = 3;
/// How much of a reply is relayed to the room.
const RELAY_CHARS: usize = 200;
/// Lines in one "what did you send".
const SENT_LINES: usize = 5;
const MAIL_RULE: &str = "a lead's email waits out the hold window so it can be undone";
const RELAY_RULE: &str = "a reply to the group's mail is relayed in one line; mail never instructs";

/// Whether `text` is shaped like an email address.
pub fn looks_like_address(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    if local.is_empty() || local.len() > 64 || text.len() > 254 || domain.contains('@') {
        return false;
    }
    let local_ok = local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "!#$%&*+/=?^_`{}~.-".contains(c))
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..");
    let labels: Vec<&str> = domain.split('.').collect();
    let labels_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    let tld_ok = labels
        .last()
        .is_some_and(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()));
    local_ok && labels_ok && tld_ok
}

/// Every address in `text`, lower-cased, including Slack's `<mailto:...>`.
pub fn addresses_in(text: &str) -> Vec<String> {
    text.split(|c: char| {
        c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | ',' | ';' | '"' | '\'' | '|')
    })
    .map(|w| {
        w.trim_start_matches("mailto:")
            .trim_end_matches(['.', ':', '!', '?'])
    })
    .filter(|w| looks_like_address(w))
    .map(str::to_lowercase)
    .collect()
}

/// Whether someone gave `address` here: in the instruction, in a person's
/// message the room holds, or in a fact the room may hear. The agent's own
/// lines do not count, so a refusal never vouches for what it refused.
pub fn address_known(
    state: &GroupState,
    room: &PlatformRoomId,
    instruction: &str,
    address: &str,
    now: Instant,
) -> bool {
    let address = address.trim().to_lowercase();
    let has = |text: &str| addresses_in(text).contains(&address);
    if has(instruction) {
        return true;
    }
    if state.rooms.get(room).is_some_and(|r| {
        r.messages
            .iter()
            .any(|m| !r.is_own(&m.id) && state.is_human(room, &m.author) && has(&m.text))
    }) {
        return true;
    }
    let viewer = Viewer::new(state, Place::of(state, room));
    state
        .facts
        .values()
        .filter(|f| f.is_live(now) && viewer.may_see(f))
        .any(|f| has(&f.object) || has(&f.subject) || f.quote.as_deref().is_some_and(has))
}

/// The subject as the room reads it: what was given, else the body's start.
fn subject_line(mail: &OutgoingMail) -> String {
    let subject = mail.subject.trim();
    let text = if subject.is_empty() {
        mail.body.lines().next().unwrap_or("")
    } else {
        subject
    };
    shorten(text, 80).trim_end_matches(['.', '!']).to_owned()
}

fn summary_of(mail: &OutgoingMail) -> String {
    format!("the email to {}: {}", mail.to, subject_line(mail))
}

/// A refused mail: one line saying why, and a declined record.
#[allow(clippy::too_many_arguments)]
fn refuse(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    origin: &Origin,
    mail: OutgoingMail,
    line: String,
    now: Instant,
    trace: &Trace,
) {
    let id = state.mint_action();
    let mut record = ActionRecord::new(
        id.clone(),
        ActionClass::Mail,
        room.clone(),
        now,
        summary_of(&mail),
        Undo::None {
            why: "it was never sent".to_owned(),
        },
    )
    .from_origin(origin);
    record.status = ActionStatus::Declined;
    record.outcome = Some(line.clone());
    record.outside = Some(OutsideAction::SendMail(mail));
    action::push(state, record);
    let trace = Trace::new(
        trace.trigger.clone(),
        "the agent emails only an address someone gave in the room",
        now,
    );
    let reply_to = origin.instruction.clone();
    send_as(
        state,
        fx,
        room,
        line,
        reply_to,
        now,
        trace,
        Recording::Line { action: id },
    );
}

/// Checks a plan's mail before anything is held or deferred: an address
/// that looks like one and that someone gave here, and something to say.
/// Each refusal gets one line; the rest come back, addresses lower-cased.
#[allow(clippy::too_many_arguments)]
pub(crate) fn screen(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    origin: &Origin,
    instruction: &str,
    actions: Vec<PlannedAction>,
    now: Instant,
    trace: &Trace,
) -> Vec<PlannedAction> {
    let mut kept = Vec::new();
    let mut mails = 0;
    for planned in actions {
        let PlannedAction::Mail { to, subject, body } = planned else {
            kept.push(planned);
            continue;
        };
        let written = to.trim();
        // A mail with nobody to send it to is a model misfire, not a request
        // the room should hear about.
        if written.is_empty() {
            fx.push(Effect::Notice(
                "The model planned an email with no address; dropped.".to_owned(),
            ));
            continue;
        }
        // "Bai Tong <bookings@baitong.com>" means the address inside it.
        let to = match addresses_in(written).as_slice() {
            [one] => one.clone(),
            _ => written.trim_start_matches("mailto:").to_lowercase(),
        };
        let mail = OutgoingMail {
            to: to.clone(),
            subject: subject.trim().to_owned(),
            body: body.trim().to_owned(),
            in_reply_to: None,
        };
        let refusal = if !looks_like_address(&to) {
            Some(format!(
                "{} isn't an email address I can send to.",
                quoted(written, 60)
            ))
        } else if !address_known(state, room, instruction, &to, now) {
            Some(format!(
                "I won't email {to}: nobody gave me that address here."
            ))
        } else if mail.body.is_empty() {
            Some("I need something to say in that email; tell me what to ask.".to_owned())
        } else if mails >= MAILS_PER_PLAN {
            Some(format!(
                "That's more emails than I send at once; ask me for {to} on its own."
            ))
        } else {
            None
        };
        match refusal {
            Some(line) => refuse(state, fx, room, origin, mail, line, now, trace),
            None => {
                mails += 1;
                kept.push(PlannedAction::Mail {
                    to: mail.to,
                    subject: mail.subject,
                    body: mail.body,
                });
            }
        }
    }
    kept
}

/// Holds a screened mail for the profile's window and says so in one line:
/// "Emailing bookings@baitong.com in 10m: Table for five. Undo?"
#[allow(clippy::too_many_arguments)]
pub(crate) fn hold(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    origin: &Origin,
    to: String,
    subject: String,
    body: String,
    now: Instant,
    trace: Trace,
) {
    let mail = OutgoingMail {
        to,
        subject,
        body,
        in_reply_to: None,
    };
    let id = state.mint_action();
    let route = hold::route(state, room, tier_of(&ActionClass::Mail), origin, now);
    let mut record = ActionRecord::new(
        id.clone(),
        ActionClass::Mail,
        room.clone(),
        now,
        summary_of(&mail),
        Undo::CancelHold,
    )
    .from_origin(origin);
    record.outside = Some(OutsideAction::SendMail(mail.clone()));
    let subject = subject_line(&mail);
    let check = reactions::emoji_for(state, room, Meaning::Check);
    let line = match &route {
        Route::Now => format!("Emailing {} now: {subject}. Undo?", mail.to),
        Route::Hold { until } => format!(
            "Emailing {} in {}: {subject}. Undo?",
            mail.to,
            window(until.seconds_since(now))
        ),
        Route::Ask { who, .. } => format!(
            "That one needs {}'s {check} first; I've asked them.",
            state.display_name(room, who)
        ),
        Route::Nobody => {
            format!("That one needs a lead's {check} and there's nobody here who can give it.")
        }
    };
    match &route {
        Route::Hold { until } | Route::Ask { until, .. } => {
            record.status = ActionStatus::Held { until: *until };
        }
        Route::Now => {}
        Route::Nobody => {
            record.status = ActionStatus::Declined;
            record.undo = Undo::None {
                why: "it was never sent".to_owned(),
            };
        }
    }
    let summary = record.summary.clone();
    let held = matches!(record.status, ActionStatus::Held { .. });
    action::push(state, record);
    let ask_text = format!(
        "{} wants me to send {summary}. React {check} and I will; otherwise it lapses tomorrow.",
        origin
            .requested_by
            .as_ref()
            .map_or_else(|| "Someone".to_owned(), |u| state.display_name(room, u))
    );
    let placing = Placing {
        id: &id,
        outside: OutsideAction::SendMail(mail),
        origin,
        room,
        ask_text,
    };
    hold::place(state, fx, placing, &route, now, trace.clone());
    let trace = Trace::new(trace.trigger, MAIL_RULE, now);
    let reply_to = origin.instruction.clone();
    send_as(
        state,
        fx,
        room,
        line,
        reply_to,
        now,
        trace,
        Recording::Line { action: id },
    );
    if held {
        fx.push(Effect::Notice(format!("Holding {summary}.")));
    }
}

fn can_edit(state: &GroupState, room: &PlatformRoomId) -> bool {
    state.me_in(room).is_some_and(|m| m.capabilities.edit)
}

/// Replaces the text of one of the agent's messages, in the room's ring too.
pub(crate) fn rewrite(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    text: String,
) {
    if let Some(held) = state
        .rooms
        .get_mut(room)
        .and_then(|r| r.messages.iter_mut().find(|m| &m.id == message))
    {
        held.text = text.clone();
    }
    edit(state, fx, room, message, text);
}

/// The line about a held action that was cancelled, where the platform edits.
pub(crate) fn cancelled_notice(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    record: &ActionRecord,
    notice: Option<&PlatformMessageId>,
) {
    let (Some(notice), true) = (notice, can_edit(state, &record.room)) else {
        return;
    };
    let text = match &record.outside {
        Some(OutsideAction::SendMail(mail)) => {
            format!("Not sent to {}: {}.", mail.to, subject_line(mail))
        }
        _ => format!("Not done: {}.", record.summary),
    };
    rewrite(state, fx, &record.room, notice, text);
}

/// The compensating mail for a sent one, drafted by rule.
pub fn cancellation(
    state: &GroupState,
    record: &ActionRecord,
    mail: &OutgoingMail,
) -> OutgoingMail {
    let group = state
        .rooms
        .get(&record.room)
        .map_or_else(|| record.room.to_string(), |r| r.name(&record.room));
    let subject = if mail.subject.to_lowercase().starts_with("re:") {
        mail.subject.clone()
    } else {
        format!("Re: {}", mail.subject)
    };
    OutgoingMail {
        to: mail.to.clone(),
        subject,
        body: format!(
            "Please disregard the previous message from {} for {group}: {}. Apologies for the noise.",
            state.agent_name,
            subject_line(mail)
        ),
        in_reply_to: record.reference.clone(),
    }
}

/// " Undone by Jo." when someone other than the asker undid it.
pub(crate) fn undoer_suffix(
    state: &GroupState,
    room: &PlatformRoomId,
    asker: Option<&PlatformUserId>,
    by: Option<&PlatformUserId>,
) -> String {
    match by {
        Some(by) if Some(by) != asker => format!(" Undone by {}.", state.display_name(room, by)),
        _ => String::new(),
    }
}

/// The integration's answer to `Effect::Outside`.
pub(crate) fn on_result(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    result: OutsideResult,
    now: Instant,
) {
    let Some(record) = action::find(state, &result.action).cloned() else {
        fx.push(Effect::Notice(format!(
            "A result came back for an outside action I have no record of: {}.",
            result.action
        )));
        return;
    };
    match record.outside.clone() {
        Some(OutsideAction::SendMail(mail)) => sent(state, fx, &record, &mail, &result, now),
        Some(OutsideAction::CancelMail { original, mail }) => {
            cancel_sent(state, fx, &record, &original, &mail, &result, now);
        }
        None => debug!(action = %result.action, "a result for an action with nothing outside"),
    }
}

fn sent(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    record: &ActionRecord,
    mail: &OutgoingMail,
    result: &OutsideResult,
    now: Instant,
) {
    let room = record.room.clone();
    let subject = subject_line(mail);
    let trace = Trace::new(
        format!("the email to {} came back from the mail server", mail.to),
        "an email is reported once, when it has gone or failed",
        now,
    );
    if result.ok {
        if let Some(r) = action::find_mut(state, &record.id) {
            r.status = ActionStatus::Sent {
                at: result.at,
                reference: result.reference.clone(),
            };
            r.reference = result.reference.clone();
            r.undo = Undo::CancelMail {
                to: mail.to.clone(),
            };
        }
        let text = format!("Sent to {}: {subject}. Undo?", mail.to);
        match (record.lines.first(), can_edit(state, &room)) {
            (Some(line), true) => rewrite(state, fx, &room, line, text),
            _ => {
                let reply_to = record.lines.first().cloned().or(record.instruction.clone());
                let as_line = Recording::Line {
                    action: record.id.clone(),
                };
                send_as(state, fx, &room, text, reply_to, now, trace, as_line);
            }
        }
        fx.push(Effect::Notice(format!("Sent {}.", record.summary)));
        return;
    }
    let error = result
        .error
        .clone()
        .unwrap_or_else(|| "no reason given".to_owned());
    if let Some(r) = action::find_mut(state, &record.id) {
        r.status = ActionStatus::Failed {
            error: error.clone(),
        };
        r.undo = Undo::None {
            why: "it never went out".to_owned(),
        };
    }
    let text = format!(
        "The email to {} didn't go: {}. Nothing was sent; ask me again to retry.",
        mail.to,
        shorten(&error, 120).trim_end_matches('.')
    );
    let reply_to = record.lines.first().cloned().or(record.instruction.clone());
    let as_line = Recording::Line {
        action: record.id.clone(),
    };
    send_as(state, fx, &room, text, reply_to, now, trace, as_line);
    fx.push(Effect::Notice(format!(
        "Failed to send {}: {error}",
        record.summary
    )));
}

fn cancel_sent(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    undo: &ActionRecord,
    original: &ActionId,
    mail: &OutgoingMail,
    result: &OutsideResult,
    now: Instant,
) {
    let room = undo.room.clone();
    let by = undo.approved_by.clone().or(undo.requested_by.clone());
    let trace = Trace::new(
        format!(
            "the cancellation to {} came back from the mail server",
            mail.to
        ),
        "a sent email is undone by a cancellation to the same address",
        now,
    );
    let asker = action::find(state, original).and_then(|r| r.requested_by.clone());
    let text = if result.ok {
        if let Some(r) = action::find_mut(state, &undo.id) {
            r.status = ActionStatus::Sent {
                at: result.at,
                reference: result.reference.clone(),
            };
            r.reference = result.reference.clone();
        }
        if let Some(r) = action::find_mut(state, original) {
            r.status = ActionStatus::Undone {
                at: result.at,
                by: by.clone(),
            };
        }
        fx.push(Effect::Notice(format!(
            "Cancellation sent: {}.",
            undo.summary
        )));
        format!(
            "Cancellation sent to {}; whether they honour it is up to them.{}",
            mail.to,
            undoer_suffix(state, &room, asker.as_ref(), by.as_ref())
        )
    } else {
        let error = result
            .error
            .clone()
            .unwrap_or_else(|| "no reason given".to_owned());
        if let Some(r) = action::find_mut(state, &undo.id) {
            r.status = ActionStatus::Failed {
                error: error.clone(),
            };
        }
        fx.push(Effect::Notice(format!(
            "The cancellation failed: {}: {error}",
            undo.summary
        )));
        format!(
            "The cancellation to {} didn't go: {}. The first email stands.",
            mail.to,
            shorten(&error, 120).trim_end_matches('.')
        )
    };
    let reply_to = undo.instruction.clone();
    let as_line = Recording::Line {
        action: undo.id.clone(),
    };
    send_as(state, fx, &room, text, reply_to, now, trace, as_line);
}

/// A subject without its "Re:" and "Fwd:" prefixes.
fn bare_subject(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let lower = s.to_lowercase();
        let Some(prefix) = ["re:", "fwd:", "fw:", "aw:"]
            .iter()
            .find(|p| lower.starts_with(**p))
        else {
            break;
        };
        s = s[prefix.len()..].trim_start();
    }
    s.to_lowercase()
}

fn bare_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).to_lowercase()
}

/// The record of the mail `mail` answers: by the ids it replies to, or by
/// its subject from the exact address the group wrote to.
fn answered(state: &GroupState, mail: &InboundMail) -> Option<ActionId> {
    let replies: Vec<String> = mail.in_reply_to.iter().map(|i| bare_id(i)).collect();
    let subject = bare_subject(&mail.subject);
    let from = mail.from.trim().to_lowercase();
    let sent = || {
        state.actions.iter().rev().filter_map(|r| {
            let out = match &r.outside {
                Some(OutsideAction::SendMail(m))
                | Some(OutsideAction::CancelMail { mail: m, .. }) => m,
                None => return None,
            };
            let reference = r.reference.as_ref()?;
            Some((r, out, reference))
        })
    };
    sent()
        .find(|(_, _, reference)| replies.contains(&bare_id(reference)))
        .or_else(|| {
            sent().find(|(_, out, _)| {
                !subject.is_empty()
                    && bare_subject(&out.subject) == subject
                    && out.to.trim().to_lowercase() == from
            })
        })
        .map(|(r, _, _)| r.id.clone())
}

/// Mail at the group's address. A reply to mail the group sent is relayed
/// to the room that asked and read for facts; anything else is a notice
/// for the host. No sentence rule runs on it and it never addresses the
/// agent.
pub(crate) fn on_inbound(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    mail: InboundMail,
    now: Instant,
) {
    let from = mail.from.trim().to_lowercase();
    let Some(id) = answered(state, &mail) else {
        fx.push(Effect::Notice(format!(
            "Mail from {from}: {}",
            shorten(&mail.subject, 120)
        )));
        return;
    };
    let Some(record) = action::find(state, &id).cloned() else {
        return;
    };
    let room = record.room.clone();
    let who = mail
        .from_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map_or_else(|| from.clone(), str::to_owned);
    let body = shorten(&mail.body, RELAY_CHARS);
    let mut line = if body.is_empty() {
        format!("{who} replied.")
    } else {
        format!("{who} replied: {body}")
    };
    if !mail.authenticated {
        line.push_str(" (unverified sender)");
    }
    if let Some(r) = action::find_mut(state, &id) {
        r.outcome = Some(line.clone());
    }
    let trace = Trace::new(
        format!("an email from {from} to the group's address"),
        RELAY_RULE,
        now,
    );
    let reply_to = record.lines.first().cloned();
    let as_line = Recording::Line { action: id.clone() };
    send_as(state, fx, &room, line, reply_to, now, trace, as_line);
    fx.push(Effect::Notice(format!(
        "Relayed a reply from {from} to {}.",
        state.room_label(&room)
    )));
    // In a DM the facts would belong to nobody who can hear them.
    let in_dm = state.rooms.get(&room).is_some_and(|r| r.is_dm());
    if !mail.body.trim().is_empty() && !in_dm {
        read_for_facts(state, fx, &record, &mail, &who, now);
    }
}

/// Asks the extractor for the facts in a reply, as said in the room that
/// sent the mail, with the sender as the source.
fn read_for_facts(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    record: &ActionRecord,
    mail: &InboundMail,
    who: &str,
    now: Instant,
) {
    let tag = match mail.message_id.as_deref().map(bare_id) {
        Some(id) if !id.is_empty() => id,
        _ => state.mint_action().to_string(),
    };
    let message = PlatformMessageId::new(format!("{MAIL_PREFIX}{tag}"));
    let room = record.room.clone();
    if state
        .extractions
        .iter()
        .any(|p| p.room == room && p.message == message)
    {
        return;
    }
    let speaker = PlatformUserId::new(mail.from.trim().to_lowercase());
    let context = match &record.outside {
        Some(OutsideAction::SendMail(sent))
        | Some(OutsideAction::CancelMail { mail: sent, .. }) => {
            vec![ContextLine {
                speaker: state.agent_name.clone(),
                text: shorten(
                    &format!("(email to {}) {}: {}", sent.to, sent.subject, sent.body),
                    400,
                ),
                seconds_before: mail.at.seconds_since(record.at).max(0),
            }]
        }
        None => Vec::new(),
    };
    let request = ExtractRequest {
        room: room.clone(),
        message: message.clone(),
        agent_name: state.agent_name.clone(),
        speaker: who.to_owned(),
        text: shorten(&mail.body, 2000),
        private: false,
        context,
        date: memory::day(mail.at, &state.timezone),
        timezone: state.timezone.clone(),
    };
    fx.push(Effect::Extract(request));
    state.extractions.push(PendingExtract {
        room,
        message,
        speaker,
        at: mail.at,
        text: mail.body.clone(),
        why: ExtractWhy::Mail {
            verified: mail.authenticated,
        },
        reply_to: None,
        guard: None,
        asked_at: now,
    });
}

/// "what did you send": the room's mail, newest first.
pub(crate) fn sent_list(state: &GroupState, message: &Message) -> String {
    let in_dm = state.rooms.get(&message.room).is_some_and(|r| r.is_dm());
    let user = &message.author;
    let lines: Vec<String> = state
        .actions
        .iter()
        .rev()
        .filter(|r| r.class == ActionClass::Mail)
        .filter(|r| {
            if in_dm {
                r.requested_by.as_ref() == Some(user) || r.approved_by.as_ref() == Some(user)
            } else {
                r.room == message.room
            }
        })
        .filter_map(|r| sent_line(state, r))
        .take(SENT_LINES)
        .collect();
    if lines.is_empty() {
        NOTHING_SENT.to_owned()
    } else {
        lines.join("\n")
    }
}

fn sent_line(state: &GroupState, r: &ActionRecord) -> Option<String> {
    let Some(OutsideAction::SendMail(mail)) = &r.outside else {
        return None;
    };
    let tz = &state.timezone;
    let subject = subject_line(mail);
    let mut line = match &r.status {
        ActionStatus::Held { until } => format!(
            "Waiting to send to {} until {}: {subject}.",
            mail.to,
            zone::local(*until, tz).strftime("%H:%M")
        ),
        ActionStatus::Sending { .. } => format!("Sending to {}: {subject}.", mail.to),
        ActionStatus::Sent { at, .. } => {
            format!("Sent to {} {}: {subject}.", mail.to, zone::stamp(*at, tz))
        }
        ActionStatus::Undone { .. } if r.reference.is_some() => {
            format!("Sent to {}, then cancelled: {subject}.", mail.to)
        }
        ActionStatus::Undone { .. } => format!("Not sent to {}: {subject}.", mail.to),
        ActionStatus::Failed { .. } => format!("Failed to send to {}: {subject}.", mail.to),
        ActionStatus::Declined | ActionStatus::Done => {
            format!("Didn't send to {}: {subject}.", mail.to)
        }
    };
    if let Some(outcome) = &r.outcome
        && r.reference.is_some()
    {
        line.push(' ');
        line.push_str(outcome);
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_look_like_addresses() {
        for good in ["bookings@baitong.com", "a.b+c@mail.example.co.uk", "x@y.io"] {
            assert!(looks_like_address(good), "{good}");
        }
        for bad in [
            "bookings",
            "@baitong.com",
            "bookings@",
            "a@b",
            "a@b.c",
            "a@@b.com",
            "a b@c.com",
            ".a@b.com",
            "a@-b.com",
            "a@b.123",
        ] {
            assert!(!looks_like_address(bad), "{bad}");
        }
    }

    #[test]
    fn addresses_are_found_in_text() {
        assert_eq!(
            addresses_in("Sarah, email Bookings@BaiTong.com and ask."),
            vec!["bookings@baitong.com"]
        );
        assert_eq!(
            addresses_in("<mailto:legal@acme.example|legal@acme.example>"),
            vec!["legal@acme.example", "legal@acme.example"]
        );
        assert!(addresses_in("no address here, @sam").is_empty());
    }

    #[test]
    fn subjects_lose_their_prefixes() {
        assert_eq!(
            bare_subject("Re: RE: Fwd: Table for five"),
            "table for five"
        );
        assert_eq!(bare_subject("Table"), "table");
        assert_eq!(bare_id("<Abc@Mail>"), "abc@mail");
    }
}
