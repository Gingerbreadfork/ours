//! The runtime paths the admin page and the member rights depend on: a
//! wipe stops the runner and hands the queues back, the files go, and a
//! "forget me" scrubs the journal while the chain still verifies.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{message, start};
use ours::runtime::admin::AdminHub;
use ours::runtime::group::RunEnd;
use ours::runtime::redact::FORGOTTEN;
use ours_store::{EventLog, SqliteStore, StoreDir};
use ours_types::{GroupId, OutboundAction};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wipe_request_ends_the_run_and_hands_the_queues_back() {
    let store: Arc<dyn EventLog> = Arc::new(SqliteStore::open_in_memory().unwrap());
    let running = start(store).await;
    let (done, mut finished) = oneshot::channel();
    running.wipes.send(done).await.unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), running.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let RunEnd::Wiped {
        inputs,
        wipes,
        mail: _,
        done,
    } = end
    else {
        panic!("expected a wipe, got {end:?}");
    };
    assert!(!inputs.is_closed());
    assert!(!wipes.is_closed());
    // The requester is still waiting; the run loop answers once the files are gone.
    assert!(finished.try_recv().is_err());
    done.send(Ok(())).unwrap();
    assert_eq!(finished.await.unwrap(), Ok(()));
}

#[test]
fn wiping_a_group_removes_its_files_and_the_pages_view() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoreDir::open(dir.path()).unwrap();
    let id = GroupId::new("the-six");
    let log = store.open_group(&id).unwrap();
    log.append(
        "message",
        b"{}",
        ours_types::Instant::from(jiff::Timestamp::now()),
    )
    .unwrap();
    drop(log);
    let charter = dir.path().join("the-six.charter.md");
    std::fs::write(&charter, "# charter").unwrap();
    let hub = AdminHub::new();
    hub.set_charter(&id, "# charter".to_owned());
    ours::run::wipe_group(&store, &hub, dir.path(), &id).unwrap();
    assert!(!charter.exists());
    assert!(!store.exists(&id).unwrap());
    assert!(store.groups().unwrap().is_empty());
    // Wiping again is fine; nothing is there to remove.
    ours::run::wipe_group(&store, &hub, dir.path(), &id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_me_scrubs_the_journal_and_keeps_the_chain() {
    let sqlite = Arc::new(SqliteStore::open_in_memory().unwrap());
    let store: Arc<dyn EventLog> = sqlite.clone();
    let running = start(store).await;
    running
        .handle
        .push(message(
            "m1",
            "general",
            "alex",
            "Q, what was that bar in March?",
        ))
        .await
        .unwrap();
    assert!(
        running
            .wait_for(|| running
                .handle
                .take_sent()
                .iter()
                .any(|o| matches!(o.action, OutboundAction::Unreact { .. })))
            .await,
        "the question was answered"
    );
    running
        .handle
        .push(message("m2", "general", "alex", "Q, forget me"))
        .await
        .unwrap();
    let redacted = running
        .wait_for(|| {
            sqlite
                .read(1, 100)
                .unwrap()
                .iter()
                .any(|r| r.redacted && String::from_utf8_lossy(&r.payload).contains(FORGOTTEN))
        })
        .await;
    assert!(redacted, "alex's words are gone from the journal");
    let records = sqlite.read(1, 100).unwrap();
    for record in &records {
        let text = String::from_utf8_lossy(&record.payload);
        assert!(
            !text.contains("what was that bar"),
            "seq {} still holds the words",
            record.seq
        );
    }
    assert!(sqlite.verify_chain().unwrap() >= records.len() as u64);
    let latest = sqlite
        .load_latest_snapshot()
        .unwrap()
        .expect("a clean snapshot was saved");
    let state: serde_json::Value = serde_json::from_slice(&latest.state).unwrap();
    fn walk(v: &serde_json::Value, path: String, hits: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(o) => {
                for (k, c) in o {
                    walk(c, format!("{path}/{k}"), hits);
                }
            }
            serde_json::Value::Array(a) => {
                for (i, c) in a.iter().enumerate() {
                    walk(c, format!("{path}[{i}]"), hits);
                }
            }
            serde_json::Value::String(s) if s.contains("what was that bar") => hits.push(path),
            _ => {}
        }
    }
    let mut hits = Vec::new();
    walk(&state, String::new(), &mut hits);
    assert!(
        hits.is_empty(),
        "forgotten words still in the state at: {hits:?}"
    );
    running.stop().await;
}
