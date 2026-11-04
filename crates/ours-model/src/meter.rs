use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use ours_types::GroupId;
use tracing::warn;

use crate::ModelError;

const SECS_PER_DAY: i64 = 86_400;

/// The default daily cap per group, in dollars.
pub const DEFAULT_DAILY_CAP: f64 = 1.00;

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Debug, Clone, Copy)]
struct DaySpend {
    day: i64,
    dollars: f64,
}

/// Dollars spent per group today, with one cap for every group. A group's
/// total starts again at zero on each UTC day boundary.
pub struct SpendMeter {
    cap: f64,
    clock: Clock,
    spent: Mutex<HashMap<GroupId, DaySpend>>,
}

impl std::fmt::Debug for SpendMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpendMeter")
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

impl Default for SpendMeter {
    fn default() -> Self {
        Self::new(DEFAULT_DAILY_CAP)
    }
}

impl SpendMeter {
    /// A meter with the given daily cap in dollars, on the system clock.
    pub fn new(cap_dollars: f64) -> Self {
        Self::with_clock(cap_dollars, || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        })
    }

    /// A meter reading the time, as Unix seconds, from `clock`. A cap that is
    /// not a finite number is taken as zero, so it blocks every call.
    pub fn with_clock(cap_dollars: f64, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        let cap = if cap_dollars.is_finite() {
            cap_dollars
        } else {
            warn!(
                cap = cap_dollars,
                "daily cap is not a finite number, so no model call is allowed"
            );
            0.0
        };
        Self {
            cap,
            clock: Arc::new(clock),
            spent: Mutex::new(HashMap::new()),
        }
    }

    /// The daily cap in dollars.
    pub fn cap(&self) -> f64 {
        self.cap
    }

    /// Dollars the group has spent on the current UTC day.
    pub fn spent_today(&self, group: &GroupId) -> f64 {
        let today = self.today();
        self.lock()
            .get(group)
            .filter(|entry| entry.day == today)
            .map_or(0.0, |entry| entry.dollars)
    }

    /// Fails with `CapExceeded` once the group has spent its cap.
    pub fn check(&self, group: &GroupId) -> Result<(), ModelError> {
        let spent = self.spent_today(group);
        if spent >= self.cap {
            return Err(ModelError::CapExceeded {
                group: group.clone(),
                spent,
                cap: self.cap,
            });
        }
        Ok(())
    }

    /// Adds `dollars` to the group's total for today. Negative and
    /// non-finite amounts are ignored.
    pub fn record(&self, group: &GroupId, dollars: f64) {
        if !dollars.is_finite() || dollars <= 0.0 {
            return;
        }
        let today = self.today();
        let mut spent = self.lock();
        let entry = spent.entry(group.clone()).or_insert(DaySpend {
            day: today,
            dollars: 0.0,
        });
        if entry.day != today {
            *entry = DaySpend {
                day: today,
                dollars: 0.0,
            };
        }
        entry.dollars += dollars;
    }

    fn today(&self) -> i64 {
        (self.clock)().div_euclid(SECS_PER_DAY)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<GroupId, DaySpend>> {
        self.spent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;

    fn group(name: &str) -> GroupId {
        GroupId::new(name)
    }

    #[test]
    fn default_cap_is_one_dollar() {
        assert_eq!(SpendMeter::default().cap(), 1.00);
    }

    #[test]
    fn untouched_group_has_spent_nothing_and_may_call() {
        let meter = SpendMeter::new(1.0);
        assert_eq!(meter.spent_today(&group("a")), 0.0);
        assert!(meter.check(&group("a")).is_ok());
    }

    #[test]
    fn blocks_exactly_at_the_cap() {
        let meter = SpendMeter::new(1.0);
        meter.record(&group("a"), 0.99);
        assert!(meter.check(&group("a")).is_ok());
        meter.record(&group("a"), 0.01);
        match meter.check(&group("a")) {
            Err(ModelError::CapExceeded { group, spent, cap }) => {
                assert_eq!(group.as_str(), "a");
                assert!((spent - 1.0).abs() < 1e-9);
                assert_eq!(cap, 1.0);
            }
            other => panic!("expected CapExceeded, got {other:?}"),
        }
    }

    #[test]
    fn groups_are_metered_separately() {
        let meter = SpendMeter::new(1.0);
        meter.record(&group("a"), 2.0);
        assert!(meter.check(&group("a")).is_err());
        assert!(meter.check(&group("b")).is_ok());
        assert_eq!(meter.spent_today(&group("b")), 0.0);
    }

    #[test]
    fn ignores_bad_amounts() {
        let meter = SpendMeter::new(1.0);
        meter.record(&group("a"), -5.0);
        meter.record(&group("a"), f64::NAN);
        meter.record(&group("a"), f64::INFINITY);
        assert_eq!(meter.spent_today(&group("a")), 0.0);
    }

    #[test]
    fn a_cap_that_is_not_a_number_blocks_every_call() {
        for cap in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let meter = SpendMeter::new(cap);
            assert_eq!(meter.cap(), 0.0, "{cap}");
            assert!(meter.check(&group("a")).is_err(), "{cap}");
        }
    }

    #[test]
    fn resets_on_the_utc_day_boundary() {
        let now = Arc::new(AtomicI64::new(SECS_PER_DAY * 20_000 + 86_399));
        let clock = Arc::clone(&now);
        let meter = SpendMeter::with_clock(1.0, move || clock.load(Ordering::SeqCst));

        meter.record(&group("a"), 1.5);
        assert!(meter.check(&group("a")).is_err());

        // One second later it is the next UTC day.
        now.fetch_add(1, Ordering::SeqCst);
        assert_eq!(meter.spent_today(&group("a")), 0.0);
        assert!(meter.check(&group("a")).is_ok());

        meter.record(&group("a"), 0.25);
        assert!((meter.spent_today(&group("a")) - 0.25).abs() < 1e-9);
    }
}
