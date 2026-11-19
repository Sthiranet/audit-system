//! MySQL: the row-based binary log as the change log.
//!
//! Expected server settings: `binlog_format=ROW`, `binlog_row_image=FULL`, `gtid_mode=ON`. Then
//! every committed transaction carries a dense GTID number and full before/after row images.
//!
//! [`binlog`] is the raw skeleton (what a binlog reader hands over after resolving column names from
//! the table map); [`normalize`] turns it into the spec v3 events that get hashed.

pub mod binlog;
pub mod normalize;

pub use binlog::{BinlogTx, RowChange, RowsEvent};
pub use normalize::{CONNECTOR, normalize_tx};
