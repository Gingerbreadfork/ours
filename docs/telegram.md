# Telegram

Ours joins a Telegram group as a bot, through the Bot API with long
polling. No public URL, no webhook, no port to open. It reads the group,
answers when addressed, opens native polls, reacts, pins, and talks to
members in private chats.

## Make the bot

1. Message [@BotFather](https://t.me/BotFather), send `/newbot`, and give it
   a name and a username ending in `bot`. The reply holds the token.
2. Send `/setprivacy`, pick the bot, and choose **Disable**. With privacy
   mode on, Telegram shows a bot only the messages that mention it, and the
   agent cannot read the room it is a member of. If the bot is already in a
   group when you change this, remove it and add it again.
3. Optionally `/setdescription` and `/setuserpic`. The agent's name in the
   room comes from `agent_name` in the config, not from BotFather; people
   can address it by that name or by its @username.

Put the token in `token` under `[[connectors]]` with `platform = "telegram"`,
or set `OURS_TELEGRAM_TOKEN`.

## Press Start first

Telegram only lets a bot message someone who has messaged it first, and
the first conversation is a DM to whoever adds the bot. So before adding
it, open `t.me/<botname>` and press Start. The same goes for anyone who
wants "what do you know about me" or a settle-up DM later: open the bot
once.

## Add it to the group

Add the bot to the group as an **administrator** with the rights to pin
and to delete messages, and not as an anonymous admin. Administrator
status is also what makes Telegram deliver reactions and member changes
to it. The connector warns at startup if privacy mode is still on.

Telegram does not let a bot list a group's members. The agent knows the
administrators from the start and learns everyone else the first time they
speak, so the leads list is right from the first minute and the roster
fills in over the first few days. The creator is the owner and
administrators are admins, which is what seeds the leads list for every
kind of group; see [personas.md](personas.md) for the rule per kind.

## Tell Ours which group

The group's chat id goes under the group's `rooms`:

    [[groups.rooms]]
    platform = "telegram"
    id = "-1001234567890"

To find the id, promote the bot first (promoting a bot in a basic group
can turn it into a supergroup, which changes the id), run the agent, and
send any message in the group. The log prints the chat's id and title with
the line "room in no group; add its id under a group's rooms to include
it". Add the id, restart, and the configured group syncs at startup.
Topics join on their own. If Telegram migrates the group later, a warning
in the log names the new id.

## Topics, polls, reactions, DMs

- **Forum topics** in a supergroup are threads to the agent: each topic is a
  room of its own under the group, with the group's roster.
- **Polls** are native, non-anonymous, two to twelve options of up to a
  hundred characters. Reacting to a poll message is not voting there; people vote in
  the poll. Anything the engine cannot express as a Telegram poll falls back
  to a reaction ballot. The agent keeps a record of its open polls in
  `telegram/polls.json` under the data directory, so a poll opened before a
  restart still counts the votes cast after it. Once a poll ends, its votes
  are dropped from that file.
- **Reactions** work both ways within Telegram's fixed set of about seventy
  emoji. 👀, 👍 and 👎 are in it; ✅, 🙋, 📌, 🤫, 🙅, ✌️ and ❌ are not, so the
  agent uses stand-ins and reads the same ones back from members: 👍
  approves a request, 😴 stands it down for the day, 🫡 means it has the
  answer, ✍ marks a promise noted, 🙈 takes you out of a split, 🤝 counts
  you twice, 👎 drops a promise. The group's charter lists them. Digit
  reactions do not exist, which is why ballots on Telegram are polls.
- **Private chats** are the agent's DMs. Telegram only lets a bot message
  someone who has messaged it first, so the first conversation, "what do you
  know about me" and the settle-up DMs need each person to open the bot once
  and send `/start`. The agent says so in the room when a DM cannot be sent.
- **History** is not available to bots, so there is no backfill on Telegram;
  the agent starts remembering from the moment it is added. After a restart
  Telegram redelivers up to a day of updates the agent had not confirmed,
  with their original times.
- **Someone who has never spoken in the group** is not yet on the roster,
  so a DM from them cannot be placed in a group until they have.
- **Mentions** are `@username` or a reply to one of the agent's messages.
  Messages are plain text; the agent never sends keyboards or buttons.

## What leaves your machine

Only the Bot API calls to `api.telegram.org`: the updates it polls for and
the messages, polls and reactions it sends. The model endpoint is yours to
choose, as everywhere else.
