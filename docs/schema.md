# The schemas and the skill contract

Ours publishes the shape of its own objects as JSON Schema, so other
programs can read its event log, write connectors for it and check what it
does. The files live in `schema/` in the repository and are written by the
binary.

    ours schema            # writes ./schema
    ours schema some/dir

The schemas are generated from the types the engine uses. They are the
types, not a description of them, so they cannot drift.

## What is in `schema/`

| File | The object | Use it to |
|---|---|---|
| `input.schema.json` | `Input` | Read the journal. Every message, timer firing, model answer, mail, admin command and outside result enters the engine as one of these, and the journal is a log of them. |
| `effect.schema.json` | `Effect` | See what the engine asked for: send, react, DM, hold, notice. |
| `command.schema.json` | `Command` | Know which configuration changes the host or the admin page can make, such as set the kind, add a lead or change the dial. |
| `plan.schema.json` | `Plan` | Check the model's answer to one addressed message. This is the schema sent to the model as the response format. |
| `group_state.schema.json` | `GroupState` | Read a snapshot of a group's state. |
| `action_record.schema.json` | `ActionRecord` | Read the audit log: what was done, on whose instruction, approved by whom, at which tier and how it is taken back. |
| `catalogue_entry.schema.json` | `CatalogueEntry` | Read the undo catalogue, one row per class of action. |
| `inbound_event.schema.json` | `InboundEvent` | Write a connector: everything a platform can tell the engine. |
| `outbound.schema.json` | `Outbound` | Write a connector: an action with the id the engine matches its result to. |
| `capabilities.schema.json` | `Capabilities` | Write a connector: what the platform can do (reactions, threads, polls, DMs, edit, delete, pin, history, timezones, external flag, typing). The engine falls back where one is missing, such as reaction ballots where there are no polls. |
| `outside_action.schema.json` | `OutsideAction` | Write an integration: what leaves the group. Every variant has a compensating variant or is its own compensation. |
| `inbound_mail.schema.json` | `InboundMail` | Write an integration: mail that reaches the group's address, always as a guest. |

Ids are opaque strings. Nothing in the schema depends on a platform's id
format.

## Using them

- To build a connector, implement the `Connector` trait in the
  `ours-connector` crate and speak `InboundEvent`, `Outbound` and
  `Capabilities`. The Discord and Slack connectors are the examples, and
  `SimConnector` is the in-memory one used by the tests.
- To read a journal from outside, read the `input` records with the
  `input.schema.json` and the `group_state` snapshots with
  `group_state.schema.json`. A redacted record keeps its place and hash and
  shows `[forgotten]` where the text was.
- To check a build, regenerate the schemas and diff them against the
  checked-in copy. A change to a published type shows up as a change to a
  file here.

## The undo-safe skill contract

An integration that wants to reach groups, such as a booking service, an
accounting tool or a ticketing system, writes to this contract. A skill:

1. declares each action it can take;
2. gives each action a consequence tier: 0 free, 1 windowed, 2
   irreversible;
3. names the role that may trigger each action;
4. names the compensating action for each, or says plainly that there is
   none, in which case it is tier 2 and asks one person first;
5. ships a test that does the action and takes it back.

The tiers behave as follows. Tier 0 is done at once and undone completely.
Tier 1 leaves the group after a hold window (two minutes among friends, ten
at work) so that `undo` can stop it, and after sending, undo issues the
compensating action and reports whether it is likely to work. Tier 2 asks
exactly one person, in DM, with a reaction: the lead whose money, account
or responsibility it is. The group is never asked.

The pieces of this that exist in the code in this release are the
schemas above: `catalogue_entry` for the declaration of an action, its tier
and how it is taken back, `outside_action` for what leaves the group and its
compensation, and `action_record` for the audit of each use. The built-in
catalogue has eleven classes (messages, reactions, polls, pins, DMs, facts,
commitments, ledger lines, calendar entries, email and undo itself), and
email is the one integration that leaves the group. It is visible on the
admin page under Actions, "What can be undone".

Not in this release: loading a third-party skill, a skills registry, and
tier-2 integrations. Nothing in the catalogue is irreversible yet.
