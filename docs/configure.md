# Configure

Everything the host decides lives in one TOML file. `ours.example.toml`
is a working starting point. Unknown keys are an error, so a typo fails at
`ours check` and not at three in the morning.

Secrets can stay out of the file. The environment wins over the file for
the names in the last section, so the file can sit in a private repository.

The tables below follow the order of the file.

## `[host]`

| Key | Default | What it does |
|---|---|---|
| `data_dir` | `"./data"` | Where the groups' files go: one SQLite file per group, the charter and decision log, telemetry, and the mail folder if you use one. |
| `admin_bind` | `"0.0.0.0:7777"` | Address and port of the admin page. Use `127.0.0.1:7777` to keep it on the machine. |
| `admin_token` | generated at start | The admin page's token. Letters, digits, `-`, `_`, `.` and `~`, at most 128 characters. When unset, a new one is made at each start and the printed link changes. Set it, or `OURS_ADMIN_TOKEN`, for a link that survives restarts. |
| `public_url` | the machine's LAN address and the admin port | How members reach the admin server, for calendar feed links. Set it when the page is behind a proxy or a name. |

## `[model]`

Required. The large model answers addressed messages. Any
OpenAI-compatible chat endpoint works.

| Key | Default | What it does |
|---|---|---|
| `base_url` | none, required | The endpoint, for example `http://localhost:8080/v1`. |
| `model` | none, required | The model name sent with each request. It is also what the pinned message and the charter tell the group the agent runs on. |
| `dialect` | `"openai"` | `openai`, `llama_cpp` or `ollama`. With `openai` the engine's reply schema is sent as a strict response format. |
| `api_key` | none | Sent as a bearer token when set. Env: `OURS_MODEL_API_KEY`. |
| `daily_cap_usd` | `1.0` | A spend cap per group per UTC day, above zero, shared by the chat model and the small model. Past it the model is not called. |
| `timeout_secs` | `60` | Per request. The small stages use it too. |
| `chat_temperature` | `0.8` | For replies. Extraction and scoring always use 0. |
| `chat_max_tokens` | `700` | The reply budget in tokens. |
| `extra` | empty | Extra fields sent with every request, such as `extra = { chat_template_kwargs = { enable_thinking = false } }`. |

## `[classifier]`

Optional. The small model scores every message, extracts facts,
commitments and tab lines, and resolves times, before the large model is
asked anything.

| Key | Default | What it does |
|---|---|---|
| `enabled` | `true` | When `false` the agent keeps only the rules pass: no extraction, no time resolver. It still works, with less. |
| `base_url`, `model`, `dialect` | the `[model]` values | Set any of the three to point the small stages at another endpoint or model. |
| `api_key` | the `[model]` key | |
| `extra` | the `[model]` extra | |

Leave the section out and the small stages run on the `[model]` endpoint.

## `[telemetry]`

Off by default. See [operations.md](operations.md) for the exact payload.

| Key | Default | What it does |
|---|---|---|
| `enabled` | `false` | Writes a text-free record per scored message to `data_dir/telemetry/<date>.jsonl`. |
| `endpoint` | none | Where batches are posted, hourly. Without it nothing leaves the machine. |

## `[mail]`

Optional. Leave the section out and the agent cannot send mail. It needs
either `smtp` or `dir`. See [mail.md](mail.md).

| Key | Default | What it does |
|---|---|---|
| `from` | required | The group's address, with an optional display name: `Noa for The Six <q@example.org>`. |
| `reply_to` | none | Where replies from people should go, when it is not `from`. |
| `dir` | none | Trial mode: write `.eml` files to `dir/out` and read replies dropped in `dir/in`. Wins over `smtp`. |

### `[mail.smtp]`

| Key | Default | What it does |
|---|---|---|
| `host`, `port`, `username` | required | |
| `password` | required in the file | Env: `OURS_SMTP_PASSWORD` replaces it. Keep `password = ""` in the file if you use the environment. |
| `starttls` | `true` | `true`: connect in the clear and upgrade with STARTTLS (port 587). `false`: TLS from the first byte (port 465). |

### `[mail.imap]`

Optional. Without it the group only sends.

| Key | Default | What it does |
|---|---|---|
| `host`, `port`, `username` | required | IMAP is always TLS from the first byte, usually port 993. |
| `password` | required in the file | Env: `OURS_IMAP_PASSWORD`. |
| `folder` | `"INBOX"` | |
| `poll_secs` | `60` | Seconds between checks, never less than one. |
| `authserv_id` | the `host` | The name your provider writes at the start of its `Authentication-Results` header. A mail counts as verified only when that header is the provider's; with no match every mail is treated as unverified. Some providers, Microsoft 365 among them, leave this name out of the header, and then no mail can be verified. |

## `[[connectors]]`

One table per platform. `discord`, `slack` and `telegram` are built; the
other platform names in the schema are not in this release. `ours check`
and `ours run` refuse a second table for the same platform and any platform
that is not built.

| Key | Default | What it does |
|---|---|---|
| `platform` | required | `discord`, `slack` or `telegram`. |
| `agent_name` | `"Noa"` | The name this connector answers to at the start of a message. |
| `token` | required | Discord bot token, Slack bot token (`xoxb-`), or the Telegram bot token from BotFather. Env: `OURS_DISCORD_TOKEN`, `OURS_SLACK_BOT_TOKEN`, `OURS_TELEGRAM_TOKEN`. |
| `guilds` | all | Discord only. A list of server ids the bot may serve. |
| `app_token` | required for Slack | Slack app-level token (`xapp-`) with `connections:write`. Env: `OURS_SLACK_APP_TOKEN`. |
| `leads_usergroup` | none | Slack only. A user group handle whose members count as admins for the leads rule. |

## `[[groups]]`

One table per group. A group is a name, a kind and the rooms that belong to
it. Rooms from both platforms can share a group, and one memory serves all
of them.

| Key | Default | What it does |
|---|---|---|
| `id` | required | A stable slug: letters, digits, `-` and `_`. It is the data file's name and must be unique. |
| `name` | required | |
| `kind` | `"friends"` | `friends`, `household`, `team`, `project`, `club`, `community`, `committee` or `cohort`. Sets the starting defaults; see [personas.md](personas.md) for the numbers. Used once, on a group's first start. After that a lead changes it by sentence. |
| `timezone` | `"UTC"` | An IANA name such as `Europe/London`. Enforced at every start. |
| `agent_name` | unset | The bot's name in this group. Unset, the first conversation asks the group what to call it and the answer survives restarts; set, it is enforced at every start. The default before anyone chooses is Noa. |
| `rooms` | required | A list of `{ platform = "discord", id = "..." }`. A Discord id is a channel id; a Slack id is a channel id such as `C0123456789`. Every room needs a connector for its platform. |
| `persona` | none | A starter persona by name. See [personas.md](personas.md). |
| `dial` | the kind's default | 0 to 3. Used until someone changes the dial by sentence. |
| `legal_hold` | `false` | Start with legal hold on: nothing expires until a lead or the host lifts it. |
| `voice` | a plain default | One line on how the agent talks. Enforced at every start. |
| `traits` | empty | Three or four words. Enforced at every start. |
| `bits` | empty | Running bits. Used on first start; leads add and drop bits by sentence after that. |
| `never_jokes_about` | empty | Subjects it never jokes about, on top of the hard rules. Enforced at every start. |
| `currency` | `"USD"` | ISO 4217 code for the tab. Enforced at every start. |
| `leads` | empty | Platform user ids of named leads. Used on first start; empty means the kind's rule alone. |
| `hosts` | empty | Platform user ids of hosts. Enforced at every start. A host's instructions are always carried out. |

Ids, never display names, identify people. On Discord turn on developer
mode, then right-click a user or channel and copy the id.

## Environment variables

| Variable | Replaces |
|---|---|
| `OURS_MODEL_API_KEY` | `[model] api_key` |
| `OURS_ADMIN_TOKEN` | `[host] admin_token` |
| `OURS_DISCORD_TOKEN` | `token` of every Discord connector |
| `OURS_SLACK_BOT_TOKEN` | `token` of every Slack connector |
| `OURS_SLACK_APP_TOKEN` | `app_token` of every Slack connector |
| `OURS_SMTP_PASSWORD` | `[mail.smtp] password`, when that table exists |
| `OURS_IMAP_PASSWORD` | `[mail.imap] password`, when that table exists |
| `RUST_LOG` | The log level; default `info`. |

## Checks made at load

`ours check` fails when a connector has no token, a Slack connector has no
`app_token`, `[mail]` has neither `smtp` nor `dir`, a group id is empty,
repeated or has other characters, a group has no rooms, a room names a
platform with no connector, or a timezone is not an IANA name.
