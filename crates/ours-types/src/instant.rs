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

    pub fn plus_seconds(self, seconds: i64) -> Instant {
        Instant(self.0 + jiff::SignedDuration::from_secs(seconds))
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
