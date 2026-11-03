# Changelog

## 0.1.0-rc.1

First release candidate. Linux x86_64 only.

### Platforms

- Discord connector: gateway, threads, native polls, reactions, DMs,
  pins, 90-day backfill, roles read as owner, admin and moderator.
- Slack connector over Socket Mode with no public URL: threads, reaction
  ballots, user groups as leads, external and guest accounts as guests, an
  app manifest from `ours manifest`.
- Invite link for Discord from `ours invite`.
- Telegram connector over the Bot API with long polling and no public URL:
  groups and supergroups, forum topics as threads, admins and the creator as
  leads, native polls, reactions, DMs, the roster learned as people speak.
- A reaction vocabulary per platform: where a platform restricts reactions
  (Telegram), the engine uses stand-ins for ✅, 🤫, 🙋, 📌, 🙅, ✌️ and ❌ and
  reads the same ones back; the charter says which.
- The first conversation recovers when the opening DM is refused, and a
  lead or host can say `setup` by DM to start or take it over.
- The first conversation asks what to call the bot before anything else;
  every group names its own, and a name chosen there survives restarts.
  Until then it answers to Noa.

### Who it listens to

- Hosts, leads, members and guests, resolved in code before the model is
  called. Leads from the platform's roles, a named list and sentences.
- Requests from members wait for one lead's ✅; guests are answered and
  never obeyed.
- Observe mode for a new group, with a "would have" report to leads.
- The first conversation: three DMs to whoever added it and one line to the
  room.

### Group memory

- Facts with scope, rooms, sources, confidence and expiry.
- `what do you know about me`, `forget that`, `forget me`, `that's wrong`,
  `remember that`, `just for this week`, and `why`.
- The journal is redacted when something is forgotten.

### Decisions, commitments and the tab

- Proposals with reaction ballots or native polls, close rules, nudges,
  deadlines, re-votes and an immutable decision record with a Markdown
  log.
- Attendance, commitments with DM nudges, and the outstanding list.
- The tab with splits, balances and settle-up DMs. No money moves.

### Time

- Member and group zones, times resolved and echoed back, availability,
  recurrences, quiet and working hours, a digest and a day-of summary.
- Calendar feeds per group and per member.

### Actions and undo

- Tier 0 actions at once, tier 1 held for a window per kind of group.
- Email from the group's address over SMTP and IMAP, or in files for a
  trial. Replies arrive as a guest and never instruct.
- `undo` by role, with a catalogue of how each kind of action is taken back.

### Persona

- The dial from 0 to 3, hard rules at every dial, running bits.
- Defaults for eight kinds of group.

### Host tools

- A single binary with `run`, `check`, `model-check`, `schema`, `replay`,
  `telemetry`, `bench`, `manifest` and `invite`.
- The admin page with a live feed, charter, memory, decisions, tab,
  actions, persona and profile, and a compliance page with export, legal
  hold and wipe.
- `ours replay` to check a journal against the engine, and `--live` to
  compare a different model or build.
- Opt-in, text-free triage telemetry with the payload shown first.
- Published JSON Schemas for the engine's objects and the connector events.
- The benchmark: fourteen scenarios and a scorer any group agent can use.

### Packaging

- A release tarball with checksums, an OCI image, a systemd unit and a
  Podman quadlet, built from a tag with provenance attestations.

### Not in this release

- Connectors other than Discord, Slack and Telegram.
- Calendar integrations beyond ICS feeds, receipt photos, a skills
  registry, encryption at rest, a children mode and federation.
- Changing settings from the admin page.
