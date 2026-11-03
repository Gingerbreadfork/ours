//! The fold. `step` takes one input and the instant it happened, updates the
//! group's state and returns what should happen next (DESIGN.md section 14).

use crate::action::{self, ActionClass, ActionRecord, Origin, Recording, Undo};
use crate::addressing::{after_address, is_addressed};
use crate::attention::{self, ACT_THRESHOLD, RAISED_HAND_RULE};
use crate::authority::{filter_actions, permitted, resolve_role};
use crate::backfill;
use crate::budget::record_unprompted;
use crate::charter::settings_summary;
use crate::chime::{self, CHIME_PROMPT};
use crate::commitment;
use crate::decide;
use crate::decision::{self, recall_decisions};
use crate::digest;
use crate::effect::{
    ChatTurn, Effect, ModelRequest, TurnRole, react, send, send_as, send_dm, typing,
};
use crate::extract;
use crate::hold;
use crate::input::{Command, Input};
use crate::leads::{self, LEADS_NOTE, decline_line};
use crate::mail;
use crate::memory::{self, Fact, FactId, Place};
use crate::observe;
use crate::patterns;
use crate::persona::who_can_you_be;
use crate::persona::{role_name, system_prompt};
use crate::pin;
use crate::plan::{Plan, PlannedAction, plan_schema};
use crate::reactions::{self, Meaning};
use crate::recall::{self, Citation, RECALLED, recall};
use crate::recur;
use crate::retention;
use crate::rights::{self, FORGOTTEN_NOTE};
use crate::sentence::{self, Sentence, TabSentence};
use crate::settings::{self, SETTING_NOTE, addressed_trigger};
use crate::setup;
use crate::state::{
    Answering, DEFERRAL_TTL_SECONDS, Deferral, DeferredChange, DmFollowUp, FollowUp, GroupState,
    Me, PendingRequest,
};
use crate::tab::{self, Adjust};
use crate::time::clock_echo;
use crate::timers::{self, MODEL_GUARD_SECONDS, TimerPurpose};
use crate::trace::{Trace, quote};
use crate::triage::rules_score;
use crate::undo;
use crate::zone::{self, Zone};
use ours_types::{
    ActionId, InboundEvent, Instant, Message, MessageKind, Platform, PlatformMessageId,
    PlatformRoomId, PlatformUserId, RequestId, Role, RoomKind, Score, TimerId,
};
use tracing::{debug, info, warn};

/// The reaction that says "working on it" (DESIGN.md section 8).
pub const EYES: &str = "👀";
/// The reaction a lead adds to approve a deferral.
pub const CHECK: &str = "✅";
/// What the agent says when it could not get an answer.
pub const FAILED_REPLY: &str = "Something went wrong on my side; try again in a minute.";
/// The answer to "why" when the agent has said nothing in the room.
pub const NOTHING_SAID: &str = "I haven't said anything here yet.";
/// The answer to "why" about a message sent before traces were kept.
pub const NO_TRACE: &str = "I have no record of why I sent that one.";
/// How many of the room's messages the model sees.
pub const CONTEXT_MESSAGES: usize = 30;
/// Decision records offered to the model with one message.
pub const DECISIONS_RECALLED: usize = 3;

const ADDRESSED_RULE: &str = "addressed messages are always answered; the model wrote the reply";
const UNPROMPTED_PROMPT: &str = "Nobody addressed you. The last message is a question nobody in the room has answered. Answer it in one or two lines if the room's messages give you the answer; otherwise reply with a null message.";

/// Folds one input into `state` and returns the effects to carry out, in order.
pub fn step(state: &mut GroupState, input: Input, now: Instant) -> Vec<Effect> {
    let span = tracing::info_span!("step", group = %state.id, input = input.label(), %now);
    let _entered = span.enter();
    state.prune(now);
    let mut fx = Vec::new();
    match input {
        Input::Inbound(event) => inbound(state, event, now, &mut fx),
        Input::TimerFired { id } => timer_fired(state, &id, now, &mut fx),
        Input::ModelReply { request, reply } => model_reply(state, &request, reply, now, &mut fx),
        Input::Scored {
            room,
            message,
            score,
        } => attention::on_scored(state, &room, &message, score, now, &mut fx),
        Input::Extracted {
            room,
            message,
            extraction,
        } => extract::on_extracted(state, &room, &message, extraction, now, &mut fx),
        Input::TimeResolved(resolution) => {
            decide::on_time_resolved(state, resolution, now, &mut fx)
        }
        Input::OutsideResult(result) => mail::on_result(state, &mut fx, result, now),
        Input::Mail(mail) => mail::on_inbound(state, &mut fx, mail, now),
        Input::Command(command) => apply_command(state, command, now, &mut fx),
    }
    // Scheduled jobs and the pinned message follow whatever the step changed.
    digest::reconcile(state, now, &mut fx);
    pin::reconcile(state, now, &mut fx);
    retention::reconcile(state, now, &mut fx);
    fx
}

fn inbound(state: &mut GroupState, event: InboundEvent, now: Instant, fx: &mut Vec<Effect>) {
    match event {
        InboundEvent::Ready {
            platform,
            me,
            me_name,
            capabilities,
        } => {
            fx.push(Effect::Notice(format!(
                "Connected to {} as {me_name}.",
                platform_name(platform)
            )));
            state.me.retain(|m| m.platform != platform);
            state.me.push(Me {
                platform,
                id: me,
                name: me_name,
                capabilities,
            });
        }
        InboundEvent::RoomSync { room, members } => {
            let first = room.kind != RoomKind::Dm
                && !state
                    .rooms
                    .values()
                    .any(|r| r.info.as_ref().is_some_and(|i| i.kind != RoomKind::Dm));
            let id = room.id.clone();
            for member in &members {
                zone::from_platform(state, member);
            }
            let entry = state.room_mut(&room.id);
            entry.members = members.into_iter().map(|m| (m.id.clone(), m)).collect();
            entry.info = Some(room);
            if first {
                first_sync(state, &id, now, fx);
            }
        }
        InboundEvent::Message(message) => on_message(state, message, now, fx),
        InboundEvent::MessageEdited(message) => {
            let held = state
                .rooms
                .get_mut(&message.room)
                .is_some_and(|r| r.replace(message));
            if !held {
                debug!("edit to a message no longer held");
            }
        }
        InboundEvent::MessageDeleted { room, id } => {
            if let Some(r) = state.rooms.get_mut(&room) {
                r.remove(&id);
                r.own_messages.retain(|m| m != &id);
            }
            if state.deferrals.remove(&id).is_some() {
                debug!(message = %id, "deferral withdrawn with its message");
            }
            decide::on_deleted(state, &room, &id);
            commitment::on_deleted(state, &room, &id, now, fx);
            tab::on_deleted(state, &room, &id, now);
            pin::forget(state, &room, Some(&id));
            let dropped = memory::on_deleted(state, &room, &id, now);
            if dropped > 0 {
                fx.push(Effect::Notice(format!(
                    "Dropped {dropped} {} with a deleted message in {}.",
                    if dropped == 1 { "fact" } else { "facts" },
                    state.room_label(&room)
                )));
            }
        }
        InboundEvent::ReactionAdded {
            room,
            message,
            user,
            emoji,
            at,
        } => {
            if state.is_human(&room, &user) {
                state.saw(&user, at);
            }
            on_reaction(state, &room, &message, &user, &emoji, now, fx);
        }
        InboundEvent::ReactionRemoved {
            room,
            message,
            user,
            emoji,
            ..
        } => {
            let mark = decide::Mark {
                room: &room,
                message: &message,
                user: &user,
                added: false,
            };
            let handled = !state.is_me(&user)
                && (decide::on_reaction(state, mark, &emoji, now, fx)
                    || tab::on_reaction(state, &room, &message, &user, &emoji, false, fx));
            if !handled {
                debug!(%message, %emoji, "reaction removed");
            }
        }
        InboundEvent::MemberJoined { room, member, .. }
        | InboundEvent::MemberUpdated { room, member } => {
            zone::from_platform(state, &member);
            state
                .room_mut(&room)
                .members
                .insert(member.id.clone(), member);
        }
        InboundEvent::MemberLeft { room, user, .. } => {
            if let Some(r) = state.rooms.get_mut(&room) {
                r.members.remove(&user);
            }
        }
        InboundEvent::ActionResult {
            id,
            ok,
            message,
            error,
        } => {
            on_action_result(state, &id, ok, message, error, now, fx);
        }
        InboundEvent::History { room, messages } => {
            for m in &messages {
                if state.is_human(&room, &m.author) {
                    state.saw(&m.author, m.at);
                }
            }
            if backfill::waiting(state, &room) {
                backfill::on_history(state, &room, messages, now, fx);
            } else {
                state.room_mut(&room).prepend(messages);
            }
        }
        InboundEvent::PollVote {
            room,
            message,
            user,
            option,
            added,
            at,
        } => {
            if state.is_human(&room, &user) {
                state.saw(&user, at);
            }
            let mark = decide::Mark {
                room: &room,
                message: &message,
                user: &user,
                added,
            };
            decide::on_poll_vote(state, mark, option, now, fx);
        }
        InboundEvent::PollEnded {
            room,
            message,
            counts,
        } => decide::on_poll_ended(state, &room, &message, counts, now, fx),
        InboundEvent::Disconnected { platform, reason } => {
            fx.push(Effect::Notice(format!(
                "Disconnected from {}: {reason}",
                platform_name(platform)
            )));
        }
    }
}

/// The group's first room: watch before speaking unprompted, and start the
/// first conversation with whoever added the agent.
fn first_sync(state: &mut GroupState, room: &PlatformRoomId, now: Instant, fx: &mut Vec<Effect>) {
    let seconds = state.profile.observe_seconds;
    if seconds > 0 && state.observe.is_none() && state.observed.is_none() {
        observe::start(state, seconds, now, fx);
    }
    setup::start(state, room, now, fx);
}

fn on_message(state: &mut GroupState, message: Message, now: Instant, fx: &mut Vec<Effect>) {
    let from_me = state.is_me(&message.author);
    let seen = state
        .rooms
        .get(&message.room)
        .is_some_and(|r| r.messages.iter().any(|m| m.id == message.id));
    let fresh = !from_me && !seen && !state.is_bot(&message.room, &message.author);
    let addressed = fresh && is_addressed(state, &message);
    let room = state.room_mut(&message.room);
    if from_me {
        room.remember_own(message.id.clone());
    }
    room.push(message.clone());
    if fresh {
        patterns::record(state, &message.room, &message.author, message.at, now);
        state.saw(&message.author, message.at);
    }
    if addressed {
        on_addressed(state, &message, now, fx);
    } else if fresh {
        if tab::link_in(&message.text).is_some() {
            link_in_room(state, &message, now, fx);
            return;
        }
        decide::heard(state, &message, now, fx);
        commitment::heard(state, &message, now, fx);
        attention::classify(state, &message, fx);
    }
}

/// A payment link posted in a room nobody addressed: taken out of memory
/// and its author told by DM where to put it.
fn link_in_room(state: &mut GroupState, message: &Message, now: Instant, fx: &mut Vec<Effect>) {
    if state.rooms.get(&message.room).is_some_and(|r| r.is_dm()) {
        return;
    }
    state.room_mut(&message.room).remove(&message.id);
    let label = state.room_label(&message.room);
    let text = format!(
        "I didn't keep the payment link you posted in {label}. DM it to me and I'll only show it to people who owe you."
    );
    let trace = Trace::new(
        format!(
            "{} posted a payment link in {label}",
            state.name_of(&message.author)
        ),
        "payment links are kept only from a DM and shown only to debtors",
        now,
    );
    send_dm(state, fx, &message.author, text, now, trace, false);
}

/// 👀 first, then the first conversation, a ruled sentence or a model call.
fn on_addressed(state: &mut GroupState, message: &Message, now: Instant, fx: &mut Vec<Effect>) {
    let role = resolve_role(state, &message.room, &message.author);
    info!(speaker = %message.author, role = role_name(role), "addressed");
    reactions::add(state, fx, &message.room, &message.id, Meaning::Eyes);
    let body = after_address(state, &message.text).to_owned();
    if setup::on_reply(state, message, &body, now, fx) {
        reactions::remove(state, fx, &message.room, &message.id, Meaning::Eyes);
        return;
    }
    let in_dm = state.rooms.get(&message.room).is_some_and(|r| r.is_dm());
    if in_dm && commitment::dm_reply(state, message, &body, now, fx) {
        reactions::remove(state, fx, &message.room, &message.id, Meaning::Eyes);
        return;
    }
    let Some(sentence) = sentence::parse(&body) else {
        if decide::addressed(state, message, &body, role, now, fx) {
            reactions::remove(state, fx, &message.room, &message.id, Meaning::Eyes);
            return;
        }
        to_model(state, message, role, now, fx);
        return;
    };
    let room = &message.room;
    let reply_to = Some(message.id.clone());
    let trigger = addressed_trigger(state, message);
    let mut waiting = false;
    match sentence {
        Sentence::WhoDoYouListenTo => {
            let line = leads::listen_line(state, room);
            let trace = Trace::new(
                trigger,
                "\"who do you listen to\" reads the leads list",
                now,
            );
            send(state, fx, room, line, reply_to, now, trace);
        }
        Sentence::Undo(what) => undo::handle(state, message, role, what, now, fx),
        Sentence::WhatDidYouSend => {
            let text = mail::sent_list(state, message);
            let trace = Trace::new(
                trigger,
                "\"what did you send\" reads the action record",
                now,
            );
            send_as(
                state,
                fx,
                room,
                text,
                reply_to,
                now,
                trace,
                Recording::Unrecorded,
            );
        }
        Sentence::Leads(s) => leads::handle(state, message, role, s, now, fx),
        Sentence::Setting(change) => settings::handle(state, message, role, change, now, fx),
        Sentence::Shush if role >= Role::Member => {
            attention::shush(state, room, &message.author, &message.id, now, fx);
        }
        Sentence::Shush => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
        Sentence::Settings => {
            let text = settings_summary(state, room, now);
            let trace = Trace::new(trigger, "\"settings\" reads the charter", now);
            send(state, fx, room, text, reply_to, now, trace);
        }
        Sentence::Why => why(state, message, trigger, now, fx),
        Sentence::WhatDoYouKnow => rights::what_do_you_know(state, message, now, fx),
        Sentence::ForgetMe => rights::forget_me(state, message, now, fx),
        Sentence::ForgetThat => rights::forget_that(state, message, role, now, fx),
        Sentence::JustThisWeek => rights::just_this_week(state, message, role, now, fx),
        Sentence::ThatsWrong { rest } => {
            waiting = rights::thats_wrong(state, message, role, &rest, now, fx);
        }
        Sentence::RememberThat { rest } => {
            waiting = rights::remember_that(state, message, role, &rest, now, fx);
        }
        Sentence::Backfill => backfill::handle(state, message, role, now, fx),
        Sentence::Decision(s) => match decide::handle(state, message, role, s, now, fx) {
            Some(wait) => waiting = wait,
            None => {
                to_model(state, message, role, now, fx);
                return;
            }
        },
        Sentence::MyZone { zone: name } => {
            let line = my_zone(state, &message.author, name, now);
            let trace = Trace::new(trigger, "anyone may say which zone they are in", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
        Sentence::Commitments(ask) => commitment::answer(state, message, ask, now, fx),
        Sentence::Tab(TabSentence::Balances { mine }) => tab::answer(state, message, mine, now, fx),
        Sentence::Tab(TabSentence::SettleUp) if role >= Role::Member => {
            tab::settle_up(state, message, now, fx);
        }
        Sentence::Tab(TabSentence::SettleUp) => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
        Sentence::Tab(TabSentence::PaymentLink { link }) => {
            tab::payment_link(state, message, &link, now, fx);
        }
        Sentence::Tab(TabSentence::Paid) => {
            if !(in_dm && tab::paid_words(state, message, now, fx)) {
                to_model(state, message, role, now, fx);
                return;
            }
        }
        Sentence::Tab(TabSentence::NotMe | TabSentence::Double) if role < Role::Member => {
            let line = decline_line(state, room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
        Sentence::Tab(word @ (TabSentence::NotMe | TabSentence::Double)) => {
            let how = if word == TabSentence::NotMe {
                Adjust::NotMe
            } else {
                Adjust::Double
            };
            if !tab::adjust_words(state, message, how, now, fx) {
                if decide::addressed(state, message, &body, role, now, fx) {
                    reactions::remove(state, fx, room, &message.id, Meaning::Eyes);
                    return;
                }
                to_model(state, message, role, now, fx);
                return;
            }
        }
        Sentence::CalendarLink => {
            let line = calendar_link(state, room, &message.author);
            let trace = Trace::new(trigger, "\"calendar link\" gives the feed for here", now);
            send(state, fx, room, line, reply_to, now, trace);
        }
        Sentence::Recur(s) => recur::handle(state, message, role, s, now, fx),
        Sentence::Setup => {
            if !setup::on_setup_word(state, message, role, now, fx) {
                to_model(state, message, role, now, fx);
                return;
            }
        }
        Sentence::WhoCanYouBe => {
            let text = who_can_you_be(state);
            let trace = Trace::new(trigger, "\"who can you be\" lists the starter voices", now);
            send(state, fx, room, text, reply_to, now, trace);
        }
    }
    // A reply that waits on the extractor keeps the 👀 until it lands.
    if !waiting {
        reactions::remove(state, fx, room, &message.id, Meaning::Eyes);
    }
}

/// Hands an addressed message to the model, with the rules pass's score.
fn to_model(
    state: &mut GroupState,
    message: &Message,
    role: Role,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if role == Role::Guest && is_instruction(state, message) {
        let body = after_address(state, &message.text).to_owned();
        leads::flag_request(state, message, &body, GUEST_RULE, now, fx);
        reactions::remove(state, fx, &message.room, &message.id, Meaning::Eyes);
        return;
    }
    let score = rules_score(state, &message.room, message);
    // A promise or money paid goes to the extractor first; the model only
    // hears it when nothing turns up.
    if score.commitment >= ACT_THRESHOLD {
        extract::addressed(state, message, now, fx);
        return;
    }
    let fact = score.fact >= ACT_THRESHOLD;
    let ask = Ask {
        room: message.room.clone(),
        message: message.id.clone(),
        speaker: message.author.clone(),
        role,
        answering: Answering::Addressed,
        score: Some(score),
    };
    ask_model(state, ask, now, fx);
    // Addressed messages skip the classifier, so the rules pass decides.
    if fact {
        extract::heard(state, message, now, fx);
    }
}

/// Verbs that open an instruction the rules pass may not label as one.
const INSTRUCTION_VERBS: [&str; 24] = [
    "email",
    "mail",
    "write",
    "ask",
    "order",
    "reply",
    "push",
    "move",
    "book",
    "send",
    "cancel",
    "add",
    "remove",
    "change",
    "set",
    "update",
    "schedule",
    "invite",
    "tell",
    "remind",
    "make",
    "do",
    "delete",
    "reschedule",
];

/// Whether an addressed message asks for something done rather than asking
/// a question.
const GUEST_RULE: &str = "guests are answered, never obeyed";
const MEMBER_RULE: &str = "a member's request waits for a lead";

/// The message behind `pending` when a member asked for an email in it.
fn unplanned_mail_request(state: &GroupState, pending: &PendingRequest) -> Option<Message> {
    let message = state
        .rooms
        .get(&pending.room)?
        .messages
        .iter()
        .find(|m| m.id == pending.message)?
        .clone();
    let body = after_address(state, &message.text).to_lowercase();
    let mentions_mail = body.contains("mail") || body.contains('@');
    (mentions_mail && is_instruction(state, &message)).then_some(message)
}

fn is_instruction(state: &GroupState, message: &Message) -> bool {
    if rules_score(state, &message.room, message).kind == MessageKind::Instruction {
        return true;
    }
    let body = after_address(state, &message.text).to_lowercase();
    // A polite request is still a request: "can you push it to Friday?"
    let mut rest = body.trim();
    let mut polite = false;
    for lead in [
        "please",
        "can you",
        "could you",
        "would you",
        "will you",
        "pls",
    ] {
        if let Some(after) = rest.strip_prefix(lead) {
            rest = after.trim_start_matches([',', ' ']);
            polite = true;
        }
    }
    if !polite && rest.trim_end().ends_with('?') {
        return false;
    }
    let first: String = rest
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned();
    INSTRUCTION_VERBS.contains(&first.as_str())
}

/// Asks the model to answer an addressed message the extractor found
/// nothing in, as `speaker` asked it.
pub(crate) fn model_for(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    speaker: &PlatformUserId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(held) = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message))
        .cloned()
    else {
        reactions::remove(state, fx, room, message, Meaning::Eyes);
        return;
    };
    let role = resolve_role(state, room, speaker);
    let score = rules_score(state, room, &held);
    let ask = Ask {
        room: room.clone(),
        message: message.clone(),
        speaker: speaker.clone(),
        role,
        answering: Answering::Addressed,
        score: Some(score),
    };
    ask_model(state, ask, now, fx);
}

/// "calendar link": the member's own feed in a DM, the group's in a room.
fn calendar_link(state: &GroupState, room: &PlatformRoomId, user: &PlatformUserId) -> String {
    let Some(base) = &state.feed_base else {
        return "There's no calendar feed yet; the host can turn it on.".to_owned();
    };
    if state.rooms.get(room).is_some_and(|r| r.is_dm()) {
        format!(
            "Your calendar: {}. It has what you're in and your own commitments.",
            member_feed_link(base, user)
        )
    } else {
        format!(
            "The group's calendar: {base}.ics. Add it to any calendar app; it updates as plans close."
        )
    }
}

/// `<url>/ics/<group>/<token>/<user>.ics` for a feed base of
/// `<url>/ics/<group>/<secret>`, the token keyed to the member.
fn member_feed_link(base: &str, user: &PlatformUserId) -> String {
    let (parent, secret) = base.rsplit_once('/').unwrap_or(("", base));
    let token = crate::ics::member_feed_token(secret, user.as_str());
    format!("{parent}/{token}/{}.ics", url_part(user.as_str()))
}

/// A user id as one URL path segment.
fn url_part(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Keeps the zone a member stated and says it back with the time there.
fn my_zone(state: &mut GroupState, user: &PlatformUserId, name: String, now: Instant) -> String {
    let local = zone::local(now, &name);
    let line = format!(
        "Noted: {name} for you, {} there now.",
        clock_echo(local.hour(), local.minute())
    );
    state.zones.insert(
        user.clone(),
        Zone {
            name,
            confidence: zone::STATED_CONFIDENCE,
        },
    );
    line
}

/// Reads back the trace of the message `message` replies to, or of the
/// agent's last message in the room. No model call.
fn why(
    state: &mut GroupState,
    message: &Message,
    trigger: String,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let text = match state.rooms.get(&message.room) {
        None => NOTHING_SAID.to_owned(),
        Some(r) => {
            let about = match message.reply_to.as_ref().filter(|id| r.is_own(id)) {
                Some(id) => Some((id, r.trace_of(id))),
                None => r.traces.back().map(|(id, t)| (id, Some(t))),
            };
            match about {
                Some((id, Some(t))) => {
                    let mut text = explain(state, t);
                    if let Some(line) = action::about_message(state, &message.room, id)
                        .and_then(|record| action::why_line(state, record))
                    {
                        text.push('\n');
                        text.push_str(&line);
                    }
                    text
                }
                Some((_, None)) => NO_TRACE.to_owned(),
                None => NOTHING_SAID.to_owned(),
            }
        }
    };
    let trace = Trace::new(trigger, "\"why\" reads the record, with no model call", now);
    send_as(
        state,
        fx,
        &message.room,
        text,
        Some(message.id.clone()),
        now,
        trace,
        Recording::Unrecorded,
    );
}

/// A trace read back, with a fourth line for the facts it relied on.
fn explain(state: &GroupState, trace: &Trace) -> String {
    let mut text = trace.explain();
    if let Some(line) = recall::from_line(state, &trace.facts) {
        text.push('\n');
        text.push_str(&line);
    }
    text
}

/// What a model call answers.
#[derive(Debug, Clone)]
pub(crate) struct Ask {
    pub room: PlatformRoomId,
    /// The message the reply is threaded to; the model sees it last.
    pub message: PlatformMessageId,
    pub speaker: PlatformUserId,
    pub role: Role,
    pub answering: Answering,
    pub score: Option<Score>,
}

pub(crate) fn ask_model(state: &mut GroupState, ask: Ask, now: Instant, fx: &mut Vec<Effect>) {
    let id = state.mint_request();
    let mut system = system_prompt(state, &ask.room, &ask.speaker, ask.role, now);
    match ask.answering {
        Answering::Addressed => {}
        Answering::Chime => {
            system.push('\n');
            system.push_str(CHIME_PROMPT);
        }
        Answering::RaisedHand | Answering::Observed => {
            system.push('\n');
            system.push_str(UNPROMPTED_PROMPT);
        }
    }
    // Observed answers go to leads who may not be in the room, so they get
    // no memory.
    let recalled = if ask.answering == Answering::Observed {
        Vec::new()
    } else {
        remembered(state, &ask, now)
    };
    if !recalled.is_empty() {
        let place = Place::of(state, &ask.room);
        let facts: Vec<&Fact> = recalled
            .iter()
            .filter_map(|id| state.facts.get(id))
            .collect();
        system.push_str("\n\n");
        system.push_str(&recall::block(state, &place, &facts));
    }
    if ask.answering != Answering::Observed
        && let Some(text) = held_text(state, &ask)
    {
        let place = Place::of(state, &ask.room);
        let decisions = recall_decisions(state, &place, &text, DECISIONS_RECALLED);
        if !decisions.is_empty() {
            system.push_str("\n\n");
            system.push_str(&decision::block(state, &decisions));
        }
    }
    // Observed answers go to leads by DM, so the room sees no typing.
    if ask.answering != Answering::Observed {
        typing(state, fx, &ask.room);
    }
    fx.push(Effect::AskModel(ModelRequest {
        id: id.clone(),
        system,
        messages: addressed_context(state, &ask),
        output_schema: plan_schema(),
    }));
    let purpose = TimerPurpose::ModelGuard {
        request: id.clone(),
    };
    let guard = timers::arm(state, fx, now.plus_seconds(MODEL_GUARD_SECONDS), purpose);
    let pending = PendingRequest {
        room: ask.room,
        message: ask.message,
        speaker: ask.speaker,
        role: ask.role,
        allowed: permitted(ask.role),
        guard,
        asked_at: now,
        answering: ask.answering,
        score: ask.score,
        recalled,
    };
    state.pending_requests.insert(id, pending);
}

/// The words of the message being answered, mentions read as names.
fn held_text(state: &GroupState, ask: &Ask) -> Option<String> {
    let held = state
        .rooms
        .get(&ask.room)?
        .messages
        .iter()
        .find(|m| m.id == ask.message)?;
    Some(render_mentions(
        state,
        &ask.room,
        after_address(state, &held.text),
    ))
}

/// Facts that may matter to the message being answered, by its words and
/// the speaker's name, as the place allows.
fn remembered(state: &GroupState, ask: &Ask, now: Instant) -> Vec<FactId> {
    let Some(held) = state
        .rooms
        .get(&ask.room)
        .and_then(|r| r.messages.iter().find(|m| m.id == ask.message))
    else {
        return Vec::new();
    };
    let text = render_mentions(state, &ask.room, after_address(state, &held.text));
    let query = format!("{text} {}", state.display_name(&ask.room, &ask.speaker));
    let place = Place::of(state, &ask.room);
    recall(state, &place, &query, now, RECALLED)
        .into_iter()
        .map(|f| f.id.clone())
        .collect()
}

/// The reply with a source line for up to two of the messages it cited.
fn with_sources(state: &GroupState, text: String, citation: &Citation) -> String {
    let lines = recall::source_lines(state, citation);
    if lines.is_empty() {
        return text;
    }
    format!("{text}\n{}", lines.join("\n"))
}

/// What a plan cited, from the facts its request offered.
fn cited(state: &GroupState, pending: &PendingRequest, plan: &Plan) -> Citation {
    let place = Place::of(state, &pending.room);
    if plan.cites.is_empty() {
        let text = plan.message.as_deref().unwrap_or_default();
        let implied = recall::implied_cites(state, &pending.recalled, text);
        return recall::citation(state, &place, &pending.recalled, &implied);
    }
    recall::citation(state, &place, &pending.recalled, &plan.cites)
}

/// Platform mentions as the model should read them: `<@id>` and `<@!id>`
/// become `@Name` for anyone in the room, and the agent's own mention
/// becomes its name, so "hey <@123>" reads as "hey @Sarah". The journaled
/// text keeps the raw form for provenance.
pub fn render_mentions(state: &GroupState, room_id: &PlatformRoomId, text: &str) -> String {
    if !text.contains("<@") {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<@") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let after = after.strip_prefix('!').unwrap_or(after);
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        match after[digits..].strip_prefix('>') {
            Some(tail) if digits > 0 => {
                let id = PlatformUserId::new(&after[..digits]);
                if state.is_me(&id) {
                    out.push('@');
                    out.push_str(&state.agent_name);
                } else if state
                    .rooms
                    .get(room_id)
                    .is_some_and(|r| r.members.contains_key(&id))
                {
                    out.push('@');
                    out.push_str(&state.display_name(room_id, &id));
                } else {
                    out.push_str(&rest[start..start + 2 + (rest.len() - start - 2 - tail.len())]);
                }
                rest = tail;
            }
            _ => {
                out.push_str("<@");
                rest = &rest[start + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The room's messages up to and including `upto`, as turns, oldest first.
/// Consecutive turns from the same side are merged and the list starts with
/// people, as most endpoints require.
/// The room's context for `ask`. A request a lead took up ends with the
/// lead saying it, after the words of the one who first asked.
fn addressed_context(state: &GroupState, ask: &Ask) -> Vec<ChatTurn> {
    let mut turns = context(state, &ask.room, &ask.message);
    if ask.answering != Answering::Addressed {
        return turns;
    }
    let Some(held) = state
        .rooms
        .get(&ask.room)
        .and_then(|r| r.messages.iter().find(|m| m.id == ask.message))
    else {
        return turns;
    };
    if held.author == ask.speaker {
        return turns;
    }
    let text = render_mentions(state, &ask.room, after_address(state, &held.text));
    let line = format!("{}: {text}", state.display_name(&ask.room, &ask.speaker));
    match turns.last_mut() {
        Some(last) if last.role == TurnRole::User => {
            last.content.push('\n');
            last.content.push_str(&line);
        }
        _ => turns.push(ChatTurn {
            role: TurnRole::User,
            content: line,
        }),
    }
    turns
}

fn context(
    state: &GroupState,
    room_id: &PlatformRoomId,
    upto: &PlatformMessageId,
) -> Vec<ChatTurn> {
    let Some(room) = state.rooms.get(room_id) else {
        return Vec::new();
    };
    let end = room
        .messages
        .iter()
        .position(|m| &m.id == upto)
        .map_or(room.messages.len(), |i| i + 1);
    let skip = end.saturating_sub(CONTEXT_MESSAGES);
    let mut turns: Vec<ChatTurn> = Vec::new();
    for m in room.messages.range(skip..end) {
        let text = render_mentions(state, room_id, &m.text);
        let (role, content) = if state.is_me(&m.author) || room.is_own(&m.id) {
            (TurnRole::Assistant, text)
        } else {
            (
                TurnRole::User,
                format!("{}: {}", state.display_name(room_id, &m.author), text),
            )
        };
        match turns.last_mut() {
            Some(last) if last.role == role => {
                last.content.push('\n');
                last.content.push_str(&content);
            }
            _ => turns.push(ChatTurn { role, content }),
        }
    }
    let first_user = turns
        .iter()
        .position(|t| t.role == TurnRole::User)
        .unwrap_or(turns.len());
    turns.drain(..first_user);
    turns
}

fn model_reply(
    state: &mut GroupState,
    request: &RequestId,
    reply: Result<Plan, String>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(pending) = state.pending_requests.remove(request) else {
        debug!(%request, "reply to a request no longer pending");
        return;
    };
    match pending.answering {
        Answering::Addressed => addressed_reply(state, &pending, reply, now, fx),
        Answering::RaisedHand => raised_hand_reply(state, &pending, reply, now, fx),
        Answering::Observed => observed_reply(state, &pending, reply, now, fx),
        Answering::Chime => chime::reply(state, &pending, reply, now, fx),
    }
}

/// The answer to an addressed message. A reaction alone is a complete
/// answer; a plan with neither a message nor a reaction says nothing.
fn addressed_reply(
    state: &mut GroupState,
    pending: &PendingRequest,
    reply: Result<Plan, String>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let reply_to = Some(pending.message.clone());
    let trigger = trigger_for(state, &pending.room, &pending.message);
    let mut notice = None;
    match reply {
        Ok(plan) => {
            // A member's email request the model answered in words instead
            // of planning still goes to the leads; words would hide that.
            if pending.role == Role::Member
                && !plan.actions.iter().any(PlannedAction::is_mail)
                && let Some(asked) = unplanned_mail_request(state, pending)
            {
                let body = after_address(state, &asked.text).to_owned();
                leads::flag_request(state, &asked, &body, MEMBER_RULE, now, fx);
                reactions::remove(state, fx, &pending.room, &pending.message, Meaning::Eyes);
                timers::cancel(state, fx, &pending.guard);
                return;
            }
            let citation = cited(state, pending, &plan);
            let trace = Trace::new(trigger.clone(), ADDRESSED_RULE, now)
                .scored(pending.score.clone())
                .citing(citation.facts.clone());
            // A plan that emails gets the hold line as its one message, so
            // the model never reports a send that has not happened.
            // A mail with nobody to send to is dropped by the screen, so it
            // must not swallow the reply the model also wrote.
            let mails = plan.actions.iter().any(|a| match a {
                PlannedAction::Mail { to, .. } => !to.trim().is_empty(),
                _ => false,
            });
            let reacts = plan
                .actions
                .iter()
                .any(|a| matches!(a, PlannedAction::React { .. }));
            match plan.message.filter(|t| !mails && !t.trim().is_empty()) {
                Some(text) => {
                    let text = with_sources(state, text, &citation);
                    send(state, fx, &pending.room, text, reply_to, now, trace.clone());
                    memory::confirm(state, &citation.facts, now);
                }
                None if reacts => debug!(message = %pending.message, "answered with a reaction"),
                None => debug!(message = %pending.message, "nothing needed saying"),
            }
            let origin = Origin::asked(&pending.speaker, &pending.message);
            let instruction = request_text(state, &pending.room, &pending.message);
            let actions = if pending.role == Role::Guest {
                plan.actions
            } else {
                mail::screen(
                    state,
                    fx,
                    &pending.room,
                    &origin,
                    &instruction,
                    plan.actions,
                    now,
                    &trace,
                )
            };
            let filtered = filter_actions(actions, &pending.allowed);
            for action in filtered.allowed {
                carry_out(
                    state,
                    fx,
                    &pending.room,
                    &origin,
                    action,
                    now,
                    trace.clone(),
                );
            }
            if !filtered.deferred.is_empty() {
                defer_actions(state, fx, pending, filtered.deferred, trigger, now);
            }
        }
        Err(error) => {
            warn!(%error, "model request failed");
            let trace = Trace::new(trigger, "a failed model call is owned up to", now);
            send(state, fx, &pending.room, FAILED_REPLY, reply_to, now, trace);
            notice = Some(format!("The model failed to answer: {error}"));
        }
    }
    reactions::remove(state, fx, &pending.room, &pending.message, Meaning::Eyes);
    timers::cancel(state, fx, &pending.guard);
    if let Some(notice) = notice {
        fx.push(Effect::Notice(notice));
    }
}

/// An answer to a question the agent raised a hand on: threaded to it,
/// counted against the budget, and the 🙋 comes off. Planned actions are
/// dropped, since nobody asked for any.
fn raised_hand_reply(
    state: &mut GroupState,
    pending: &PendingRequest,
    reply: Result<Plan, String>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let mut notice = None;
    match reply {
        Ok(plan) => {
            let citation = cited(state, pending, &plan);
            if let Some(text) = plan.message.filter(|t| !t.trim().is_empty()) {
                let trace = attention::unanswered_trace(
                    state,
                    &pending.room,
                    &pending.message,
                    now,
                    RAISED_HAND_RULE,
                )
                .citing(citation.facts.clone());
                let text = with_sources(state, text, &citation);
                let reply_to = Some(pending.message.clone());
                send(state, fx, &pending.room, text, reply_to, now, trace);
                memory::confirm(state, &citation.facts, now);
                record_unprompted(state, now);
            }
        }
        Err(error) => {
            warn!(%error, "model request failed");
            notice = Some(format!(
                "The model failed to answer an open question, so the hand came down: {error}"
            ));
        }
    }
    reactions::remove(
        state,
        fx,
        &pending.room,
        &pending.message,
        Meaning::RaisedHand,
    );
    timers::cancel(state, fx, &pending.guard);
    if let Some(notice) = notice {
        fx.push(Effect::Notice(notice));
    }
}

/// An answer written while observing goes to leads and hosts as a "would
/// have" line, never to the room.
fn observed_reply(
    state: &mut GroupState,
    pending: &PendingRequest,
    reply: Result<Plan, String>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    timers::cancel(state, fx, &pending.guard);
    let text = match reply {
        Ok(plan) => plan.message.filter(|t| !t.trim().is_empty()),
        Err(error) => {
            warn!(%error, "model request failed");
            fx.push(Effect::Notice(format!(
                "The model failed to answer an open question while observing: {error}"
            )));
            None
        }
    };
    let Some(text) = text.filter(|_| state.is_observing()) else {
        return;
    };
    if let Some(line) = attention::would_have(state, &pending.room, &pending.message, Some(&text)) {
        let trace = attention::unanswered_trace(
            state,
            &pending.room,
            &pending.message,
            now,
            attention::OBSERVE_RULE,
        );
        observe::report(state, fx, &line, trace);
        record_unprompted(state, now);
    }
}

/// "Sam in #dinner: "Q, thurs or fri?"", or a plain phrase when the message
/// is no longer held.
pub(crate) fn trigger_for(
    state: &GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
) -> String {
    state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message))
        .map_or_else(
            || format!("a message in {}", state.room_label(room)),
            |m| addressed_trigger(state, m),
        )
}

/// The text of the message that asked, as the room holds it.
fn request_text(state: &GroupState, room: &PlatformRoomId, message: &PlatformMessageId) -> String {
    state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == message))
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// Carries out one planned action for `origin`, leaving its record.
fn carry_out(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    room: &PlatformRoomId,
    origin: &Origin,
    action: PlannedAction,
    now: Instant,
    trace: Trace,
) {
    match action {
        PlannedAction::Say { text } => {
            let recording = Recording::For {
                origin: origin.clone(),
            };
            send_as(state, fx, room, text, None, now, trace, recording);
        }
        PlannedAction::React { emoji } => {
            let Some(message) = origin.instruction.clone() else {
                return;
            };
            let Some(emoji) = reactions::fit(state, room, &emoji) else {
                debug!(%emoji, "a reaction this platform does not allow");
                return;
            };
            let id = react(state, fx, room, &message, &emoji);
            let whose = origin.requested_by.as_ref().map_or_else(
                || "a".to_owned(),
                |u| format!("{}'s", state.display_name(room, u)),
            );
            let undo = Undo::Unreact {
                room: room.clone(),
                message,
                emoji: emoji.clone(),
            };
            let summary = format!("my {emoji} on {whose} message");
            let record =
                ActionRecord::new(id, ActionClass::Reaction, room.clone(), now, summary, undo)
                    .from_origin(origin);
            action::push(state, record);
        }
        PlannedAction::Mail { to, subject, body } => {
            mail::hold(state, fx, room, origin, to, subject, body, now, trace);
        }
        PlannedAction::Nothing => {}
    }
}

/// Planned actions the asker may not have wait for a lead's ✅. A guest's
/// are dropped: guests are answered, never obeyed.
fn defer_actions(
    state: &mut GroupState,
    fx: &mut Vec<Effect>,
    pending: &PendingRequest,
    actions: Vec<PlannedAction>,
    trigger: String,
    now: Instant,
) {
    if pending.role == Role::Guest {
        info!(count = actions.len(), "dropped actions a guest asked for");
        if actions.iter().any(PlannedAction::is_mail) {
            let line = decline_line(state, &pending.room);
            let trace = Trace::new(trigger, "guests are answered, never obeyed", now);
            let reply_to = Some(pending.message.clone());
            send_as(
                state,
                fx,
                &pending.room,
                line,
                reply_to,
                now,
                trace,
                Recording::Unrecorded,
            );
        }
        return;
    }
    let request_text = state
        .rooms
        .get(&pending.room)
        .and_then(|r| r.messages.iter().find(|m| m.id == pending.message))
        .map(|m| m.text.clone())
        .unwrap_or_default();
    let deferral = Deferral {
        room: pending.room.clone(),
        asked_by: pending.speaker.clone(),
        request: pending.message.clone(),
        request_text,
        change: DeferredChange::Actions { actions },
        expires: now.plus_seconds(DEFERRAL_TTL_SECONDS),
    };
    let trace = Trace::new(trigger, "a member's request waits for a lead's ✅", now);
    leads::open_deferral(state, fx, deferral, now, trace);
}

fn timer_fired(state: &mut GroupState, id: &TimerId, now: Instant, fx: &mut Vec<Effect>) {
    let Some(timer) = state.timers.remove(id) else {
        debug!(timer = %id, "timer no longer armed");
        return;
    };
    match timer.purpose {
        TimerPurpose::ModelGuard { request } => {
            let Some(pending) = state.pending_requests.remove(&request) else {
                return;
            };
            warn!(%request, "model did not answer in time");
            match pending.answering {
                Answering::Addressed => {
                    let trigger = trigger_for(state, &pending.room, &pending.message);
                    let trace = Trace::new(trigger, "a failed model call is owned up to", now);
                    send(
                        state,
                        fx,
                        &pending.room,
                        FAILED_REPLY,
                        Some(pending.message.clone()),
                        now,
                        trace,
                    );
                    reactions::remove(state, fx, &pending.room, &pending.message, Meaning::Eyes);
                    fx.push(Effect::Notice(format!(
                        "The model did not answer within {MODEL_GUARD_SECONDS} s; said so in the room."
                    )));
                }
                Answering::RaisedHand => {
                    reactions::remove(
                        state,
                        fx,
                        &pending.room,
                        &pending.message,
                        Meaning::RaisedHand,
                    );
                    fx.push(Effect::Notice(format!(
                        "The model did not answer within {MODEL_GUARD_SECONDS} s; the hand came down."
                    )));
                }
                Answering::Observed => {
                    fx.push(Effect::Notice(format!(
                        "The model did not answer within {MODEL_GUARD_SECONDS} s; nothing to report."
                    )));
                }
                Answering::Chime => {
                    fx.push(Effect::Notice(format!(
                        "The model did not answer within {MODEL_GUARD_SECONDS} s; no chime."
                    )));
                }
            }
        }
        TimerPurpose::RaiseHand { room, message } => {
            attention::raise_hand(state, &room, &message, now, fx);
        }
        TimerPurpose::AnswerAnyway { room, message } => {
            attention::answer_anyway(state, &room, &message, now, fx);
        }
        TimerPurpose::ObserveEnd => observe::end(state, now, fx),
        TimerPurpose::ExtractGuard { room, message } => {
            extract::on_guard(state, &room, &message, now, fx);
        }
        TimerPurpose::BackfillGuard { room } => backfill::on_guard(state, &room, now, fx),
        TimerPurpose::Nudge {
            proposal,
            which,
            user,
        } => decide::on_nudge(state, &proposal, which, user, now, fx),
        TimerPurpose::Close { proposal } => decide::on_close_timer(state, &proposal, now, fx),
        TimerPurpose::TimeGuard { room, message } => {
            decide::on_time_guard(state, &room, &message, now, fx);
        }
        TimerPurpose::Due { commitment, which } => {
            commitment::on_due(state, &commitment, which, now, fx);
        }
        TimerPurpose::Digest { room } => digest::on_digest(state, &room, now, fx),
        TimerPurpose::DayOf { room, entry } => digest::on_day_of(state, &room, &entry, now, fx),
        TimerPurpose::Repin { room } => pin::on_repin(state, &room, fx),
        TimerPurpose::Recur { id } => recur::on_recur(state, &id, now, fx),
        TimerPurpose::Release { action } => hold::on_timer(state, fx, &action, now),
        TimerPurpose::Retention => retention::on_timer(state, now, fx),
    }
}

fn on_action_result(
    state: &mut GroupState,
    id: &ActionId,
    ok: bool,
    message: Option<PlatformMessageId>,
    error: Option<String>,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if decide::poll_created(state, id, ok, message.clone(), now, fx) {
        if !ok {
            let why = error.as_deref().unwrap_or("no reason given");
            fx.push(Effect::Notice(format!("A poll failed to send: {why}")));
        }
        return;
    }
    let pending = state.pending_actions.remove(id);
    let dm = state.pending_dms.remove(id);
    if !ok {
        if let Some(FollowUp::StateMessage { room }) =
            pending.as_ref().and_then(|p| p.then.as_ref())
        {
            pin::forget(state, room, None);
        }
        backfill::on_fetch_failed(state, id, now, fx);
        setup::on_refused(state, id, now, fx);
        let what = if pending.is_some() {
            "A message"
        } else if dm.is_some() {
            "A DM"
        } else {
            "An action"
        };
        let why = error.as_deref().unwrap_or("no reason given");
        fx.push(Effect::Notice(format!("{what} failed to send: {why}")));
        return;
    }
    setup::on_delivered(state, id);
    let Some(message_id) = message else {
        return;
    };
    if let Some(pending) = pending {
        let follows = matches!(
            pending.then,
            Some(
                FollowUp::Ballot { .. } | FollowUp::TabLine { .. } | FollowUp::StateMessage { .. }
            )
        );
        if !follows {
            action::on_message_sent(state, id, &pending, &message_id, now);
        }
        let own = own_message(state, &pending.room, &message_id, pending.text, now);
        let room = state.room_mut(&pending.room);
        room.remember_own(message_id.clone());
        room.push(Message {
            reply_to: pending.reply_to,
            ..own
        });
        if let Some(trace) = pending.trace {
            room.push_trace(message_id.clone(), trace);
        }
        match pending.then {
            Some(FollowUp::OpenDeferral { deferral }) => {
                reactions::add(state, fx, &pending.room, &message_id, Meaning::Check);
                state.deferrals.insert(message_id, deferral);
            }
            Some(FollowUp::Ballot { proposal }) => {
                decide::ballot_posted(state, &proposal, &message_id, fx);
            }
            Some(FollowUp::TabLine { lines }) => {
                tab::line_posted(state, &pending.room, &lines, &message_id, fx);
            }
            Some(FollowUp::StateMessage { room }) => {
                pin::posted(state, &room, &message_id, now, fx);
            }
            None => {}
        }
    } else if let Some(dm) = dm {
        if let Recording::Line { action } = &dm.record {
            action::attach(state, action, &message_id, true);
        }
        if let Some(room_id) = state.dm_room_of(&dm.user) {
            action::on_dm_sent(state, id, &dm, &room_id, &message_id, now);
            let own = own_message(state, &room_id, &message_id, dm.text, now);
            let room = state.room_mut(&room_id);
            room.remember_own(message_id.clone());
            room.push(own);
            if let Some(trace) = dm.trace {
                room.push_trace(message_id.clone(), trace);
            }
        }
        match &dm.then {
            Some(DmFollowUp::Due { commitment }) => {
                commitment::nudge_posted(state, commitment, message_id.clone());
            }
            Some(DmFollowUp::Request { deferral }) => {
                state.deferrals.insert(message_id.clone(), deferral.clone());
            }
            Some(then) => tab::dm_posted(state, then, message_id.clone()),
            None => {}
        }
        if dm.feedback {
            observe::remember_target(state, message_id, dm.user);
        }
    }
}

/// A message the agent sent, as the room's ring holds it.
fn own_message(
    state: &GroupState,
    room: &PlatformRoomId,
    id: &PlatformMessageId,
    text: String,
    now: Instant,
) -> Message {
    let author = state.me_in(room).map_or_else(
        || PlatformUserId::new(state.agent_name.clone()),
        |m| m.id.clone(),
    );
    Message {
        id: id.clone(),
        room: room.clone(),
        author,
        text,
        at: now,
        reply_to: None,
        mentions: Vec::new(),
        mentions_me: false,
        attachments: Vec::new(),
        edited: false,
    }
}

fn on_reaction(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    emoji: &str,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    if state.is_me(user) {
        return;
    }
    let mark = decide::Mark {
        room,
        message,
        user,
        added: true,
    };
    if decide::on_reaction(state, mark, emoji, now, fx) {
        return;
    }
    if state.rooms.get(room).is_some_and(|r| r.is_dm()) {
        let check = reactions::means(state, room, emoji, Meaning::Check);
        if check && hold::on_dm_check(state, fx, message, user, now) {
            return;
        }
        if commitment::on_dm_reaction(state, room, message, user, emoji, now, fx)
            || tab::on_dm_reaction(state, room, message, user, emoji, now, fx)
        {
            return;
        }
    } else if tab::on_reaction(state, room, message, user, emoji, true, fx)
        || commitment::on_room_reaction(state, room, message, user, emoji, now, fx)
    {
        return;
    }
    if reactions::means(state, room, emoji, Meaning::Check) && state.deferrals.contains_key(message)
    {
        on_check(state, room, message, user, now, fx);
        return;
    }
    if observe::on_reaction(state, room, message, user, emoji, fx) {
        return;
    }
    if attention::on_question_reaction(state, room, message, user, emoji, now, fx) {
        return;
    }
    // A 🤫 from anyone, guests included, is a hard rule (DESIGN.md section 16).
    let own = state.rooms.get(room).is_some_and(|r| r.is_own(message));
    let shush = reactions::means(state, room, emoji, Meaning::Shush);
    if own && shush && state.is_human(room, user) {
        attention::shush(state, room, user, message, now, fx);
    }
}

/// A ✅ on a deferral: approved when it comes from a lead who may approve it.
fn on_check(
    state: &mut GroupState,
    room: &PlatformRoomId,
    message: &PlatformMessageId,
    user: &PlatformUserId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let Some(deferral) = state.deferrals.get(message) else {
        return;
    };
    let request = matches!(deferral.change, DeferredChange::Request { .. });
    // A flagged request is approved from the lead's DM, not the room.
    if &deferral.room != room && !request {
        return;
    }
    let role = resolve_role(state, &deferral.room, user);
    if role < Role::Lead || !permitted(role).covers(&deferral.change.needs()) {
        debug!(%user, "a ✅ from someone who cannot approve");
        return;
    }
    let Some(deferral) = state.deferrals.remove(message) else {
        return;
    };
    if request {
        // The other leads' copies of the same flag are spent.
        let asked = deferral.request.clone();
        state.deferrals.retain(|_, d| {
            !(d.request == asked && matches!(d.change, DeferredChange::Request { .. }))
        });
    }
    approve(state, deferral, user, now, fx);
}

/// Carries out a deferral as if the approving lead had asked, naming both.
fn approve(
    state: &mut GroupState,
    deferral: Deferral,
    approver: &PlatformUserId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let room = &deferral.room;
    let asker = state.display_name(room, &deferral.asked_by);
    let lead = state.display_name(room, approver);
    let credit = format!("Asked by {asker}, approved by {lead}.");
    let trace = Trace::new(
        format!(
            "{lead} approved {asker}'s {} with a ✅",
            quote(&deferral.request_text, 60)
        ),
        "a lead's ✅ carries out a member's request",
        now,
    );
    let reply_to = Some(deferral.request.clone());
    match deferral.change {
        DeferredChange::Leads { change } => {
            let line = leads::apply(state, room, &change);
            let text = format!("{line} {credit}");
            let note = Recording::Note {
                why: LEADS_NOTE.to_owned(),
            };
            send_as(state, fx, room, text, reply_to, now, trace, note);
            fx.push(Effect::Notice(format!("Leads: {line} {credit}")));
        }
        DeferredChange::Setting { change } => {
            let line = settings::apply(state, &change, now, fx);
            let text = format!("{line} {credit}");
            let note = Recording::Note {
                why: SETTING_NOTE.to_owned(),
            };
            send_as(state, fx, room, text, reply_to, now, trace, note);
            fx.push(Effect::Notice(format!("Setting: {line} {credit}")));
        }
        DeferredChange::Memory { change } => {
            let line = rights::apply(
                state,
                &change,
                &deferral.asked_by,
                &deferral.request,
                now,
                fx,
            );
            let text = format!("{line} {credit}");
            let note = Recording::Note {
                why: FORGOTTEN_NOTE.to_owned(),
            };
            send_as(state, fx, room, text, reply_to, now, trace, note);
            fx.push(Effect::Notice(format!("Memory: {line} {credit}")));
        }
        DeferredChange::Undo { ref action } => {
            undo::approved(state, fx, action, &deferral, approver, now, trace);
        }
        DeferredChange::Backfill => {
            let request = &deferral.request;
            let asker = &deferral.asked_by;
            backfill::start(state, room, asker, request, Some(&credit), now, fx);
        }
        DeferredChange::Request { text, room, from } => {
            take_up(
                state,
                &room,
                &from,
                &deferral.request,
                text,
                approver,
                now,
                fx,
            );
        }
        DeferredChange::Actions { actions } => {
            let origin = Origin {
                requested_by: Some(deferral.asked_by.clone()),
                approved_by: Some(approver.clone()),
                instruction: Some(deferral.request.clone()),
            };
            let actions = mail::screen(
                state,
                fx,
                room,
                &origin,
                &deferral.request_text,
                actions,
                now,
                &trace,
            );
            for action in actions {
                carry_out(state, fx, room, &origin, action, now, trace.clone());
            }
            fx.push(Effect::Notice(format!(
                "Carried out a deferred request. {credit}"
            )));
        }
    }
}

/// Runs a guest's request through the addressed path as the approving
/// lead's own words, in the room it was made in. The room keeps the
/// guest as the author of what they said.
#[allow(clippy::too_many_arguments)]
fn take_up(
    state: &mut GroupState,
    room: &PlatformRoomId,
    from: &PlatformUserId,
    request: &PlatformMessageId,
    text: String,
    approver: &PlatformUserId,
    now: Instant,
    fx: &mut Vec<Effect>,
) {
    let held = state
        .rooms
        .get(room)
        .and_then(|r| r.messages.iter().find(|m| &m.id == request))
        .cloned();
    let held = match held {
        Some(m) => m,
        None => {
            let original = Message {
                id: request.clone(),
                room: room.clone(),
                author: from.clone(),
                text: text.clone(),
                at: now,
                reply_to: None,
                mentions: Vec::new(),
                mentions_me: false,
                attachments: Vec::new(),
                edited: false,
            };
            state.room_mut(room).messages.push_back(original.clone());
            original
        }
    };
    let message = Message {
        author: approver.clone(),
        text,
        ..held
    };
    on_addressed(state, &message, now, fx);
}

fn apply_command(state: &mut GroupState, command: Command, now: Instant, fx: &mut Vec<Effect>) {
    let notice = match command {
        Command::SetKind { kind } => {
            settings::set_kind(state, kind);
            format!(
                "Kind set to {}; profile reset to its defaults.",
                settings::kind_name(kind)
            )
        }
        Command::SetLeads { users } => {
            state.leads = users.into_iter().collect();
            let names: Vec<String> = state.leads.iter().map(|u| state.name_of(u)).collect();
            format!(
                "Leads list set to: {}.",
                if names.is_empty() {
                    "nobody".to_owned()
                } else {
                    names.join(", ")
                }
            )
        }
        Command::AddLead { user } => {
            let name = state.name_of(&user);
            state.leads.insert(user);
            format!("{name} added to the leads list.")
        }
        Command::RemoveLead { user } => {
            let name = state.name_of(&user);
            state.leads.remove(&user);
            format!("{name} removed from the leads list.")
        }
        Command::SetHosts { users } => {
            state.hosts = users.into_iter().collect();
            let names: Vec<String> = state.hosts.iter().map(|u| state.name_of(u)).collect();
            format!(
                "Hosts set to: {}.",
                if names.is_empty() {
                    "nobody".to_owned()
                } else {
                    names.join(", ")
                }
            )
        }
        Command::SetPersona { persona } => {
            state.persona = persona;
            state.persona_name = None;
            "Persona updated.".to_owned()
        }
        Command::UsePersona { name } => {
            let line = crate::persona::use_starter(state, &name);
            format!("Persona: {line}")
        }
        Command::SetLegalHold { on } => retention::set_legal_hold(state, on, now, fx),
        Command::SetAgentName { name, by_host } => {
            let line = settings::set_agent_name(state, &name);
            if by_host && !name.trim().is_empty() {
                state.name_from_host = true;
            }
            line
        }
        Command::SetTimezone { zone: name } => {
            if zone::is_known(&name) {
                state.timezone = name;
                format!("Time zone set to {}.", state.timezone)
            } else {
                format!("Unknown time zone {name:?}; kept {}.", state.timezone)
            }
        }
        Command::SetModelLabel { label } => {
            state.model_label = label.trim().to_owned();
            format!("Model label set to {}.", state.model_label)
        }
        Command::SetProfile { profile } => {
            state.profile = profile;
            "Profile updated.".to_owned()
        }
        Command::ChangeSetting { change } => {
            let line = settings::apply(state, &change, now, fx);
            format!("Setting: {line}")
        }
        Command::SetDecisionRule { rule } => {
            state.decision_rule = rule;
            format!(
                "New proposals close on {}.",
                decide::default_rule(state).name()
            )
        }
        Command::SetMemberZone { user, zone: name } => {
            let who = state.name_of(&user);
            match name.as_deref().map(zone::canonical) {
                None => {
                    state.zones.remove(&user);
                    format!("{who} now uses the group's zone.")
                }
                Some(Some(name)) => {
                    let line = format!("{who}'s zone set to {name}.");
                    state.zones.insert(
                        user,
                        Zone {
                            name,
                            confidence: zone::STATED_CONFIDENCE,
                        },
                    );
                    line
                }
                Some(None) => format!("Unknown time zone for {who}; nothing changed."),
            }
        }
        Command::SetCurrency { code } => {
            let code = code.trim().to_uppercase();
            if code.len() == 3 && code.chars().all(|c| c.is_ascii_alphabetic()) {
                state.currency = code;
                format!("The tab is in {} now.", state.currency)
            } else {
                format!("Unknown currency {code:?}; kept {}.", state.currency)
            }
        }
        Command::SetFeedBase { url } => {
            let url = url.trim().trim_end_matches('/').trim_end_matches(".ics");
            if url.is_empty() {
                state.feed_base = None;
                "Calendar feed links are off.".to_owned()
            } else {
                state.feed_base = Some(url.to_owned());
                format!("Calendar feeds at {url}.ics, and one link per member.")
            }
        }
    };
    fx.push(Effect::Notice(notice));
}

fn platform_name(platform: Platform) -> String {
    format!("{platform:?}").to_lowercase()
}

#[cfg(test)]
mod mention_tests {
    use super::render_mentions;
    use crate::state::GroupState;
    use ours_types::{GroupId, Kind, Member, Platform, PlatformRole, Room, RoomKind};

    fn member(id: &str, name: &str) -> Member {
        Member {
            id: id.into(),
            display_name: name.to_owned(),
            platform_role: PlatformRole::Regular,
            is_bot: false,
            timezone: None,
        }
    }

    #[test]
    fn mentions_read_as_names() {
        let mut state = GroupState::new(GroupId::new("g"), Kind::Friends);
        state.agent_name = "Sarah".to_owned();
        state.me.push(crate::state::Me {
            platform: Platform::Sim,
            id: "9".into(),
            name: "Sarah".into(),
            capabilities: Default::default(),
        });
        let room_id = ours_types::PlatformRoomId::new("r");
        let room = state.rooms.entry(room_id.clone()).or_default();
        room.info = Some(Room {
            platform: Platform::Sim,
            id: room_id.clone(),
            name: None,
            kind: RoomKind::Channel,
            parent: None,
            container: None,
        });
        room.members.insert("1".into(), member("1", "Sam"));
        assert_eq!(render_mentions(&state, &room_id, "hey <@9>"), "hey @Sarah");
        assert_eq!(
            render_mentions(&state, &room_id, "<@!1> and <@9>, dinner?"),
            "@Sam and @Sarah, dinner?"
        );
        assert_eq!(
            render_mentions(&state, &room_id, "<@404> unknown"),
            "<@404> unknown"
        );
        assert_eq!(render_mentions(&state, &room_id, "a <@ b"), "a <@ b");
        assert_eq!(render_mentions(&state, &room_id, "plain"), "plain");
    }
}
