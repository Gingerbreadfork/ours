//! Receiving mail: the `Inbox` trait, IMAP through async-imap, and an
//! inbox that reads `.eml` files from a directory.

use crate::config::{ImapConfig, MailConfig};
use crate::error::MailError;
use crate::parse::parse_eml;
use async_imap::{Client, Session};
use async_trait::async_trait;
use futures::TryStreamExt;
use ours_types::{InboundMail, Instant};
use rustls::pki_types::ServerName;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tracing::{debug, warn};

/// Larger messages are cut off here; the rest is never downloaded.
const MAX_MESSAGE_BYTES: u64 = 4 * 1024 * 1024;
/// At most this many messages are taken in one poll; the rest wait.
const MAX_PER_POLL: usize = 100;
/// One whole IMAP session, connect to logout, may take this long.
const POLL_TIMEOUT: Duration = Duration::from_secs(120);

/// Where a fetched message lives in its inbox, so it can be marked seen
/// once it has been handed on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Receipt(Spot);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Spot {
    Imap { validity: Option<u32>, uid: u32 },
    File(PathBuf),
}

impl Receipt {
    pub(crate) fn imap(validity: Option<u32>, uid: u32) -> Self {
        Self(Spot::Imap { validity, uid })
    }

    fn file(path: PathBuf) -> Self {
        Self(Spot::File(path))
    }
}

/// One message from an inbox.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub mail: InboundMail,
    pub receipt: Receipt,
}

/// A source of new mail. A message comes back from every poll until it is
/// marked seen.
#[async_trait]
pub trait Inbox: Send + Sync {
    /// Unseen mail, oldest first, leaving out the messages in `skip`. A
    /// message that cannot be read as mail is set aside instead.
    async fn poll(&self, skip: &HashSet<Receipt>) -> Result<Vec<Fetched>, MailError>;

    /// Marks messages seen so no later poll returns them. Marking one
    /// twice does no harm.
    async fn mark_seen(&self, receipts: &[Receipt]) -> Result<(), MailError>;
}

fn now() -> Instant {
    Instant(jiff::Timestamp::now())
}

/// Orders mail oldest first; equal dates keep the order they arrived in.
fn oldest_first(mails: &mut [Fetched]) {
    mails.sort_by_key(|m| m.mail.at);
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

    async fn connect(&self) -> Result<Client<TlsStream<TcpStream>>, MailError> {
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
        Ok(Client::new(tls))
    }
}

#[async_trait]
impl Inbox for ImapInbox {
    async fn poll(&self, skip: &HashSet<Receipt>) -> Result<Vec<Fetched>, MailError> {
        timed(async { fetch_unseen(self.connect().await?, &self.config, now(), skip).await }).await
    }

    async fn mark_seen(&self, receipts: &[Receipt]) -> Result<(), MailError> {
        if receipts.is_empty() {
            return Ok(());
        }
        timed(async { store_seen(self.connect().await?, &self.config, receipts).await }).await
    }
}

async fn timed<T>(session: impl Future<Output = Result<T, MailError>>) -> Result<T, MailError> {
    tokio::time::timeout(POLL_TIMEOUT, session)
        .await
        .map_err(|_| MailError::Imap("timed out".to_owned()))?
}

fn imap_error(e: async_imap::error::Error) -> MailError {
    MailError::Imap(e.to_string())
}

async fn login<S>(mut client: Client<S>, config: &ImapConfig) -> Result<Session<S>, MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    match client.read_response().await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(MailError::Imap("closed before the greeting".to_owned())),
        Err(e) => return Err(MailError::Imap(e.to_string())),
    }
    client
        .login(&config.username, &config.password)
        .await
        .map_err(|(e, _)| imap_error(e))
}

async fn logout<S>(mut session: Session<S>)
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    if let Err(e) = session.logout().await {
        debug!(error = %e, "imap logout failed");
    }
}

/// Logs in and takes the unseen messages of the folder, oldest first,
/// leaving out `skip`. None of them is marked seen.
async fn fetch_unseen<S>(
    client: Client<S>,
    config: &ImapConfig,
    fallback_now: Instant,
    skip: &HashSet<Receipt>,
) -> Result<Vec<Fetched>, MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let mut session = login(client, config).await?;
    let result = take_unseen(
        &mut session,
        &config.folder,
        config.receiver(),
        fallback_now,
        skip,
    )
    .await;
    logout(session).await;
    result
}

/// Logs in and marks the messages seen.
async fn store_seen<S>(
    client: Client<S>,
    config: &ImapConfig,
    receipts: &[Receipt],
) -> Result<(), MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let mut session = login(client, config).await?;
    let result = flag_receipts(&mut session, &config.folder, receipts).await;
    logout(session).await;
    result
}

async fn flag_receipts<S>(
    session: &mut Session<S>,
    folder: &str,
    receipts: &[Receipt],
) -> Result<(), MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let validity = session
        .select(folder)
        .await
        .map_err(imap_error)?
        .uid_validity;
    // After a UIDVALIDITY change the old uids name nothing; those messages
    // come back from the next poll under new ones.
    let uids: Vec<u32> = receipts
        .iter()
        .filter_map(|r| match r.0 {
            Spot::Imap { validity: v, uid } if v == validity => Some(uid),
            _ => None,
        })
        .collect();
    flag_seen(session, &uids).await
}

async fn flag_seen<S>(session: &mut Session<S>, uids: &[u32]) -> Result<(), MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    if uids.is_empty() {
        return Ok(());
    }
    session
        .uid_store(uid_set(uids), "+FLAGS.SILENT (\\Seen)")
        .await
        .map_err(imap_error)?
        .try_collect::<Vec<_>>()
        .await
        .map_err(imap_error)?;
    Ok(())
}

async fn take_unseen<S>(
    session: &mut Session<S>,
    folder: &str,
    receiver: &str,
    fallback_now: Instant,
    skip: &HashSet<Receipt>,
) -> Result<Vec<Fetched>, MailError>
where
    S: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    let validity = session
        .select(folder)
        .await
        .map_err(imap_error)?
        .uid_validity;
    let mut uids: Vec<u32> = session
        .uid_search("UNSEEN UNDELETED")
        .await
        .map_err(imap_error)?
        .into_iter()
        .filter(|&uid| !skip.contains(&Receipt::imap(validity, uid)))
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
    let mut broken: Vec<u32> = Vec::new();
    for fetch in &fetched {
        let Some(uid) = fetch.uid else { continue };
        let Some(body) = fetch.body() else {
            warn!(uid, "imap message came back without a body");
            broken.push(uid);
            continue;
        };
        let arrived = fetch
            .internal_date()
            .and_then(|d| Instant::from_millis(d.timestamp_millis()))
            .unwrap_or(fallback_now);
        match parse_eml(body, arrived, Some(receiver)) {
            Ok(mail) => mails.push(Fetched {
                mail,
                receipt: Receipt::imap(validity, uid),
            }),
            Err(e) => {
                warn!(uid, error = %e, "skipping an unreadable message");
                broken.push(uid);
            }
        }
    }
    drop(fetched);

    // Nothing will ever take an unreadable message, so it is marked seen
    // now rather than fetched again on every poll.
    if let Err(e) = flag_seen(session, &broken).await {
        warn!(error = %e, "cannot mark unreadable messages seen");
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

/// Reads `.eml` files from a directory. A file moves into `seen/` when it
/// is marked seen, and into `failed/` when it cannot be read as mail.
#[derive(Debug, Clone)]
pub struct DirInbox {
    dir: PathBuf,
    authserv_id: Option<String>,
    /// Unreadable files that could not be moved aside; later polls pass them by.
    stuck: Arc<Mutex<HashSet<PathBuf>>>,
}

impl DirInbox {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            authserv_id: None,
            stuck: Arc::default(),
        }
    }

    /// Trust Authentication-Results headers with this authserv-id.
    /// Without it every mail from the directory is unverified.
    pub fn with_authserv_id(mut self, id: impl Into<String>) -> Self {
        self.authserv_id = Some(id.into());
        self
    }

    fn stuck(&self) -> MutexGuard<'_, HashSet<PathBuf>> {
        self.stuck.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn set_aside(&self, path: &Path) {
        if let Err(e) = move_to(path, &self.dir.join("failed")).await {
            warn!(file = %path.display(), error = %e, "cannot move mail file to failed");
            self.stuck().insert(path.to_owned());
        }
    }
}

#[async_trait]
impl Inbox for DirInbox {
    async fn poll(&self, skip: &HashSet<Receipt>) -> Result<Vec<Fetched>, MailError> {
        let mut paths = match list_eml(&self.dir).await {
            Ok(paths) => paths,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        {
            let stuck = self.stuck();
            paths.retain(|p| !stuck.contains(p) && !skip.contains(&Receipt::file(p.clone())));
        }
        paths.sort();
        paths.truncate(MAX_PER_POLL);

        let mut mails = Vec::with_capacity(paths.len());
        for path in paths {
            let parsed = match read_capped(&path).await {
                Ok(bytes) => parse_eml(&bytes, now(), self.authserv_id.as_deref()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => Err(e.into()),
            };
            match parsed {
                Ok(mail) => mails.push(Fetched {
                    mail,
                    receipt: Receipt::file(path),
                }),
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "cannot read mail file");
                    self.set_aside(&path).await;
                }
            }
        }
        oldest_first(&mut mails);
        Ok(mails)
    }

    async fn mark_seen(&self, receipts: &[Receipt]) -> Result<(), MailError> {
        let seen = self.dir.join("seen");
        let mut failed = None;
        for receipt in receipts {
            let Spot::File(path) = &receipt.0 else {
                continue;
            };
            match move_to(path, &seen).await {
                Ok(()) => {}
                // Moved already by an earlier call.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!(file = %path.display(), error = %e, "cannot move mail file to seen");
                    failed.get_or_insert(e);
                }
            }
        }
        failed.map_or(Ok(()), |e| Err(e.into()))
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
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn eml(from: &str, subject: &str, date: Option<&str>) -> String {
        let date = date.map(|d| format!("Date: {d}\r\n")).unwrap_or_default();
        format!("From: {from}\r\nSubject: {subject}\r\n{date}\r\nBody of {subject}\r\n")
    }

    const NO_SENDER: &str = "Subject: no sender\r\n\r\nhi\r\n";

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn subjects(mails: &[Fetched]) -> Vec<&str> {
        mails.iter().map(|m| m.mail.subject.as_str()).collect()
    }

    fn receipts(mails: &[Fetched]) -> Vec<Receipt> {
        mails.iter().map(|m| m.receipt.clone()).collect()
    }

    #[tokio::test]
    async fn dir_inbox_moves_a_file_to_seen_only_when_marked() {
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
        write("broken.eml", NO_SENDER.to_owned());

        let inbox = DirInbox::new(dir.path());
        let none = HashSet::new();
        let mails = inbox.poll(&none).await.unwrap();
        assert_eq!(subjects(&mails), ["oldest", "middle", "newest"]);
        assert_eq!(
            names(dir.path()),
            ["a.eml", "b.eml", "c.EML", "failed", "notes.txt"]
        );
        assert_eq!(names(&dir.path().join("failed")), ["broken.eml"]);

        let held: HashSet<Receipt> = receipts(&mails).into_iter().collect();
        assert!(inbox.poll(&held).await.unwrap().is_empty());
        assert_eq!(inbox.poll(&none).await.unwrap().len(), 3);

        inbox.mark_seen(&receipts(&mails)).await.unwrap();
        assert_eq!(names(dir.path()), ["failed", "notes.txt", "seen"]);
        assert_eq!(names(&dir.path().join("seen")), ["a.eml", "b.eml", "c.EML"]);
        assert!(inbox.poll(&none).await.unwrap().is_empty());
        inbox.mark_seen(&receipts(&mails)).await.unwrap();
    }

    #[tokio::test]
    async fn dir_inbox_does_not_overwrite_an_earlier_seen_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("seen")).unwrap();
        std::fs::write(dir.path().join("seen/a.eml"), "old").unwrap();
        std::fs::write(dir.path().join("a.eml"), eml("a@x.example", "again", None)).unwrap();

        let inbox = DirInbox::new(dir.path());
        let mails = inbox.poll(&HashSet::new()).await.unwrap();
        assert_eq!(mails.len(), 1);
        inbox.mark_seen(&receipts(&mails)).await.unwrap();
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
        assert!(inbox.poll(&HashSet::new()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unreadable_files_that_cannot_be_moved_do_not_starve_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        // A plain file where failed/ would go, so nothing can be set aside.
        std::fs::write(dir.path().join("failed"), "").unwrap();
        for n in 0..MAX_PER_POLL {
            std::fs::write(dir.path().join(format!("a{n:03}.eml")), NO_SENDER).unwrap();
        }
        std::fs::write(dir.path().join("b.eml"), eml("b@x.example", "good", None)).unwrap();

        let inbox = DirInbox::new(dir.path());
        let none = HashSet::new();
        assert!(inbox.poll(&none).await.unwrap().is_empty());
        assert_eq!(subjects(&inbox.poll(&none).await.unwrap()), ["good"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn files_that_cannot_be_opened_are_set_aside_and_do_not_starve_the_rest() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut locked = Vec::new();
        for n in 0..MAX_PER_POLL {
            let name = format!("a{n:03}.eml");
            let path = dir.path().join(&name);
            std::fs::write(&path, eml("a@x.example", "locked", None)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            locked.push(name);
        }
        // File permissions do not stop root.
        if std::fs::read(dir.path().join(&locked[0])).is_ok() {
            return;
        }
        std::fs::write(dir.path().join("b.eml"), eml("b@x.example", "good", None)).unwrap();

        let inbox = DirInbox::new(dir.path());
        let none = HashSet::new();
        assert!(inbox.poll(&none).await.unwrap().is_empty());
        assert_eq!(names(&dir.path().join("failed")), locked);
        assert_eq!(subjects(&inbox.poll(&none).await.unwrap()), ["good"]);
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

    /// A scripted IMAP server on loopback, one connection after another.
    /// It keeps each message's \Seen flag and records the uid set of each
    /// STORE.
    async fn fake_imap(messages: Vec<(u32, String)>) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stores = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&stores);
        tokio::spawn(async move {
            let seen = Mutex::new(HashSet::new());
            while let Ok((stream, _)) = listener.accept().await {
                serve(stream, &messages, &seen, &recorded).await;
            }
        });
        (port, stores)
    }

    async fn serve(
        stream: TcpStream,
        messages: &[(u32, String)],
        seen: &Mutex<HashSet<u32>>,
        stores: &Mutex<Vec<String>>,
    ) {
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
            let set_text = rest.split_whitespace().nth(2).unwrap_or("").to_owned();
            let set: Vec<u32> = set_text.split(',').filter_map(|u| u.parse().ok()).collect();
            let reply = if upper.starts_with("LOGIN") {
                format!("{tag} OK LOGIN completed\r\n")
            } else if upper.starts_with("SELECT") {
                format!(
                    "* FLAGS (\\Seen \\Deleted)\r\n* {} EXISTS\r\n* 0 RECENT\r\n\
                     * OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-WRITE] SELECT completed\r\n",
                    messages.len()
                )
            } else if upper.starts_with("UID SEARCH") {
                let seen = seen.lock().unwrap();
                let uids: String = messages
                    .iter()
                    .rev()
                    .filter(|(u, _)| !seen.contains(u))
                    .map(|(u, _)| format!(" {u}"))
                    .collect();
                format!("* SEARCH{uids}\r\n{tag} OK SEARCH completed\r\n")
            } else if upper.starts_with("UID FETCH") {
                let mut out = String::new();
                for (n, (uid, text)) in messages.iter().enumerate() {
                    if !set.contains(uid) {
                        continue;
                    }
                    out.push_str(&format!(
                        "* {} FETCH (UID {uid} INTERNALDATE \"02-Nov-2026 18:00:00 +0000\" BODY[]<0> {{{}}}\r\n{text})\r\n",
                        n + 1,
                        text.len()
                    ));
                }
                out.push_str(&format!("{tag} OK FETCH completed\r\n"));
                out
            } else if upper.starts_with("UID STORE") {
                stores.lock().unwrap().push(set_text);
                seen.lock().unwrap().extend(set);
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
    }

    async fn client(port: u16) -> Client<TcpStream> {
        Client::new(TcpStream::connect(("127.0.0.1", port)).await.unwrap())
    }

    #[tokio::test]
    async fn imap_marks_a_message_seen_only_when_asked() {
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
            (13, NO_SENDER.to_owned()),
        ];
        let (port, stores) = fake_imap(messages).await;
        let config = ImapConfig {
            host: "127.0.0.1".to_owned(),
            port,
            username: "u".to_owned(),
            password: "p".to_owned(),
            folder: "INBOX".to_owned(),
            poll_secs: 60,
            authserv_id: None,
        };
        let none = HashSet::new();
        let fetch = async |skip: &HashSet<Receipt>| {
            fetch_unseen(client(port).await, &config, Instant::UNIX_EPOCH, skip)
                .await
                .unwrap()
        };

        let mails = fetch(&none).await;
        assert_eq!(subjects(&mails), ["earlier", "later"]);
        assert_eq!(mails[0].mail.from, "b@x.example");
        assert_eq!(mails[0].mail.body, "Body of earlier");
        assert_eq!(mails[0].receipt, Receipt::imap(Some(1), 12));
        // Fetching marks only the unreadable one seen.
        assert_eq!(*stores.lock().unwrap(), ["13"]);

        let held: HashSet<Receipt> = receipts(&mails).into_iter().collect();
        assert!(fetch(&held).await.is_empty());
        assert_eq!(fetch(&none).await.len(), 2);

        store_seen(client(port).await, &config, &receipts(&mails[..1]))
            .await
            .unwrap();
        assert_eq!(*stores.lock().unwrap(), ["13", "12"]);
        assert_eq!(subjects(&fetch(&none).await), ["later"]);

        // A uid from before a UIDVALIDITY change is left alone.
        store_seen(client(port).await, &config, &[Receipt::imap(Some(2), 11)])
            .await
            .unwrap();
        assert_eq!(stores.lock().unwrap().len(), 2);
    }
}
