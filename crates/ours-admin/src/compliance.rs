//! The compliance page: plain, printable HTML rendered from the report.

use crate::source::{ComplianceReport, GroupFacts};
use std::fmt::Write;

const STYLE: &str = "\
:root { --bg: #faf9f6; --fg: #1d1c1a; --muted: #6b675f; --line: #dcd8cd; --ok: #2f6b4f; --bad: #b3402d; }
@media (prefers-color-scheme: dark) { :root { --bg: #151614; --fg: #e8e6e0; --muted: #98948a; --line: #2d2e2a; --ok: #7fc4a0; --bad: #e48b79; } }
* { box-sizing: border-box; }
body { margin: 0 auto; max-width: 48rem; padding: 16px; background: var(--bg); color: var(--fg); font: 16px/1.5 system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif; }
h1 { font-size: 1.25rem; margin: 0 0 4px; } h2 { font-size: 1rem; margin: 24px 0 4px; } h3 { font-size: 1rem; margin: 16px 0 4px; }
.muted { color: var(--muted); font-size: 0.875rem; }
pre { margin: 8px 0; padding: 8px; overflow-x: auto; white-space: pre-wrap; overflow-wrap: anywhere; border: 1px solid var(--line); border-radius: 8px; font-size: 0.8125rem; }
table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
th, td { padding: 4px 8px 4px 0; text-align: left; vertical-align: top; border-top: 1px solid var(--line); overflow-wrap: anywhere; }
a { color: var(--ok); }
button { font: inherit; color: inherit; min-height: 44px; padding: 8px 12px; background: none; border: 1px solid var(--line); border-radius: 8px; }
button.wipe { color: var(--bad); border-color: var(--bad); }
.tools { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 16px; padding: 8px 0; }
#msg { color: var(--bad); }
@media print { .tools, #msg { display: none; } body { background: #fff; color: #000; } }
";

const SCRIPT: &str = "\
const msg = document.getElementById('msg');
async function post(url, body) {
  const r = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  if (!r.ok) { msg.textContent = 'That failed (' + r.status + ').'; return false; }
  return true;
}
for (const b of document.querySelectorAll('[data-hold]')) {
  b.addEventListener('click', async () => {
    const on = b.dataset.hold !== 'on';
    if (await post('/api/groups/' + encodeURIComponent(b.dataset.group) + '/legal-hold', { on })) location.reload();
  });
}
for (const b of document.querySelectorAll('[data-wipe]')) {
  b.addEventListener('click', async () => {
    const id = b.dataset.group;
    const typed = prompt('This deletes everything Ours keeps for this group and cannot be undone. Type the group id (' + id + ') to confirm.');
    if (typed === null) return;
    if (await post('/api/groups/' + encodeURIComponent(id) + '/wipe', { confirm: typed })) location.reload();
  });
}
";

/// Escapes text for HTML content and attribute values.
pub(crate) fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn url_part(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn section(out: &mut String, title: &str, body: &str) {
    let _ = writeln!(
        out,
        "<section>\n<h2>{}</h2>\n{body}\n</section>",
        esc(title)
    );
}

fn group_block(out: &mut String, g: &GroupFacts) {
    let id = esc(g.id.as_str());
    let path = url_part(g.id.as_str());
    let _ = writeln!(
        out,
        "<h3>{} <span class=\"muted\">{} · {id}</span></h3>",
        esc(&g.name),
        esc(&g.kind)
    );
    let rows: [(&str, String); 16] = [
        ("File", format!("{} ({} bytes)", esc(&g.file), g.file_bytes)),
        ("Retention", format!("{} days", g.retention_days)),
        ("Legal hold", on_off(g.legal_hold).to_owned()),
        ("Members", g.members.to_string()),
        ("Leads", g.leads.to_string()),
        ("Facts, group scope", g.facts.group.to_string()),
        ("Facts, room scope", g.facts.room.to_string()),
        ("Facts, private scope", g.facts.private.to_string()),
        ("Facts, self-stated", g.facts.self_stated.to_string()),
        ("Decisions", g.decisions.to_string()),
        ("Commitments", g.commitments.to_string()),
        ("Ledger lines", g.ledger_lines.to_string()),
        ("Action records", g.actions.to_string()),
        ("Journal records", g.journal_records.to_string()),
        ("Redacted journal records", g.redacted_records.to_string()),
        ("Snapshots", g.snapshots.to_string()),
    ];
    out.push_str("<table>\n");
    for (name, value) in rows {
        let _ = writeln!(
            out,
            "<tr><th scope=\"row\">{name}</th><td>{value}</td></tr>"
        );
    }
    out.push_str("</table>\n");
    let hold_label = if g.legal_hold {
        "Turn legal hold off"
    } else {
        "Turn legal hold on"
    };
    let _ = writeln!(
        out,
        "<div class=\"tools\"><a href=\"/api/groups/{path}/export.json\">Export (JSON)</a> \
         <a href=\"/api/groups/{path}/export.md\">Export (Markdown)</a> \
         <button type=\"button\" data-hold=\"{}\" data-group=\"{id}\">{hold_label}</button> \
         <button type=\"button\" class=\"wipe\" data-wipe=\"1\" data-group=\"{id}\">Wipe</button></div>",
        on_off(g.legal_hold),
    );
}

/// The whole page.
pub(crate) fn render(report: &ComplianceReport) -> String {
    let host = &report.host;
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"color-scheme\" content=\"light dark\">\n<title>Ours: privacy and compliance</title>\n\
         <link rel=\"icon\" href=\"/favicon.svg\" type=\"image/svg+xml\">\n<style>\n{STYLE}</style>\n</head>\n<body>\n\
         <h1>Ours: privacy and compliance</h1>\n\
         <p class=\"muted\">Version {}. Read from this install. <a href=\"/\">Back to the admin page</a></p>\n\
         <p id=\"msg\" role=\"alert\"></p>\n",
        esc(&host.version),
    );

    section(
        &mut out,
        "What is kept",
        "<p>Per group: facts, decisions, commitments, the tab, action records, \
        the journal of inputs used for replay, and snapshots of state. There are no transcripts beyond the \
        journal and a short ring of recent lines.</p>",
    );

    section(
        &mut out,
        "Where it lives",
        &format!(
            "<p>One SQLite file per group, readable by the host and backed up by copying it. \
         Data directory: <code>{}</code>. The admin page is bound to <code>{}</code>.</p>",
            esc(&host.data_dir),
            esc(&host.admin_bind)
        ),
    );

    section(
        &mut out,
        "Who sees what",
        "<ul>\
        <li>The host sees everything. The host runs the machine and can read the file.</li>\
        <li>Leads see what members see. No role gets more memory, and nobody gets patterns.</li>\
        <li>Rooms are barriers. A fact is said only where it was seen or where a lead shared it.</li>\
        <li>Guests are answered from the current room and nothing else.</li></ul>",
    );

    section(
        &mut out,
        "Member rights",
        "<p>Every member and guest can say, in a direct message:</p>\
        <ul><li>\"what do you know about me\"</li><li>\"forget that\"</li><li>\"forget me\"</li></ul>\
        <p>These are handled in DM, logged, and redacted from the journal. The admin page cannot override \
        them except by wiping the whole group.</p>",
    );

    let mut retention = String::from(
        "<p>Durable facts stay until corrected or forgotten. Perishable facts expire. \
         The action log is kept for the days shown below. Legal hold is turn off expiry.</p>\n<table>\n\
         <tr><th>Group</th><th>Kind</th><th>Retention</th><th>Legal hold</th></tr>\n",
    );
    for g in &report.groups {
        let _ = writeln!(
            retention,
            "<tr><td>{}</td><td>{}</td><td>{} days</td><td>{}</td></tr>",
            esc(&g.name),
            esc(&g.kind),
            g.retention_days,
            on_off(g.legal_hold),
        );
    }
    retention.push_str("</table>");
    section(&mut out, "Retention", &retention);

    let model_where = if host.model.local {
        "runs on this machine or its network"
    } else {
        "is a remote service"
    };
    let mut leaves = format!(
        "<p>Only these leave this machine.</p><ul>\
         <li>The large model: {} at <code>{}</code>, which {model_where}.</li>",
        esc(&host.model.label),
        esc(&host.model.endpoint),
    );
    match &host.mail {
        Some(from) => {
            let _ = write!(
                leaves,
                "<li>Email, sent from <code>{}</code>.</li>",
                esc(from)
            );
        }
        None => leaves.push_str("<li>Email: not configured.</li>"),
    }
    if !host.connectors.is_empty() {
        let _ = write!(
            leaves,
            "<li>Chat platforms: {}.</li>",
            esc(&host.connectors.join(", "))
        );
    }
    let telemetry = &host.telemetry;
    let _ = write!(
        leaves,
        "<li>Telemetry: {}",
        if telemetry.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if let Some(endpoint) = &telemetry.endpoint {
        let _ = write!(leaves, ", sent to <code>{}</code>", esc(endpoint));
    }
    leaves.push_str(
        ".</li></ul>\n<p>A telemetry record is text-free. This is the whole payload:</p>\n",
    );
    let _ = write!(leaves, "<pre>{}</pre>", esc(&telemetry.sample));
    section(&mut out, "What leaves this machine", &leaves);

    section(
        &mut out,
        "Audit",
        "<p>The action log is append-only and exportable. Each record shows the \
        message that triggered it and, for held actions, who approved it.</p>",
    );

    section(
        &mut out,
        "Out of scope",
        "<p>Groups with minors and sensitive groups are out of scope for this \
        version, as the README says. The host is responsible for their group.</p>",
    );

    out.push_str("<section>\n<h2>Groups</h2>\n");
    if report.groups.is_empty() {
        out.push_str("<p>No groups yet.</p>\n");
    }
    for g in &report.groups {
        group_block(&mut out, g);
    }
    let _ = write!(
        out,
        "</section>\n<script>\n{SCRIPT}</script>\n</body>\n</html>\n"
    );
    out
}
