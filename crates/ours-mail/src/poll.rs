//! The loop that polls an inbox and forwards what it finds.

use crate::recv::{Fetched, Inbox, Receipt};
use ours_types::InboundMail;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// After repeated failures the wait between polls grows to this.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// One mail from the inbox, and the handle that marks it seen.
#[derive(Debug)]
pub struct Delivery {
    pub mail: InboundMail,
    pub ack: Ack,
}

/// Marks one delivered mail seen. A mail that is never acknowledged stays
/// unseen, and is delivered again after a restart but not before.
#[derive(Debug)]
pub struct Ack {
    receipt: Receipt,
    done: mpsc::UnboundedSender<Receipt>,
}

impl Ack {
    /// Call once the mail is safely kept, or deliberately dropped.
    pub fn ack(self) {
        // The poller is gone only when stopping; the mail then comes again.
        let _ = self.done.send(self.receipt);
    }
}

/// Mail handed on and not yet marked seen.
#[derive(Default)]
struct Held {
    out: HashSet<Receipt>,
    /// The acknowledged ones among `out`.
    acked: Vec<Receipt>,
}

impl Held {
    async fn mark_seen(&mut self, inbox: &dyn Inbox) {
        if self.acked.is_empty() {
            return;
        }
        match inbox.mark_seen(&self.acked).await {
            Ok(()) => {
                for receipt in self.acked.drain(..) {
                    self.out.remove(&receipt);
                }
            }
            Err(error) => warn!(%error, count = self.acked.len(), "cannot mark mail seen yet"),
        }
    }
}

/// Polls `inbox` every `every` and sends each new mail to `sink`. A mail is
/// marked seen only after its `Ack`, so a stop before then delivers it
/// again; receivers should ignore a Message-ID they already have. A failed
/// poll is retried after a wait that doubles up to ten minutes, or `every`
/// if that is longer. Returns when the sink is closed.
pub async fn poll_loop(inbox: Arc<dyn Inbox>, every: Duration, sink: mpsc::Sender<Delivery>) {
    let (done, mut acks) = mpsc::unbounded_channel();
    let mut held = Held::default();
    let mut wait = every;
    loop {
        while let Ok(receipt) = acks.try_recv() {
            held.acked.push(receipt);
        }
        held.mark_seen(&*inbox).await;
        match inbox.poll(&held.out).await {
            Ok(fetched) => {
                wait = every;
                if !fetched.is_empty() {
                    debug!(count = fetched.len(), "inbound mail polled");
                }
                for Fetched { mail, receipt } in fetched {
                    held.out.insert(receipt.clone());
                    let ack = Ack {
                        receipt,
                        done: done.clone(),
                    };
                    if sink.send(Delivery { mail, ack }).await.is_err() {
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
        // Until the next poll, mark mail seen as soon as all of it is acknowledged.
        let next = tokio::time::sleep(wait);
        tokio::pin!(next);
        loop {
            tokio::select! {
                () = &mut next => break,
                Some(receipt) = acks.recv() => {
                    held.acked.push(receipt);
                    if held.acked.len() >= held.out.len() {
                        held.mark_seen(&*inbox).await;
                    }
                }
            }
        }
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

    fn fetched(uid: u32, subject: &str) -> Fetched {
        Fetched {
            mail: mail(subject),
            receipt: Receipt::imap(Some(1), uid),
        }
    }

    async fn next(rx: &mut mpsc::Receiver<Delivery>) -> Delivery {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }

    /// Answers each poll from a script, then with nothing.
    struct Scripted(Mutex<VecDeque<Result<Vec<Fetched>, MailError>>>);

    #[async_trait]
    impl Inbox for Scripted {
        async fn poll(&self, _: &HashSet<Receipt>) -> Result<Vec<Fetched>, MailError> {
            self.0.lock().unwrap().pop_front().unwrap_or(Ok(Vec::new()))
        }

        async fn mark_seen(&self, _: &[Receipt]) -> Result<(), MailError> {
            Ok(())
        }
    }

    /// Keeps each message until it is marked seen, as a mailbox does.
    #[derive(Default)]
    struct Mailbox {
        unseen: Mutex<Vec<Fetched>>,
        marked: Mutex<Vec<Receipt>>,
    }

    impl Mailbox {
        fn holding(mails: Vec<Fetched>) -> Arc<Self> {
            Arc::new(Self {
                unseen: Mutex::new(mails),
                marked: Mutex::default(),
            })
        }

        fn marked(&self) -> Vec<Receipt> {
            self.marked.lock().unwrap().clone()
        }

        async fn wait_for_marks(&self, count: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.marked().len() < count {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .unwrap();
        }
    }

    #[async_trait]
    impl Inbox for Mailbox {
        async fn poll(&self, skip: &HashSet<Receipt>) -> Result<Vec<Fetched>, MailError> {
            let unseen = self.unseen.lock().unwrap();
            Ok(unseen
                .iter()
                .filter(|m| !skip.contains(&m.receipt))
                .cloned()
                .collect())
        }

        async fn mark_seen(&self, receipts: &[Receipt]) -> Result<(), MailError> {
            self.unseen
                .lock()
                .unwrap()
                .retain(|m| !receipts.contains(&m.receipt));
            self.marked.lock().unwrap().extend_from_slice(receipts);
            Ok(())
        }
    }

    #[tokio::test]
    async fn forwards_mail_and_keeps_going_after_an_error() {
        let inbox = Arc::new(Scripted(Mutex::new(VecDeque::from([
            Ok(vec![fetched(1, "one"), fetched(2, "two")]),
            Err(MailError::Imap("connection reset".to_owned())),
            Ok(vec![fetched(3, "three")]),
        ]))));
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(poll_loop(inbox, Duration::from_millis(5), tx));

        let mut subjects = Vec::new();
        for _ in 0..3 {
            let delivery = next(&mut rx).await;
            subjects.push(delivery.mail.subject);
            delivery.ack.ack();
        }
        assert_eq!(subjects, ["one", "two", "three"]);

        drop(rx);
        task.abort();
    }

    #[tokio::test]
    async fn mail_is_marked_seen_only_after_its_ack() {
        let inbox = Mailbox::holding(vec![fetched(1, "one"), fetched(2, "two")]);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(poll_loop(
            Arc::clone(&inbox) as Arc<dyn Inbox>,
            Duration::from_millis(5),
            tx,
        ));
        let one = next(&mut rx).await;
        let two = next(&mut rx).await;
        assert_eq!(
            (one.mail.subject.as_str(), two.mail.subject.as_str()),
            ("one", "two")
        );

        // Several polls go by without marking or delivering either again.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(inbox.marked().is_empty());
        assert!(rx.try_recv().is_err());

        one.ack.ack();
        inbox.wait_for_marks(1).await;
        assert_eq!(inbox.marked(), [Receipt::imap(Some(1), 1)]);

        // Stopping before the second ack leaves it to be delivered again.
        task.abort();
        drop(two);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(poll_loop(
            Arc::clone(&inbox) as Arc<dyn Inbox>,
            Duration::from_millis(5),
            tx,
        ));
        assert_eq!(next(&mut rx).await.mail.subject, "two");
        task.abort();
    }

    #[tokio::test]
    async fn acknowledged_mail_is_marked_before_the_next_poll() {
        let inbox = Mailbox::holding(vec![fetched(1, "one"), fetched(2, "two")]);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(poll_loop(
            Arc::clone(&inbox) as Arc<dyn Inbox>,
            Duration::from_secs(3600),
            tx,
        ));
        next(&mut rx).await.ack.ack();
        next(&mut rx).await.ack.ack();
        inbox.wait_for_marks(2).await;
        task.abort();
    }

    #[tokio::test]
    async fn stops_when_the_sink_is_closed() {
        let inbox = Arc::new(Scripted(Mutex::new(VecDeque::from([Ok(vec![fetched(
            1, "one",
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
