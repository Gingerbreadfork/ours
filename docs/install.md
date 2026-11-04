# Install

Ours is one binary, `ours`, and one config file, `ours.toml`. This
page takes you from a download to the agent saying hello in a room. The
Linux on x86_64 and aarch64 are the supported builds in 0.1.0-rc.2.

## What you need

- A Linux box that stays on. A small one is enough; the model is the
  heavy part.
- A model endpoint that speaks the OpenAI chat API: a llama.cpp server, an
  Ollama install, or a hosted service. See [configure.md](configure.md).
- A bot on Discord ([discord.md](discord.md)), Slack ([slack.md](slack.md)),
  or both.

## The five-minute path

1. Download the tarball and `SHA256SUMS` from the release page, then check
   the download.

       sha256sum -c SHA256SUMS --ignore-missing
       gh attestation verify ours-0.1.0-rc.2-<arch>-linux.tar.gz --repo Gingerbreadfork/ours

   If a `SHA256SUMS.bundle` is attached, `cosign verify-blob` checks the
   signature on `SHA256SUMS` too.

2. Unpack it and put the binary on your path.

       tar xzf ours-0.1.0-rc.2-<arch>-linux.tar.gz    # x86_64 or aarch64
       cd ours-0.1.0-rc.2-<arch>-linux
       sudo install -m 0755 ours /usr/local/bin/ours

3. Make the config. The tarball carries `ours.example.toml`.

       cp ours.example.toml ours.toml

   Set `[model]` to your endpoint, put your Discord channel id in the
   group's `rooms`, and keep the bot token out of the file by exporting it.

       export OURS_DISCORD_TOKEN=...

4. Check it. `ours check` loads the file, applies the environment and
   prints what would run. `ours model-check` asks the model one question
   with the engine's own schema, so you know the endpoint works before
   anyone talks to it.

       ours check
       ours model-check

5. Run it.

       ours run

   The log shows the admin page link and a QR code for your phone. Open the
   link; the Feed tab shows `connected to Discord as Noa` once the connector
   is up. For Slack the line reads `connected to Slack as Noa`. The agent
   name is whatever `agent_name` says.

## The config file location

`ours` reads `ours.toml` in the current directory. To use another path,
put `--config` (or `-c`) before the subcommand:

    ours --config /etc/ours/ours.toml run

Set `RUST_LOG` to change the log level. The default is `info`.

## The container image

The image is published to `ghcr.io/gingerbreadfork/ours` with the same
version tags as the release. It runs as a non-root user, reads its config
from `/etc/ours/ours.toml` and keeps its data in `/data`. Set
`data_dir = "/data"` in your config, since the image does not rewrite it.

    docker run -d --name ours \
      -p 7777:7777 \
      -v /etc/ours/ours.toml:/etc/ours/ours.toml:ro \
      -v ours-data:/data \
      --env-file /etc/ours/env \
      ghcr.io/gingerbreadfork/ours:0.1.0-rc.2

`-p 7777:7777` publishes the admin page on every interface, over plain
HTTP, so a phone on your network can open it. Anyone who can read the
traffic can take the admin token. On a network you do not trust, publish
it on loopback with `-p 127.0.0.1:7777:7777` and put a TLS proxy in front;
see [admin.md](admin.md).

`deploy/ours.container` is the same thing as a Podman quadlet. Copy it to
`/etc/containers/systemd/` and run `systemctl daemon-reload`. Verify the
image with `gh attestation verify oci://ghcr.io/gingerbreadfork/ours:0.1.0-rc.2 --repo Gingerbreadfork/ours`.

If your model runs on the host, the container cannot reach it at
`localhost`. Use the host's address, or run the container with host
networking.

## The systemd unit

`deploy/ours.service` runs the binary under a dynamic user with a
read-only file system except for its state directory.

    sudo install -m 0755 ours /usr/local/bin/ours
    sudo install -d /etc/ours
    sudo install -m 0640 ours.toml /etc/ours/ours.toml
    sudo install -m 0600 deploy/env.example /etc/ours/env   # then fill it in
    sudo install -m 0644 deploy/ours.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl enable --now ours

In `/etc/ours/ours.toml` set `data_dir = "/var/lib/ours"`. Tokens go
in `/etc/ours/env`, one `NAME=value` per line, see
[configure.md](configure.md) for the names. `journalctl -u ours -f`
shows the log, including the admin link.

The unit stops the process with SIGINT so each group writes a snapshot on
the way out. SIGTERM does the same.

## The first conversation

When the agent is added to a room it starts a short conversation with the
person who added it, by DM. It looks for a host named in the config first,
then the room's owner. It never starts one with a bot.

1. It asks what kind of group this is and lists the eight kinds by number.
   Reply with a number or a name, or `skip`.
2. It asks whom it should take instructions from, showing the current
   leads. Reply with names, `everyone` or `keep`.
3. It says in five lines what it will and will not do, with the group's
   own numbers for how often it speaks and how long it holds outgoing
   mail.
4. It posts one line to the room: who it is, what it keeps, whom it listens
   to and how anyone can see or delete what it holds on them.

Every default can be changed later by a sentence. See [say.md](say.md).

## Observe mode

A new group starts in observe mode. For the first day (three days in team,
project and committee groups) the agent reads and scores everything but does
nothing unprompted in a room. Whatever it would have said goes to the leads
and hosts by DM instead, and their 👍 and 👎 on those DMs are counted.
Addressed messages are always answered, observing or not.

When the period ends, leads and hosts get one DM with the count. The agent
keeps watching until a lead says `Noa, go`. `Noa, observe` or `Noa, watch 3 more
days` watches again. `Noa, settings` says whether it is still watching.

## Backups, upgrades and what to do next

- Back up by copying the data directory, see [operations.md](operations.md).
- Add rooms and groups in `ours.toml`, see [configure.md](configure.md).
- Open the admin page, see [admin.md](admin.md).
- Read [privacy.md](privacy.md) before you add the agent to a workplace.
