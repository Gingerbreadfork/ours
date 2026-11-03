# The admin page

The admin page is a single page served by the same process as the agent. It
is for the host. It shows what the agent saw and did, what it knows, and
the compliance tools. In this release it is read-only apart from legal
hold and wipe; every setting is changed by sentence (see
[say.md](say.md)) or in the config file.

## Getting in

`ours run` prints a link and a QR code to the log:

    admin page url=http://192.168.1.20:7777/?token=...

Open the link, or scan the code with a phone on the same network. The page
trades the token for a cookie that lasts 30 days and removes it from the
address bar. After that the plain address works in that browser.

The token is 32 random bytes made at each start, unless you set
`admin_token` or `OURS_ADMIN_TOKEN`, in which case the link survives
restarts. Programs can send it as `Authorization: Bearer <token>`.

With the default `admin_bind` of `0.0.0.0:7777` anyone on the network who
has the token can read everything the agent holds. Bind it to `127.0.0.1`
and put your own TLS proxy in front, or keep it on a trusted network. The
calendar feed links (`/ics/...`) and `/healthz` do not need the token; the
feed links carry their own secret.

## The tabs

Pick a group at the top, then:

- **Feed.** A live stream of what the agent heard and did, one line each:
  messages scored, replies, reactions, holds, undo, deferrals, setting
  changes, connection state (`connected to Discord as Noa`). Tick a kind
  to hide it.
- **Charter.** Every setting the group has, as Markdown, plus the calendar
  feed address. The same text is written to `<data_dir>/<group id>.charter.md`
  whenever it changes.
- **Memory.** The fact store. Filter by text, show retracted facts, and see
  each fact's scope, confidence, a link to its source message, the day it
  was first seen and its expiry.
- **Decisions.** Open, upcoming and closed proposals, with a link to the
  decision log as Markdown (also `<data_dir>/<group id>.decisions.md`).
- **Tab.** The promises, the ledger lines and the balances.
- **Actions.** Every action the agent took, with its tier, who asked, who
  approved and how it is taken back. Tier-0 (free) actions can be hidden. A
  fold-out lists the whole undo catalogue.
- **Persona.** The voice, traits, bits and dial.
- **Profile.** The kind's settings in force: leads rule, budgets,
  thresholds, hold window, hours, retention and legal hold.

Each tab is read from the group's live state and its journal.

## Compliance

`/compliance`, linked from the page header as Privacy, is a plain page that
prints well. It states, from this install: what is kept, where it lives,
who sees what, member rights, retention per group, what leaves the machine
(including the exact telemetry payload), audit and what is out of scope.
Under it, each group has a table of counts (members, leads, facts by
scope, decisions, commitments, ledger lines, action records, journal
records, redacted journal records, snapshots) and four tools.

- **Export (JSON)** and **Export (Markdown).** The group's data in one file
  each, at `/api/groups/<id>/export.json` and `export.md`. This is the
  answer to a data request.
- **Legal hold.** One button toggles it. On, nothing expires; off, expiry
  resumes. It can also start on from the config (`legal_hold = true`).
- **Wipe.** Deletes everything Ours keeps for the group: its database and
  the files beside it. It asks you to type the group id. It cannot be
  undone. Wipe is also the only way to override a member's right to be
  forgotten, which the page otherwise cannot.

## The API

The page is built on a small JSON API, all behind the same token: `GET
/api/groups`, `/api/groups/<id>/state`, `charter`, `facts`, `decisions`,
`decisions.md`, `ledger`, `actions`, `persona`, `profile`, `events` and
`stream` (server-sent events), `/api/catalogue`, and the two exports; `POST
/api/groups/<id>/legal-hold` with `{"on": true}` and `/api/groups/<id>/wipe`
with `{"confirm": "<group id>"}`.

## Not in this release

- Changing settings, leads or the persona from the page.
- Importing a charter.
- A success-metrics page. The feed and the Profile tab show the budget and
  what the agent did; the numbers in DESIGN.md section 19 are not yet
  computed.
