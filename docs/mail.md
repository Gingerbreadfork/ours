# Mail

The group can have an email address. The agent sends from it when a lead
asks it to email someone, such as a venue, and reads replies sent to it.
Leave out the `[mail]` section and the agent cannot send mail at all.

Mail is a tier-1 action: it leaves the group, so it waits out a hold
window and can be taken back. The window is two minutes for friends, household, club and cohort groups,
five for communities and ten for team, project and committee groups. The
group's window is in its charter: the admin page's Charter tab, or
`<data_dir>/<group id>.charter.md`.

## The address

Use a mailbox you control, such as `q@example.org`, and give the agent its
own account. Anything that account can see, the agent can read, and the
host is responsible for it. Do not point it at a personal inbox. See
[privacy.md](privacy.md).

## Dir mode, for testing

    [mail]
    from = "Noa for The Six <q@example.org>"
    dir = "./data/mail"

No server is needed. Outgoing mail is written as `.eml` files into
`dir/out`. A reply is "received" when you drop an `.eml` file into
`dir/in`. Once every group has it, the file moves to `dir/in/seen`; a
file that cannot be read as mail moves to `dir/in/failed`. Message ids,
`In-Reply-To` and the `X-Ours-Action` header are filled in, so you can
open a sent file in a mail client and answer it. Dir mode wins over `smtp`.

## SMTP and IMAP

    [mail]
    from = "Noa for The Six <q@example.org>"
    reply_to = "host@example.org"

    [mail.smtp]
    host = "smtp.fastmail.com"
    port = 465
    username = "q@example.org"
    password = ""            # or OURS_SMTP_PASSWORD
    starttls = false         # false: TLS on 465. true: STARTTLS on 587.

    [mail.imap]
    host = "imap.fastmail.com"
    port = 993
    username = "q@example.org"
    password = ""            # or OURS_IMAP_PASSWORD
    folder = "INBOX"
    poll_secs = 60

`starttls` defaults to `true`, so for port 465 set it to `false`. IMAP is
always TLS from the first byte. Leave out `[mail.imap]` and the group only
sends. `reply_to` is added to every outgoing mail, which is how the
venue can reach the host.

## What the agent will send

Only an address that someone gave in the room, or that it remembers there.
It never guesses one. It will not send to an address it was not given, to
more than a few addresses at once, or without something to say. The mail
is plain text, signed with the agent's name for the group, and names
members only when a booking needs a name. Nothing said in private or in
another room goes into it.

Only a lead can ask for mail. A member who asks gets the usual deferral:
`Happy to. Needs a lead's ✅.` A lead's ✅ on that reply approves it, and the mail then waits out the window as any other. With no ✅ in a day the request lapses.

## What a held mail looks like

The agent says one line in the room:

    Emailing bookings@baitong.com in 10m: Table for five, Thursday 7:30. Undo?

During the window:

- `Noa, undo` or `Noa, undo the email` stops it. The line becomes `Not sent to
  bookings@baitong.com: Table for five, Thursday 7:30.`
- `Noa, what did you send` lists the room's mail, newest first, with each
  one's state: waiting, sent, sent then cancelled, not sent or failed.

After the window it goes, and the agent says `Sent to ...: ... Undo?`.
Undo after that sends a short cancellation to the same address, asking the
recipient to disregard the earlier message, and says plainly that whether
they honour it is up to them. If the mail server refuses a message the
agent says it did not go and nothing was sent.

## Replies

Mail that reaches the address is always treated as a guest. It never
instructs. A reply to something the agent sent is relayed to the room in
one line, `<who> replied: ...`, cut to about 200 characters and marked
`(unverified sender)` when the sender could not be authenticated. A mail
counts as a reply only when it answers the message id the agent sent or
comes from the exact address the agent wrote to, never just the same
domain. Authentication is read only from your own provider's
`Authentication-Results` header (see `authserv_id` in configure.md), so a
header the sender wrote themselves does not count. A sender counts as
verified when that header reports `dmarc=pass` for the From domain, or,
when it gives no DMARC verdict, a DKIM or SPF pass for the From domain or a
subdomain of it. Any other DMARC result means unverified. Over IMAP a
message is flagged as read only after every group has kept it; after a
crash it is read again, and a mail already handled is ignored. Mail that answers
nothing the agent sent is not kept. Every group hears incoming mail, and only the one that sent the
original keeps it.
