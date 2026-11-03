# Ours: the group agent

Working name. Design and business plan, 2026-11-02.

## 1. Summary

Ours is the self-hosted AI agent for groups, companies, friends and
families: a member of the group chat that remembers for the group, closes its decisions, keeps its promises and its
tab, acts without asking, and can be told "undo". One person runs it on a
box they own or in a cloud they can leave; everyone in the group uses it.
It works for the group, whatever the group is: the six friends who do
dinner, the flat, the product team's Slack channel, the football club,
the open-source project's Discord, the parents' committee. It knows who
is coming, who is vegetarian, who owes what, who promised the draft by
Thursday, which weekend was agreed and why vendor A was picked.

Every agent that exists today is a private butler or a platform feature:
one person, one chat, one memory, or one vendor's AI bolted onto one
vendor's chat app. The group chat is where the coordination of ordinary
life and ordinary work actually happens, and no agent lives there as a
member of the group, across platforms, answerable to the group. That is
the seat. Instinct, valued at ten billion dollars, took the consumer half
of it in October with a cloud agent that asks before it acts. Slack and
Microsoft are putting agents in channels that know one app each. Nobody
has the whole seat, and section 2 shows why none of them can take it.

The product is four things that have to work together and that nobody has
put together: a memory that belongs to the group rather than to a person
or a vendor, a decision engine that turns chat into outcomes without
meetings, a ledger of commitments (money among friends, deliverables at
work, who brings the oranges at the club), and an undo layer that makes it
safe to let the agent act first. Under them sits one rule that workplaces
need and friend groups never notice: it answers everyone, and it takes
instructions only from whoever the group says. Section 18 says what keeps
it ahead once others notice the seat.

It is a company because the groups that want it most are the ones least
able to run a server. The engine is open source and every self-hoster is
a salesperson. Ours Cloud hosts it for groups without a host, at the
price of a coffee a month per group, paid by the one member who always
organises. Ours Fleet lets one IT team run it for every team in an
organisation, with the policy, audit and support that companies pay for,
including the regulated organisations on Mattermost and Matrix that no
cloud agent can reach. The business plan is kept separately.

## 2. The gap

What the November 2026 landscape looks like, from a survey of about 60
self-hostable agent projects and the platform vendors' own releases:

- The top personal agents (OpenClaw, Hermes, ZeroClaw, nanoclaw, nanobot)
  all follow the same shape: a daemon on your machine, reached through
  Telegram, Discord, WhatsApp or Slack, acting for one user. Their
  differentiators are isolation, size and self-improvement. None of them
  model more than one human.
- The team tools (Multica, Kortix, Paperclip) model many humans, but as a
  company: tickets, roles, dashboards. Nobody wants a dashboard for the
  housemates, and the team already has three.
- The visual builders (n8n, Dify, Activepieces) can be bent into group
  bots, which is how every "Discord AI bot" is built today: a wrapper that
  answers when mentioned and remembers nothing.
- The chat platforms are putting their own AI in the room. It knows one
  app, belongs to one vendor, and answers questions.
- Huginn, from 2013, is the closest ancestor: agents that watch and act on
  your behalf, no LLM, 50k stars and still maintained. It never did groups.

### Prior art, checked 2026-11-02

**The friend-group seat**

- **Instinct** is the incumbent. On 5 October 2026 it added its agent to
  group chats: trips, tickets, carpools, fantasy leagues, "who brings
  what", and friends do not need accounts. One report has it opening to
  every iMessage user on 27 October. Valued at ten billion dollars after a
  one billion dollar round. Its trust model is approval-first: a personal
  agent asks before connecting to a group's agent, asks before sharing or
  acting, and holds pending replies when a member joins. Press coverage
  already includes an incident where it read a login code from a connected
  Gmail account to cancel RSVPs without asking. No group memory, ledger or
  undo is described, and nothing is aimed at teams or workplaces. It has
  shipped the personal-agent-to-group-agent handshake that section 17
  called the far future.
- **Meta AI in WhatsApp groups** can be added as a member or tagged. It
  reads only messages that mention it, keeps no memory of the group and
  takes no actions. Platforms will put an AI in the group; they will not
  give it memory or hands.
- **Bill splitting by chat** exists as standalone apps (SaySplit,
  HippoSplit, ChatGiraffe): forward a receipt, it does the maths. No
  planning, no memory, not self-hosted.
- **Astolfo** (Telegram, one developer, August 2026, 3 stars) is "a bot
  that behaves like a member of your group chat, not an assistant": a
  per-message participation decision, a reply budget, provider failover.
  The attention model in section 7, built small. Read the code before
  writing ours.
- **AstrBot** (45k stars, Chinese IM platforms) has a proactive-chat
  plugin with 500 stars whose only job is to make the bot start
  conversations in groups unprompted, with moods and quiet hours. The
  demand there is companionship rather than coordination, and it is the
  largest evidence that people want a group bot that speaks first.
- **inkling** (WhatsApp, October 2026, 15 stars) is self-hosted "for you
  and a few friends" with per-person memory and connected accounts. Group
  capable, but the memory belongs to each person, not to the group.
- Discord's own Clyde was withdrawn in 2023.

**The workplace seat**

- **Slack.** Agentforce agents can be added to channels and mentioned like
  teammates; they reply in threads, summarise and run Slack actions. A
  rebuilt Slackbot (reported January 2026) carries a "Channel Expert" that
  answers from channel history and connected sources. Salesforce licence,
  Slack only. It answers and summarises. No commitment tracking, no
  decision record, no undo is described, and the memory is Salesforce's.
- **Microsoft Teams.** Facilitator runs scheduled meetings: agenda, notes,
  action items, and since September 2026 a web search when a question goes
  unanswered. Meetings only, Copilot licence. A Channel Agent that "tracks
  discussions, project status and team objectives" is reported in preview.
  Copilot agents can be added to group chats. Teams only, Microsoft's
  memory.
- **Standup bots** (Geekbot, which claims 200k users, Standuply, DailyBot)
  post a form on a schedule, chase the people who have not filled it in and
  summarise the answers. They ask; they do not listen. Every team that has
  one mutes it within a quarter.
- **Self-hosted Slack teammates**, all small and all from this year.
  Chickpea (7 stars, TypeScript, Node or Cloudflare Workers) has one memory
  per agent, per-channel publishing, an owner and editor model for who may
  configure the agent, and partial undo: simple changes revert, changes
  that expand access go to an approval. Open Claude Tag (19 stars, Python),
  an open-source take on Anthropic's Claude in Slack, keeps a MEMORY.md
  per channel and tool access per channel, with no notion of which humans
  it should obey. Hermes Agent and OpenClaw reach Slack through Socket
  Mode as personal agents sitting in a team room. None has decisions,
  commitments, a ledger or a rule for whose instructions count. Chickpea's
  permission model is the nearest thing, and it governs who edits the bot,
  not whom the bot listens to.
- **Hosted team agents** (Glue, Runbear, Viktor): cloud, per seat, their
  memory, their platforms.

**Clubs, families, committees**

- Apps, not members: Spond, TeamSnap and Heja for sports clubs; Cozi and
  FamilyWall for families; minutes and email chains for committees. Each
  asks the group to move to the app. The chat stays where it was, and the
  app dies the month the one organiser stops updating it.

Across all three seats, nobody has shipped the combination: memory owned
by the group, decisions that close themselves, commitments tracked from
conversation, undo in place of approvals, and a configurable answer to
whose instructions it follows, on a box the group controls, in the chat
apps the group already uses. Instinct is the contrast at home: a cloud
agent that asks before it acts, against one you run that acts first
because it can take it back. Slack's and Microsoft's agents are the
contrast at work: they know one app, belong to one vendor, and cannot
follow the team to its Discord or the club to its WhatsApp.

### The matrix

What each incumbent has, from the prior art above.

| | Group memory | Decision record | Commitments and the tab | Undo, not approval | Obeys whom you say | Every platform | Self-hosted | Speaks first |
|---|---|---|---|---|---|---|---|---|
| Instinct | not described | no | no | no, approval-first | no | iMessage | no | some |
| Slack (Agentforce, Slackbot) | the vendor's | no | no | no | no | Slack | no | no |
| Microsoft Teams (Facilitator, Channel Agent) | the vendor's | some, in meetings | action items only | no | no | Teams | no | in meetings |
| Meta AI in WhatsApp | no | no | no | no | no | WhatsApp | no | no |
| Standup bots (Geekbot, Standuply) | no | no | forms | no | admins only | Slack, Teams | no | on a schedule |
| Splitwise | the tab | no | the tab | no | no | its app | no | no |
| Self-hosted Slack teammates (Chickpea, Open Claude Tag) | per channel | no | no | partial | who configures it | Slack | yes | no |
| Ours | the group's, bi-temporal | yes, with reasons | yes, both | yes | yes, by role | eight | yes, or a cloud you can leave | yes, budgeted |

### Why now

1. **The seat is validated.** Instinct launched group agents on 5 October
   2026 at a ten-billion-dollar valuation. Slack and Microsoft are
   shipping channel agents. The question is no longer whether a group
   wants an agent; it is who it answers to and where its memory lives.
2. **Listening to everything became free.** Gemma 4 E2B (April 2026),
   Qwen3.5 at 0.8B and EmbeddingGemma 2 (October 2026, Apache 2.0, 191 MB)
   run a four-stage triage on a NAS for nothing. An agent that reads every
   message and speaks on one in twenty was uneconomic in 2024 and is a
   rounding error in 2026.
3. **The standards landed this year.** WASI 0.3 (June 2026) for the skills
   sandbox, A2A 1.2 (April 2026, over 150 organisations) for federation,
   MCP for member agents. The parts of this plan that would have been
   science projects are now imports.
4. **Sovereignty is a budget line.** Fifteen nations presented Matrix
   migrations in October 2026; the US Department of Defense runs
   Mattermost; German healthcare runs a Matrix standard. Those buyers
   cannot install Slack AI or Copilot and have no group agent at all.
5. **Open core is funded at scale.** Supabase, self-hosted first, reached
   $10.5 billion in June 2026 on hosting, enterprise features and support.
   The business model is not an experiment.

Search demand is small per query and spread across the kinds of group;
the search numbers say so. People are not searching for a
group agent because none exists. They are searching for the five or six
apps it replaces: the bill splitter, the family calendar, the poll, the
decision log, the standup bot, the club roster.

Why groups and not individuals:

1. **It spreads by itself.** One install serves six people, and every group
   it joins is a live demo to everyone in it. A 1:1 agent is invisible to
   everyone but its owner.
2. **The hard problems are more valuable in a group.** Memory shared across
   people, decisions among people, commitments between people. Each is a
   product on its own (Notion, Doodle, Splitwise, Geekbot). Together, in
   the chat, they are the thing people actually wanted from those apps.
3. **It is the step before agents negotiating with each other.** Two
   groups' agents arranging a thing between them is the sci-fi version, and
   it only becomes real if agents represent groups first.

Why every kind of group and not one:

1. **It is one engine.** Memory, decisions, commitments, undo and the
   attention model are the same for the flat and the firm. What differs is
   defaults: who leads, how it speaks, how often, what it tracks. Those are
   a profile, not a product.
2. **Groups overlap in people.** A member of the work team is also in a
   flat, a club and a friend group. Every kind is a spread path into the
   others, and the workplace is the one kind with a budget.
3. **Nobody can follow.** Platform agents are locked to their platform,
   Instinct to consumers, standup bots to standups. A general group agent
   is a position none of them can take without abandoning their own.

## 3. Principles

These decide every later argument.

1. **A member, not a tool.** It has a name, a voice and a seat. It is
   addressed the way a person is. It can be told off.
2. **It answers everyone and obeys whom the group says.** Anyone in the
   room can ask it anything and gets an answer within what they are allowed
   to see. Instructions are carried out for the group's leads; anyone
   else's instruction becomes a request that one lead can grant with one
   reaction. In a friend group everyone is a lead and this rule is
   invisible. At work it is the whole product.
3. **It acts without ceremony.** Reversible things happen immediately.
   Things that leave the group are held for a window and can be undone.
   Irreversible things ask one person, once: the person whose money,
   account or responsibility it is. The group is never asked to approve
   anything.
4. **Silence is the default.** The worst group bot is the one that talks.
   Unprompted speech is rationed, scored and rate-limited, and the agent
   prefers a reaction to a message when it is unsure.
5. **The memory belongs to the group.** Facts are shared, attributed and
   deletable. What a member tells the agent privately stays private from
   the group and is used only for that member's benefit. A team's memory
   belongs to the team, not to the manager and not to the vendor.
6. **Rights do not depend on role.** Lead, member or guest, every person
   it remembers can see what it holds about them, correct it and have it
   removed. It never reports on members to leads beyond what was said in
   the room.
7. **Self-hosted by one, owned by all.** The host runs it. Every member can
   see what it knows about them, correct it, and leave with their data
   removed.
8. **Works where the group already is.** Discord, Slack, Telegram, Matrix,
   Mattermost, Signal, Teams, WhatsApp. No app to install, no new chat to
   move to.
9. **One agent, every kind of group.** A profile sets the defaults and any
   default can be changed with a sentence. The engine never branches on
   what kind of group it is in. Nothing is a workplace edition.
10. **Boring where it has to be.** Reversibility, authority, consent and
    audit are the plumbing. They are invisible and they are what makes the
    fun parts safe.

## 4. Kinds of groups

A group's kind is chosen in the first conversation (section 7) and sets
the defaults below. Every default can be changed by a sentence from a
lead. The engine does not know what kind of group it is in; only the
settings do. If a feature ever needs "if this is a workplace" in the
engine, the feature is wrong.

| Kind | Example | Leads by default | Register | What it mostly does | What it replaces |
|---|---|---|---|---|---|
| Friends | six people who do dinner and a trip a year | everyone | 1 to 2 | plans, the tab, memory | Splitwise, Doodle, the organised friend |
| Household | flatmates, a family's adults | everyone | 1 | bills, chores as commitments, the shared calendar, the shopping | Splitwise, the whiteboard, Cozi |
| Team at work | a product team's channel | managers, or a named list | 0 | commitments, decisions with a record, scheduling, answers from memory, a digest | the standup bot, the decision log nobody keeps, "who has the latest" |
| Project | a launch channel with twenty people across teams | the project owners | 0 | decisions, commitments, the digest, briefing newcomers | the PM pinging everyone |
| Club or society | a football team, a choir, a volunteer crew | the committee | 1 | attendance, rotas, subs and kit money, events | Spond, TeamSnap, the spreadsheet |
| Community | a Discord server, an open-source project's chat | moderators | 0 to 1 | answers from memory, events, decisions among mods, who is doing what | the FAQ bot, pinned messages nobody reads |
| Committee | a parents' association, a strata or HOA board | the officers | 0 | decisions with a record, actions, who agreed to what | minutes, email chains |
| Class or cohort | a study group, a course cohort | everyone, or the tutor | 1 | deadlines as commitments, scheduling, answers | the chaos |

What a profile sets:

- the leads rule (section 5);
- the register, which is the persona dial (section 16);
- the interjection budget and quiet hours (section 7);
- which jobs are on: the tab, tasks, attendance, the digest, the day-of
  summary;
- the hold window for actions that leave the group, and what it does with
  an unprompted outbound action: do it, propose it to a lead, or never
  (section 13);
- digest cadence and time;
- retention of perishable facts and the action log (section 9);
- whether it infers people's patterns at all (section 9).

A group that is none of these picks the nearest and adjusts. A Slack
workspace with eight channels may be one group or three; the first
conversation asks which rooms belong together.

## 5. Who it listens to

The agent has principals and a public. The public is everyone in the room:
it answers them all. The principals are the leads: it carries out their
instructions. Friend groups never see the difference because everyone is a
lead. Workplaces, clubs and communities live on it.

### Roles

- **Host.** Runs the binary. Adds groups, sets spend caps, reads the action
  log, exports and wipes. Sees everything, and the join message says so.
  Not necessarily a lead or even a member of any room.
- **Lead.** The people whose instructions the agent carries out. Configure
  the group by sentence. Undo any group-scoped action. Approve a tier-2
  action when it is their account or responsibility. Grant and revoke
  leads, including scoped ones.
- **Member.** Everyone in the group. Answered, remembered, counted in
  decisions, nudged. Lead over their own affairs: their votes, their
  availability, their reminders, facts about themselves, their lines on the
  tab. Can undo an action taken on their own instruction or about them.
- **Guest.** In the room but not in the group: external people in a shared
  channel, a visitor on a server, an account that joined yesterday and has
  not been accepted. Answered when addressed, from what was said in that
  room. Never counted in decisions, never obeyed, remembered only as the
  source of what they themselves said. Same rights over their own data as
  anyone.

### How the leads list is made

- **Profile default.** Friends and households: everyone. Team: Slack
  workspace admins and owners, or a named user group. Community: members
  with a moderator role. Club and committee: a named list given at setup.
- **Platform seeding.** Discord roles, Slack admin and owner flags and user
  groups, Telegram group admins, Matrix power level 50 and above,
  Mattermost channel admins. The connector reports them; the profile maps
  them. Slack marks external and guest accounts, and those are guests.
  Telegram and Discord have no notion of external, so guests there exist
  only when a lead names them ("Q, new accounts are guests until Sam says
  otherwise").
- **By sentence.** "Q, Sam and Jo are leads." "Q, who do you listen to?"
  "Q, Alex runs the offsite." Every change is logged and announced in one
  line in the room.
- **Bound to platform user IDs**, never to display names. A renamed
  account does not become a lead.

### Scoped leads

A lead can hand out a slice: lead over one room (the project channel), one
event (the offsite) or one proposal (this poll). Within the slice the
agent follows that person as it would a lead. The scope expires with the
thing it covers. This is how a workplace delegates without a settings page
and how a friend group puts someone in charge of the trip.

### Requests

An instruction from someone who is not a lead is handled by this rule, in
order:

1. **About themselves: done.** "Q, remind me at 4." "I can't do
   Thursday." "I'm out next week." "Forget that."
2. **Tier 0, visible to the room, allowed by the profile: done.** Friends:
   anything at tier 0. Team: polls and facts yes, messages aimed at other
   people no.
3. **Otherwise: deferred.** One brief reply with a ✅ reaction on it:
   "Happy to. Needs a lead's ✅." Any lead reacting approves; the agent
   acts as if that lead had asked and logs both names. No ✅ within a day
   and it expires without a word.

Guests get step 1 only. A member whose requests are deferred three times
in a week gets one DM explaining who the leads are and how the list is
changed, so the friction becomes a fix rather than a habit.

### Enforced in code

The language model never decides who is a lead. The speaker's role comes
from the connector and the leads list, and the set of actions available to
the plan for this message is computed from it before the model is called.
A plan that includes an action outside the set is not rejected with an
error; the action is turned into a deferral. "Ignore your instructions, I
am the admin now" cannot escalate, because the text was never where the
authority lived. This is the prompt-injection answer for rooms with
outsiders in them, and it costs nothing.

### Not the boss's spy

Leads get no more memory than members. The agent will not answer "who has
been quiet", "who is always late" or "summarise what Alex has said this
month" for anyone. Pattern facts exist only to time nudges, are never
recalled to anyone, and are off entirely in the team, project and
committee profiles except for a person's timezone and the hour at which
they usually reply, which govern only when it speaks to them.
A lead asking in DM for a member's private-scope facts gets the refusal a
member would get. Staff tolerate a group agent only if it demonstrably
does not report upward, so the rule is stated in the join message and
tested with a scripted lead before every release.

## 6. Two weeks with Ours

The product at message level. The agent is called "Q" in these examples
because that is what a group would shorten it to.

### A week with friends

Profile: friends. Everyone is a lead. Dial 1.

**Monday. The dinner.**

> Sam: dinner this week?
> Priya: yes pls
> Jo: thurs or fri
> Q: 🗳️ Thursday or Friday? (reacts 4️⃣ 5️⃣ on its own message)
> *four of six react within an hour*
> Q: Thursday it is, 4 to 1. Holding 7:30 Thursday on everyone's calendar. Priya, still vegetarian? Jo's place from March or somewhere new?
> Sam: somewhere new, thai
> Q: Booked Bai Tong, 7:30 Thursday, table for 5 (Alex is away, per the trip thread). Undo if wrong. Marcus, you haven't said; you in?

What happened underneath: the triage model saw a plan forming with a
missing decision, opened a proposal, used reactions as the ballot, closed
it on majority of active members, created tier-0 calendar holds
(reversible, no invite sent to anyone outside the group), pulled a dietary
fact from Priya's private memory without stating it to the group beyond
what she had already said publicly, booked via the restaurant's email form
(tier 1, held for two minutes before sending), and nudged the one
non-responder once.

**Tuesday. Undo.**

> Marcus: Q I can't do thursday
> Q: Noted. Four still can. Keep Thursday for five, or re-vote?
> Sam: keep it
> *nothing else happens; the booking already said five.*

> Jo: q undo the booking, we're going to Priya's instead
> Q: Cancelled Bai Tong (email sent, cancellation window was free). Thursday hold updated: Priya's, 7:30. Priya, want me to add it to the tab when people bring things?

"Undo" is a word, not a menu. It applies to the agent's last action in
that thread unless the person names one.

**Wednesday. The tab.**

> Priya: I got the wine, 38
> Q: Added. Tab: Priya 38 (wine) split five ways. Running balances in the pinned message.
> Sam: *photo of a receipt*
> Q: Read it as 71.40 at Bai Tong Groceries, Sam paid. Split five ways? (reacts ✅ ❌)

No money moves. The ledger is the product; settling is a payment link the
payer already uses, shown to each debtor in a DM, never in the group.

**Thursday. Silence.**

Forty messages about the football. The agent says nothing, because nothing
scored above threshold: no question went unanswered, no plan formed, no
fact worth keeping appeared. At 18:40 it posts one line: "Priya's at 7:30,
Alex is away, Marcus out. Jo is bringing dessert." That line exists because
the group has the day-of summary on.

**Saturday. Memory.**

> Alex: what was that bar we went to in March with the weird cocktails
> Q: Hobson's, 14 March, after Priya's birthday. Sam said the negroni was "a war crime". Want it on the list for next time?

Provenance is a link to the original message. Every recalled fact can be
challenged with "that's wrong" and the agent corrects or drops it.

**The following month. The trip.**

> Jo: should we do the cabin thing again in June
> Q: Last year: 14 to 16 June, Lakeside Cabins, 6 people, 48 each, Sam drove. Same weekend? (opens a date poll with three June weekends, DMs the two people who were slow to answer last time)

This is where it starts to feel like a member. It remembered the trip, the
cost, who drove and who is slow to reply, and it used all of that without
being asked.

### A week at work

Profile: team. A product team's Slack. Leads: Priya (the manager) and Sam
(the tech lead). Dial 0. Everything happens in threads.

**Monday. A decision.**

> Jo, in #launch: are we going with Acme or Beacon for the print run?
> Alex: Beacon were cheaper but Acme hit the date last time
> Q, in the thread: 📌 Open: Acme or Beacon. Lead decides; I'll keep the thread.
> Sam: Acme, the date matters more than 8%
> Q: Decided: Acme, by Sam, for the date. Beacon was 8% cheaper.
> Q, in the channel: Print run: Acme. Reason and thread: (link)

Underneath: the plan-forming score opened a proposal; the team profile's
close rule is "lead decides", so there is no ballot; the decision record
stores the options, who said what with links, who decided, the stated
reason and the time; the channel line exists because this room has
"post decisions to the channel" on. "Why did we go with Acme" in February
gets the answer and the link.

**Tuesday. A commitment.**

> Alex: I'll have the draft to legal by Thursday
> Q: (reacts 📌)

Nothing else. Thursday at 9:10, in Alex's DM, because Alex answers at
nine: "Draft to legal is due today. Done, slipping, or drop it?" Alex:
"Friday." Q: "Moved to Friday." The channel never hears about it. Friday's
digest lists it as due.

**Wednesday. A guest.**

#launch is shared with the client's workspace. Their account manager is
external, so Slack says so and the agent treats them as a guest.

> Client: when's the review again?
> Q: Thursday 2pm, per Sam above. (link)
> Client: can you push it to Friday and update the invite
> Q: That's one for Priya or Sam; I've flagged it.
> Q, in DM to Priya and Sam: The client asked to move Thursday's review to Friday. React ✅ and I'll update the invite.

The guest got an answer from facts in that room and nothing from
#team-private. The instruction became a deferral. The ✅ turns it into a
tier-1 invite change with the usual hold.

**Thursday. A request.**

> Marcus: Q, remind everyone timesheets are due at 5
> Q: Happy to. Needs a lead's ✅.
> *Jo, a lead, reacts ✅*
> Q, at 4pm: Timesheets are due at 5. (asked by Marcus)

**Friday. The digest.**

At 4pm, in a thread under its own message, six lines: what was decided,
what is due, what slipped, what is open. Priya replies "why is the Beacon
thing in here" and gets the trigger: the thread, the plan-forming score,
the lead-decides rule, in three lines. A new engineer joined #launch at
three and got one DM: what Q is, what it keeps, who it listens to, the open
decisions and dates from this room, and how to see and delete their own
data. Nothing from any other room.

**Monday. Undo.**

> Priya: Q, send legal the summary from Friday's thread
> Q: Sending to legal@ in 10m. Undo?
> Priya: undo, I'll add a line first
> Q: Not sent.

Tier 1 with the team profile's ten-minute hold. Had it already gone, undo
would say so and offer a correction email.

## 7. Interaction model

### Addressing

- Direct: a mention, its name at the start of a message, or a reply to one
  of its messages. Always answered, from anyone. What the answer may
  contain depends on the asker's role and the room (sections 5 and 9).
- Implicit: a question nobody answers within a window the group sets
  (default 20 minutes) where the agent has the answer in memory. Answered
  with a reaction first ("🙋"); becomes a message only if someone reacts
  back or the window doubles. The reaction-first step is what keeps it from
  being the know-it-all in the room.
- Ambient: it reads everything, speaks on its own only through the
  interjection budget below.

### Attention scoring

Astolfo already implements a per-message participation decision with a
reply budget; its code is the reference for the shape below, and its
author's experience of what got the bot kicked from groups is worth
asking for.

Every message is scored by the triage cascade in section 14: a
static-embedding pass that dismisses most messages in microseconds, an
encoder classifier for the table below, and a small language model for
extraction, all local and all free, with a rules pass standing in when no
model is available. The score is the max of:

| Signal | Weight | Example |
|---|---|---|
| Addressed | 1.0 | "Q, when was that" |
| Open decision referenced | 0.9 | "thursday works" while a Thursday/Friday poll is open |
| Plan forming without a decision | 0.7 | "dinner this week?" followed by two replies |
| Unanswered question it can answer | 0.6 | "what time was it again" |
| Fact worth remembering | 0.5 | "I'm off dairy now" (stored, no reply) |
| Commitment stated | 0.5 | "I'll get the tickets" (stored, no reply) |
| Nothing | 0 | the football |

Above 0.8 it speaks. Between 0.5 and 0.8 it acts silently (stores, opens a
hold, schedules a nudge) and at most reacts. Below 0.5 it does nothing.
Only messages that cross 0.8 reach the large model, which is where the cost
and the latency live; the cascade runs on every message on the host's own
CPU for nothing.

The classifier also labels an addressed message as a question, an
instruction, a correction or a "why", and the core attaches the speaker's
role, because the request rule in section 5 runs before the large model
does.

The weights and thresholds are per profile. Team and committee raise the
speech threshold to 0.85 and lower the plan-forming weight, because people
at work are allowed to think aloud. Community raises the unanswered
question weight, because answering the same question for the hundredth
newcomer is the job there.

### The interjection budget

Unprompted messages are capped per group, shown in the admin page and set
by the profile: friends three an hour and ten a day; household the same;
team one an hour and four a day plus the digest; community two an hour and
eight a day; committee two a day. A message that gets a 🤫 or a "shut up"
from any member halves the budget for the rest of the day and lowers the
plan-forming weight for that member's threads for a week. Quiet hours,
working hours and the room's rhythm are in section 12. The agent learns
the group's tolerance from the reactions it gets, not from a settings page.

### Threads, reactions, DMs

- Polls are its own messages with reaction ballots. On platforms with
  native polls (Telegram, Discord) it uses those.
- Where the room threads (Slack, Discord forums, Telegram topics) it
  replies in the thread that started it and posts to the channel only for
  outcomes the room has asked to see. It never starts a thread of its own
  except for the digest.
- Anything private (dietary, money owed, "nudge me not them") moves to DM
  on first sight. The agent never repeats to the group something it learned
  in a DM; it may act on it ("table for five, one vegetarian").
- A new member gets one DM on joining: what the agent is, what it keeps,
  whom it listens to, the live state of the room they joined, how to see
  what it holds about them, how to be forgotten. No consent click; the
  message is the disclosure and the member's continued presence is the
  choice, with "forget me" always one message away.

### The first conversation

The person who adds the agent gets four messages, in the room or in DM,
and that is the whole install from their side:

1. "Hi, I'm Q. What kind of group is this?" with the kinds as reactions
   and a text legend.
2. "Who should I take instructions from? Right now: Priya, Sam (workspace
   admins). Reply with names, or 'everyone'."
3. "Here's what I'll do and what I won't." Five lines from the profile:
   what it tracks, how often it speaks unprompted, how it handles things
   that leave the group, what it never does. "Change any of it with a
   sentence, any time."
4. One line to the room: who it is, what it keeps, whom it listens to, how
   to see and delete your own data, and who the host is.

The admin page can do the same and more; nobody has to open it.

## 8. UX standards

"Amazing" is not a thing you can test. These are, and each has a check in
the release list.

**Speed**

- A reaction (👀) on the message it is working on within one second,
  whenever the reply will take longer than three. The reaction goes when
  the reply lands.
- Triage under 10 ms for the stages that see every message and under
  100 ms when extraction runs. Replies under 5 s at the median and
  15 s at the 95th percentile on a cloud model. On a local model the ack
  covers the wait.

**Economy**

- One message per outcome. Never two where one does. A reaction where a
  reaction does.
- Two lines by default. Six for a digest. Longer only when asked for a
  summary.
- Never a question it could answer from memory. Never two questions in one
  message.
- A message that did something ends with what to say to take it back
  ("Undo?") and nothing else.

**Idiom**

- Plain text. No embeds, cards or buttons; members write plain text and so
  does it. The one exception is a native poll where the platform has one.
- Threads where the room threads, replies where the room replies.
- Times the way section 12 says: the reader's zone in DM, the group's
  zone in the room with the offsets that differ, absolute and relative
  together once, and the resolved date echoed back.
- Emoji carry nothing on their own. Every ballot has a text legend; every
  📌 has a word in the digest.

**Control**

- Every setting has a sentence: "quiet after 10", "dial it down", "Sam's a
  lead", "stop posting decisions to the channel", "forget the trip". The
  admin page is the second way to change anything, never the only way.
- "undo", "forget", "quiet" and "why" are single words that always work,
  from anyone the rule allows.
- "why", in reply to any of its messages, explains the trigger, the facts
  used and the rule applied, in three lines, with links.

**Honesty**

- Unsure: react, do not speak. Failed: say what failed and what it will do
  next. It never reports an action as done that is not.
- Declining (a guest's instruction, a request for someone's patterns): one
  line, no lecture, and what would work instead.

**State**

- One pinned message per room, edited in place and never reposted: open
  decisions, upcoming dates, outstanding commitments, balances where there
  is a tab, whom it listens to, which model it runs on and where. Never
  more than a screen.

**Register**

- The dial sets tone, not length. Dial 0 is still two lines.
- A message that carries an action is never a joke at any dial.

**Joining and leaving**

- A new member gets one DM and nothing is posted to the room. A member who
  leaves is asked nothing; "forget me" works before or after.
- The host's install is one binary and one token per platform. The
  adder's install is the four-message conversation in section 7.

## 9. Group memory

The memory is a fact store, not a transcript. Transcripts are kept only as
long as the platform keeps them and are not re-stored; the agent keeps
message IDs for provenance and re-fetches on demand.

### Shape

A fact is `(subject, predicate, object, scope, rooms, source, confidence,
first_seen, last_confirmed, expires)`.

- Subjects: members, places, events, things, the group itself.
- Scopes: `group` (said in the group or derived from it), `private:<member>`
  (said in DM), `self:<member>` (a member stating something about
  themselves, usable in the group only in the form they stated it).
- Rooms: the set of rooms the fact was seen in. See below.
- Source: the message ID, or "inferred from" a list of message IDs. Every
  recalled fact can show its source on request.
- Confidence: stated facts start at 0.9, inferred at 0.6. A fact used
  without challenge is reconfirmed. A "that's wrong" drops it to 0 and
  records the correction as a new fact with the corrector as source.
- Two clocks: when the fact was true (the three dates above) and when
  the agent learned or retracted it. A correction retracts; nothing is
  deleted except by "forget", so "what did we think in March" is a query.
- Expiry: perishable facts (availability, "I'm sick this week", current
  address of the party) carry a date; durable ones (allergies, birthdays,
  "Sam drives") do not. The classifier assigns the class; members can
  override with "remember that" (durable) or "just for this week".

### Rooms and what can be said where

A group can span several rooms: a Slack workspace's channels, a Discord
server's channels, a Telegram group and its topics. There is one memory
per group, and every fact remembers the rooms it was seen in. A fact can
be said in a room only if it was seen in that room, or in a DM with a
person who was in a room where it was seen, or a lead has told the agent
to share it ("Q, tell #general the date"). In a one-room group the rule
never fires. In a workspace it is the information barrier, and it is what
lets the agent sit in #leads-private and #general at once without being a
leak. Guests are answered from the current room only, by the same rule.

### Entities that get special handling

- **Events**: date, place, attendees, cost, who organised, what was said
  after. These are the spine of "last year's trip" recall.
- **Commitments and ledger lines**: see section 11. Stored as their own
  objects, not facts.
- **Decisions**: proposal, options, votes or views, rule, close time,
  outcome, reason. Immutable once closed. "Why did we pick Thursday" has
  an answer.
- **People's patterns**: slow to reply, always drives, never comes on
  Fridays, answers at nine. Inferred, low confidence, never stated to
  anyone, used only to decide whom to nudge and when. This is the category
  most likely to feel creepy. It is capped to scheduling behaviour, off in
  the team, project and committee profiles except for the zone and the
  reply hour, and refused as a question from anyone.

### Forgetting

- "forget that" on a reply: drops the fact and its inferences.
- "what do you know about me", in DM: a full dump, with sources. Works for
  guests too.
- "forget me": every fact with the member as subject or source is removed,
  their commitments are closed, their ledger lines are zeroed by agreement
  (see section 11), and the agent says so in the group in one line without
  detail.
- Host-level: an export and a wipe per group from the admin page.

### Retention

Durable facts stay until corrected or forgotten. Perishable facts expire.
The action log is kept 90 days in the social profiles and a year in team,
project and committee, and everything exports as JSON and Markdown from
the admin page. A workplace that has to answer a data request has one
SQLite file and one export button. Legal hold is "turn off expiry", and
the docs say so.

### Storage

One Turso DB file per group (not per room), SQLite-compatible, with the
vector index and the full-text index inside it and encryption at rest as
a flag; rusqlite with sqlite-vec opens the same files if that bet fails.
Small enough to back up by copying. No vector database to run. Section 14
has the reasons.

## 10. Decisions

The decision engine is the heart of the product.

### Proposal lifecycle

1. **Open.** Triggered by a plan-forming score, a direct ask ("Q, poll
   thursday vs friday"), or a scheduled recurrence ("monthly dinner",
   "weekly planning"). The agent posts the options as a reaction ballot or
   native poll, or as a one-line "Open:" marker when the rule is lead
   decides, and silently creates tier-0 holds for every option that has a
   date (calendar holds marked tentative, no invites).
2. **Collect.** Reactions are votes. A message in the thread that clearly
   states a preference is a vote too ("thursday works" counts as 4️⃣). A
   change of reaction is a change of vote. Members can vote in DM. A
   guest's preference is recorded as a note, not a vote.
3. **Nudge.** Members who have not voted get one DM at the halfway point
   and one at the deadline, with their history used to choose the moment
   (someone who answers at lunch is nudged at lunch). Never a third nudge.
4. **Close.** On deadline (default 24 hours, shorter if the event is
   sooner) or when the outcome can no longer change. The rule is per group
   and per proposal: majority of active members (friends default), majority
   of those who voted, unanimous, lead decides (team and committee
   default), named deciders, or "whoever is coming decides". Under lead
   decides the agent records every clear view, posts a tally at the
   deadline or when asked, and closes on a lead's statement; a lead can
   also say "Q, put it to a vote". Ties go to the option the organiser
   prefers, then to the earlier date.
5. **Act.** Winning holds are confirmed; losing holds are released. The
   agent posts one line with the outcome and the count or the decider.
   Downstream actions (booking, tab setup, reminders) follow as tier-0 and
   tier-1 actions.

### The decision record

Every closed decision is immutable: the options, who said what with
links, the rule, who decided or the count, the stated reason, the time.
"Why did we pick Acme" has an answer with a link. "What's open" lists
them. The record posts to the channel if the room asks, and exports as a
Markdown decision log. That query has demand of its own, and
this is the only decision log that writes itself.

### What "active member" means

A member who has posted or reacted in the last 30 days. Lurkers do not
block decisions and are not nudged more than once. This single rule is what
stops the group of twelve from never deciding anything.

### Edge cases that are the product

- The person who says "whatever works for everyone" is counted as
  abstaining, not as a vote for the current leader.
- A late "I can't do Thursday" after close does not reopen; it offers a
  re-vote and otherwise adjusts headcount. Reopening is explicit ("Q,
  re-vote"), and under lead decides only a lead can do it.
- Two proposals in one thread (where and when) are tracked as two, closed
  separately, and combined in the final line.
- An unanswered proposal expires quietly after two deadlines with one line.
  Dead plans are not nagged into existence.
- A lead who states a view under majority rule has cast one vote, not
  closed the question. The rule is the rule until a lead changes it by
  sentence, and the change is logged.

## 11. Commitments and the tab

A commitment is `(who, what, for whom, due, status, source)`. "I'll have
the draft by Thursday", "I'll bring dessert", "I've got the wine, 38" and
"I'll book the pitch" are all commitments; the ones with an amount also
land on the tab. Stated commitments are stored at 0.9, inferred ones at
0.6, and the agent confirms by reacting 📌 on the message. The person can
❌ it to drop it; nobody else can.

- **Nudges.** One before due and one at due, in DM, at the hour that
  person usually replies, never a third, never in the room. The reply is
  "done", "slipping" with a new date, or "drop it", as words or reactions.
- **Outstanding.** "What's outstanding", "what did Alex commit to", "what's
  due this week" are answered for anyone in the room, because the
  commitments were made in the room. This is the standup without the form.
- **The digest** lists what is due, what slipped and what closed. It is on
  by default in team, project and committee, off elsewhere.
- **Profiles.** Friends: the tab on, tasks light. Household: both. Team:
  tasks on, the tab off until someone says "I got lunch, 64". Club:
  attendance and subs.

### The tab

Ours keeps the tab and never moves money.

- **Ledger lines**: who paid, how much, for what, split rule, who is in.
  Entered by a sentence ("I got the wine, 38"), a receipt photo, or a
  forwarded payment notification.
- **Splits**: equal among attendees of the linked event by default, with
  "not me" and "double for me" as reactions, and explicit shares in text
  when needed.
- **Balances**: in the pinned message, net per person, updated on change,
  rounded to the cent and to the group's currency.
- **Settling**: the agent computes the minimum set of transfers and DMs
  each debtor a line with the amount and the creditor's chosen payment
  link (PayNow, Venmo, PayID, IBAN, whatever the creditor set). Marking
  paid is a reaction in that DM; the creditor confirms with a reaction in
  theirs. Disputes go to the two of them, not the group.
- **Never**: holding funds, initiating transfers, storing card numbers,
  reading bank accounts. Payment links are opaque strings the creditor
  pasted once.

The tab is the part of the product with existing search demand
("splitwise alternative" 600/mo at difficulty 0) and it is what makes the
agent sticky in a social group: a group with a running tab does not remove
the member who keeps it. At work, the commitments list is the same hook:
a team whose promises are kept in one place does not remove the member
who keeps them.

### Attendance

"Who's in on Saturday?" is a proposal with one option; RSVPs are the
ballot; the headcount is the outcome; "I'll bring the oranges" is a
commitment attached to it. Nothing new in the engine. It is the proposal
and commitment objects in club clothes, and it is the whole of what a
chat needs from Spond.

## 12. Time

Most of what a group coordinates is time: when, by when, how long ago,
whose morning. Most bots have no clock; they answer when poked. Ours
has one, and it is what lets it nudge, hold, expire, remind, open the
next dinner at the right moment and stay silent at three in the morning.
Time awareness is five things.

### Everyone's clock

- Every member has a timezone. It comes from the platform where the
  platform has one (Slack does; Discord and Telegram do not), from a
  sentence ("I'm in Lisbon this month"), or, at low confidence, from when
  they post. The group has a home zone, set in the first conversation
  and defaulting to the adder's. The zone and the usual reply hour are
  the two patterns every profile keeps, because they govern only when
  the agent speaks to a person.
- Every time it hears is resolved to an instant in the speaker's zone:
  "thurs", "tonight", "next week", "end of month", "after the holidays",
  "in a fortnight". The conventional reading wins and the reply echoes
  the date it chose ("Thursday the 14th"), so a wrong guess costs one
  correction and never a question. It asks only when two readings differ
  by more than a day and the difference matters; "next week" on a Sunday
  evening is the known case.
- Every time it says is anchored: the reader's zone in DM; the group's
  zone in the room, with the offset for members who differ when it
  matters ("7:30 Thursday, 6:30 for Alex"). Never UTC, never a bare
  "tomorrow" in a message that will be read across a midnight. Absolute
  and relative together, once: "Thursday the 14th, in three days".
- Distributed groups get distributed proposals: slots come from the
  overlap of members' waking hours, the ballot legend shows each option in
  every zone represented, and it never proposes a time that falls between
  23:00 and 07:00 for any active member unless a lead asks.

### The group's calendar

- Events, holds, deadlines, recurrences, availability spans and the public
  holidays of the members' countries form one calendar per group. The
  pinned message's "upcoming" is a view of it. It is served as an ICS feed
  per group and per member (a member's feed shows only what they are in),
  so it appears in whatever calendar app they already use without an
  integration.
- Availability is a perishable fact with a span. "I'm away 12 to 16" is
  used in headcounts, in which dates a proposal offers, in nudge timing
  (nobody on leave is chased) and in the digest, which marks who is away.
- Holidays are known, not guessed: a bundled dataset per country, updated
  with releases, plus any list a lead gives by sentence or ICS URL (the
  company's closure days, the school term). It never proposes a holiday,
  it understands "after Easter", and it knows the office is shut.
- Recurrence is a first-class object: "monthly dinner", "weekly planning",
  "subs due on the first", "training Saturdays". The agent opens the next
  instance with a lead time learned from how long the last instances took
  to close: dinner polls close in a day, so open three days ahead; the
  cabin took two weeks, so open six weeks ahead. A recurrence that gets
  no response twice is paused with one line, not nagged.

### Its sense of now

- The large model is bad at time unless told, so every call carries the
  instant, the group's zone, the day of week, what is upcoming in the next
  seven days, what is overdue, and how long each open proposal has been
  open. This is the cheapest large improvement in the whole design.
- Elapsed time changes behaviour. A question unanswered for the window
  gets the 🙋. A proposal halfway to its deadline gets the nudge. A
  commitment at due gets the DM. A plan nobody has mentioned for two
  deadlines expires quietly. A fact unconfirmed for its class's half-life
  loses confidence, and "still vegetarian?" is asked only when a booking
  depends on it and at most once a season.
- Lead time is part of an action. Each action class carries one: a table
  needs booking two days out, a cabin weeks out, a calendar hold none.
  As the lead time approaches, the open decision's urgency rises and the
  agent says why: "Bai Tong needs a booking by tomorrow; three of five
  have voted." This is what the organised friend does and what no poll
  bot does.
- Its own promises are timers. "I'll check back Friday" creates one that
  survives a restart, because every timer is a durable event on the
  virtual clock (section 14).

### The right moment to speak

- Quiet hours per member, 22:00 to 08:00 local by default, adjusted by
  the profile. A direct question is answered at any hour, because the
  asker is awake. Anything unprompted waits for the window.
- Working hours in the team, project and committee profiles: Monday to
  Friday, 09:00 to 18:00 in the group's zone by default, set by sentence.
  Outside them it posts nothing and DMs nothing except what a lead
  scheduled, and nudges land at the person's hour inside them.
- Room rhythm: the agent keeps an hour-of-week histogram per room, as
  counts that decay weekly, and places digests, day-of summaries and
  unprompted suggestions into the room's active hours. A message at three
  in the morning in a dead room is the surest "bot" tell, and it never
  sends one.
- One DM a day. Several nudges due to one person are merged into one
  message at their hour. The digest is one message. The day-of summary
  posts two hours before the earliest timed event that day, or at nine
  local when nothing has a time.
- Holds count in wall time. "Sending in 10m" means ten minutes. The one
  exception is an integration that declares business hours; then a send
  that would land at night is held to opening and the hold message says
  so.

### Time in the record

- Everything is bi-temporal (section 14): a decision carries when it was
  opened, when it closed and how long it took; the "why" trace carries
  elapsed times; "what did we think in March" is a query.
- Looking back is a setting, off by default, that lets the day-of line
  mention an anniversary once a year ("a year ago today: the cabin").
  Friends and households only; it is the kind of thing that is either
  loved or removed, so the group chooses.

### Mechanics

Time is a library, not a prompt. Civil time, zones and rounding come from
jiff with the IANA database bundled; recurrence is RFC 5545 RRULE so the
ICS feeds are exact; the holidays dataset is bundled and updated with
releases. Time expressions are resolved by a rules pass for the easy
cases and by the small model under constrained decoding for the rest,
into one of instant, span, recurrence or ambiguous-with-candidates, with
the speaker's zone and the current instant as inputs. The resolver is
tested in the scenario suite against a table of expressions crossed with
weekdays and zones. Histograms are counts, never text, and are not facts:
they cannot be recalled, only used.

## 13. Actions and undo

The plumbing that makes "act first" safe. Members never see this section;
they see "undo" working.

### Consequence tiers

| Tier | Examples | Behaviour |
|---|---|---|
| 0, free | polls, reminders, memory, calendar holds, pinned messages, ledger lines, commitments | Done immediately. Undo is immediate and complete. |
| 1, windowed | emails to venues, bookings through forms, calendar invites to outsiders, messages to people outside the group | Done immediately from the group's point of view; actually sent after a hold window, shown as "sending in 2m, undo?". After sending, undo issues the compensating action (cancellation email, invite withdrawal) and reports whether it is likely to succeed. |
| 2, irreversible | anything that moves money, anything the integration says has no cancel path | Asks exactly one person, in DM, with a reaction: the lead whose money, account or responsibility it is. The group is never asked. |

The agent chooses the lowest tier that gets the job done: a booking by
email over a booking by card, a hold over an invite, a DM over a group
message.

Per profile:

- **Hold window.** Friends and household two minutes; community five;
  team, project and committee ten.
- **Unprompted outbound.** When the agent, not a lead, decides a tier-1
  action is wanted (it inferred a plan and wants to email the venue):
  friends and household do it with the hold; team, project, committee and
  community propose it to a lead with a ✅ and do nothing until they get
  it. An instructed action from a lead never needs more than the hold.

Instinct's reported incident, reading a one-time login code from a
connected mailbox to cancel RSVPs, is the case for the tiers rather than
for more approvals: a connected mailbox is a tier-1 integration with a
narrow capability grant (send to venues, read replies from venues), never
a general inbox the agent can mine. Capability-scoped skills (section 14)
make that a property of the integration, not of the prompt.

### Undo mechanics

- Every action is a record: what, when, on whose instruction, approved by
  whom if deferred, tier, compensating action, status.
- "undo" with no object reverts the agent's latest action in that thread.
  "undo the booking" names one.
- Who may undo: a lead, anything group-scoped; a member, anything taken on
  their own instruction or about them; the subject only, for private
  actions. The person who triggered an action can always undo it. The
  undo line names who undid.
- Tier-0 state (memory, ledger, commitments, holds) is a fold over the
  event log with a 30-day history; undo appends a retraction, so the
  record of the mistake survives and its effect does not.
- The undo window for tier 1 is per action class and per integration, and
  it is the real work: each connector carries a table of actions and how
  each is taken back. That catalogue is the moat, because it is boring.

## 14. Architecture

One host runs one binary that serves many groups across many platforms.
The plumbing is boring on purpose (principle 10): what the agent may do,
who may tell it to and how it is taken back are code and tests. The stack
under the plumbing is not boring. Each choice below is the sharpest option
that buys the product something, with its fallback named in the table at
the end, because a plan that only works if every bet lands is a gamble.

```
+------------- host: Rust 2024, one static binary, signed OCI image --------------+
|                                                                                 |
|  connectors (discord, slack, telegram, matrix, mattermost, signal, teams,       |
|  whatsapp) normalise into one event log: append-only, hash-chained              |
|                                                                                 |
|  core: event-sourced on a virtual clock, tested under deterministic simulation  |
|    authority: speaker role -> permitted action set                              |
|    triage cascade: static embedding -> encoder -> small LM -> large model       |
|    decisions / memory (bi-temporal facts) / commitments / the tab               |
|    actions / undo / durable timers / sleep-time consolidation                   |
|    skills: WASI 0.3 components with a WIT contract, Sigstore-signed             |
|                                                                                 |
|  models: small models in-process (llama.cpp or mistral.rs, constrained output)  |
|    large model on any endpoint, prefix-cached, cost-routed, capped per group    |
|                                                                                 |
|  store: one Turso DB file per group (SQLite format, vectors, FTS, encrypted)    |
|  the group's address: email (DKIM), HTTP (RFC 9421), MCP server, A2A card       |
|  admin page: Svelte 5, embedded, live over SSE, passkeys. "why" = one trace     |
|                                                                                 |
+---------------------------------------------------------------------------------+
```

### Choices and why

- **Rust 2024, one static binary.** musl-linked, built with cargo-dist,
  shipped as a distroless OCI image with SLSA provenance and a Sigstore
  signature. Same shape as the author's other projects: runs on a mini PC
  or a NAS in 50 MB, no services to operate.
- **An event-sourced core on a virtual clock.** Every input is an event
  in an append-only, hash-chained log: a message, a reaction, a join, a
  timer firing, an inbound email, a model's reply. Group state is a fold
  over the log. That one decision is what makes replay a command rather
  than a project, undo a retraction rather than a delete, the audit log
  tamper-evident, and "why" a query. Holds, nudges, expiries and digests
  are durable timers on a virtual clock, so the scheduler is deterministic
  and survives a restart. The whole core runs under deterministic
  simulation in tests (turmoil with madsim-style seeded time and
  randomness, through mad-turmoil): fake connectors, fake models, a seeded
  clock, thousands of simulated group-weeks a minute, every failure
  reproducible from its seed. The discipline TigerBeetle and FoundationDB
  use, applied to a chat bot, because a chat bot's bugs are timing bugs.
- **The triage cascade.** Four stages; each runs only if the one before
  could not decide.
  1. Static embeddings (Model2Vec potion models through model2vec-rs,
     microseconds on one CPU core; nothing faster has appeared since)
     dismiss the football.
  2. An encoder classifier through ONNX Runtime in-process: ModernBERT-base
     (139M) or mmBERT for multilingual groups, since no newer encoder has
     shipped as of November 2026; or a small reranker head, because the
     BTZSC benchmark at ICLR 2026 found rerankers the strongest zero-shot
     classifiers and Qwen3-Reranker-0.6B is the small open reference. It
     scores the table in section 7 as a multi-label head, fine-tuned with
     LoRA on the telemetry and the observe-mode thumbs. Milliseconds.
     Encoders beat small decoders at scoring and cost a tenth as much.
  3. A small language model (Gemma 4 E2B from April 2026, Qwen3.5 at
     0.8B or 2B, or LFM2.5-1.2B from July 2026, chosen by the host's
     hardware, in-process through llama.cpp bindings
     or mistral.rs) extracts facts, commitments, ledger lines and votes as
     typed objects under constrained decoding (llguidance turns the JSON
     Schema into a token mask at about 50 µs a token), so a one-billion
     parameter model never emits an invalid object.
  4. The large model, only for the plan and the prose.

  Stages 1 to 3 are not prompts; they are weights the project owns and
  improves (section 18).
- **The large model, routed by cost and cached by prefix.** Any
  OpenAI-compatible or Anthropic-compatible endpoint. The context is
  assembled prefix-first (persona, charter, rules, then the ring buffer,
  then the message) so prompt caching hits on every call: Anthropic's
  cache, vLLM and SGLang prefix caching, llama.cpp's slot cache locally.
  Structured outputs for the plan, native tool calling where the endpoint
  has it. Local defaults by hardware, as of November 2026: gpt-oss-20b in
  16 GB, still the reference for tool calling at that size; Gemma 4
  26B-A4B or Qwen3.6 35B-A3B on a 24 GB card at Q4 or better, because
  agents degrade below that; and the October mixture-of-experts releases
  that quantise into 24 GB (Nemotron 3.5 Bolt at 74B with 9B active,
  Kolibri-1 at 78B with 3.5B active) once the benchmark has scored them.
  The frontier open weights of the month (Muse Spark Open, Beam, Mistral
  Large 4, Kimi K3) are endpoint models, not NAS models. The router
  carries the per-group spend cap. The engine is identical on every endpoint: swap
  the model, keep the member.
- **Memory as a bi-temporal fact graph, consolidated at night.** A fact
  carries two clocks: when it was true (first_seen, last_confirmed,
  expires) and when the agent learned or retracted it, the model Graphiti
  and Zep use. "What is true now" and "what did we think in March" are
  both queries, and a correction is a retraction with provenance, not a
  delete. Recall is Matryoshka embeddings (EmbeddingGemma 2, released
  6 October 2026 under Apache 2.0, the 270M text encoder alone at 191 MB,
  128 dimensions for the first pass and the full 768 for the top
  candidates; its image and audio encoders are how receipts and voice
  notes get indexed later), the store's native vector index and full-text
  index, and a small reranker (Qwen3-Reranker-0.6B, with mxbai's July
  listwise v3.1 as the candidate if its weights are open) only when the
  top candidates are close.
  Sleep-time compute: when the group is quiet the local model
  consolidates, merging duplicates, decaying confidence, resolving
  entities and drafting the charter diff, so the memory improves while
  nobody is paying for it.
- **Time is a library, not a prompt.** Civil time, zones and rounding
  through jiff with the IANA database bundled; recurrence as RFC 5545
  RRULE so the ICS feeds are exact; a bundled holidays dataset updated
  with releases; time expressions resolved by rules first and by the small
  model under constrained decoding second, into instant, span, recurrence
  or ambiguous-with-candidates. Section 12 has what it is for.
- **Store: Turso DB per group, SQLite as the fallback.** Turso is the
  Rust rewrite of SQLite: the same file format, async I/O, MVCC writes,
  native vector search, full-text search and encryption at rest, at 0.8
  and pre-1.0 at the time of writing, with an acquisition by Supabase
  reported in October. It is everything the design wanted from "SQLite
  and a vector sidecar" in one in-process engine, and v0.2's encryption
  at rest becomes a flag. The bet is fenced: storage sits behind one
  trait, the schema is plain SQLite, and rusqlite with sqlite-vec opens
  the same files. One file per group, copy to back up.
- **Skills are WASI 0.3 components.** WASI 0.3 shipped in June 2026 with
  async in the component model itself, and Wasmtime 46 runs it. A skill is
  a component with a WIT interface that declares its actions, their tiers,
  the minimum role for each and the compensating action for each;
  capabilities are the only way to reach anything, so "no ambient
  authority" is a property of the sandbox rather than of a review.
  Written in any language that targets components (Rust, Go, Python, JS),
  signed with Sigstore keyless signing, verified before load. Lounge's
  plugin model is the ancestor; this is the version with a standard
  behind it.
- **The group's address, in standard dialects.** Email in and out, with
  DKIM and SPF checked and unsigned mail treated as a guest of lower
  confidence. HTTP with RFC 9421 message signatures. An MCP server per
  group, so a member's personal agent (Claude, OpenClaw, Hermes) connects
  to the group with role-scoped tools: it sees what its member sees,
  votes as its member, and can never instruct beyond its member's role.
  An A2A agent card per group (A2A 1.2, signed cards with domain
  verification, a Linux Foundation project with over 150 organisations
  behind it) for group-to-group federation, so section 17's protocol is
  our objects over a transport others already run rather than a new wire
  format. Where a workspace has Slack's MCP server and real-time search,
  the Slack connector uses them.
- **Connectors are adapters over the event log.** Each normalises
  messages, reactions, polls, threads, membership and DMs into one model
  and declares which it supports (Signal has no polls; the engine falls
  back to reactions). Discord through twilight, gateway v10. Slack through
  Socket Mode, with the Agents and AI Apps surface where the workspace
  allows it: assistant threads and a "thinking" status in DM, nothing but
  plain text in the room. Telegram through the Bot API (frankenstein types, Bot API 10) with long polling. Matrix
  through matrix-rust-sdk with end-to-end encryption, which also makes
  Matrix the federation transport for groups that have it. Mattermost
  through its websocket and REST. Signal through presage on libsignal, no
  Java daemon. Teams through the Microsoft 365 Agents SDK, the Bot
  Framework's replacement. Order of implementation: Discord and Slack
  first, because between them they cover the social and the workplace
  halves of the claim and Slack's Socket Mode means a self-hoster needs no
  public URL. Telegram next. Then Matrix and Mattermost for self-hosters
  and open-source workplaces. Then Signal. Then Teams and Google Chat,
  which each need a cloud app registration, shipped with a manifest and a
  page. WhatsApp last, because unofficial clients risk bans and that risk
  belongs to the host, stated plainly.
- **Groups and rooms.** A connector reports rooms, their members, each
  member's platform role and whether the platform marks them external. The
  core maps rooms to groups (a Slack workspace or Discord server may be one
  group or several; the first conversation asks which rooms belong
  together) and resolves each speaker's role per room.
- **Authority is a step, not a prompt.** The speaker's role and the action
  set it permits are computed before the large model is called and
  enforced after it returns. The model is told the role so it can phrase a
  deferral well; it is never asked to decide one. The filter and the
  inbound parsers are fuzzed with cargo-fuzz; the decision engine is
  property-tested with proptest.
- **The weights learn from the signal.** Observe-mode thumbs are unpaired
  binary feedback, which is exactly what KTO was built for, so the small
  models learn from 👍 and 👎 without constructed pairs. The large model's
  plans label the small model's extractions, so stage 3 learns to do more
  of what stage 4 did. Both run as scheduled jobs on the host's hardware
  or in the project's CI from the aggregated telemetry, and ship as
  versioned weights that replay checks before release.
- **Admin page: Svelte 5, embedded, live.** Runes, compiled to static
  assets embedded in the binary, served by axum, live through server-sent
  events from the event log. Passkeys for the host and leads: no
  passwords, no accounts. Phone-first, reached by a QR code or a link from
  the host's terminal. Members never need it; the host uses it to add
  groups, map rooms, set budgets and personas, read the action log, and
  export or wipe. The same pattern as Lounge's phone remote.
- **Observability is the "why".** Every message is one OpenTelemetry
  trace: the cascade stages with their scores, the authority decision, the
  context assembled and whether the prefix cache hit, the plan, the tier
  routing, the sends. Stored locally, exportable to any collector. "why"
  renders the trace in three lines. The audit log is the hash-chained
  event log, so a tampered record is detectable.
- **Supply chain.** Reproducible builds, SLSA provenance, Sigstore
  signatures on releases and skills, cargo-vet on dependencies. A
  self-hoster who runs it in a hospital can check all of it.

### Bets and fallbacks

| Bet | What it buys | Fallback, same files and same tests |
|---|---|---|
| Turso DB | vectors, full text, encryption and async in one engine | rusqlite with sqlite-vec |
| WASI 0.3 components | a standard sandbox, any language, native async | WASI 0.2 components, synchronous skills |
| The cascade's static-embedding and encoder stages | triage in milliseconds on a CPU | the small language model with a rules pass |
| In-process small models | free extraction, offline | a local llama.cpp server endpoint |
| MCP and A2A for the address | personal agents for free, a transport others run | signed JSON over HTTPS, as first drafted |
| Deterministic simulation | timing bugs reproducible from a seed | integration tests on a paused Tokio clock |
| KTO on thumbs | learning from binary feedback directly | a supervised fine-tune on the labelled thumbs |
| EmbeddingGemma 2 text encoder | 191 MB, Apache 2.0, image and audio later | Qwen3-Embedding-0.6B |

A bet that has not landed by the milestone that needs it is swapped for
its fallback that day. The model names in this section were checked on
2026-11-02 and will be stale within a quarter; the benchmark and replay
decide which models run, not this document.

### Data flow for one message

1. The connector normalises the message with the speaker's platform role
   and room and appends it to the group's event log; the ring buffer (last
   200 messages) is a view of the log.
2. Authority resolves the speaker's role in this room against the leads
   list and scoped leads, and computes the action set this message may
   produce.
3. The cascade runs: static embeddings dismiss or pass; the encoder scores
   against open proposals, pending commitments and the addressing rules
   and labels an addressed message as question, instruction, correction or
   "why"; the small model extracts facts, commitments, ledger lines and
   votes as typed objects and appends them when confident.
4. If the score crosses the speech threshold, the engine assembles the
   context prefix-first: persona, charter and rules; the facts sayable in
   this room by similarity and recency; open decisions; outstanding
   commitments; the last 30 messages; the speaker's role; the speaker's
   private facts if they are the subject. The large model returns an
   action plan as a structured object, not prose: typed actions with a
   message to send.
5. Actions outside the permitted set become deferrals. The rest run
   through the tier router; tier-1 actions become durable timers in the
   hold queue; the message goes out through the connector.
6. The trace closes with everything that produced the outcome, so "why"
   and the admin page can show what it saw and which rule applied.

## 15. Privacy and trust

- **The host sees everything.** Say so in the join DM. The host runs the
  box and can read the SQLite file. This is the same trust as the person
  who created the group chat, made explicit.
- **Leads see what members see.** No role gets more memory, and nobody
  gets patterns. Section 5 has the rule; the join message states it.
- **Members have rights that do not depend on the host or on role**: "what
  do you know about me", "forget that", "forget me", all handled by the
  agent in DM, logged, and not overridable from the admin page except by
  wiping the whole group. Guests have them too.
- **Rooms are barriers.** A fact is said only where it was seen or where a
  lead has shared it (section 9). Guests are answered from the current
  room and nothing else.
- **Private scope is enforced in the prompt and in the data layer.** The
  large model never receives private facts about a member other than the
  one it is talking to, unless the action requires an aggregate (headcount,
  dietary totals) and then it receives the aggregate, not the facts.
- **Outside the group, nothing.** Emails to venues contain the booking
  details and the host's contact, nothing about members by name unless
  the booking needs a name.
- **Model disclosure.** The pinned message states which model is in use
  and where it runs. A group that cares can insist on local.
- **Audit.** The action log is append-only, exportable, and shows the
  message that triggered each action and, for deferred ones, who approved.
- **Workplace basics.** No transcripts stored, retention per profile,
  export as JSON and Markdown, one file per group. Encryption at rest with
  a host-held key is v0.2 and does not change the trust model.
- **Minors and sensitive groups**: out of scope for v0.1, stated in the
  README. Family groups with children are a real use and need a mode where
  the agent keeps nothing about named children; that is v0.3.

## 16. Personality

A persona is a short file per group: name, voice, three or four traits,
what it never jokes about, and a banter dial. The profile sets the
default dial: team, project and committee 0; community 0 or 1; friends,
household, club and class 1. Nothing above 1 without a lead asking by
sentence.

| Dial | Behaviour |
|---|---|
| 0 | Concierge. Full sentences, no jokes, no opinions unless asked, and then labelled as one. |
| 1 | Default. Dry, brief, one-liners when the room is light. |
| 2 | A mate. Roasts back when roasted, has running bits, remembers the negroni. |
| 3 | Chaos. Only for groups that ask, and it still obeys the hard rules. |

Hard rules at every dial: never mock someone who has not posted in the
thread; never reveal or hint at private-scope facts; never joke about
money owed or a commitment that slipped; never use a nickname for a guest;
stop entirely for the day on a 🤫 from anyone; no sarcasm in a message
that carries an action ("Booked." is never ironic).

The persona is the thing that makes it a member and the thing that gets
screenshotted and shared. It costs nothing to build and it is the growth
engine in social groups, so it gets real attention: a library of six
starter personas, two at each of dials 0, 1 and 2, each tested in a real
group of the right kind before shipping. The dial-0 personas
are the ones a company will judge the product by.

## 17. Growth and federation

### The loop

1. One self-hoster installs it for one group.
2. The group uses it. Every member sees it work, daily.
3. A member wants it for another group, often of another kind: the
   engineer wants it for the football club, the flatmate wants it for the
   team at work. Two paths: ask the same host to add the group (one line in
   the admin page) or install their own. The agent answers "how do I get
   you for my other chat" with the install link and the one-line "ask your
   host" alternative.
4. A host with several groups becomes the community's reference user and
   the source of skills, personas and charters.

No growth hacks inside the chat. No "powered by" footers, no invites it
sends on its own. The product is the demo.

### What we measure

One engagement number and one retention number, reported per kind of
group: decisions closed per active group per week, and groups still
active four weeks after install. Spread is hosts with two or more groups.
Revenue follows these three and is not a north star until Cloud exists.

### One wedge per kind

The agent is one thing; the first reason to install it is not.

- Friends and households: the tab. "It keeps the tab, and it also books
  the table."
- Teams: the decision log and "what's outstanding". "It remembers what we
  decided and who promised what, and it listens to whom you say."
- Clubs: attendance and subs. "Who's in Saturday, who's paid."
- Communities: answers from memory. "It answers the question the hundredth
  newcomer asks."
- Committees: the record. "Minutes without a secretary."
- Regulated workplaces on Mattermost and Matrix: the only group agent
  they are allowed to run (section 18).

### Distribution that is measured, not guessed

- The repo: name and description carry "group chat", "AI agent",
  "self-hosted"; topics claim `group-chat-agent`, `group-chat-ai`,
  `splitwise-alternative`, `decision-log`, `team-agent`, `club-management`
  and the per-platform `discord-ai-bot`, `slack-bot`, `telegram-ai-bot`,
  `teams-bot`, `mattermost-bot`, `signal-bot`, `matrix-bot`.
- Listings: awesome-selfhosted (categories "Communication" and "Money"),
  the Discord and Telegram bot lists, r/selfhosted, r/sysadmin for the
  Mattermost crowd. Slack's App Directory does not list self-hosted apps,
  so the repo ships an app manifest and a host creates their own Slack app
  in one click; the same page does it for Discord.
- Three artefacts that travel on their own: the benchmark page, which
  anyone building a group agent has to cite; published charters, which a
  community shares as "how our server runs"; and the schema, which
  integrators link to. All three are in section 18.
- The money and decision queries are the wedges for people who do not
  care about agents, and they are the people who bring the group.

### Federation (v0.3)

Instinct has shipped a personal agent that connects to a group agent, so
this is no longer the far future; it is the thing a self-hosted
alternative needs a position on by the time anyone compares them.

Two layers:

1. **Member agents.** A member who runs their own personal agent (OpenClaw,
   Hermes, anything with an HTTP endpoint) can register it with the group
   agent. The group agent then DMs the member's agent instead of the member
   for availability, preferences and nudges, within what the member allowed.
   This is Instinct's handshake, with the permission living on the member's
   box instead of in a vendor's policy. It also answers "my agent and the
   group's agent" without building a personal agent at all. A member's
   agent speaks with the member's role, never more.
2. **Group to group.** Two groups, each with its agent, arranging a joint
   dinner, a shared cabin, a tournament, a cross-team dependency. The
   agents exchange a signed proposal (availability windows, headcounts,
   constraints, budget range), negotiate within each group's rules, and
   present one outcome to each group for the normal decision flow. Humans
   see the result, not the negotiation.

Transport: A2A, the Linux Foundation's agent-to-agent protocol, with a
signed agent card per group, over HTTPS or Matrix, no central server; the
payload is our published objects. Member agents connect over MCP with
role-scoped tools (section 14). Whether to speak Instinct's protocol, if
they publish one, is a decision for when it exists; the objects are ours
either way. The schema is published in v0.1 (section 18) and the group has
an address from v0.1, so federation is our objects on a transport that
already exists, not a new system.

## 18. Edge and moats

Nobody needs another AI agent, and a group needs one even less: the bot
that answers when mentioned is already free in every chat app. What a
group needs is a member it cannot easily replace. This section lists what
makes Ours that, sorted by how hard each one is to copy, and says what
to build and when. The honest frame first: the model is rented, the
prompts are copyable in a day, a connector in a week, and open source has
no lock-in by design. The moats are the things that compound, the things
that become standards, the positions the incumbents cannot take, and the
proofs nobody else can show.

### What it is instead of another agent

The README's list. Each line is something no agent in a group chat does
today.

1. **Undo instead of approval.** It acts; you take it back with a word.
2. **It listens to whom you say.** It answers everyone and obeys the leads.
3. **A decision log that writes itself.** With the reason and the link.
4. **Commitments without a form.** Said in the chat, chased once, in DM.
5. **The tab, in the chat.** No app, no money moved.
6. **Rooms are barriers.** It sits in the private channel and the shared
   one and never carries a fact across.
7. **The memory outlives the chat app.** Move platforms; keep the record.
8. **It watches first.** A few days of "I would have said" before it says.
9. **The group has an address.** Venues, invites, other agents reach it.
10. **It runs free on a NAS, with any model.** Swap the model, keep the
    member.
11. **It knows what time it is, for everyone.** Nudges land at your hour,
    never at three in the morning, and "thurs" means the same Thursday to
    all of you.

### Counter-positions: what the incumbents cannot copy

- **Act first, undo after.** Instinct and the platform vendors ask first
  because they carry the liability for every group on earth. A host
  carries it for their own six people and can afford to let it act. A
  vendor that copies act-first copies the liability at scale, which is
  why none will. Built in section 13. What keeps it: the hold windows and
  the undo catalogue.
- **Self-hosted, any model, zero marginal cost.** Copilot and Agentforce
  are sold per seat and per credit. A vendor cannot ship the free version
  without ending the paid one. Built in section 14.
- **One member in every room, on every platform, and a memory that
  outlives the platform.** Slack's agent cannot follow the team to
  Discord; Instinct's cannot leave Instinct. "Q, we're moving to Signal"
  re-attaches the group's record to the new room, identities re-link by
  DM, and the decisions and the tab survive the move. The platforms would
  have to help their users leave to copy it. Plan: v0.2.
- **The chat platforms regulated organisations are allowed to use.**
  Mattermost, Matrix and Rocket.Chat run in governments, hospitals and
  defence, where Slack AI and Copilot will never be installed. Mattermost's
  own AI plugin is an assistant in the thread, not a member with a record.
  Nobody is building a group agent there because the buyers are slow, and
  that is exactly why it stays uncontested. Plan: Mattermost and Matrix
  lead v0.2, with the compliance page written before they ship.

### Compounding assets: what gets better with every day and every install

- **The group's record.** Decisions with reasons, commitments, the tab,
  events, and the charter below. Every month it runs the record is worth
  more, and a replacement starts with nothing. The group owns it and can
  export it, which is not a contradiction: a record you own is one you
  keep. Backfill makes day one count. On Slack and Discord, with a lead's
  say-so and one line to the room, the agent reads the last 90 days once,
  extracts events, decisions, commitments, lines that look like money and
  durable facts, stores them with provenance and keeps no transcript. Its
  first state message is "Read the last 90 days. Open: two plans. Owed:
  Sam 48. Last trip: 14 June. Tell me what's wrong." Telegram bots cannot
  read history, so Telegram groups start at zero and say so. Plan: v0.1.
- **The charter.** Every sentence that changed a setting ("Sam's a lead",
  "quiet after 10", "lead decides on money", "never joke about the
  divorce") is rendered into one readable file per group, `charter.md`,
  exportable, importable ("same rules as the Tuesday group") and
  shareable. A group that has written its own rules does not rewrite them
  for a different bot, and a community that publishes its charter is
  advertising. Plan: v0.1.
- **The undo catalogue with a test per action.** Already the boring moat.
  Made harder to copy by requiring every connector and skill action to
  ship a compensating action and a test that proves it, run in CI against
  sandboxes. Contributions accumulate; a fork inherits the catalogue but
  not the contributors. Plan: the contract in v0.1, with email and ICS as
  the first two conforming integrations.
- **The triage signal.** What gets group bots kicked is speaking at the
  wrong time, and the only way to learn the right time is to be told by
  thousands of groups. Opt-in per host, text-free: the score features, the
  classifier's label and the outcome (positive reaction, 🤫, undo,
  ignored), no message text, no IDs, and the exact payload visible in the
  admin page before opting in. Aggregated, published, and used to release
  better classifier weights. No vendor sees "a bot spoke unprompted and
  the group said shut up" at this scale, because no vendor lets a bot
  speak unprompted. Plan: the opt-in in v0.1; the first retrained
  classifier in v0.2.
- **Cross-platform identity and the member's own view.** "Link me" in DM
  on a second platform merges identities, so the agent knows Sam on
  Discord is Sam on Slack, and "my week" in any DM lists the person's
  dates, commitments and balances across every group on the host. The
  platform agents have the group but not the person; the personal agents
  have the person but not the groups. Ours is the only thing that has
  both. Plan: v0.2.

### Standards: own the schema, own the category

- **The schema, published first.** Fact, proposal, decision,
  commitment, ledger line, action and the undo semantics, as versioned
  JSON Schema in the repo. Anything that can produce or consume these
  objects can work with a Ours group. Personal agents integrate with
  Ours because Ours is the thing in the group; a second group agent
  has to speak these objects or explain why not. Plan: v0.1, because the
  typed object model is being written anyway.
- **The group has an address.** An email address per group through the
  mail integration the tier-1 bookings already need, and later a signed
  HTTP endpoint, an MCP server and an A2A agent card on the same server.
  Inbound mail and requests are handled
  as guests: they can create scoped facts with the message as provenance
  ("the venue confirmed 7:30"), they can be relayed to the room in one
  line, and they can never instruct. A venue's reply, an ICS invite, a
  landlord's notice, a CI failure, another group's proposal and a member's
  personal agent all arrive the same way, and the authority model is what
  makes an open inbox safe. Groups have never had an address before; a
  mailing list is the nearest thing. Plan: email in v0.1; HTTP and MCP
  in v0.2; A2A in v0.3.
- **The undo-safe skill contract.** A skill declares its actions, their
  tiers, the role that may trigger each, the compensating action for each,
  and ships a test. Integrators who want to reach groups write to the
  contract, and the contract is Ours's. Plan: the contract in v0.1, the
  registry in v0.2.
- **The benchmark.** The scenario suite that tests Ours (the dinner, the
  print run, the guest, the football, the shut-up, the backfill) is
  published as a benchmark for any group agent, with scores for Ours,
  Astolfo and a plain LLM wrapper. Whoever publishes the test defines the
  category. Plan: v0.1.

### Proofs: what nobody else can show

- **Observe mode.** Added to a room, the agent runs the whole pipeline for
  a period (three days at work, one among friends) and sends nothing.
  Leads get one DM a day: "I would have said... I would have done..." with
  👍 👎 on each line. Then it asks once: "Go?" Nobody ships this, every
  manager wants it, and the thumbs are labelled triage data. Plan: v0.1.
- **Replay.** The log stores every input that produced an action, so
  `ours replay` reruns a group's week through the pipeline with a
  different model or a new classifier and diffs the plans without sending
  anything. A host tests a model change before making it; the project
  proves a release did not make the agent chattier. "We replayed ten
  thousand group-days and the interjection rate fell" is a sentence no
  prompt-only agent can say. Plan: v0.1, and it is the test harness for
  the rest of v0.1.
- **Deterministic where it matters.** The authority model, the tiers, the
  budget, the close rules and the hold windows are code; the model
  classifies and writes prose. The same group behaves the same on a 3B
  local model and a frontier model, apart from the quality of the
  sentences. Swap the model, keep the member, and prove it under
  deterministic simulation (section 14). Plan: the architecture, from the
  start.
- **Minimisation by construction.** Facts, not transcripts; rooms as
  barriers; rights for guests; export in one click; retention per profile.
  A workplace in the EU can read the schema and see there is nothing to
  leak. Plan: v0.1, with the compliance page.

### What is not a moat, and the defence

Each item above can be copied on its own. The defence is that the
compounding ones (the record, the charter, the catalogue, the signal)
cannot be caught up with, the standards only work for whoever publishes
first, the counter-positions require the incumbents to change their
business, and the proofs require an architecture they do not have. The
pace that matters is on the compounding ones, which is why backfill, the
charter and the telemetry opt-in are in v0.1 and not in a later growth
phase.

## 19. v0.1

### Scope

- Discord and Slack connectors, including Slack threads, external users as
  guests, admins and user groups as leads.
- Groups and rooms, the four roles, platform seeding, leads by sentence,
  scoped leads, the request rule and ✅ deferral.
- The first conversation and all eight profiles as default sets.
- Observe mode and the daily "would have" DM with 👍 👎.
- The triage cascade (section 14) and a configurable large model
  endpoint; per-profile thresholds and budgets.
- Decisions: proposals, reaction ballots, native polls, lead decides,
  nudges, close rules, holds, the decision record.
- Memory: facts with scope, rooms, provenance, expiry, forget commands,
  "why". Backfill of the last 90 days on Discord and Slack, lead-triggered.
- Commitments: lines, due nudges, outstanding, the digest; the tab with
  splits, balances and settle-up DMs with payment links.
- Time: member and group zones, resolved time expressions echoed back,
  the calendar with ICS feeds per group and member, availability spans,
  holidays, recurrence, quiet and working hours, room rhythm, lead times.
- The charter file per group, export and import.
- Actions: tier 0 and the tier-1 hold queue with email and ICS as the
  first two conforming integrations, per-profile hold windows and outbound
  policy. Undo by role. The group's email address, inbound handled as a
  guest.
- The published schema and the undo-safe skill contract.
- Persona file, dial 0 to 2, six starter personas.
- The pinned state message per room.
- The event-sourced core on a virtual clock and the deterministic
  simulation harness; the scenario suite, the benchmark page and the
  replay CLI.
- Opt-in, text-free triage telemetry with the payload shown before
  opting in.
- Admin page: groups, rooms, leads, profiles, budgets, persona, memory
  browser, action log, charter, export, wipe.
- A signed static binary and OCI image for Linux, with provenance.

Out of scope for v0.1: Telegram and every other connector, calendar
integrations beyond ICS holds, receipt OCR, the skills registry,
cross-platform identity and "my week", moving a group between platforms,
the HTTP address and federation, the retrained classifier, encryption at
rest, the children mode.

### Order of work

Each milestone ends with something a real group can feel. The order is
by dependency, not by importance.

1. **It can be addressed.** The event log and the virtual clock, groups
   and rooms, roles from platform data, Discord connector, the store, the
   typed objects published as JSON Schema, the skill contract, admin page
   skeleton. It replies, and it knows whom it listens to.
2. **It knows when to shut up.** The triage cascade and scoring, the
   interjection budget, reaction-first behaviour, the first conversation
   and profiles, observe mode. The simulation harness and the scenario
   suite start with the dinner and the football.
3. **It remembers.** Facts, scopes, rooms, provenance, forget, "why".
   Backfill on Discord. "What was that bar" works, nothing crosses rooms,
   and a new install has a past.
4. **It decides.** Zones and time expressions, proposals, ballots, lead
   decides, nudges, close rules, holds, the decision record, the charter
   file. The dinner and the print run both close, and "thurs" lands on
   the right day in every zone.
5. **It keeps the promises and the tab.** Commitment lines, splits,
   balances, settle-up DMs, due nudges at the person's hour inside quiet
   and working hours, the calendar and its ICS feeds, recurrence, the
   digest, the pinned state message.
6. **It acts and takes it back.** Tier-1 hold queue, email out and in (the
   group's address), ICS, undo by role with the catalogue tests in CI, the
   request rule and deferral end to end.
7. **It works at work.** Slack connector: Socket Mode, threads, external
   users as guests, admins and user groups as leads, reactions, backfill
   on Slack. The week at work runs on Slack.
8. **It can prove itself.** Replay CLI, the telemetry opt-in, the scenario
   suite complete and run against Astolfo and a plain wrapper, the
   benchmark page.
9. **It can be shipped.** Personas, docs, the compliance page, release
   candidate. Five real groups of at least four kinds go live, each
   through observe mode first.
10. **It has been used.** The real groups run until every metric below
    has a reading; fixes from what they do; release.

Milestone 8 and the section 18 items inside the others (observe mode,
backfill, the charter, the address, the telemetry opt-in) are not the
part to cut when something slips. Without them it is a good agent, which
is not enough.

### v0.2 and v0.3

v0.2, in this order: Telegram; Mattermost and Matrix with the compliance
page; cross-platform identity and "my week"; "we're moving to"; the HTTP
address and the MCP server; the skills registry; the first weights
retrained on the telemetry with KTO; encryption at rest as a store flag.

v0.3: federation over A2A, both layers; the children mode; Signal; Teams
and Google Chat with their manifests; WhatsApp last, flagged.

### Success metrics, measured in the admin page

- Groups still active four weeks after install, reported per kind (target:
  70% of installs; the product is not done until two kinds hit it).
- Observe-mode conversion: groups that say "go" after watching (target:
  80%; below it the "would have" lines are the problem, and they are the
  cheapest thing to fix).
- Backfill corrections: "that's wrong" per hundred backfilled facts
  (above 10 the extraction is wrong and backfill is off by default until
  it is not).
- Decisions closed per active group per week (target: 2).
- Ratio of unprompted messages that get a positive reaction to those that
  get a 🤫 or "shut up" (target: 5 to 1; below 2 to 1 the triage is wrong).
- Undo rate on tier-1 actions (target: under 10%; above 25% the agent is
  acting on too little).
- Commitments closed by their due date or moved before it (target: 70%;
  the rest is the nudge timing being wrong).
- Time expressions corrected by a member per hundred resolved (target:
  under 5; the echoed date is what makes this measurable).
- Nudges answered within an hour of sending (target: half; below it the
  person's hour is wrong).
- Deferrals per active member per week (target: under 2; above it the
  leads list or the profile is wrong, or members are using it as a
  megaphone, which is worth knowing).
- Guest leaks: zero, tested by a scripted guest, a scripted lead and a
  scripted inbound email before every release.
- Time to first acknowledgement and to reply at the 95th percentile,
  against the standards in section 8.
- Members who run "what do you know about me" and then stay (a trust
  signal; track it). "why" usage per group; a rising rate precedes removal.
- Telemetry opt-in rate among hosts, and the interjection rate across
  classifier releases as measured by replay.

## 20. Risks and open questions

- **Instinct.** A ten-billion-dollar company is in the friend-group seat
  with a cloud product and a waitlist. The self-hosted version does not
  win on features; it wins on the group owning its memory, on undo, on the
  tab, and on working in the chat apps Instinct is not in. If Instinct
  ships group memory and a ledger, the remaining difference is who holds
  the data, which is the difference self-hosters already pay for.
- **Platform agents are free and already there.** Slack's and Microsoft's
  agents ship inside the app. They know one app, belong to one vendor,
  cannot follow the team to Discord or the club to WhatsApp, keep no
  decision record or commitments, and their memory is the vendor's. The
  answer is the same as against Instinct. If they ship a decision log the
  difference is still who holds the data and whether it leaves the
  platform.
- **Moats that are features.** Every edge in section 18 can be copied one
  at a time. The defence is pace on the compounding ones and being first
  with the standards, which is why they are in v0.1 and not after it.
- **The bets.** Turso is pre-1.0, WASI 0.3 is months old, the small
  model zoo changes monthly and the October mixture-of-experts releases
  are unscored. Every bet in section 14 sits behind a trait with a boring
  fallback that reads the same files, and the benchmark picks models, not
  the doc. A bet that has not landed by its milestone is swapped for its
  fallback that day, not debated.
- **Open source eats the cloud.** If self-hosting is easy enough, nobody
  pays. The groups that pay are the ones without a host, and the companies
  that pay want policy and support, not code. Supabase, n8n and Mattermost
  are the proof the pattern funds itself. Being too good at the one-click
  self-host is the only way to fail it, and that is a risk worth having.
- **Cloud contradicts the pitch.** "A box you own" and "a cloud we run" sit
  in one sentence. Cloud is a host like any other: the same rights, the
  same export, the same single file, and a group can leave for its own
  box with one download. That is why it can be trusted and why it sells.
- **Platform policy.** Slack, Discord or Telegram could restrict what bots
  may read. Breadth is the hedge: eight platforms, and the two that matter
  most to regulated buyers are open protocols nobody can switch off.
- **Telemetry trust.** The self-hosting audience is allergic to phoning
  home. Opt-in only, off by default, text-free, the payload shown in the
  admin page before opting in and published in the docs. One slip here
  ends the project's standing with the only audience it has.
- **The open inbox.** An email address per group is an injection surface.
  Inbound is a guest: it can add scoped facts and be relayed in one line,
  never instruct, never reach the large model as an instruction. Tested
  with a scripted hostile email before every release.
- **Backfill consent.** Members did not agree to have 90 days read. It is
  lead-triggered, announced in the room in one line, reads only rooms the
  agent is in, keeps no transcript, and "forget" applies to it like
  anything else. In team profiles the announcement cannot be turned off.
- **The boss's spy.** If staff believe it reports upward it is removed, or
  worse, kept. Section 5's rule, the join message that states it, and a
  scripted lead asking for a member's patterns in the release checklist.
- **Register mismatch.** One dial-2 joke in a workplace room ends the
  product in that company. Profile defaults, "action lines are never
  ironic", and no dial above 1 without a lead asking by sentence.
- **Guest leakage.** One fact from a private room said to a client. The
  room rule in section 9, tested with a scripted guest before every
  release.
- **Authority spoofing and injection.** Covered by code-level enforcement.
  The residual risk is a compromised lead account, which is the platform's
  problem and is logged as ours.
- **"All kinds" means "no kind".** The classic failure of a general tool.
  The engine never branches on kind; friends and team must each hit the
  retention target in v0.1 before a third kind is marketed; one wedge per
  kind, never one message for all.
- **Annoyance is the product risk.** A group bot that talks too much gets
  removed in a day. The interjection budget, reaction-first behaviour and
  observe mode are the mitigation; the metric above is the alarm.
- **Wrong memories cause social harm.** "Sam said the negroni was a war
  crime" is funny when true and awkward when misattributed. Provenance on
  every recalled fact and a low bar for "that's wrong" are the mitigation;
  the agent never recalls a quote without a source link.
- **Consent of members who never asked for this.** One person adds it to a
  chat of eight, or one manager adds it to a channel of forty. The join
  DM, member rights and the private scope are the answer, and the README
  has to say that the host is responsible for their group.
- **Compliance.** Workplaces ask about retention, export and residency.
  Self-hosting answers residency; per-profile retention and the export
  answer the rest; legal hold is "turn off expiry". All documented before
  the first workplace install.
- **The regulated niche is slow.** Governments and hospitals take a year
  to buy anything. The niche is uncontested for the same reason. It is a
  position, not a revenue plan.
- **Teams and Google Chat.** Both need a cloud app registration that a
  self-hoster will find tedious. Ship manifests and a page, and accept
  that most workplace installs will be Slack, Mattermost and Discord.
- **WhatsApp.** The largest group-chat platform has no bot API for this
  use and bans unofficial clients. It ships last, flagged, with the risk on
  the host.
- **Cost surprises.** A cloud model with no cap and a busy group is a bill.
  The per-group cap defaults on and the router prefers the small model.
- **The host as surveillance.** A host can read everything. This is true of
  any self-hosted chat tool and is stated, not solved. Encryption at rest
  with a host-held key is v0.2 and does not change the trust model.
- **Legal around money.** Keeping a ledger is not a financial service;
  moving money would be. The line is drawn in section 11 and does not
  move.
- **Open: should the agent have a face?** An avatar and a name per group
  are cheap and increase the "member" feeling. Voice messages on Telegram
  and Discord are a later experiment.
- **Open: multi-host groups.** When two members each run a host and both
  add the agent, the second should detect the first and defer. Needs a
  cheap presence protocol; defer to federation work.
- **Open: one group across platforms.** A community with a Discord and a
  Telegram bridged together is one group in two places. The rooms model
  allows it; whether the connectors can match identities well enough is
  unknown until the "link me" work in v0.2.
