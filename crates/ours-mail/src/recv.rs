//! Receiving mail: the `Inbox` trait, IMAP through async-imap, and an
//! inbox that reads `.eml` files from a directory.

use crate::config::{ImapConfig, MailConfig};
use crate::error::MailError;
use crate::parse::parse_eml;
use async_imap::Client;
use async_trait::async_trait;
use futures::TryStreamExt;
use ours_types::{InboundMail, Instant};
use rustls::pki_types::ServerName;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, warn};

/// Larger messages are cut off here; the rest is never downloaded.
const MAX_MESSAGE_BYTES: u64 = 4 * 1024 * 1024;
/// At most this many messages are taken in one poll; the rest wait.
const MAX_PER_POLL: usize = 100;
/// One whole poll, connect to logout, may take this long.
const POLL_TIMEOUT: Duration = Duration::from_secs(120);

/// A source of new mail. Each message is returned once, oldest first.
#[async_trait]
pub trait Inbox: Send + Sync {
    async fn poll(&self) -> Result<Vec<InboundMail>, MailError>;
}

fn now() -> Instant {
    Instant(jiff::Timestamp::now())
}

/// Orders mail oldest first; equal dates keep the order they arrived in.
fn oldest_first(mails: &mut [InboundMail]) {
    mails.sort_by_key(|m| m.at);
}

/// Unseen mail in one IMAP folder, over TLS from the first byte.
pub struct ImapInbox {
    config: ImapConfig,
    connector: TlsConnector,
}

impl Debug for ImapInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapInbox")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ImapInbox {
    /// Fails with `Config` when the config has no `[imap]` section.
    pub fn new(config: &MailConfig) -> Result<Self, MailError> {
        let imap = config
            .imap
            .clone()
            .ok_or_else(|| MailError::Config("no imap section".to_owned()))?;
        if imap.host.trim().is_empty() {
            return Err(MailError::Config("imap host is empty".to_owned()));
        }
        ServerName::try_from(imap.host.clone())
            .map_err(|e| MailError::Config(format!("imap host {:?}: {e}", imap.host)))?;

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| MailError::Config(format!("tls: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            config: imap,
            connector: TlsConnector::from(Arc::new(tls)),
        })
    }

    async fn poll_once(&self) -> Result<Vec<InboundMail>, MailError> {
        let imap = |e: &dyn std::fmt::Display| MailError::Imap(e.to_string());
        let tcp = TcpStream::connect((self.config.host.as_str(), self.config.port))
            .await
            .map_err(|e| imap(&e))?;
        let name = ServerName::try_from(self.config.host.clone()).map_err(|e| imap(&e))?;
        let tls = self
            .connector
            .connect(name, tcp)
            .await
            .map_err(|e| imap(&e))?;
        fetch_unseen(Client::new(tls), &self.config, now()).await
    }
}

#[async_trait]
impl Inbox for ImapInbox {
    async fn poll(&self) -> Result<Vec<InboundMail>, MailError> {
        tokio::time::timeout(POLL_TIMEOUT, self.poll_once())
            .await
            .map_err(|_| MailError::Imap("poll timed out".to_owned()))?
    }
}

fn imap_error(e: async_imap::error::Error) -> MailError {
    MailError::Imap(e.to_string())
}

/// Logs in, takes the unseen messages of the folder, marks them seen and
/// returns them oldest first. A message that cannot be parsed is marked
/// seen too, so it is not fetched again on every poll.
async fn fetch_unseen<S>(
    mut client: Client<S>,
    config: &ImapConfig,
    fallback_now: Instant,
) -> Result<Vec<InboundMail>, MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    match client.read_response().await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(MailError::Imap("closed before the greeting".to_owned())),
        Err(e) => return Err(MailError::Imap(e.to_string())),
    }
    let mut session = client
        .login(&config.username, &config.password)
        .await
        .map_err(|(e, _)| imap_error(e))?;

    let result = take_unseen(
        &mut session,
        &config.folder,
        config.receiver(),
        fallback_now,
    )
    .await;
    if let Err(e) = session.logout().await {
        debug!(error = %e, "imap logout failed");
    }
    result
}

async fn take_unseen<S>(
    session: &mut async_imap::Session<S>,
    folder: &str,
    receiver: &str,
    fallback_now: Instant,
) -> Result<Vec<InboundMail>, MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    session.select(folder).await.map_err(imap_error)?;
    let mut uids: Vec<u32> = session
        .uid_search("UNSEEN UNDELETED")
        .await
        .map_err(imap_error)?
        .into_iter()
        .collect();
    uids.sort_unstable();
    uids.truncate(MAX_PER_POLL);
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uid_set(&uids);

    let query = format!("(UID INTERNALDATE BODY.PEEK[]<0.{MAX_MESSAGE_BYTES}>)");
    let fetched: Vec<_> = session
        .uid_fetch(&set, query)
        .await
        .map_err(imap_error)?
        .try_collect()
        .await
        .map_err(imap_error)?;

    let mut mails = Vec::with_capacity(fetched.len());
    let mut done: Vec<u32> = Vec::with_capacity(fetched.len());
    for fetch in &fetched {
        let Some(uid) = fetch.uid else { continue };
        done.push(uid);
        let Some(body) = fetch.body() else {
            warn!(uid, "imap message came back without a body");
            continue;
        };
        let arrived = fetch
            .internal_date()
            .and_then(|d| Instant::from_millis(d.timestamp_millis()))
            .unwrap_or(fallback_now);
        match parse_eml(body, arrived, Some(receiver)) {
            Ok(mail) => mails.push(mail),
            Err(e) => warn!(uid, error = %e, "skipping an unreadable message"),
        }
    }
    drop(fetched);

    if !done.is_empty() {
        session
            .uid_store(uid_set(&done), "+FLAGS.SILENT (\\Seen)")
            .await
            .map_err(imap_error)?
            .try_collect::<Vec<_>>()
            .await
            .map_err(imap_error)?;
    }
    oldest_first(&mut mails);
    Ok(mails)
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Reads `.eml` files from a directory and moves each one into `seen/`
/// once it has been read.
#[derive(Debug, Clone)]
pub struct DirInbox {
    dir: PathBuf,
    authserv_id: Option<String>,
}

impl DirInbox {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            authserv_id: None,
        }
    }

    /// Trust Authentication-Results headers with this authserv-id.
    /// Without it every mail from the directory is unverified.
    pub fn with_authserv_id(mut self, id: impl Into<String>) -> Self {
        self.authserv_id = Some(id.into());
        self
    }
}

#[async_trait]
impl Inbox for DirInbox {
    async fn poll(&self) -> Result<Vec<InboundMail>, MailError> {
        let mut paths = match list_eml(&self.dir).await {
            Ok(paths) => paths,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        paths.sort();
        paths.truncate(MAX_PER_POLL);

        let seen_dir = self.dir.join("seen");
        let mut mails = Vec::with_capacity(paths.len());
        for path in paths {
            let bytes = match read_capped(&path).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "cannot read mail file");
                    continue;
                }
            };
            let parsed = parse_eml(&bytes, now(), self.authserv_id.as_deref());
            // A file that stays in place would be returned again next poll.
            if let Err(e) = move_to(&path, &seen_dir).await {
                warn!(file = %path.display(), error = %e, "cannot move mail file to seen");
                continue;
            }
            match parsed {
                Ok(mail) => mails.push(mail),
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "skipping an unreadable message")
                }
            }
        }
        oldest_first(&mut mails);
        Ok(mails)
    }
}

async fn list_eml(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let is_eml = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("eml"));
        if is_eml && entry.file_type().await?.is_file() {
            paths.push(path);
        }
    }
    Ok(paths)
}

async fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(MAX_MESSAGE_BYTES).read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn move_to(path: &Path, dir: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut target = dir.join(name.as_ref());
    let mut n = 1;
    while tokio::fs::try_exists(&target).await? {
        target = dir.join(format!("{name}.{n}"));
        n += 1;
    }
    tokio::fs::rename(path, target).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn eml(from: &str, subject: &str, date: Option<&str>) -> String {
        let date = date.map(|d| format!("Date: {d}\r\n")).unwrap_or_default();
        format!("From: {from}\r\nSubject: {subject}\r\n{date}\r\nBody of {subject}\r\n")
    }

    #[tokio::test]
    async fn dir_inbox_reads_oldest_first_and_moves_files() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: String| std::fs::write(dir.path().join(name), text).unwrap();
        // File names sort the other way round from the dates.
        write(
            "a.eml",
            eml(
                "a@x.example",
                "newest",
                Some("Tue, 04 Nov 2025 10:00:00 +0000"),
            ),
        );
        write(
            "b.eml",
            eml(
                "b@x.example",
                "oldest",
                Some("Mon, 03 Nov 2025 10:00:00 +0000"),
            ),
        );
        write(
            "c.EML",
            eml(
                "c@x.example",
                "middle",
                Some("Mon, 03 Nov 2025 20:00:00 +0000"),
            ),
        );
        write("notes.txt", "not mail".to_owned());
        write("broken.eml", "Subject: no sender\r\n\r\nhi\r\n".to_owned());

        let inbox = DirInbox::new(dir.path());
        let mails = inbox.poll().await.unwrap();
        let subjects: Vec<_> = mails.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, ["oldest", "middle", "newest"]);

        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt", "seen"]);
        let mut seen: Vec<_> = std::fs::read_dir(dir.path().join("seen"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        seen.sort();
        assert_eq!(seen, ["a.eml", "b.eml", "broken.eml", "c.EML"]);

        assert!(inbox.poll().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dir_inbox_does_not_overwrite_an_earlier_seen_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("seen")).unwrap();
        std::fs::write(dir.path().join("seen/a.eml"), "old").unwrap();
        std::fs::write(dir.path().join("a.eml"), eml("a@x.example", "again", None)).unwrap();

        let mails = DirInbox::new(dir.path()).poll().await.unwrap();
        assert_eq!(mails.len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("seen/a.eml")).unwrap(),
            "old"
        );
        assert!(dir.path().join("seen/a.eml.1").exists());
    }

    #[tokio::test]
    async fn dir_inbox_on_a_missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = DirInbox::new(dir.path().join("not-there"));
        assert!(inbox.poll().await.unwrap().is_empty());
    }

    #[test]
    fn imap_inbox_needs_an_imap_section() {
        let config: MailConfig = toml::from_str(
            "from = \"q@example.org\"\n[smtp]\nhost = \"h\"\nport = 587\nusername = \"u\"\npassword = \"p\"\n",
        )
        .unwrap();
        assert!(matches!(
            ImapInbox::new(&config).unwrap_err(),
            MailError::Config(_)
        ));
        let with_imap: MailConfig = toml::from_str(
            "from = \"q@example.org\"\n[smtp]\nhost = \"h\"\nport = 587\nusername = \"u\"\npassword = \"p\"\n\
             [imap]\nhost = \"imap.example.org\"\nport = 993\nusername = \"u\"\npassword = \"p\"\n",
        )
        .unwrap();
        assert!(ImapInbox::new(&with_imap).is_ok());
    }

    /// A scripted IMAP server on loopback holding three unseen messages.
    /// It records the sequence set of each STORE it receives.
    async fn fake_imap(messages: Vec<(u32, String)>) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stores = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&stores);
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read, mut write) = stream.into_split();
            let mut read = BufReader::new(read);
            let _ = write.write_all(b"* OK fake IMAP ready\r\n").await;
            let mut line = String::new();
            loop {
                line.clear();
                if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let mut words = line.trim().splitn(2, ' ');
                let tag = words.next().unwrap_or("").to_owned();
                let rest = words.next().unwrap_or("").to_owned();
                let upper = rest.to_ascii_uppercase();
                let reply = if upper.starts_with("LOGIN") {
                    format!("{tag} OK LOGIN completed\r\n")
                } else if upper.starts_with("SELECT") {
                    format!(
                        "* FLAGS (\\Seen \\Deleted)\r\n* {} EXISTS\r\n* 0 RECENT\r\n\
                         * OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-WRITE] SELECT completed\r\n",
                        messages.len()
                    )
                } else if upper.starts_with("UID SEARCH") {
                    let uids: Vec<String> =
                        messages.iter().rev().map(|(u, _)| u.to_string()).collect();
                    format!(
                        "* SEARCH {}\r\n{tag} OK SEARCH completed\r\n",
                        uids.join(" ")
                    )
                } else if upper.starts_with("UID FETCH") {
                    let mut out = String::new();
                    for (n, (uid, text)) in messages.iter().enumerate() {
                        out.push_str(&format!(
                            "* {} FETCH (UID {uid} INTERNALDATE \"02-Nov-2026 18:00:00 +0000\" BODY[]<0> {{{}}}\r\n{text})\r\n",
                            n + 1,
                            text.len()
                        ));
                    }
                    out.push_str(&format!("{tag} OK FETCH completed\r\n"));
                    out
                } else if upper.starts_with("UID STORE") {
                    let set = rest.split_whitespace().nth(2).unwrap_or("").to_owned();
                    recorded.lock().unwrap().push(set);
                    format!("{tag} OK STORE completed\r\n")
                } else if upper.starts_with("LOGOUT") {
                    let _ = write
                        .write_all(format!("* BYE bye\r\n{tag} OK LOGOUT completed\r\n").as_bytes())
                        .await;
                    return;
                } else {
                    format!("{tag} BAD unknown\r\n")
                };
                if write.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
        (port, stores)
    }

    #[tokio::test]
    async fn imap_fetch_parses_marks_seen_and_returns_oldest_first() {
        let messages = vec![
            (
                11,
                eml(
                    "a@x.example",
                    "later",
                    Some("Tue, 04 Nov 2025 10:00:00 +0000"),
                ),
            ),
            (
                12,
                eml(
                    "b@x.example",
                    "earlier",
                    Some("Mon, 03 Nov 2025 10:00:00 +0000"),
                ),
            ),
            (13, "Subject: no sender\r\n\r\nhi\r\n".to_owned()),
        ];
        let (port, stores) = fake_imap(messages).await;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let config = ImapConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: "u".to_owned(),
            password: "p".to_owned(),
            folder: "INBOX".to_owned(),
            poll_secs: 60,
            authserv_id: None,
        };
        let mails = fetch_unseen(Client::new(tcp), &config, Instant::UNIX_EPOCH)
            .await
            .unwrap();

        let subjects: Vec<_> = mails.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, ["earlier", "later"]);
        assert_eq!(mails[0].from, "b@x.example");
        assert_eq!(mails[0].body, "Body of earlier");
        // The unreadable one is marked seen along with the rest.
        assert_eq!(*stores.lock().unwrap(), ["11,12,13"]);
    }
}
