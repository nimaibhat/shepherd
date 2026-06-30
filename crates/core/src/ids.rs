//! Strongly typed identifiers. They are strings at runtime but distinct types at
//! compile time, so a SandboxId cannot be passed where a SessionId is expected.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! string_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            /// Generate a fresh id with the type's prefix.
            pub fn new() -> Self {
                let raw = Uuid::new_v4().simple().to_string();
                Self(format!("{}_{}", $prefix, &raw[..12]))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
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
                Self(s.to_string())
            }
        }
    };
}

string_id!(SessionId, "ses");
string_id!(SandboxId, "sbx");

/// Provider identifier, e.g. "docker" or "e2b".
pub type ProviderId = String;
