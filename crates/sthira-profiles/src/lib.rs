//! Connector layer · source profiles that turn a change log into spec v2 events.
//!
//! [`audit_api`] is the profile for a live application audit-log API: it reads the log, normalises
//! each row into the [`sthira_core::EventV2`] that gets hashed, and describes what each change means.
//!
//! Another source (a database change stream, CCTV hashes, pharma batches) would get its own profile;
//! the kernel and node layers don't change.

pub mod audit_api;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

/// RFC 3339 UTC with microseconds (the spec's timestamp format) from Unix milliseconds.
pub fn iso_micros(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The business meaning of a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Description {
    pub kind: String,
    pub label: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_micros_is_rfc3339_utc_with_microseconds() {
        assert_eq!(iso_micros(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(iso_micros(1_762_000_000_123), "2025-11-01T12:26:40.123000Z");
    }
}
