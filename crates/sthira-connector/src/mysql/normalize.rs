//! Binlog transaction → spec v3 events.
//!
//! | Binlog                               | Event v3             |
//! |--------------------------------------|----------------------|
//! | (fixed)                              | `source.connector`   |
//! | `server_uuid`                        | `source.server`      |
//! | rows event db / table                | `source.db` / `.table` |
//! | `server_uuid:tx`, `tx`               | `source.gtid`, `source.tx` |
//! | file, rows event `end_log_pos`       | `source.file`, `source.pos` |
//! | row index within the transaction     | `source.row`         |
//! | primary-key columns of the row image | `key`                |
//! | `WRITE/UPDATE/DELETE_ROWS`           | `op`                 |
//! | before / after image                 | `before` / `after`   |
//! | commit timestamp                     | `committed_at`       |
//! | node receive time                    | `captured_at`        |

use serde_json::{Map, Value};
use sthira_core::{CapturedTx, Event, Op, RecordKey, SPEC_VERSION, Source};

use super::binlog::{BinlogTx, RowChange, RowsEvent};
use crate::iso_micros;

/// `source.connector` of every event read from a MySQL binary log.
pub const CONNECTOR: &str = "mysql-binlog";

fn op_of(e: RowsEvent) -> Op {
    match e {
        RowsEvent::WriteRows => Op::Insert,
        RowsEvent::UpdateRows => Op::Update,
        RowsEvent::DeleteRows => Op::Delete,
    }
}

/// A column value as a key string: strings as is, everything else as its JSON text.
fn key_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn key_of(r: &RowChange) -> RecordKey {
    let image: Option<&Map<String, Value>> = r.after.as_ref().or(r.before.as_ref());
    r.pk.iter().map(|c| (c.clone(), image.and_then(|m| m.get(c)).map(key_value).unwrap_or_default())).collect()
}

/// Turns one binlog transaction into the spec v3 events that get hashed, one per changed row.
pub fn normalize_tx(t: &BinlogTx, chain: &str, captured_at: &str) -> CapturedTx {
    let gtid = t.gtid();
    let committed_at = iso_micros(t.timestamp * 1000);
    let events = t
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| Event {
            v: SPEC_VERSION,
            chain: chain.to_string(),
            source: Source {
                connector: CONNECTOR.to_string(),
                server: t.server_uuid.clone(),
                db: r.db.clone(),
                table: r.table.clone(),
                gtid: gtid.clone(),
                tx: t.tx.to_string(),
                file: t.file.clone(),
                pos: r.end_log_pos.to_string(),
                row: i as u32,
            },
            key: key_of(r),
            op: op_of(r.event),
            before: r.before.clone().map(Value::Object),
            after: r.after.clone().map(Value::Object),
            committed_at: committed_at.clone(),
            captured_at: captured_at.to_string(),
        })
        .collect();
    CapturedTx { tx: t.tx, gtid, events }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_every_field() {
        let t: BinlogTx = serde_json::from_value(json!({
            "server_uuid": "u-1", "tx": 42, "file": "binlog.000003", "timestamp": 1_763_632_801,
            "rows": [
                {"db": "erp", "table": "order_lines", "event": "WRITE_ROWS", "end_log_pos": 900,
                 "pk": ["order_id", "line_no"], "before": null,
                 "after": {"order_id": 5001, "line_no": 2, "sku": "SKU-1"}},
                {"db": "erp", "table": "customers", "event": "DELETE_ROWS", "end_log_pos": 1100,
                 "pk": ["id"], "before": {"id": "c-9", "name": "x"}, "after": null}
            ]
        }))
        .unwrap();
        let c = normalize_tx(&t, "DT", "2026-11-20T10:00:05.000000Z");
        assert_eq!((c.tx, c.gtid.as_str(), c.events.len()), (42, "u-1:42", 2));
        let e = &c.events[0];
        assert_eq!(e.v, 3);
        assert_eq!(e.source.connector, CONNECTOR);
        assert_eq!((e.source.tx.as_str(), e.source.pos.as_str(), e.source.row), ("42", "900", 0));
        assert_eq!(e.key_label(), "line_no=2,order_id=5001");
        assert_eq!(e.op, Op::Insert);
        assert_eq!(e.committed_at, "2025-11-20T10:00:01.000000Z");
        let d = &c.events[1];
        assert_eq!((d.op, d.source.row, d.key_label().as_str()), (Op::Delete, 1, "id=c-9"));
        assert!(d.after.is_none() && d.before.is_some());
    }
}
