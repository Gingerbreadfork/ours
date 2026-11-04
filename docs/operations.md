# Operations

## What is on disk

Everything is under `data_dir`:

| Path | What it is |
|---|---|
| `<group id>.db` (with `-wal` and `-shm` while running) | The group: its hash-chained journal of inputs and snapshots of state. SQLite. |
| `<group id>.charter.md` | The group's settings as Markdown, rewritten when they change. |
| `<group id>.decisions.md` | The decision log, rewritten when a decision closes. |
| `telemetry/<date>.jsonl` | Telemetry records, only when telemetry is on. |
| `telegram/polls.json` | The agent's open Telegram polls and their votes, so votes count across a restart. Only with a Telegram connector. |
| the `[mail] dir` folder | Only in mail dir mode. |

There is no other state. The `.md` files are views and can be deleted.

## Back up

One group is one file. Stop the agent and copy the `.db` file, or take a
consistent copy while it runs with SQLite's own tool.

    sqlite3 data/the-six.db ".backup 'the-six.bak'"

Copying only the `.db` file of a running agent can miss what is still in
the `-wal` file. To restore, stop the agent, put the file back as
`<group id>.db`, delete any stale `-wal` and `-shm` beside it, and start.
The state is rebuilt from the newest snapshot and the log after it.

## Upgrade

1. Read the CHANGELOG.
2. Back up.
3. Replace the binary (or pull the new image) and restart.
4. Run the replay check below on a copy of the data.

The journal and snapshots are read by the new version. Nothing is migrated
by hand. The first start of a new version may add an index to each
journal, which takes a moment on a large file.

## Checking the journal

Each group's journal is a hash chain. Every start walks it and logs
`journal chain verified` with the record count and the head hash, or an
error naming the first record that does not match. `ours check` does the
same for every group and fails if any chain is broken. Write the head hash
down somewhere outside the machine from time to time: a chain that still
verifies but has a different head was rewritten.

## Logs

The agent writes structured logs to standard error. Under systemd they are
in the journal: `journalctl -u ours -f`. `RUST_LOG` sets the level
(`info` by default; `RUST_LOG=ours=debug` for more). Each inbound event
has its own span. The admin link and its QR code are logged at start.
Tokens and passwords are redacted wherever the configuration is printed.

`GET /healthz` on the admin port answers `ok` without a token, for a
supervisor.

## Signals and snapshots

- SIGINT and SIGTERM stop the process. Every group finishes what it is
  doing, writes a snapshot and exits. The process waits up to ten seconds.
- A snapshot is also written every 200 inputs, so a restart replays only the
  tail. Only the newest two are kept and their space is reused. A file from
  an earlier version keeps its size until you run `sqlite3 <group id>.db
  VACUUM` with the agent stopped.
- If a connector stops for good (for example Slack tokens revoked), the
  whole process stops with a message, on purpose, so the failure is seen.
  The unit file restarts it, and it will keep stopping until the cause is
  fixed.

## Replay as a check after an upgrade

`ours replay` re-runs a group's journal through the engine without
sending anything and reports any difference.

    cp -r data /tmp/check-data
    # copy ours.toml to check.toml and set data_dir = "/tmp/check-data"
    ours --config check.toml replay the-six

With no options the replay uses the answers the journal logged, so it is a pure
re-fold, and any difference in what the engine does is a determinism bug
worth reporting. Options:

| Option | What it does |
|---|---|
| `--live` | Answer the engine's requests with the configured model, classifier, extractor and time resolver instead of the log. It shows what a different model or build would have said and done over everything that happened. Nothing is sent. |
| `--from <seq>` / `--to <seq>` | Compare only that range of the journal. |
| `--out <file>` | Write the Markdown report to a file. |
| `--json` | Print the report as JSON. |

Run it against a copy, since the replay opens the group's file.

## The benchmark

The published scenarios and the scorer are in the source repository under
`benchmark/`. It runs fourteen scenarios of group life and scores recall,
precision, silence, budget, content and actions.

    ours bench --agent ours --agent wrapper --agent rules \
      --scenarios benchmark/scenarios --out benchmark/RESULTS.md

`ours` runs the real engine with your configured models. `wrapper` is a
plain model-per-mention baseline on the same model. `rules` runs Ours
with no model at all and needs no configuration. Any other bot is scored
from recorded transcripts with `--agent transcript:<path>`; see
`benchmark/README.md` and `benchmark/FORMAT.md`. Add `--transcripts <dir>`
to keep what each agent did.

The scores for this release are in `benchmark/RESULTS.md`.

## Telemetry

Off unless `[telemetry] enabled = true`. It exists to improve the triage
classifier, and what it sends contains no text.

- One record per message the classifier scored, built by re-folding the
  journal.
- Each hour a batch is appended to `data_dir/telemetry/<date>.jsonl`, one
  record per line. If `endpoint` is set, the same batch is posted there as
  a JSON array. Without an endpoint nothing leaves the machine.
- `ours telemetry --show 5` prints the first five records from your own
  journal and sends nothing. Add `--group <id>` to pick a group; without it
  the first group is used.

This is the exact shape of a record:

    {
      "host": "3f9c2a7d1e5b8c40",
      "group": "a81d4c0e9b7f2361",
      "kind": "friends",
      "dial": 1,
      "at_hour_utc": 18,
      "score": {
        "addressed": 0.0,
        "open_decision": 0.0,
        "plan_forming": 0.0,
        "unanswered_question": 0.6,
        "fact": 0.0,
        "commitment": 0.0,
        "banter": 0.0,
        "kind": "question",
        "answerable_from_context": true,
        "source": { "type": "small_model", "model": "gemma-4-e2b" }
      },
      "rules": {
        "addressed": 0.0,
        "open_decision": 0.0,
        "plan_forming": 0.0,
        "unanswered_question": 0.6,
        "fact": 0.0,
        "commitment": 0.0,
        "banter": 0.0,
        "kind": "question",
        "answerable_from_context": false,
        "source": { "type": "rules" }
      },
      "acted": "raised_hand",
      "outcome": {
        "positive_reactions": 2,
        "shushed": false,
        "undone": false,
        "answered_by_human": false,
        "window_hours": 24
      }
    }

- `host` is a random id made once per host, 16 hex characters.
- `group` is the first 16 hex characters of SHA-256 over a random salt and
  the group id. The salt is made once per group, kept in the group's file
  and never sent, so the tag cannot be traced back to the group or matched
  across hosts. A host that sent telemetry before this release gets a new
  tag for each group.
- `kind` is the group's kind and `dial` its persona dial, 0 to 3.
- `at_hour_utc` is the hour the message was scored, 0 to 23.
- `score` is the six signals, banter, the message kind (question,
  instruction, statement, greeting or other), whether it was answerable from
  context, and where the score came from: `{"type": "rules"}` or
  `{"type": "small_model", "model": <the model's name>}`.
- `rules` is the rules pass's score for the same message, same fields.
- `acted` is the most the agent did about it: nothing, reacted,
  raised_hand, opened_poll, chimed or spoke.
- `outcome`, over the `window_hours` after the message, counts reactions
  from people on what the agent said or, after a 🙋, on the question, other
  than 👎 and 🤫; whether anyone 🤫'd it or said "shut up"; whether what it
  did was undone; and whether another person answered the question first.

No message text, message ids, user ids, room ids or names are ever in a
record. The compliance page prints the same sample.

## Spend

`daily_cap_usd` limits model spend per group per UTC day. The chat model
and the small model count against the same cap, even when `[classifier]`
uses its own endpoint. The meter is in memory and starts at zero on
restart. The admin page does not chart spend in
this release; watch your model provider, or run a local model.
