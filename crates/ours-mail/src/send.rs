//! Sending mail: the `Mailer` trait, SMTP through lettre, and a mailer
//! that writes `.eml` files.

use crate::config::MailConfig;
use crate::error::MailError;
use async_trait::async_trait;
use lettre::message::header::{ContentType, Header, HeaderName, HeaderValue};
use lettre::message::{Mailbox, Message};
use lettre::transport::smtp::PoolConfig;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use ours_types::{ActionId, OutgoingMail};
use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

/// Sends one mail and returns its Message-ID, without angle brackets.
#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, action: &ActionId, mail: &OutgoingMail) -> Result<String, MailError>;
}

/// A fresh Message-ID for `domain`: `local@domain`, no angle brackets.
pub fn generate_message_id(domain: &str) -> String {
    let local = ulid::Ulid::generate().to_string().to_ascii_lowercase();
    let domain = domain.trim().to_ascii_lowercase();
    if domain.is_empty() {
        format!("{local}@localhost.invalid")
    } else {
        format!("{local}@{domain}")
    }
}

/// Carries the action id so a sent mail can be traced back to its action.
#[derive(Debug, Clone)]
struct XOursAction(String);

impl Header for XOursAction {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("X-Ours-Action")
    }

    fn parse(s: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(s.to_owned()))
    }

    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), self.0.clone())
    }
}

/// The group's own addresses, shared by both mailers.
#[derive(Debug, Clone)]
struct Identity {
    from: Mailbox,
    reply_to: Option<Mailbox>,
}

impl Identity {
    fn new(config: &MailConfig) -> Result<Self, MailError> {
        let from = config
            .from
            .trim()
            .parse::<Mailbox>()
            .map_err(|e| MailError::Config(format!("from {:?}: {e}", config.from)))?;
        let reply_to = config
            .reply_to
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.parse::<Mailbox>()
                    .map_err(|e| MailError::Config(format!("reply_to {s:?}: {e}")))
            })
            .transpose()?;
        Ok(Self { from, reply_to })
    }

    fn domain(&self) -> &str {
        self.from.email.domain()
    }

    fn build(
        &self,
        action: &ActionId,
        mail: &OutgoingMail,
        message_id: &str,
    ) -> Result<Message, MailError> {
        let to = mail
            .to
            .trim()
            .parse::<Mailbox>()
            .map_err(|e| MailError::Rejected {
                code: 501,
                message: format!("invalid recipient {:?}: {e}", mail.to),
            })?;

        let mut builder = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(single_line(&mail.subject))
            .message_id(Some(format!("<{message_id}>")))
            .header(ContentType::TEXT_PLAIN)
            .header(XOursAction(single_line(action.as_str())));
        if let Some(reply_to) = &self.reply_to {
            builder = builder.reply_to(reply_to.clone());
        }
        if let Some(parent) = mail.in_reply_to.as_deref().and_then(clean_message_id) {
            builder = builder
                .in_reply_to(format!("<{parent}>"))
                .references(format!("<{parent}>"));
        }
        builder
            .body(mail.body.clone())
            .map_err(|e| MailError::Config(format!("building the message: {e}")))
    }
}

/// Control characters become spaces so a value cannot add header lines.
fn single_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A message id with its angle brackets removed, or `None` when it is
/// empty or could not be written into a header safely.
fn clean_message_id(id: &str) -> Option<String> {
    let id = id.trim().trim_start_matches('<').trim_end_matches('>');
    let valid = !id.is_empty()
        && id.is_ascii()
        && !id
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>'));
    if valid {
        Some(id.to_owned())
    } else {
        warn!("ignoring a malformed in_reply_to id");
        None
    }
}

/// SMTP through lettre: rustls, pooled connections, one retry.
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    identity: Identity,
    retry_delay: Duration,
}

impl std::fmt::Debug for SmtpMailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpMailer")
            .field("from", &self.identity.from.to_string())
            .finish_non_exhaustive()
    }
}

impl SmtpMailer {
    /// Builds the pooled transport. Nothing connects until the first send.
    pub fn new(config: &MailConfig) -> Result<Self, MailError> {
        let identity = Identity::new(config)?;
        let smtp = &config.smtp;
        let builder = if smtp.starttls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
        }
        .map_err(|e| MailError::Config(format!("smtp host {:?}: {e}", smtp.host)))?;
        let mut builder = builder
            .port(smtp.port)
            .timeout(Some(Duration::from_secs(30)))
            .pool_config(PoolConfig::new().max_size(2));
        if !smtp.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                smtp.username.clone(),
                smtp.password.clone(),
            ));
        }
        Ok(Self {
            transport: builder.build(),
            identity,
            retry_delay: Duration::from_secs(5),
        })
    }

    #[cfg(test)]
    fn with_transport(
        transport: AsyncSmtpTransport<Tokio1Executor>,
        config: &MailConfig,
    ) -> Result<Self, MailError> {
        Ok(Self {
            transport,
            identity: Identity::new(config)?,
            retry_delay: Duration::from_millis(10),
        })
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, action: &ActionId, mail: &OutgoingMail) -> Result<String, MailError> {
        let message_id = generate_message_id(self.identity.domain());
        let message = self.identity.build(action, mail, &message_id)?;
        let bytes = message.formatted();

        let mut attempt = 0;
        loop {
            match self.transport.send_raw(message.envelope(), &bytes).await {
                Ok(_) => return Ok(message_id),
                Err(e) if attempt == 0 && is_retryable(&e) => {
                    warn!(%action, error = %e, "smtp send failed, retrying once");
                    attempt += 1;
                    tokio::time::sleep(self.retry_delay).await;
                }
                Err(e) => return Err(smtp_error(&e)),
            }
        }
    }
}

/// Retried unless it is a refusal, a TLS failure or a client-side error.
fn is_retryable(e: &lettre::transport::smtp::Error) -> bool {
    !e.is_permanent() && !e.is_tls() && !e.is_client()
}

fn smtp_error(e: &lettre::transport::smtp::Error) -> MailError {
    match e.status() {
        Some(code) if e.is_permanent() => MailError::Rejected {
            code: u16::from(code),
            message: std::error::Error::source(e)
                .map(ToString::to_string)
                .unwrap_or_else(|| e.to_string()),
        },
        _ => MailError::Transport(e.to_string()),
    }
}

/// Writes each mail as an `.eml` file named `<unix ms>-<message id>.eml`.
#[derive(Debug, Clone)]
pub struct FileMailer {
    dir: PathBuf,
    identity: Identity,
}

impl FileMailer {
    /// Only `from` and `reply_to` of the config are used.
    pub fn new(dir: impl Into<PathBuf>, config: &MailConfig) -> Result<Self, MailError> {
        Ok(Self {
            dir: dir.into(),
            identity: Identity::new(config)?,
        })
    }
}

#[async_trait]
impl Mailer for FileMailer {
    async fn send(&self, action: &ActionId, mail: &OutgoingMail) -> Result<String, MailError> {
        let message_id = generate_message_id(self.identity.domain());
        let bytes = self.identity.build(action, mail, &message_id)?.formatted();

        tokio::fs::create_dir_all(&self.dir).await?;
        let stamp = jiff::Timestamp::now().as_millisecond();
        let name = format!("{stamp:013}-{}.eml", message_id.replace('@', "_at_"));
        // Written under a name DirInbox ignores, then renamed, so a reader
        // never sees half a file.
        let temp = self.dir.join(format!(".{name}.tmp"));
        tokio::fs::write(&temp, &bytes).await?;
        tokio::fs::rename(&temp, self.dir.join(&name)).await?;
        Ok(message_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SmtpConfig;
    use std::collections::{HashSet, VecDeque};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn config(from: &str) -> MailConfig {
        MailConfig {
            from: from.to_owned(),
            reply_to: Some("Host <host@example.org>".to_owned()),
            smtp: SmtpConfig {
                host: "smtp.example.org".to_owned(),
                port: 587,
                username: "q".to_owned(),
                password: "p".to_owned(),
                starttls: true,
            },
            imap: None,
        }
    }

    fn mail() -> OutgoingMail {
        OutgoingMail {
            to: "Priya <priya@venue.example>".to_owned(),
            subject: "Table for six, Thursday".to_owned(),
            body: "Hello,\n\nCould we have a table for six at 7:30?\n".to_owned(),
            in_reply_to: None,
        }
    }

    #[test]
    fn message_ids_are_unique_and_well_formed() {
        let ids: HashSet<String> = (0..1000)
            .map(|_| generate_message_id("Example.ORG"))
            .collect();
        assert_eq!(ids.len(), 1000);
        for id in &ids {
            let (local, domain) = id.split_once('@').unwrap();
            assert_eq!(domain, "example.org");
            assert_eq!(local.len(), 26);
            assert!(local.chars().all(|c| c.is_ascii_alphanumeric()));
            assert!(!id.contains(['<', '>', ' ']));
        }
        assert_eq!(
            generate_message_id("").split_once('@').unwrap().1,
            "localhost.invalid"
        );
    }

    #[tokio::test]
    async fn file_mailer_writes_an_eml_with_the_expected_headers() {
        let dir = tempfile::tempdir().unwrap();
        let mailer = FileMailer::new(
            dir.path().join("outbox"),
            &config("Q for The Six <q@example.org>"),
        )
        .unwrap();
        let action = ActionId::new("01JACTION");
        let id = mailer.send(&action, &mail()).await.unwrap();

        let files: Vec<_> = std::fs::read_dir(dir.path().join("outbox"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].extension().unwrap(), "eml");

        let raw = std::fs::read(&files[0]).unwrap();
        let parsed = mail_parser::MessageParser::new().parse(&raw).unwrap();
        assert_eq!(parsed.message_id(), Some(id.as_str()));
        assert_eq!(parsed.subject(), Some("Table for six, Thursday"));
        let from = parsed.from().unwrap().first().unwrap();
        assert_eq!(from.address(), Some("q@example.org"));
        assert_eq!(from.name(), Some("Q for The Six"));
        let to = parsed.to().unwrap().first().unwrap();
        assert_eq!(to.address(), Some("priya@venue.example"));
        let reply_to = parsed.reply_to().unwrap().first().unwrap();
        assert_eq!(reply_to.address(), Some("host@example.org"));
        let action_header = parsed.headers().get("X-Ours-Action").unwrap();
        assert_eq!(
            String::from_utf8_lossy(action_header.raw_value()).trim(),
            "01JACTION"
        );
        assert!(parsed.in_reply_to().is_none());
        assert!(parsed.date().is_some());
        assert!(
            parsed
                .body_text(0)
                .unwrap()
                .contains("table for six at 7:30")
        );
        assert!(parsed.is_content_type("text", "plain"));
    }

    #[tokio::test]
    async fn replies_carry_in_reply_to_and_references() {
        let dir = tempfile::tempdir().unwrap();
        let mailer = FileMailer::new(dir.path(), &config("q@example.org")).unwrap();
        let mut reply = mail();
        reply.in_reply_to = Some("<earlier-1@venue.example>".to_owned());
        mailer.send(&ActionId::new("A1"), &reply).await.unwrap();

        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let raw = std::fs::read(path).unwrap();
        let parsed = mail_parser::MessageParser::new().parse(&raw).unwrap();
        assert_eq!(
            parsed.in_reply_to().unwrap().first(),
            Some("earlier-1@venue.example")
        );
        assert_eq!(
            parsed.references().unwrap().first(),
            Some("earlier-1@venue.example")
        );
    }

    #[tokio::test]
    async fn headers_cannot_be_injected_through_the_subject_or_parent() {
        let dir = tempfile::tempdir().unwrap();
        let mailer = FileMailer::new(dir.path(), &config("q@example.org")).unwrap();
        let mut bad = mail();
        bad.subject = "Hi\r\nBcc: spy@evil.example".to_owned();
        bad.in_reply_to = Some("x@y>\r\nBcc: spy@evil.example".to_owned());
        mailer.send(&ActionId::new("A2"), &bad).await.unwrap();

        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let raw = std::fs::read(path).unwrap();
        let parsed = mail_parser::MessageParser::new().parse(&raw).unwrap();
        assert!(parsed.bcc().is_none());
        assert!(parsed.in_reply_to().is_none());
    }

    #[tokio::test]
    async fn an_invalid_recipient_is_rejected_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let mailer = FileMailer::new(dir.path(), &config("q@example.org")).unwrap();
        for to in ["not an address", "a@x.example, b@y.example", ""] {
            let mut bad = mail();
            bad.to = to.to_owned();
            let err = mailer.send(&ActionId::new("A3"), &bad).await.unwrap_err();
            assert!(
                matches!(err, MailError::Rejected { code: 501, .. }),
                "{to:?}: {err:?}"
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_bad_from_address_is_a_config_error() {
        let err = FileMailer::new("/nonexistent", &config("nope")).unwrap_err();
        assert!(matches!(err, MailError::Config(_)));
        let err = SmtpMailer::new(&config("nope")).unwrap_err();
        assert!(matches!(err, MailError::Config(_)));
    }

    #[tokio::test]
    async fn smtp_mailer_builds_for_both_tls_modes() {
        let mut c = config("q@example.org");
        assert!(SmtpMailer::new(&c).is_ok());
        c.smtp.starttls = false;
        c.smtp.port = 465;
        assert!(SmtpMailer::new(&c).is_ok());
    }

    /// A scripted SMTP server on loopback. Each DATA gets the next reply;
    /// every message body it receives is recorded.
    async fn fake_smtp(replies: Vec<&'static str>) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let received = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&received);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let replies = Arc::clone(&replies);
                let received = Arc::clone(&received);
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut read = BufReader::new(read);
                    let _ = write.write_all(b"220 fake ESMTP\r\n").await;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let cmd = line.trim().to_ascii_uppercase();
                        if cmd.starts_with("DATA") {
                            let _ = write.write_all(b"354 go ahead\r\n").await;
                            let mut data = String::new();
                            loop {
                                line.clear();
                                if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                                    return;
                                }
                                if line == ".\r\n" {
                                    break;
                                }
                                data.push_str(&line);
                            }
                            let reply = replies.lock().unwrap().pop_front().unwrap_or("250 queued");
                            received.lock().unwrap().push(data);
                            let _ = write.write_all(format!("{reply}\r\n").as_bytes()).await;
                        } else if cmd.starts_with("QUIT") {
                            let _ = write.write_all(b"221 bye\r\n").await;
                            return;
                        } else if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
                            let _ = write.write_all(b"250 fake\r\n").await;
                        } else {
                            let _ = write.write_all(b"250 ok\r\n").await;
                        }
                    }
                });
            }
        });
        (port, seen)
    }

    fn plain_transport(port: u16) -> AsyncSmtpTransport<Tokio1Executor> {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
            .port(port)
            .build()
    }

    #[tokio::test]
    async fn a_transient_error_is_retried_once_with_the_same_message_id() {
        let (port, received) = fake_smtp(vec!["451 try again later", "250 queued"]).await;
        let mailer =
            SmtpMailer::with_transport(plain_transport(port), &config("q@example.org")).unwrap();
        let id = mailer.send(&ActionId::new("A4"), &mail()).await.unwrap();

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        for data in received.iter() {
            assert!(data.contains(&format!("Message-ID: <{id}>")), "{data}");
            assert!(data.contains("X-Ours-Action: A4"), "{data}");
        }
    }

    #[tokio::test]
    async fn a_second_transient_error_is_a_transport_error() {
        let (port, received) = fake_smtp(vec!["451 later", "451 still later"]).await;
        let mailer =
            SmtpMailer::with_transport(plain_transport(port), &config("q@example.org")).unwrap();
        let err = mailer
            .send(&ActionId::new("A5"), &mail())
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Transport(_)), "{err:?}");
        assert_eq!(received.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_permanent_error_is_rejected_without_retry() {
        let (port, received) = fake_smtp(vec!["550 no such mailbox"]).await;
        let mailer =
            SmtpMailer::with_transport(plain_transport(port), &config("q@example.org")).unwrap();
        let err = mailer
            .send(&ActionId::new("A6"), &mail())
            .await
            .unwrap_err();
        match err {
            MailError::Rejected { code, message } => {
                assert_eq!(code, 550);
                assert!(message.contains("no such mailbox"), "{message}");
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_transport_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mailer =
            SmtpMailer::with_transport(plain_transport(port), &config("q@example.org")).unwrap();
        let err = mailer
            .send(&ActionId::new("A7"), &mail())
            .await
            .unwrap_err();
        assert!(matches!(err, MailError::Transport(_)), "{err:?}");
    }
}
