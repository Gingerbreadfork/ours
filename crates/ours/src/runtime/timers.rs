//! Durable timers for the engine. The engine asks for a timer as an effect
//! and later sees it fire as an input; this is the piece in between. The
//! pending set also lives in the engine's state, so after a restart the
//! binary re-arms every timer from the rebuilt state.

use ours_types::{Instant, TimerId};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};

/// A timer that has fired, delivered to the group's input queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub id: TimerId,
    pub at: Instant,
}

#[derive(Debug, Default)]
struct Pending {
    by_time: BTreeMap<(Instant, TimerId), ()>,
    by_id: HashMap<TimerId, Instant>,
}

/// Arms and cancels timers for one group. Clone it freely; `run` delivers
/// firings in time order, then id order, so the order is stable across runs.
#[derive(Debug, Clone, Default)]
pub struct TimerWheel {
    pending: Arc<Mutex<Pending>>,
    changed: Arc<Notify>,
}

impl TimerWheel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, id: TimerId, at: Instant) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(old) = pending.by_id.remove(&id) {
                pending.by_time.remove(&(old, id.clone()));
            }
            pending.by_time.insert((at, id.clone()), ());
            pending.by_id.insert(id, at);
        }
        self.changed.notify_one();
    }

    pub fn cancel(&self, id: &TimerId) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(at) = pending.by_id.remove(id) {
                pending.by_time.remove(&(at, id.clone()));
            }
        }
        self.changed.notify_one();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .by_id
            .len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Everything due at or before `now`, removed from the set.
    pub fn due(&self, now: Instant) -> Vec<Fired> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut fired = Vec::new();
        while let Some(((at, id), ())) = pending.by_time.first_key_value() {
            if *at > now {
                break;
            }
            let (at, id) = (*at, id.clone());
            pending.by_time.remove(&(at, id.clone()));
            pending.by_id.remove(&id);
            fired.push(Fired { id, at });
        }
        fired
    }

    /// The next instant anything is due.
    pub fn next_due(&self) -> Option<Instant> {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.by_time.first_key_value().map(|((at, _), ())| *at)
    }

    /// Delivers firings until the receiver goes away. Sleeps until the next
    /// deadline or until `set` or `cancel` changes it. Runs on the real clock.
    pub async fn run(self, fired: mpsc::Sender<Fired>) {
        loop {
            let now = Instant::from(jiff::Timestamp::now());
            for item in self.due(now) {
                if fired.send(item).await.is_err() {
                    return;
                }
            }
            let sleep_for = match self.next_due() {
                Some(at) => Duration::from_millis((at.as_millis() - now.as_millis()).max(0) as u64),
                None => Duration::from_secs(3600),
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep_for.min(Duration::from_secs(3600))) => {}
                _ = self.changed.notified() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> Instant {
        Instant::from_millis(ms).expect("in range")
    }

    #[test]
    fn fires_in_time_order_then_id_order() {
        let wheel = TimerWheel::new();
        let (a, b, c) = (TimerId::new("a"), TimerId::new("b"), TimerId::new("c"));
        wheel.set(c.clone(), at(2_000));
        wheel.set(b.clone(), at(1_000));
        wheel.set(a.clone(), at(1_000));
        assert_eq!(wheel.next_due(), Some(at(1_000)));
        let fired: Vec<TimerId> = wheel.due(at(1_500)).into_iter().map(|f| f.id).collect();
        assert_eq!(fired, vec![a, b]);
        assert_eq!(wheel.len(), 1);
        let fired: Vec<TimerId> = wheel.due(at(5_000)).into_iter().map(|f| f.id).collect();
        assert_eq!(fired, vec![c]);
        assert!(wheel.is_empty());
    }

    #[test]
    fn cancel_and_reset_replace_the_deadline() {
        let wheel = TimerWheel::new();
        let id = TimerId::new("guard");
        wheel.set(id.clone(), at(1_000));
        wheel.set(id.clone(), at(9_000));
        assert!(wheel.due(at(2_000)).is_empty());
        wheel.cancel(&id);
        assert!(wheel.is_empty());
        assert!(wheel.due(at(10_000)).is_empty());
    }

    #[tokio::test]
    async fn run_delivers_a_past_due_timer() {
        let wheel = TimerWheel::new();
        let (tx, mut rx) = mpsc::channel(4);
        wheel.set(TimerId::new("old"), at(0));
        tokio::spawn(wheel.run(tx));
        let fired = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("in time");
        assert_eq!(fired.map(|f| f.id), Some(TimerId::new("old")));
    }
}
