//! The raw shape of a row-based MySQL binary log, one committed transaction at a time.
//!
//! In the log a transaction is a `GTID` event, a `BEGIN` query event, one `TABLE_MAP` plus one
//! `WRITE_ROWS` / `UPDATE_ROWS` / `DELETE_ROWS` event per table touched, and an `XID` commit. A reader
//! collapses that into [`BinlogTx`]: the GTID, the commit timestamp, and every changed row with its
//! table, its rows event position and its full images (column names resolved from the table map
//! and `information_schema`). Nothing here is interpreted; that is [`super::normalize`]'s job.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One committed transaction as read from the binary log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BinlogTx {
    /// The source server's `server_uuid`; the GTID is `server_uuid:tx`.
    pub server_uuid: String,
    /// GTID transaction number: dense and increasing per server.
    pub tx: u64,
    /// Binary log file holding the transaction, e.g. `binlog.000042`.
    pub file: String,
    /// Commit timestamp, Unix seconds (the binlog event header's resolution).
    pub timestamp: i64,
    pub rows: Vec<RowChange>,
}

/// Which rows event carried the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RowsEvent {
    WriteRows,
    UpdateRows,
    DeleteRows,
}

/// One changed row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowChange {
    pub db: String,
    pub table: String,
    pub event: RowsEvent,
    /// `end_log_pos` of the rows event in the transaction's file.
    pub end_log_pos: u64,
    /// Primary-key columns of the table, from `information_schema`.
    pub pk: Vec<String>,
    /// Full image before the change (`UPDATE_ROWS`, `DELETE_ROWS`).
    pub before: Option<Map<String, Value>>,
    /// Full image after the change (`WRITE_ROWS`, `UPDATE_ROWS`).
    pub after: Option<Map<String, Value>>,
}

impl BinlogTx {
    pub fn gtid(&self) -> String {
        format!("{}:{}", self.server_uuid, self.tx)
    }
}
