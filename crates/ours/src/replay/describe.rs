//! What a step did that someone could see, as one line per effect, and the
//! counts the report compares.

use super::Tally;
use ours_core::trace::shorten;
use ours_core::{Answering, Effect, FollowUp, GroupState, Input};
use ours_types::{InboundEvent, Instant, OutboundAction};
use std::collections::VecDeque;

/// Characters of a message kept in a line.
const TEXT_CHARS: usize = 300;

/// How many unprompted and chime entries the budget holds for `now`. The
/// difference across a step is what that step spent.
pub(crate) fn spent_at(state: &GroupState, now: Instant) -> (usize, usize) {
    let at = |times: &VecDeque<Instant>| times.iter().filter(|t| **t == now).count();
    (at(&state.budget.spent), at(&state.budget.chimes))
}

/// Adds the visible effects of one step to `lines` and `tally`. `spent`
/// and `chimed` are what the step took from the budget.
pub(crate) fn observe(
    state: &GroupState,
    effects: &[Effect],
    spent: usize,
    chimed: usize,
    lines: &mut Vec<String>,
    tally: &mut Tally,
) {
    let mut room_messages = 0u64;
    for effect in effects {
        match effect {
            Effect::Send(out) => {
                let ballot = state
                    .pending_actions
                    .get(&out.id)
                    .is_some_and(|p| matches!(p.then, Some(FollowUp::Ballot { .. })));
                if let Some(line) = action_line(&out.action, ballot) {
                    lines.push(line);
                }
                match &out.action {
                    OutboundAction::SendMessage { .. } => {
                        tally.messages += 1;
                        room_messages += 1;
                        if ballot {
                            tally.polls += 1;
                        }
                    }
                    OutboundAction::React { .. } => tally.reactions += 1,
                    OutboundAction::SendDm { .. } => tally.dms += 1,
                    OutboundAction::CreatePoll { .. } => tally.polls += 1,
                    _ => {}
                }
            }
            Effect::AskModel(request) => {
                tally.asks += 1;
                lines.push(match state.pending_requests.get(&request.id) {
                    Some(p) => format!(
                        "asked the model ({}) about {} in #{}",
                        answering_name(p.answering),
                        p.message,
                        p.room
                    ),
                    None => "asked the model".to_owned(),
                });
            }
            Effect::Outside { outside, .. } => {
                tally.mails += 1;
                lines.push(match outside {
                    ours_types::OutsideAction::SendMail(m) => {
                        format!("mailed {}: {}", m.to, shorten(&m.subject, TEXT_CHARS))
                    }
                    ours_types::OutsideAction::CancelMail { mail, .. } => {
                        format!("mailed a cancellation to {}", mail.to)
                    }
                });
            }
            Effect::Redact { .. } => {}
            Effect::Notice(text) => {
                tally.notices += 1;
                lines.push(format!("notice: {}", shorten(text, TEXT_CHARS)));
            }
            Effect::SetTimer { .. }
            | Effect::CancelTimer { .. }
            | Effect::Classify(_)
            | Effect::Extract(_)
            | Effect::ResolveTime(_) => {}
        }
    }
    let spent = u64::try_from(spent).unwrap_or(u64::MAX);
    tally.unprompted_messages += room_messages.min(spent);
    tally.chime_ins += u64::try_from(chimed).unwrap_or(u64::MAX);
}

fn action_line(action: &OutboundAction, ballot: bool) -> Option<String> {
    Some(match action {
        OutboundAction::SendMessage {
            room,
            text,
            reply_to,
        } => {
            let what = if ballot { "posted a ballot" } else { "said" };
            let to = reply_to
                .as_ref()
                .map(|m| format!(" replying to {m}"))
                .unwrap_or_default();
            format!("{what} in #{room}{to}: {}", shorten(text, TEXT_CHARS))
        }
        OutboundAction::React {
            room,
            message,
            emoji,
        } => format!("reacted {emoji} on {message} in #{room}"),
        OutboundAction::Unreact {
            room,
            message,
            emoji,
        } => format!("took {emoji} off {message} in #{room}"),
        OutboundAction::SendDm { user, text } => {
            format!("DM to {user}: {}", shorten(text, TEXT_CHARS))
        }
        OutboundAction::EditMessage { room, id, text } => {
            format!("edited {id} in #{room}: {}", shorten(text, TEXT_CHARS))
        }
        OutboundAction::DeleteMessage { room, id } => format!("deleted {id} in #{room}"),
        OutboundAction::Pin { room, message } => format!("pinned {message} in #{room}"),
        OutboundAction::CreatePoll {
            room,
            question,
            options,
            ..
        } => format!(
            "opened a poll in #{room}: {} [{}]",
            shorten(question, TEXT_CHARS),
            options.join(" / ")
        ),
        OutboundAction::EndPoll { room, message } => format!("ended poll {message} in #{room}"),
        OutboundAction::FetchHistory { room, limit, .. } => {
            format!("fetched {limit} messages of #{room}")
        }
        OutboundAction::Typing { .. } => return None,
    })
}

pub(crate) fn answering_name(answering: Answering) -> &'static str {
    match answering {
        Answering::Addressed => "addressed",
        Answering::RaisedHand => "raised hand",
        Answering::Observed => "observed",
        Answering::Chime => "chime",
    }
}

/// One line for what came in, using the names the state knows.
pub(crate) fn summarise(state: &GroupState, input: &Input) -> String {
    match input {
        Input::Inbound(InboundEvent::Message(m))
        | Input::Inbound(InboundEvent::MessageEdited(m)) => {
            format!(
                "{} in {}: {}",
                state.display_name(&m.room, &m.author),
                state.room_label(&m.room),
                shorten(&m.text, 160)
            )
        }
        Input::Inbound(InboundEvent::ReactionAdded {
            room,
            message,
            user,
            emoji,
            ..
        }) => format!(
            "{} reacted {emoji} on {message} in {}",
            state.display_name(room, user),
            state.room_label(room)
        ),
        Input::Inbound(InboundEvent::ActionResult {
            id, ok, message, ..
        }) => match (ok, message) {
            (true, Some(m)) => format!("{id} went out as {m}"),
            (true, None) => format!("{id} went out"),
            (false, _) => format!("{id} failed"),
        },
        Input::ModelReply { request, reply } => match reply {
            Ok(plan) => format!(
                "the model answered {request}: {}",
                shorten(plan.message.as_deref().unwrap_or("(no message)"), 160)
            ),
            Err(error) => format!("the model failed {request}: {}", shorten(error, 160)),
        },
        Input::Scored { message, score, .. } => {
            format!("{message} scored {:.2} as {:?}", score.max(), score.kind)
        }
        Input::TimerFired { id } => match state.timers.get(id) {
            Some(timer) => {
                let purpose = serde_json::to_value(&timer.purpose)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
                    .unwrap_or_else(|| "timer".to_owned());
                format!("timer {id} ({purpose}) fired")
            }
            None => format!("timer {id} fired, no longer armed"),
        },
        Input::Command(command) => shorten(&format!("command {command:?}"), 160),
        Input::Mail(mail) => format!("mail from {}: {}", mail.from, shorten(&mail.subject, 120)),
        other => other.label().to_owned(),
    }
}
