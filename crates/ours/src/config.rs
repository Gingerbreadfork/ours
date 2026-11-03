//! The host's configuration file. Secrets may be left out of the file and
//! supplied through the environment instead.

use ours_types::{Kind, Platform};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: HostConfig,
    pub model: ModelSection,
    /// The small model that scores every message. Defaults to `[model]`;
    /// `enabled = false` keeps the rules pass only.
    #[serde(default)]
    pub classifier: ClassifierSection,
    /// The group's email address. Leave out and the agent cannot send mail;
    /// set `dir` instead of `smtp` to write .eml files locally.
    #[serde(default)]
    pub mail: Option<MailSection>,
    /// Opt-in, text-free triage telemetry (DESIGN.md section 18). Off by
    /// default; `ours telemetry --show` prints exactly what would be sent.
    #[serde(default)]
    pub telemetry: TelemetrySection,
    #[serde(default)]
    pub connectors: Vec<ConnectorSection>,
    #[serde(default)]
    pub groups: Vec<GroupSection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_admin_bind")]
    pub admin_bind: SocketAddr,
    #[serde(default)]
    pub admin_token: Option<String>,
    /// How members reach the admin server from outside the LAN, for calendar
    /// feed links; the LAN address is used when unset.
    #[serde(default)]
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSection {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub dialect: ModelDialect,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_cap")]
    pub daily_cap_usd: f64,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Sampling temperature for chat replies; extraction and scoring use 0.
    #[serde(default = "default_chat_temperature")]
    pub chat_temperature: f32,
    /// The reply budget in tokens.
    #[serde(default = "default_chat_max_tokens")]
    pub chat_max_tokens: u32,
    /// Extra fields sent with every model request, such as
    /// `chat_template_kwargs = { enable_thinking = false }`.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// A missing section means the small stages run on the chat model.
impl Default for ClassifierSection {
    fn default() -> Self {
        ClassifierSection {
            enabled: true,
            base_url: None,
            model: None,
            dialect: None,
            api_key: None,
            extra: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySection {
    #[serde(default)]
    pub enabled: bool,
    /// Where batches go. Unset means a local file only, under data/telemetry.
    #[serde(default)]
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MailSection {
    pub from: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    /// A directory for .eml files instead of a provider; mail "arrives" from
    /// files dropped in it too.
    #[serde(default)]
    pub dir: Option<PathBuf>,
    #[serde(default)]
    pub smtp: Option<ours_mail::SmtpConfig>,
    #[serde(default)]
    pub imap: Option<ours_mail::ImapConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierSection {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// When unset, the `[model]` endpoint and model are used.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub dialect: Option<ModelDialect>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelDialect {
    #[default]
    #[serde(rename = "openai")]
    OpenAi,
    LlamaCpp,
    Ollama,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorSection {
    pub platform: Platform,
    #[serde(default = "default_agent_name")]
    pub agent_name: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub guilds: Option<Vec<String>>,
    /// Slack's app-level token (xapp-...) for Socket Mode.
    #[serde(default)]
    pub app_token: Option<String>,
    /// Slack: a user group handle whose members count as admins for the leads rule.
    #[serde(default)]
    pub leads_usergroup: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// The bot's name in this group. Unset, the first conversation asks the
    /// group what to call it, and a name chosen there survives restarts.
    #[serde(default)]
    pub agent_name: Option<String>,
    pub rooms: Vec<RoomRef>,
    /// A starter persona by name (see docs/personas.md); applied while the
    /// group still has the plain default.
    #[serde(default)]
    pub persona: Option<String>,
    /// The persona dial, 0 to 3; the kind's default when unset.
    #[serde(default)]
    pub dial: Option<u8>,
    /// Legal hold at startup: nothing expires until a lead or the host lifts it.
    #[serde(default)]
    pub legal_hold: bool,
    /// One line on how the agent talks.
    #[serde(default)]
    pub voice: Option<String>,
    /// Three or four traits, as words.
    #[serde(default)]
    pub traits: Vec<String>,
    /// Running bits and in-jokes she may use when they fit.
    #[serde(default)]
    pub bits: Vec<String>,
    /// Subjects she never jokes about, on top of the hard rules.
    #[serde(default)]
    pub never_jokes_about: Vec<String>,
    /// ISO 4217 code for the tab.
    #[serde(default = "default_currency")]
    pub currency: String,
    #[serde(default)]
    pub leads: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomRef {
    pub platform: Platform,
    pub id: String,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}

fn default_admin_bind() -> SocketAddr {
    ([0, 0, 0, 0], 7777).into()
}

fn default_chat_temperature() -> f32 {
    0.8
}

fn default_chat_max_tokens() -> u32 {
    700
}

fn default_cap() -> f64 {
    1.0
}

fn default_timeout() -> u64 {
    60
}

fn default_agent_name() -> String {
    "Noa".to_owned()
}

fn default_currency() -> String {
    "USD".to_owned()
}

fn default_timezone() -> String {
    "UTC".to_owned()
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.apply_environment();
        config.validate()?;
        Ok(config)
    }

    /// Secrets from the environment win over the file, so the file can be
    /// committed to a host's private repo without them.
    fn apply_environment(&mut self) {
        if let Ok(key) = std::env::var("OURS_MODEL_API_KEY") {
            self.model.api_key = Some(key);
        }
        if let Ok(token) = std::env::var("OURS_ADMIN_TOKEN") {
            self.host.admin_token = Some(token);
        }
        if let Some(mail) = &mut self.mail {
            if let (Some(smtp), Ok(password)) =
                (&mut mail.smtp, std::env::var("OURS_SMTP_PASSWORD"))
            {
                smtp.password = password;
            }
            if let (Some(imap), Ok(password)) =
                (&mut mail.imap, std::env::var("OURS_IMAP_PASSWORD"))
            {
                imap.password = password;
            }
        }
        if let Ok(token) = std::env::var("OURS_DISCORD_TOKEN") {
            for connector in &mut self.connectors {
                if connector.platform == Platform::Discord {
                    connector.token = Some(token.clone());
                }
            }
        }
        if let Ok(token) = std::env::var("OURS_TELEGRAM_TOKEN") {
            for connector in &mut self.connectors {
                if connector.platform == Platform::Telegram {
                    connector.token = Some(token.clone());
                }
            }
        }
        if let Ok(token) = std::env::var("OURS_SLACK_BOT_TOKEN") {
            for connector in &mut self.connectors {
                if connector.platform == Platform::Slack {
                    connector.token = Some(token.clone());
                }
            }
        }
        if let Ok(token) = std::env::var("OURS_SLACK_APP_TOKEN") {
            for connector in &mut self.connectors {
                if connector.platform == Platform::Slack {
                    connector.app_token = Some(token.clone());
                }
            }
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for connector in &self.connectors {
            if connector.platform == Platform::Slack
                && connector.app_token.as_deref().unwrap_or("").is_empty()
            {
                return Err(ConfigError::Invalid(
                    "the Slack connector needs app_token (xapp-...) as well as token (xoxb-...)"
                        .to_owned(),
                ));
            }
            if connector.token.as_deref().unwrap_or("").is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "connector {:?} has no token (set it in the file or the environment)",
                    connector.platform
                )));
            }
        }
        if let Some(mail) = &self.mail
            && mail.dir.is_none()
            && mail.smtp.is_none()
        {
            return Err(ConfigError::Invalid(
                "[mail] needs either smtp or dir".to_owned(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for group in &self.groups {
            if group.id.is_empty()
                || !group
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(ConfigError::Invalid(format!(
                    "group id {:?} must be letters, digits, '-' or '_'",
                    group.id
                )));
            }
            if !ids.insert(&group.id) {
                return Err(ConfigError::Invalid(format!(
                    "group id {:?} is used twice",
                    group.id
                )));
            }
            if group.rooms.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "group {:?} has no rooms",
                    group.id
                )));
            }
            for room in &group.rooms {
                if !self.connectors.iter().any(|c| c.platform == room.platform) {
                    return Err(ConfigError::Invalid(format!(
                        "group {:?} has a room on {:?} but no connector for it",
                        group.id, room.platform
                    )));
                }
            }
            if jiff::tz::TimeZone::get(&group.timezone).is_err() {
                return Err(ConfigError::Invalid(format!(
                    "group {:?} has an unknown timezone {:?}",
                    group.id, group.timezone
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_file_parses() {
        let text = include_str!("../../../ours.example.toml");
        let config: Config = toml::from_str(text).expect("example parses");
        assert_eq!(config.groups.len(), 1);
        assert_eq!(config.groups[0].kind, Kind::Friends);
        assert_eq!(config.connectors[0].platform, Platform::Discord);
    }

    #[test]
    fn rejects_room_without_connector() {
        let text = r#"
[host]
[model]
base_url = "http://localhost:8080/v1"
model = "m"
[[groups]]
id = "g"
name = "G"
rooms = [{ platform = "slack", id = "C1" }]
"#;
        let config: Config = toml::from_str(text).expect("parses");
        let err = config.validate().expect_err("must fail");
        assert!(err.to_string().contains("no connector"));
    }
}
