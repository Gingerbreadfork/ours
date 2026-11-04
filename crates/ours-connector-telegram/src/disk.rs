//! The poll records on disk, so votes and closes on a poll the agent opened
//! before a restart still reach the engine. They are one JSON file in the
//! connector's state directory, replaced whole on each change by a task of
//! its own, so no file I/O runs on the async runtime.

use crate::state::SavedPoll;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const FILE: &str = "polls.json";
const TEMP: &str = "polls.json.tmp";

#[derive(Debug, Serialize, Deserialize)]
struct PollFile {
    polls: Vec<SavedPoll>,
}

/// Takes the poll records after each change and hands them to the writer.
#[derive(Debug)]
pub struct Saver(watch::Sender<Vec<u8>>);

impl Saver {
    pub fn save(&self, polls: Vec<SavedPoll>) {
        match serde_json::to_vec(&PollFile { polls }) {
            Ok(bytes) => {
                self.0.send_replace(bytes);
            }
            Err(error) => tracing::warn!(%error, "could not encode the poll records"),
        }
    }
}

/// The saved poll records, oldest first. Empty when there is no file yet,
/// and empty, with a warning, when it cannot be read.
pub async fn load(dir: &Path) -> Vec<SavedPoll> {
    let path = dir.join(FILE);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read the saved polls");
            return Vec::new();
        }
    };
    match serde_json::from_slice::<PollFile>(&bytes) {
        Ok(file) => file.polls,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not read the saved polls");
            Vec::new()
        }
    }
}

/// Start the writer. When changes come faster than the disk takes them,
/// only the latest is written. The task ends once the saver is dropped and
/// its last change is on disk.
pub fn start(dir: PathBuf) -> (Saver, JoinHandle<()>) {
    let (send, changes) = watch::channel(Vec::new());
    (Saver(send), tokio::spawn(writer(dir, changes)))
}

/// A saver whose changes the test reads itself.
#[cfg(test)]
pub fn watched() -> (Saver, watch::Receiver<Vec<u8>>) {
    let (send, changes) = watch::channel(Vec::new());
    (Saver(send), changes)
}

#[cfg(test)]
pub fn decode(bytes: &[u8]) -> Vec<SavedPoll> {
    serde_json::from_slice::<PollFile>(bytes).unwrap().polls
}

async fn writer(dir: PathBuf, mut changes: watch::Receiver<Vec<u8>>) {
    while changes.changed().await.is_ok() {
        let bytes = changes.borrow_and_update().clone();
        let at = dir.clone();
        let written = tokio::task::spawn_blocking(move || write(&at, &bytes)).await;
        let error = match written {
            Ok(Ok(())) => continue,
            Ok(Err(error)) => error.to_string(),
            Err(error) => error.to_string(),
        };
        tracing::warn!(dir = %dir.display(), %error, "could not save the poll records");
    }
}

/// Replace the file in one step: write a temporary file beside it, flush
/// it to disk, then rename it over the old one.
fn write(dir: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(TEMP);
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, dir.join(FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::Target;
    use crate::state::State;
    use crate::testutil::*;
    use serde_json::json;

    /// Two polls, the second in a topic and with a vote on it.
    fn polls() -> Vec<SavedPoll> {
        let mut state = State::new("Q".to_owned(), &bot_user());
        state.record_poll("P1".to_owned(), Target::chat(GROUP), 300, 2);
        let topic = Target {
            chat: GROUP,
            topic: Some(12),
        };
        state.record_poll("P2".to_owned(), topic, 301, 3);
        let vote = json!({ "poll_answer": { "poll_id": "P2", "user": person(SAM),
            "option_ids": [2], "option_persistent_ids": [] } });
        state.handle(&update(vote), at());
        state.saved_polls()
    }

    #[tokio::test]
    async fn saved_polls_come_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("telegram");
        let (saver, writer) = start(at.clone());
        saver.save(polls()[..1].to_vec());
        saver.save(polls());
        drop(saver);
        writer.await.unwrap();
        assert_eq!(load(&at).await, polls());
        assert!(!at.join(TEMP).exists());
    }

    #[tokio::test]
    async fn no_file_or_a_broken_one_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).await.is_empty());
        std::fs::write(dir.path().join(FILE), b"{ \"polls\": [ {").unwrap();
        assert!(load(dir.path()).await.is_empty());
    }
}
