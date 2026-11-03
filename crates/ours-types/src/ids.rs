//! String newtypes for every identifier. Platform ids are whatever the
//! platform gave us; internal ids are ULIDs so they sort by creation time.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

macro_rules! ulid_id {
    ($(#[$doc:meta])* $name:ident) => {
        string_id!($(#[$doc])* $name);

        impl $name {
            /// A fresh, time-sortable id.
            pub fn generate() -> Self {
                Self(ulid::Ulid::generate().to_string())
            }
        }
    };
}

ulid_id!(
    /// A group: one memory, one charter, one SQLite file.
    GroupId
);
ulid_id!(
    /// An outbound action the engine asked a connector to perform.
    ActionId
);
ulid_id!(
    /// A request the engine made to a model.
    RequestId
);
ulid_id!(
    /// A durable timer on the virtual clock.
    TimerId
);
string_id!(
    /// A user as the platform identifies them. Never a display name.
    PlatformUserId
);
string_id!(
    /// A room (channel, DM, thread) as the platform identifies it.
    PlatformRoomId
);
string_id!(
    /// A message as the platform identifies it; the unit of provenance.
    PlatformMessageId
);
