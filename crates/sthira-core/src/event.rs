//! Spec v3: the normalised change event, the exact structure that is canonicalised and hashed into a
//! leaf. It is shaped after a database's own change log, read at the row level: MySQL's binary log
//! with `binlog_format=ROW`, `binlog_row_image=FULL` and GTIDs is the first source. Oracle redo/LogMiner
//! or SAP change documents map onto the same fields (`server`/`gtid`/`tx`/`file`/`pos` become the
//! source's own transaction id and log position).
//!
//! Every field is always present: an absent value is an explicit JSON `null`, never a missing key,
//! so one change has exactly one canonical form. Who made the change is not part of the event: a
//! database log does not record the application user. When a client needs it, the application writes
//! it as an ordinary row inside the same transaction, and that row is anchored like any other.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Spec version written into every event.
pub const SPEC_VERSION: u32 = 3;

/// The kind of row change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Insert,
    Update,
    Delete,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Insert => "insert",
            Op::Update => "update",
            Op::Delete => "delete",
        }
    }
}

/// Where in the source's change log the row change sits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The connector that read the change, e.g. `mysql-binlog`.
    pub connector: String,
    /// The source server's identity (MySQL `server_uuid`).
    pub server: String,
    /// Database (schema) name.
    pub db: String,
    pub table: String,
    /// Global transaction id, `server:tx`.
    pub gtid: String,
    /// The GTID transaction number, as a decimal string. Dense per server: a missing number is a
    /// transaction the node never received.
    pub tx: String,
    /// Binary log file holding the rows event.
    pub file: String,
    /// `end_log_pos` of the rows event in `file`, as a decimal string.
    pub pos: String,
    /// Index of this row within its transaction, 0-based.
    pub row: u32,
}

/// The changed row's primary key: column name to value, every value as a string. Composite keys
/// have several entries; the map is ordered, so the key has one canonical form.
pub type RecordKey = BTreeMap<String, String>;

/// A normalised change event (spec v3).
///
/// - `before` is `null` for inserts, `after` is `null` for deletes; updates carry both full images.
/// - Decimal amounts are strings, never JSON floats.
/// - Timestamps are RFC 3339 UTC with microseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u32,
    pub chain: String,
    pub source: Source,
    pub key: RecordKey,
    pub op: Op,
    pub before: Option<Value>,
    pub after: Option<Value>,
    /// When the source committed the transaction (the binlog event timestamp).
    pub committed_at: String,
    /// When the Sthiranet node received it.
    pub captured_at: String,
}

impl Event {
    /// The row image that describes the record: `after`, or `before` for a delete.
    pub fn row(&self) -> Option<&Value> {
        self.after.as_ref().or(self.before.as_ref())
    }

    /// The GTID transaction number.
    pub fn tx_num(&self) -> Option<u64> {
        self.source.tx.parse().ok()
    }

    /// The key as `col=value[,col=value]`.
    pub fn key_label(&self) -> String {
        self.key.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
    }
}
