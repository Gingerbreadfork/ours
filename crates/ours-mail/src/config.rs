//! The `[mail]` section of the host's configuration.

use serde::Deserialize;
use std::fmt;

/// The group's address and the servers behind it.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    /// The group's address, with an optional display name:
    /// `Q for The Six <q@example.org>`.
    pub from: String,
    /// Where replies should go when it is not `from`.
    #[serde(default)]
    pub reply_to: Option<String>,
    pub smtp: SmtpConfig,
    /// Absent when the group only sends.
    #[serde(default)]
    pub imap: Option<ImapConfig>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// True: connect in the clear and upgrade with STARTTLS (port 587).
    /// False: TLS from the first byte (port 465).
    #[serde(default = "default_starttls")]
    pub starttls: bool,
}

/// IMAP is always TLS from the first byte (port 993).
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    #[serde(default = "default_folder")]
    pub folder: String,
    /// Seconds between polls.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// The authserv-id our mail server writes in Authentication-Results.
    /// Only headers with this id are believed; defaults to `host`. Hosts
    /// whose provider writes a different id must set it, otherwise every
    /// mail is treated as unverified.
    #[serde(default)]
    pub authserv_id: Option<String>,
}

impl ImapConfig {
    /// The authserv-id whose Authentication-Results headers are trusted.
    pub fn receiver(&self) -> &str {
        self.authserv_id.as_deref().unwrap_or(&self.host)
    }

    /// The time between polls; never shorter than a second.
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.poll_secs.max(1))
    }
}

fn default_starttls() -> bool {
    true
}

fn default_folder() -> String {
    "INBOX".to_owned()
}

fn default_poll_secs() -> u64 {
    60
}

impl fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailConfig")
            .field("from", &self.from)
            .field("reply_to", &self.reply_to)
            .field("smtp", &self.smtp)
            .field("imap", &self.imap)
            .finish()
    }
}

impl fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("starttls", &self.starttls)
            .finish()
    }
}

impl fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("folder", &self.folder)
            .field("poll_secs", &self.poll_secs)
            .field("authserv_id", &self.authserv_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
from = "Q for The Six <q@example.org>"
reply_to = "host@example.org"

[smtp]
host = "smtp.example.org"
port = 587
username = "q@example.org"
password = "hunter2"

[imap]
host = "imap.example.org"
port = 993
username = "q@example.org"
password = "hunter3"
"#;

    #[test]
    fn full_config_with_defaults() {
        let c: MailConfig = toml::from_str(FULL).unwrap();
        assert_eq!(c.from, "Q for The Six <q@example.org>");
        assert_eq!(c.reply_to.as_deref(), Some("host@example.org"));
        assert!(c.smtp.starttls);
        assert_eq!(c.smtp.port, 587);
        let imap = c.imap.unwrap();
        assert_eq!(imap.folder, "INBOX");
        assert_eq!(imap.poll_secs, 60);
        assert_eq!(imap.interval(), std::time::Duration::from_secs(60));
    }

    #[test]
    fn implicit_tls_and_no_imap() {
        let c: MailConfig = toml::from_str(
            r#"
from = "q@example.org"

[smtp]
host = "smtp.example.org"
port = 465
username = "q"
password = "p"
starttls = false
"#,
        )
        .unwrap();
        assert!(!c.smtp.starttls);
        assert_eq!(c.smtp.port, 465);
        assert!(c.reply_to.is_none());
        assert!(c.imap.is_none());
    }

    #[test]
    fn explicit_imap_values() {
        let text = format!("{FULL}folder = \"Group\"\npoll_secs = 15\n");
        let c: MailConfig = toml::from_str(&text).unwrap();
        let imap = c.imap.unwrap();
        assert_eq!(imap.folder, "Group");
        assert_eq!(imap.poll_secs, 15);
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        let top = format!("colour = \"red\"\n{FULL}");
        assert!(toml::from_str::<MailConfig>(&top).is_err());
        let smtp = FULL.replace("port = 587", "port = 587\ntls = true");
        assert!(toml::from_str::<MailConfig>(&smtp).is_err());
        let imap = FULL.replace("port = 993", "port = 993\nssl = true");
        assert!(toml::from_str::<MailConfig>(&imap).is_err());
    }

    #[test]
    fn passwords_do_not_reach_debug_output() {
        let c: MailConfig = toml::from_str(FULL).unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("hunter2"));
        assert!(!shown.contains("hunter3"));
        assert!(shown.contains("<redacted>"));
        assert!(shown.contains("smtp.example.org"));
    }
}
