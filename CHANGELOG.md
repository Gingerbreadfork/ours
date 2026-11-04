# Changelog

## 0.0.1

First release. Linux x86_64 and aarch64.

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

### Fixed since the earlier builds

#### Privacy and authority

- A member's `that's wrong` or `remember that` about someone else's fact
  waits for a lead's ✅, as `forget that` does. A lone `that's wrong`
  applies to the agent's last message only if it was recent or is the
  latest in the room.
- A card number said anywhere is never kept: the message is not read, it
  is scrubbed from the journal, and its author is told why. A payment link
  said in a room is scrubbed from the journal too.
- A lead over one room is a lead only there, not in DMs.
- New members get one DM on joining saying what Ours keeps, whom it
  listens to, that the host sees everything, and how to see or delete what
  it holds about them.
- The model label in the pinned message, charter and prompt says where the
  model runs, and names the hosted service when it is not on the host's
  network.
- The action log keeps its records for the whole retention period. Past
  10,000 records the oldest go early, with a notice, and never under a
  legal hold.
- `forget me` stops any settle-up waiting on you.
- Deleting a message takes its words out of `why`.
- The telemetry group tag rests on a per-group salt that never leaves the
  host.

#### Mail

- A sender counts as verified only when the provider's DMARC check passes
  for the From domain, or, with no DMARC verdict, a DKIM or SPF pass
  aligned with it.
- Mail is marked read only after every group has kept it, and a mail
  delivered twice is read once. In dir mode an unreadable file moves to
  `in/failed`.
- An unverified reply never overwrites a fact held more surely.

#### Decisions, the tab and time

- `settle up` settles the tab where it is asked; a room's settle-up leaves
  other rooms' tabs alone.
- A member's calendar feed includes entries a lead decided.
- A re-vote won by an option with no date takes the old date off the
  calendar.
- A poll whose option turns out to be soon closes in time.
- Undoing a lapsed poll says it lapsed.
- The same promise for another day is kept as a second one.
- An addressed message that only mentions an option is not counted as a
  vote.
- Setting up a recurrence again changes the one there.
- Working hours can run past midnight.
- "in November" said in November runs from today.
- Whole-day entries follow daylight saving changes.
- `what do I owe` counts every currency, and `not me` from someone outside
  a split changes nothing.
- A digest line lists five items and counts the rest.
- A failed edit of the pinned message is tried again, and a lost one is
  posted again.
- `why please` works.
- Due times written with seconds or an offset are no longer dropped.

#### Platforms

- Discord: an edit fetched from a server outside the allowlist, or from a
  channel the bot does not know, is no longer taken for a DM. A removal
  during a server outage still ends its rooms. Members load correctly when
  their chunks arrive out of order. A forum's id is logged when its
  threads first sync.
- Discord and Telegram: a long reply that fails partway is kept as sent.
- Slack: reactions on thread messages land in the thread after a restart,
  edited messages stay plain text, and a refreshed connection syncs rooms
  again.
- Telegram: votes on a poll opened before a restart still count, and a
  member seen for the first time after a restart is not taken for someone
  who just joined.

#### Host tools

- Every start and `ours check` verify each journal's hash chain and log
  its head hash.
- Only the newest two snapshots are kept, and the compliance page reads an
  index instead of the whole journal.
- `ours check` refuses two connectors for one platform and platforms that
  are not built. `daily_cap_usd` must be above zero, and a classifier on
  its own endpoint shares the same cap.
- Stopping waits for the groups to save, then stops everything else.
- After a wipe, calendar links use the new secret.
- The admin live feed no longer drops an event that reads like an earlier
  one.
- `ours replay --live` pairs each time answer with its own question.
  `ours bench` refuses scenario times past the clock, warns about
  transcripts for no scenario, creates the `--out` directory, and no
  longer scores a refused mail as held.

#### Packaging

- The release image is published only after the checks pass, a tag must
  match the version, and the toolchain is pinned.

### Not in this release

- Connectors other than Discord, Slack and Telegram.
- Calendar integrations beyond ICS feeds, receipt photos, a skills
  registry, encryption at rest, a children mode and federation.
- Changing settings from the admin page.
