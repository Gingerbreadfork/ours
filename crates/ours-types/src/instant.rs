//! A point in time. Wraps `jiff::Timestamp` so it can carry a JSON Schema
//! and so no crate but this one spells the time library's name.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Instant(pub jiff::Timestamp);

impl Instant {
    pub const UNIX_EPOCH: Instant = Instant(jiff::Timestamp::UNIX_EPOCH);

    pub fn from_millis(ms: i64) -> Option<Self> {
        jiff::Timestamp::from_millisecond(ms).ok().map(Instant)
    }

    pub fn as_millis(self) -> i64 {
        self.0.as_millisecond()
    }

    /// Stops at the ends of the supported range instead of overflowing, so
    /// a far-off time read from a message cannot panic the fold.
    pub fn plus_seconds(self, seconds: i64) -> Instant {
        let edge = if seconds < 0 {
            jiff::Timestamp::MIN
        } else {
            jiff::Timestamp::MAX
        };
        Instant(
            self.0
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .unwrap_or(edge),
        )
    }

    pub fn seconds_since(self, earlier: Instant) -> i64 {
        self.0.duration_since(earlier.0).as_secs()
    }
}

impl fmt::Debug for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<jiff::Timestamp> for Instant {
    fn from(t: jiff::Timestamp) -> Self {
        Instant(t)
    }
}

impl JsonSchema for Instant {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Instant")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "date-time",
            "description": "An RFC 3339 timestamp in UTC"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plus_seconds_stops_at_the_ends_of_the_range() {
        let late = Instant(jiff::Timestamp::MAX).plus_seconds(-10);
        assert_eq!(late.plus_seconds(86_400), Instant(jiff::Timestamp::MAX));
        let early = Instant(jiff::Timestamp::MIN).plus_seconds(10);
        assert_eq!(early.plus_seconds(-86_400), Instant(jiff::Timestamp::MIN));
        assert_eq!(
            Instant::UNIX_EPOCH.plus_seconds(i64::MAX),
            Instant(jiff::Timestamp::MAX)
        );
    }

    #[test]
    fn plus_seconds_inside_the_range_is_exact() {
        let at = Instant::UNIX_EPOCH.plus_seconds(90);
        assert_eq!(at.seconds_since(Instant::UNIX_EPOCH), 90);
    }
}
