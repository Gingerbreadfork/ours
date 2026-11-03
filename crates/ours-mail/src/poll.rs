//! The loop that polls an inbox and forwards what it finds.

use crate::recv::Inbox;
use ours_types::InboundMail;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// After repeated failures the wait between polls grows to this.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// Polls `inbox` every `every` and sends each mail to `sink`. A failed
/// poll is logged and retried after a wait that doubles up to ten minutes
/// (or `every`, if that is longer). Returns when the sink is closed.
pub async fn poll_loop(inbox: Arc<dyn Inbox>, every: Duration, sink: mpsc::Sender<InboundMail>) {
    let mut wait = every;
    loop {
        match inbox.poll().await {
            Ok(mails) => {
                wait = every;
                if !mails.is_empty() {
                    debug!(count = mails.len(), "inbound mail polled");
                }
                for mail in mails {
                    if sink.send(mail).await.is_err() {
                        info!("mail sink closed, stopping the poller");
                        return;
                    }
                }
            }
            Err(error) => {
                wait = next_wait(wait, every);
                warn!(%error, retry_in_secs = wait.as_secs_f32(), "mail poll failed");
            }
        }
        tokio::time::sleep(wait).await;
    }
}

fn next_wait(current: Duration, every: Duration) -> Duration {
    (current * 2).min(MAX_BACKOFF.max(every))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MailError;
    use async_trait::async_trait;
    use ours_types::Instant;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn mail(subject: &str) -> InboundMail {
        InboundMail {
            from: "a@x.example".to_owned(),
            from_name: None,
            subject: subject.to_owned(),
            body: String::new(),
            at: Instant::UNIX_EPOCH,
            message_id: None,
            in_reply_to: Vec::new(),
            authenticated: false,
        }
    }

    /// Answers each poll from a script, then with nothing.
    struct Scripted(Mutex<VecDeque<Result<Vec<InboundMail>, MailError>>>);

    #[async_trait]
    impl Inbox for Scripted {
        async fn poll(&self) -> Result<Vec<InboundMail>, MailError> {
            self.0.lock().unwrap().pop_front().unwrap_or(Ok(Vec::new()))
        }
    }

    #[tokio::test]
    async fn forwards_mail_and_keeps_going_after_an_error() {
        let inbox = Arc::new(Scripted(Mutex::new(VecDeque::from([
            Ok(vec![mail("one"), mail("two")]),
            Err(MailError::Imap("connection reset".to_owned())),
            Ok(vec![mail("three")]),
        ]))));
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(poll_loop(inbox, Duration::from_millis(5), tx));

        let mut subjects = Vec::new();
        for _ in 0..3 {
            let m = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            subjects.push(m.subject);
        }
        assert_eq!(subjects, ["one", "two", "three"]);

        // Closing the sink ends the loop at the next mail it tries to send.
        drop(rx);
        task.abort();
    }

    #[tokio::test]
    async fn stops_when_the_sink_is_closed() {
        let inbox = Arc::new(Scripted(Mutex::new(VecDeque::from([Ok(vec![mail(
            "one",
        )])]))));
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        tokio::time::timeout(
            Duration::from_secs(5),
            poll_loop(inbox, Duration::from_millis(5), tx),
        )
        .await
        .unwrap();
    }

    #[test]
    fn the_wait_doubles_up_to_ten_minutes() {
        let every = Duration::from_secs(60);
        let mut wait = every;
        let mut seen = Vec::new();
        for _ in 0..6 {
            wait = next_wait(wait, every);
            seen.push(wait.as_secs());
        }
        assert_eq!(seen, [120, 240, 480, 600, 600, 600]);
        // A slow cadence is never made faster by a failure.
        let slow = Duration::from_secs(3600);
        assert_eq!(next_wait(slow, slow), slow);
    }
}
