# Security

Ours runs inside group chats and holds what groups say. Treat anything
that lets one person read, change or act as another as a security issue:
a guest instructing the agent, a member reading another member's private
facts or calendar, a forged email counting as verified, a journal that
keeps words after "forget me", an admin route reachable without the token.

## Reporting

Do not open a public issue for a vulnerability. Use GitHub's private
vulnerability reporting on this repository, or email the address in the
repository profile. Say what you found, how to reproduce it, and which
version. You will get an acknowledgement within three days and a fix or a
plan within fourteen.

## Scope

- The engine's authority model (roles, the permitted action set, deferral).
- Memory scopes, member rights, journal redaction, export and wipe.
- The admin server, its token and its calendar feed links.
- The connectors' handling of untrusted input (mentions, reactions, polls, mail).
- The release artefacts: tarballs, checksums, provenance, the container image.

Out of scope: the model's own behaviour beyond what the engine enforces,
and the platforms themselves.

## Supported versions

The latest release and the current main branch.
