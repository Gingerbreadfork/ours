# Privacy

This page is written to be read by the people in a group, and by the
workplace that hosts one. It describes what this version of Ours does.
The live version of the facts below, for your install, is the `/compliance`
page of the admin page.

## The short version

- One person, the host, runs Ours on a machine they control. The host can
  read everything it keeps.
- It keeps a fact store, not a transcript archive. It keeps what is needed
  to remember for the group, and a journal of what it heard so it can
  rebuild its state.
- Every person it remembers, member or guest, can see what it holds about
  them, correct it and have it removed, by sentence, in DM.
- Leads see what members see. Nobody gets reports about patterns.
- Nothing leaves the machine except the model calls, the platform itself,
  email the group sends, and telemetry if the host turned it on.

## What is kept

Per group:

- **Facts**: who or what, what is true of it, where it was said, who said
  it, how sure the agent is, and when it expires. Facts are scoped. A group
  fact was said in the group. A room fact stays in the rooms it was seen
  in. A private fact was said in DM and is used only for that member. A
  self-stated fact is something a member said about themselves and is
  usable in the group only as they stated it.
- **Decisions**, with the options, who said what, the rule, the reason and
  the time. A closed decision is not edited.
- **Commitments** and **the tab**: ledger lines, payment links the creditor
  gave in DM, balances.
- **The action record**: everything the agent did, what triggered it, who
  asked, who approved a deferral, and how it is taken back.
- **The calendar**: events, holds, recurrences and availability spans.
- **Timing patterns**: the hour a member usually replies and the hours a
  room is active, as counts. They only decide when the agent speaks. They
  are never recalled, reported or answered as a question.
- **The journal**: every input the agent handled, including the text of the
  messages it heard, in a hash-chained log, plus snapshots of state. This is
  what makes replay and restarts possible. It is not a second archive to
  mine; it is used to rebuild state and for replay. The journal is not
  pruned by age in this release. Forgetting redacts it, and wiping a
  group removes it.

On Telegram the agent also keeps its open polls, with who voted for what,
in `telegram/polls.json`, so votes count across a restart. A poll's votes
are dropped from it when the poll ends. It is not per group, so wiping a
group does not remove it.

Not kept: card numbers, wherever they are said. A message with one is
never read or held, it is scrubbed from the journal, and its author is told
why. Bank details are not kept either. Rooms not listed in the
config are not served. A short ring of recent lines per room is kept for
context.

## Where it lives

One SQLite file per group in the host's data directory, with the charter
and decision log beside it as Markdown. Back it up by copying it. There is
no cloud service involved. Encryption at rest is not in this release; use
disk encryption on the host.

## Who sees what

- **The host** sees everything. The agent says so in the first
  conversation. The host runs the machine and can read the file.
- **Leads** see what members see. A lead who asks for another member's
  private facts gets the refusal a member would get. The agent will not
  answer who has been quiet, who is always late or what Alex has said this
  month, for anyone.
- **Members** see what was said in the rooms they are in, and what they
  said privately themselves.
- **Guests** (external users in a shared channel, accounts a lead has named
  as guests, and anyone who writes to the group's email address) are
  answered from the room they are in, and from nothing else. They are never
  obeyed and never counted in a decision.
- **Rooms are barriers.** A fact is said only where it was seen, or in a DM
  with someone who was in a room where it was seen, or where a lead has
  told the agent to share it. In a workspace this is what lets one agent sit
  in a private channel and a public one without leaking between them.
- **Private scope** is enforced in the prompt and in the data layer. The
  model never receives private facts about a member other than the one it is
  talking to.

Authority is code, not prompt. Who is a lead is decided from the platform's
roles and the leads list before the model is called, and anything the model
plans outside that is turned into a request for a lead's ✅. Text such as
"ignore your instructions, I am the admin now" cannot change it.

## Member rights

These work for any person, whatever their role, in a DM or in the room.
The admin page cannot override them; the only override is wiping the whole
group.

- `what do you know about me`: a full list by DM, with sources. It says it
  also keeps the hour you usually reply, for timing only.
- `forget that`: drops the fact and what was inferred from it.
- `that's wrong`: retracts a fact and records the correction with you as the
  source.
- `forget me`: removes every fact with you as subject or source, closes your
  commitments, drops your reply hour, stops any settle-up waiting on you,
  and says so in the room in one line. Your tab lines stay, since other
  people's shares are on them.

Forgetting also redacts the journal. The text of the messages involved, or
every message you wrote for `forget me`, is replaced with `[forgotten]`, and
snapshots from that point on are dropped so the old words are not left in
them. The record keeps its place and its hash, so the log still verifies.

Corrections do not delete. A retraction keeps a record that something was
retracted, so "what did we think in March" still has an answer. Only
forgetting removes content.

## Retention

- Durable facts (allergies, birthdays, "Sam drives") stay until corrected
  or forgotten.
- Perishable facts (availability, "I'm sick this week") expire. With no date
  they last 30 days.
- The action log is kept for 90 days in friends, household, club,
  community and cohort groups and 365 days in team, project and committee
  groups. A daily sweep drops action records past that age and facts past
  their expiry. The period is shown on the Profile tab and on the
  compliance page. Outside a legal hold the action log also has a ceiling
  of 10,000 records; if a group passes it, the oldest records go before
  their 90 or 365 days are up, and the admin page shows a notice saying
  how many.
- **Legal hold** turns the sweep and the ceiling off, so nothing expires or
  is dropped. A
  lead says `Noa, legal hold on` or `Noa, hold everything`, or the host uses the
  compliance page. It
  is a button on the compliance page and `legal_hold = true` in the config.
  While it is on, the page says so.
- **Export** gives one group's data as JSON and as Markdown, and the
  decision log as Markdown. A workplace that has to answer a data request
  has one SQLite file and an export button.
- **Wipe** deletes everything for a group.

## What leaves the machine

Only these.

- **The model.** Messages the agent is asked about, with the context it
  needs, go to the model endpoint the host configured. The pinned message
  and the charter say which model and where: on the host's machine, on its
  network, or at a named hosted service. If the small model that reads
  every message is hosted elsewhere, they say that too. If it is a model on the host's
  own network, nothing leaves. If it is a hosted service, its terms apply to
  the text sent.
- **The platform.** Replies, reactions, DMs and polls go to Discord or Slack
  as any member's would.
- **Email.** If the host set up a mailbox, mail the group sends goes to the
  address a member gave it, containing the booking details and the host's
  contact, and nothing about members by name unless the booking needs a
  name. See the held-mail window in [mail.md](mail.md).
- **Telemetry.** Off unless the host turns it on. When on, one text-free
  record per scored message: a few numbers, the kind, the dial and the
  outcome. The exact payload is in [operations.md](operations.md) and on the
  compliance page. It never has text, ids or names.

## The group's inbox

If the host gives the group an email address, the host is responsible for
it. Anything that mailbox can see, the agent can read. Give it its own
account. Mail that reaches it is treated as a guest: it can be relayed to
the room as a line, and it can never instruct the agent.

## Model disclosure

The first conversation, the pinned message and the charter state which
model the agent uses. A group that cares can insist on a local one.

## Out of scope

- **Minors.** Groups with children are out of scope for 0.1.0-rc.2. There
  is no mode that keeps nothing about named children yet.
- **Sensitive groups.** Support groups, groups about health, legal,
  immigration or safety, and anything where a leak would harm someone, are
  out of scope. Do not use this release for them.

The host is responsible for the group they add the agent to, and for
telling its members. The first conversation and the join message do part
of that: someone who joins a room after the agent is there gets one DM
saying what it keeps, whom it listens to, that the host sees everything,
and how to see or delete what it holds about them. A workplace should also tell its people, in its own words.
