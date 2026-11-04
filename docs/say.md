# What to say to Noa

Noa is the agent's default name. Everything here works with whatever name
the group gave it.

## Addressing

The agent answers when you mention it, start a message with its name
(`Noa, ...`, `Noa: ...`, `@Noa ...`), reply to one of its messages, or DM it.
Matching is case-insensitive and the name must stand alone, so `Q&A is at
four` is not addressed to it. Anyone can address it, including guests.

The sentences below are handled by rule, with no model call. They are
forgiving about case, curly quotes, a trailing `please` and the like.
Anything else addressed to the agent goes to the model, which can answer,
react or write an email, inside what the speaker's role allows.

A 👀 appears on your message while the agent is working on it.

## Who may say what

The roles come from the platform and the leads list, never from the
words. See [admin.md](admin.md) and the table in
[personas.md](personas.md) for who counts as a lead.

- Host: runs the agent. Always obeyed.
- Lead: obeyed.
- Member: answered, and obeyed about their own affairs. Anything that
  changes the group gets `Happy to. Needs a lead's ✅.` and any lead's ✅
  on that reply approves it as if the lead had asked. With no ✅ in a day
  the request lapses without a word.
- Guest: answered from the room they are in; never obeyed. A guest's
  request for a change gets a one-line decline.

In the tables, "anyone" includes guests, "member" means member or above,
and "lead" means lead or host.

## Questions

| Say | Who | What it does |
|---|---|---|
| `Noa, who do you listen to?` (also `who are the leads`) | anyone | Names the leads and hosts. |
| `Noa, settings` (also `charter`, `show settings`) | anyone | Six lines: kind, model, leads, budget left today, quiet and working hours, dial, whether it is still observing. |
| `why` (best as a reply to one of its messages) | anyone | Explains the trigger, the facts used and the rule applied. |
| `Noa, what did you send` | anyone | The room's emails, newest first, five at most, with their state. |
| `Noa, what's open` | anyone | Open proposals. |
| `Noa, where are we` (also `tally`, `status`, `how's the vote going`) | anyone | The tally of what is open in the room. |
| `Noa, why did we pick Thursday` (also `what did we decide about the venue`, `what did we decide`) | anyone | The closed decision with its options, who said what, the rule and the reason. |
| `Noa, calendar link` (also `ics`, `calendar feed`) | anyone | The calendar feed link for the room. |

Ordinary questions ("Noa, what was that bar in March?") go to the model with
the group's memory, within what the asker may see.

## Memory and rights

Every person the agent remembers can see, correct and remove what it holds
about them, whatever their role. See [privacy.md](privacy.md).

| Say | Who | What it does |
|---|---|---|
| `Noa, what do you know about me` (also `what do you remember about me`, `my data`) | anyone | Everything it holds about you, with sources, by DM. It says that it also keeps the hour you usually reply, for timing only. |
| `Noa, forget me` (also `delete my data`, `forget everything about me`) | anyone | Removes every fact with you as subject or source, closes your commitments, drops your reply hour and stops settle-ups waiting on you. One line in the room, no detail. |
| `forget that` (reply to a message, or reply to one of its own) | anyone for facts about themselves; a lead for others | Drops the fact from that message, or the facts the agent's reply cited. A member's request about someone else's fact waits for a lead's ✅. |
| `that's wrong, it's Lisbon` (also `wrong`, `that's not right`) | same | Retracts the fact and records the correction with you as the source. Said alone, it means the agent's last message only if that was in the last ten minutes or is the latest in the room. |
| `remember that Sam is vegetarian` (also `note that`, `keep in mind that`, `make a note that`) | same | Keeps the fact, as durable. In a reply, remembers the message you replied to. |
| `just for this week` (reply to a fact) | same | Makes the fact expire in a week. |
| `Noa, backfill` (also `catch up`, `read the last 90 days`, `read the history`) | lead | Reads the room's last 90 days and keeps what is worth keeping. One at a time per room, and one `undo` takes all of it back. A member's request waits for a lead's ✅. |
| `Noa, my timezone is Europe/Lisbon` (also `I'm in Lisbon this month`) | anyone | Sets your zone. Times to you are then in your clock. |

Things a person says about themselves in plain words are noticed and kept
by the small model when the classifier is enabled: "I'm off dairy now",
"I'm away 12 to 16", "I can't do Thursday". Nothing needs a command.

## Decisions and polls

The close rule decides who wins: majority of active members (friends,
household, club, community, cohort), or a lead decides (team, project,
committee). An active member has posted or reacted in the last 30 days.
Reactions are votes. On Discord a choice poll is a native poll; on Slack it
is a numbered reaction ballot.

| Say | Who | What it does |
|---|---|---|
| `Noa, poll thursday vs friday` (also `poll where: thai or pizza`, or just `Noa, thurs or fri for dinner?`) | member | Opens a poll on two to ten options, with a deadline of 24 hours, or half the time to the soonest dated option, never under two. The topic comes before a colon. |
| `Noa, who's in for Saturday` | member | Opens an attendance poll with one option and a 👍 ballot. |
| `re-vote` (also `vote again`) | member; lead if the old one was lead decides | Reopens a closed proposal as a copy. If one is still open it says so. |
| `Noa, go with Acme, the date matters` (also `let's go with`, `decision is`, `pick`) | lead | Closes the open proposal on that option, with the reason if given. |
| `Noa, lead decides on this one` | lead | Changes how the open proposal closes. |
| `Noa, majority of voters` / `majority of active members` / `unanimous` / `whoever is coming decides` | lead | Without `on this one` it changes the rule for new proposals. |
| `Noa, put it to a vote` | lead | Majority of voters, for the open proposal. |
| `Noa, deadline tomorrow 6pm` (also `close the poll`, `voting closes friday`) | lead | Moves the deadline. If the words are unclear it asks the small model, and if that fails it says to try something like `friday 6pm`. |

Members get one DM nudge at the halfway point and one near the deadline,
never a third. A late `I can't do Thursday` after close does not reopen
anything; it adjusts the headcount and offers a re-vote. An unanswered
proposal expires quietly after two deadlines.

Polls and attendance need a room; in a DM it tells you to ask in the
room.

## Commitments

Said in plain words, no command: `I'll have the draft by Thursday`, `I'll
bring dessert`. The agent reacts 📌 on the message. The person can ❌ it to
drop it; nobody else can. It nudges by DM before it is due and when it is
due, at the hour that person usually replies, and the reply is `done`,
`slipping` with a new date, or `drop it`.

| Say | Who | What it does |
|---|---|---|
| `Noa, what's outstanding` (also `commitments`, `who's doing what`) | anyone | Open commitments in the room. |
| `Noa, what's due this week` | anyone | What is due in the next seven days. |
| `Noa, my commitments` (also `what did I commit to`) | anyone | Yours. |
| `Noa, what did Alex commit to` | anyone | One person's. |

## The tab

The agent keeps the tab and never moves money.

| Say | Who | What it does |
|---|---|---|
| `I got the wine, 38` | member, plain words | Adds a ledger line, split among the people there. |
| `Noa, what's the tab` (also `balances`, `who owes what`) | anyone | Net balance per person. |
| `Noa, what do I owe` (also `my balance`) | anyone | Yours. |
| `not me` (also `count me out`) / `double for me` | member | Adjusts your share of the latest line. |
| `Noa, settle up` | member | DMs each debtor an amount and the creditor's payment link, minimum transfers, for the tab where you ask: a room settles its own lines and those entered by DM; from a DM, every room you are in. |
| `my payment link is ...` | anyone, in a DM only | Keeps the link as an opaque string. Said in a room, it is not kept. A card number said anywhere is never kept. |
| `paid` | in a DM | Marks your debt paid; the creditor confirms with a reaction. |

## Time and calendar

| Say | Who | What it does |
|---|---|---|
| `Noa, calendar link` | anyone | The feed for the room. A member's own feed shows only what they are in. |
| `Noa, monthly dinner on the first Thursday` (also `weekly planning every Monday 10am`, `subs due on the first`) | member, in a room | Creates a recurrence on the group calendar. Saying it again for the same thing changes it instead of adding a second one. |
| `Noa, pause the dinner` / `resume the dinner` | member, in a room | Pauses or resumes a recurrence. A recurrence that gets no response twice is paused with one line. |
| `Noa, my timezone is ...` | anyone | See Memory and rights. |

Every time the agent hears is resolved to an instant in the speaker's zone
and the date it chose is echoed back, such as `Thursday the 14th`. A wrong
guess costs one correction.

## Setup

| Say | Who | What happens |
|---|---|---|
| `setup` (by DM; also `start setup`, `redo setup`, `/setup`) | the person being asked, or a lead or host | Repeats the current question of the first conversation; a lead or host takes it over once the person asked has had three days, or at once if nobody was ever asked. |

## Settings only leads can change

A member's version of any of these waits for a lead's ✅. A guest's is
declined. A setting changes back by saying it the other way.

| Say | What it does |
|---|---|
| `Noa, Sam and Jo are leads` (also `Sam is a lead`, `make Sam a lead`) | Adds named leads. |
| `Noa, Sam is not a lead` | Removes them. |
| `Noa, leads: Sam, Jo` / `leads are everyone` | Replaces the named list, or makes everyone a lead. |
| `Noa, quiet after 10` / `quiet hours 10pm to 7am` / `no quiet hours` | Quiet hours in the group's zone. |
| `Noa, working hours 9 to 6` / `no working hours` | Working hours, Monday to Friday. They may run past midnight, such as 6pm to 2am. |
| `Noa, dial it down` / `dial it up` / `dial to 2` | The persona dial, 0 to 3. |
| `Noa, be the clerk` / `sound like the host` / `use the keeper persona` / `switch to the wag voice` | Pick a starter persona by name; `use the plain default` goes back. A dial 2 starter leaves the dial where it is. |
| `Noa, who can you be?` | Anyone: lists the six starter personas and which is in use. |
| `Noa, legal hold on` / `hold everything` | Nothing expires until it is lifted; the charter says since when. |
| `Noa, legal hold off` / `stop holding everything` | Lifts it. |
| `Noa, you have a bit: the negroni` / `drop the bit about the negroni` | A running bit. |
| `Noa, this is a team` (also `kind: club`, `set kind to household`) | Resets the profile to that kind's defaults. |
| `Noa, digest at 4pm` / `no digest` / `digest on` | The daily digest. |
| `Noa, day-of summary on` / `off` | The day-of summary. |
| `Noa, go` (also `go ahead`, `start talking`) | Ends observe mode. |
| `Noa, observe` / `watch 3 more days` / `watch another day` | Observes again, for the kind's period or the days given (one to 60). |

## Undo

`undo` and `cancel that` are single words that always work, from anyone
the rule allows.

| Say | What it does |
|---|---|
| `Noa, undo` (also `undo that`, `cancel it`) | Reverts the agent's latest action in this thread, or the one you replied to. |
| `Noa, undo the email` (also `booking`, `poll`, `message`, `reaction`, `fact`, `commitment`, `tab`, `calendar`, `pin`) | Names what to undo. |
| `Noa, don't send it` | Cancels a held email. |

A lead can undo anything group-scoped. A member can undo what they asked
for or what is about them: their own facts, commitments and tab lines. A
member's undo of someone else's action waits for a lead's ✅. A guest is
declined. The undo line says who undid it. A decision stands: `re-vote`
opens it again. What is forgotten stays forgotten; `remember that` keeps
it again.

## Reactions the agent uses

| Reaction | Meaning |
|---|---|
| 👀 | Working on your message. It goes when the reply lands. |
| 🙋 | It has the answer to a question nobody answered. React back, or wait for the window to double, and it posts. |
| ✅ on its deferral | A lead approves the member's request. |
| 📌 | It recorded a commitment from that message. |
| 1️⃣ 2️⃣ 3️⃣ ... | The ballot. Reacting is voting. |
| 🤫 | From anyone: the agent stands down for the day, no chime-ins or unprompted lines until local midnight. |
| 👍 / 👎 on a "would have" DM | Feedback during observe mode, counted for the leads. |

On Telegram, which allows only a fixed set of reactions, the agent uses
stand-ins and reads them back the same way: 👍 approves, 😴 stands it down,
🫡 means it has the answer, ✍ a promise noted, 🙈 takes you out of a split,
🤝 counts you twice, 👎 drops a promise. The group's charter lists them.
