# Slack

The Slack connector uses Socket Mode, so the host needs no public URL and
no request URL. It needs two tokens.

## Make the app

    ours manifest

prints the app manifest. It is the same file as `manifests/slack.yaml`,
with the steps in its comments.

1. At api.slack.com/apps choose Create New App, then From an app manifest.
   Pick the workspace and paste the manifest.
2. Optionally change `display_information.name`, and set
   `bot_user.display_name` to the agent's name. Keep it the same as
   `agent_name` in `ours.toml` (default `Noa`).
3. Install the app to the workspace (Install App in the sidebar).
4. Copy the Bot User OAuth Token (`xoxb-...`) from OAuth & Permissions.
   This is `token`, or `OURS_SLACK_BOT_TOKEN`.
5. Under Basic Information, App-Level Tokens, choose Generate Token and
   Scopes, add `connections:write` and generate it. It starts `xapp-`. This
   is `app_token`, or `OURS_SLACK_APP_TOKEN`. Without
   `connections:write` the connector cannot connect.

The scopes in the manifest are channel, private channel, DM and group DM
history and read, `chat:write`, reactions read and write, `pins:write`,
`users:read`, `usergroups:read` and `files:read`.

## Configure

    [[connectors]]
    platform = "slack"
    agent_name = "Noa"
    # token and app_token from the environment
    leads_usergroup = "leads"

    [[groups]]
    id = "product"
    name = "Product"
    kind = "team"
    rooms = [
      { platform = "slack", id = "C0123456789" },
      { platform = "slack", id = "C0987654321" },
    ]

A Slack channel id is in the channel's details, at the bottom, or in the
channel link. A workspace with several channels can be one group or
several; list the channels that belong together under one group and they
share one memory, with the room barrier keeping what was said in a private
channel out of the others.

## Invite the bot to channels

Slack only sends messages, reactions and joins for channels the app is a
member of. In each channel type `/invite @Noa`. DMs work without an invite.

## Who is a lead

- Workspace owners are owners and admins are admins.
- Set `leads_usergroup` to a user group handle (or its id) and its members
  count as admins for the leads rule. The group is read at startup.
- You can also name leads in the config or by sentence.

For team, project and committee groups, the default rule makes owners and
admins leads, plus anyone you name. Friends and household groups make
everyone a lead.

## Guests and Slack Connect

Slack marks single-channel and multi-channel guests, Slack Connect people
from other workspaces, and anyone whose home is another workspace. The
connector reports all of them as external, and the engine treats external
accounts as guests whatever any list says. A guest is answered from what
was said in the room they are in, is never obeyed and is never counted in a
decision. An owner of another workspace in a shared channel is still a
guest here. An account the connector could not look up is a guest until it
is known.

## Threads, polls and ballots

Each Slack thread is a room of its own under its channel. The agent
replies in the thread that started a conversation.

Slack has no native polls, so ballots are reactions. `Noa, poll thursday or
friday` posts the options with a numbered legend, and the group votes by
reacting 1️⃣, 2️⃣ and so on. Changing a reaction changes the vote.
Attendance uses a single 👍. The agent maps reaction names to emoji for
you.

## When it stops

If the app is uninstalled or its tokens are revoked the connector stops the
process with a message to reinstall it and update both tokens.
