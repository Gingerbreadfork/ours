# Discord

The Discord connector uses the gateway, so the host needs no public
address. It reads messages, reactions and polls in the servers it is in,
and acts through the REST API.

## Make the application

1. Open the Discord developer portal and choose New Application. The name
   is only what the portal shows. The name people see in a room is the
   bot's username; set it to match `agent_name` (default `Noa`).
2. Under Bot, choose Reset Token and copy it. This is the bot token. Put it
   in `/etc/ours/env` as `OURS_DISCORD_TOKEN`, or in `token` under the
   connector.
3. Still under Bot, turn on both privileged intents:
   - Server Members Intent, to learn who is in the room and what role each
     person holds.
   - Message Content Intent, to read what is said.

   Without them Discord refuses the connection.
4. Copy the Application ID from General Information.

The connector asks Discord for these gateway intents: guilds, guild
members, guild messages, message content, guild message reactions, direct
messages, direct message reactions, and guild and direct message polls.

## Add it to a server

    ours invite --client-id <application id>

prints the invite link. Open it, pick the server and approve. The link asks
for these permissions and no more: View Channels, Send Messages, Send
Messages in Threads, Read Message History, Add Reactions, Manage Messages,
Pin Messages and Create Polls. It does not ask for Administrator and does
not ask for Embed Links, because the agent only writes plain text. Manage
Messages is there for servers that still tie pinning to it.

If you run with `guilds = ["<server id>"]` under the connector, the bot
serves only those servers.

## Point a group at a channel

Turn on developer mode in Discord (User Settings, Advanced). Right-click a
channel and choose Copy Channel ID, then list it under the group:

    [[groups]]
    id = "the-six"
    name = "The Six"
    rooms = [{ platform = "discord", id = "123456789012345678" }]

Restart `ours run`. In the admin page's Feed you should see
`connected to Discord as Noa`, and the agent starts the first conversation
with whoever added it. See [install.md](install.md).

## Who is a lead

Discord has no admin flag the way Slack does, so the connector reads
permissions.

- The server owner is an owner. Anyone with Administrator is an admin.
- Anyone who holds both Manage Messages and Moderate Members, through any
  role, counts as a moderator.

What each of those means depends on the group's kind. In a friends or
household group everyone is a lead. In a team, project or committee,
owners and admins are. In a community, moderators are too. Club,
committee and cohort groups take only the named leads. You can always name
leads in the config with `leads = [...]` (Discord user ids) or by
sentence: `Noa, Sam and Jo are leads`. Discord has no notion of external
accounts, so guests exist only when a lead says so.

## Threads

The agent treats a Discord thread as a room of its own under the channel it
hangs from. It replies in the thread that started a conversation, and
answers in a thread when it is spoken to there.

## Polls

Discord has native polls and the agent uses them. `Noa, poll thai or pizza`
creates a poll with the two options as answers, a day long by default, and
the agent counts the votes from Discord. A poll takes two to ten options,
a question up to 300 characters, and each option up to 55. Attendance
questions (`Noa, who's in for Saturday`) use a single 👍 reaction instead,
since a poll needs two answers.

## When it does not connect

- A message that the connection was refused or closed with an auth error
  almost always means a privileged intent is off, or the token was reset.
- The bot is in the server but silent: check that the channel id is under
  the group's `rooms`, and that the bot can see the channel.
- A connector that gives up stops the whole process on purpose, loudly,
  rather than leaving an agent that can never hear anything.
