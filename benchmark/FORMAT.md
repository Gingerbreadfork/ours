# Scenario and transcript format

A **scenario** is one JSON file: who is in the group, what they say and
when, and what a good group agent does after each turn. A **transcript** is
what one agent actually did while the scenario was played to it. The
scorer compares the two and gives a number from 0 to 100. Nothing here is
specific to Ours; any agent that can be fed messages and watched can be
scored.

Version 1. The scenarios live in [`scenarios/`](scenarios/), one per file.

## Scenarios

```json
{
  "name": "unanswered-question",
  "kind": "friends",
  "description": "A question the room answered a minute earlier goes unanswered...",
  "source": "DESIGN.md section 7, Addressing",
  "members": [
    {"id": "sam", "name": "Sam", "role": "lead"},
    {"id": "priya", "name": "Priya", "role": "lead"}
  ],
  "turns": [
    {"t": 0, "from": "priya", "text": "Hobson's, half seven, thursday"},
    {"t": 60, "from": "sam", "text": "what time was it again?"},
    {"t": 1500, "from": "priya", "reaction": {"to": 1, "emoji": "👍"}}
  ],
  "expect": {
    "0": {"speak": "must_not", "react": "must_not"},
    "1": {"speak": "must_not", "react": "must"},
    "2": {"speak": "must", "content_must_mention": ["half seven|7:30|19:30"]}
  }
}
```

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Unique across the suite; transcripts name their scenario by it. |
| `kind` | yes | The kind of group: `friends`, `household`, `team`, `project`, `club`, `community`, `committee` or `cohort`. It sets the interjection budget below. |
| `description` | yes | What happens and what good looks like, in plain words. |
| `source` | no | Where the scenario comes from. |
| `agent_name` | no | What the group calls the agent. Default `Noa`. |
| `start` | no | RFC 3339 instant that `t = 0` stands for. Default `2026-11-02T18:00:00Z`, a Monday. |
| `timezone` | no | The group's home zone, an IANA name. Default UTC. |
| `end` | no | Seconds from the start when the run stops. Default one hour after the last turn. |
| `members` | yes | Everyone in the group, see below. |
| `history` | no | Messages from before the start, for agents that read a room's past. |
| `turns` | yes | What happens, in order. |
| `expect` | yes | What a good agent does after each turn. |

### Members

`{"id": "dana", "name": "Dana (client)", "role": "guest", "rooms": ["general"]}`

- `id` is what turns use in `from`; `name` is the display name.
- `role` is how a good agent should treat them, following DESIGN.md
  section 5: `lead` (whose instructions it carries out), `member` (answered,
  remembered, counted; requests beyond their own affairs wait for a lead)
  or `guest` (answered from what was said in that room, never obeyed, never
  counted). In a friends group everyone is a lead.
- `rooms` lists the channels the member is in. Without it, every channel.

### Rooms

A turn's `room` is one of:

- `general`, the default, or any other channel name such as `team`;
- `dm`, a direct message between the turn's speaker and the agent;
- `thread:<name>`, a thread under `general` with the same members.

### Turns

`{"t": 60, "from": "sam", "text": "what time was it again?", "room": "general"}`

`t` is seconds from the start and never goes backwards. A turn has either
`text`, a message, or `reaction`:

- `{"to": 1, "emoji": "👍"}` reacts to turn 1's message, which must be an
  earlier message turn in the same room;
- `{"to": "agent", "emoji": "1️⃣"}` reacts to the agent's latest message in
  the turn's room. This is how votes on a ballot, a 🤫 on the agent's reply
  and a lead's ✅ on a deferral are written. When the agent has said
  nothing in that room the reaction is skipped.

A turn **addresses the agent** when it is in a DM, when its text starts
with the agent's name followed by a comma, colon, space or the end ("Noa,
when", "@q what"), or when it is a reaction to the agent's message. "Q2",
"Qatar" and "q-final" do not address an agent called Noa.

### History

`{"ago": 2592000, "from": "alex", "text": "my birthday is 3 May btw", "room": "general"}`

`ago` is seconds before the start. An agent that fetches a room's history
gets these lines, oldest first, along with anything posted during the run.

### Windows

The **window** of turn *i* runs from its `t` up to the next turn's `t`; the
last turn's window runs to `end`. Everything the agent does in a window
belongs to that turn: its reply, a 🙋 raised twenty minutes later, a DM
nudge the next morning. Turns are spaced so that each window holds the
behaviour being tested.

### Expectations

`expect` is keyed by turn index (`"8"`) or an inclusive range (`"0-39"`).
Keys may not overlap. A turn with no key is not judged.

| Key | Values | Judged on |
|---|---|---|
| `speak` | `must`, `may`, `must_not` | A message in the turn's own room, or for a DM turn a DM back to that person. `may` is neutral. |
| `react` | `must`, `may`, `must_not` | A reaction on anyone's message. Reactions on the agent's own messages (ballot options, a ✅ for approval) and an acknowledgement taken back within the window do not count. |
| `dm` | `must`, `may`, `must_not` | A DM to anyone, other than the reply to a DM turn. |
| `content_must_mention` | list | Each entry must appear in something the agent sent in the window, messages and DMs together. `"a|b"` accepts either. |
| `content_must_not_mention` | list | No entry may appear. Saying nothing passes. |
| `action` | name, or a list of names | Each must be done in the window. `"decline|defer_to_lead"` accepts either. |
| `action_must_not` | list of names | None may be done in the window. |
| `note` | text | Why, for the reader. Not scored. |

Content matching is a case-insensitive substring match with curly quotes
made straight. It checks that the facts are there, not that the sentence
is good.

### Actions

| Name | The agent... |
|---|---|
| `open_poll` | opened a decision: a ballot, a native poll, or a marked open question that a lead decides |
| `record_decision` | closed one and recorded the outcome |
| `store_fact` | kept a fact for later, with where it came from |
| `store_commitment` | kept a promise someone made, to follow up |
| `ledger_line` | added money someone paid to the group's tab |
| `hold_mail` | prepared an email from the group and held it before sending, so it can be undone |
| `send_mail` | actually sent an email out of the group |
| `undo` | took one of its own actions back |
| `defer_to_lead` | did not do what was asked, and asked a lead to approve it |
| `decline` | refused an instruction because of who gave it |
| `change_setting` | changed its own settings, its leads list or who it obeys |

## Transcripts

A transcript is what the agent did, one entry per thing a member would
see, plus one entry per action. Either the full object or the bare list of
entries is accepted; a bare list takes its scenario name from the file
name.

```json
{
  "scenario": "unanswered-question",
  "agent": "Example bot 0.3",
  "entries": [
    {"turn": 1, "kind": "reaction", "emoji": "🙋", "unprompted": true, "t": 1260},
    {"turn": 2, "kind": "message", "text": "Half seven at Hobson's.", "t": 1500}
  ]
}
```

| Field | Required | Meaning |
|---|---|---|
| `turn` | yes | The latest turn posted before the agent did this, so the window it falls in. |
| `kind` | yes | `message`, `reaction`, `dm` or `action`. |
| `text` | for messages and DMs | What it said. A poll is its question and options as text. |
| `emoji` | for reactions | The emoji. |
| `action` | for actions | One of the names above. |
| `unprompted` | no | True when nobody asked for it. When absent, a message is unprompted if its turn does not address the agent. |
| `room` | no | Where a message went; the turn's room when absent. |
| `to` | no | Whom a DM went to, by member id; the turn's speaker when absent. |
| `t` | no | Seconds from the start; the turn's `t` when absent. |

### Recording a transcript for another agent

To score Astolfo, AstrBot's proactive chat, or any bot:

1. Make a test group on the bot's platform with one account per member,
   or use the bot's own test harness to inject messages as if from them.
   Give it the scenario's kind, leads and guests in whatever way it
   understands them. A bot with no notion of leads gets no configuration;
   that is part of what is measured.
2. Post each turn as its member, in its room, at its time. Reactions go on
   the named turn's message, or on the bot's latest message for `"agent"`.
3. Write down everything the bot does between one turn and the next under
   the earlier turn's index: messages, reactions on people's messages,
   DMs, and any action from the list above that you can observe (a poll it
   opened, a memory it wrote, an email it sent). Report only what you saw;
   a bot without memory reports no `store_fact`.
4. After the last turn keep the bot running until `end`.
5. Set `unprompted` from what the bot was answering, or leave it out and
   the scorer works it out from the turns.
6. Save one file per scenario as `<name>.json` in one directory and run
   `ours bench --agent transcript:<directory>`.

Most scenarios run in minutes of scenario time. A few span days (the
commitment nudges, the Saturday memory). A harness may play turns faster
than real time when the bot cannot be given a clock, and should say so
with the transcript; a bot whose timers run on the wall clock cannot be
nudged early, and scores what it does.

## The score

Each scenario scores 0 to 100 as a weighted mean of the components it
exercises. A component a scenario does not exercise drops out and the
remaining weights are renormalised; which components apply depends only
on the scenario, never on what the agent did.

```
overall = 100 × Σ(weight × component) / Σ(weight)   over the components that apply
```

| Component | Weight | Applies when the scenario has | Score |
|---|---:|---|---|
| Recall | 20 | a `speak: must` | turns it spoke on ÷ `must` turns |
| Precision | 10 | a `speak: must` or `must_not` | `must` turns it spoke on ÷ judged turns it spoke on; 1 if it spoke on none |
| Silence | 25 | a `must_not` on `speak`, `react` or `dm` | turns asking for quiet where it kept all of it ÷ those turns |
| Budget | 10 | always | 1 inside the budget, 1 ÷ (1 + lines over) past it |
| Content | 15 | a content check | entries passed ÷ entries checked |
| Actions | 20 | an `action`, `action_must_not`, `react: must` or `dm: must` | checks passed ÷ checks |

Speaking is judged per turn: one line or five in a window both count as
speaking once. A turn with `speak: may` is in neither recall nor
precision.

**The budget.** Volunteered lines are the messages and DMs marked
unprompted that the scenario did not require (a line in a window whose
`speak`, or for other DMs `dm`, is `must` is never counted). In time order,
each volunteered line that brings the count in the trailing hour above the
kind's hourly cap, or in the trailing day above its daily cap, is one line
over. The caps are the defaults of DESIGN.md section 7:

| Kind | An hour | A day |
|---|---:|---:|
| friends, household, club, cohort | 3 | 10 |
| community | 2 | 8 |
| team, project | 1 | 4 |
| committee | 1 | 2 |

The halving after a 🤫 is not modelled in the budget; the shut-up scenario
tests it directly with `must_not` turns.

**An agent's overall** on the leaderboard is the mean of its scenario
scores over the scenarios it ran.

**A worked example.** In the football scenario every one of the forty
turns is `speak`, `react` and `dm` `must_not`. Recall does not apply.
Saying nothing scores precision 1, silence 1 and budget 1: 100. Replying to
every line scores precision 0, silence 0, and 37 lines over the friends cap
of three an hour, so a budget of 1 ÷ 38: about half a point.

## Checks on load

A scenario is refused when its `start` or `timezone` does not parse, two
members share an id, a turn names an unknown member or a channel its
member is not in, `t` goes backwards, a turn has both or neither of `text`
and `reaction`, a reaction targets a later turn, a reaction turn or a
message in another room, an `expect` key is not an index or range, names a
turn that does not exist or overlaps another key, or an action name is not
in the list.
