//! Connector layer · the harness that captures a database's change log and hands whole committed
//! transactions to the node as spec v3 events, through [`sthira_core::ChangeSource`].
//!
//! The client's database decides the connector (MySQL, Oracle, SAP, …). Until the capture side is
//! settled, this crate holds only what the prototype needs, offline:
//!
//! - [`mysql`]: the raw shape of a row-based MySQL binary log ([`mysql::BinlogTx`]), and its
//!   normalisation into spec v3 events. A live binlog reader would produce the same type.
//! - [`generator`]: deterministic dummy data: an ERP-like workload written as binlog transactions.
//! - [`replay`]: [`replay::ReplaySource`], a `ChangeSource` that replays a generated file at a set rate.
//! - [`describe`]: what a change means, for people. Display only, never hashed.

pub mod describe;
pub mod generator;
pub mod mysql;
pub mod replay;

use chrono::{DateTime, SecondsFormat, Utc};

/// RFC 3339 UTC with microseconds (the spec's timestamp format) from Unix milliseconds.
pub fn iso_micros(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The current time in the spec's format.
pub fn now_iso() -> String {
    iso_micros(Utc::now().timestamp_millis())
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
