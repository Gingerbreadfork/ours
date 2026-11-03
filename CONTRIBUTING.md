# Contributing

Ours is a Rust 2024 workspace. DESIGN.md is the plan; read the sections
named below before touching a crate. This file is the engineering contract.

## Crate map

| Crate | Owns | DESIGN.md |
|---|---|---|
| `ours-types` | ids, enums, `Instant`, the connector event model (`InboundEvent`, `Outbound`) | 5, 14 |
| `ours-connector` | the `Connector` trait and `SimConnector` | 14 |
| `ours-core` | the engine: group state, the pure `step` fold, authority, triage, timers | 5, 7, 9, 10, 11, 12, 13 |
| `ours-store` | the event log and snapshots on SQLite | 14 |
| `ours-connector-discord` | Discord through twilight | 14 |
| `ours-connector-slack` | Slack through Socket Mode | 5, 14 |
| `ours-connector-telegram` | Telegram through the Bot API, long polling | 5, 14 |
| `ours-model` | the large-model client and the rules triage | 7, 14 |
| `ours-admin` | the axum admin server and its embedded page | 8, 14 |
| `ours-mail` | the group's email address: SMTP out, IMAP in | 13, 18 |
| `ours` | the binary: config, wiring, CLI, replay, benchmark, telemetry | 14 |

Add dependencies to your crate's `Cargo.toml`; use `workspace = true` when
the root already lists the crate, otherwise pin a version in your crate.

## Rules that are not negotiable

- The engine is a pure fold. `ours-core` does no I/O, reads no clock and
  spawns nothing: `step(&mut state, input, now) -> Vec<Effect>`. Every
  outside fact (a message, a timer firing, a model reply, an admin
  command) arrives as an `Input`; everything it wants done leaves as an
  `Effect`. That is what makes replay, undo and deterministic tests
  possible. If you need randomness, take a seed from the input.
- Authority is code, not prompt. The speaker's `Role` and the permitted
  action set are computed before any model call and enforced after it.
- Nothing in a room is ever an embed, card or button. Plain text.
- No `unwrap` or `expect` outside tests; errors are `thiserror` types per
  crate, `anyhow` only in the binary and in tests. The workspace lints
  deny them everywhere, so put
  `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`
  at your crate root and `#![allow(clippy::unwrap_used, clippy::expect_used)]`
  at the top of each file under `tests/`.
- `tracing`, never `println`. One span per inbound event.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test` must pass. `scripts/check.sh` runs all three for the
  workspace.
- Tests are not optional. The engine ships with scenario tests that read
  like DESIGN.md section 6. Connectors and clients ship with mocked tests.
- One rustls crypto provider across the workspace, aws-lc-rs. Nothing may
  enable `ring`; `cargo tree -e features -i rustls | grep -c 'feature "ring"'`
  must print 0.
- Comments: one or two plain lines saying what, or the one non-obvious
  why. No history in comments.
- No em-dashes anywhere. Dates as 2026-11-02.

## Naming

The product is Ours. The agent's default name in a room is Noa; the simulator and the tests call it Q. Crates
are `ours-*`. Modules are nouns. Types keep the design's words: lead,
member, guest, host, room, group, kind, tier, hold, charter, commitment,
ledger line, proposal, decision, fact.

## Building and testing

    cargo build --release
    scripts/check.sh                 # fmt, clippy -D warnings, every test
    cargo test -p ours-core        # one crate
    cargo run -p ours -- bench --agent rules   # the benchmark, no model needed

If you change what the engine says or does, add or update a scenario in
`benchmark/scenarios/` as well as the crate's tests.

## Pull requests

One logical change per pull request, with a title in the imperative mood.
Say what the change does and how to verify it. A change to the engine's
behaviour needs a scenario test; a change to a connector needs a mocked
test; a change to anything a member can see needs a line in CHANGELOG.md.
