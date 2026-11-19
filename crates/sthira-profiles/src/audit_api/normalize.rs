//! Audit API row → spec v2 event.
//!
//! | API field                         | Event v2                       |
//! |-----------------------------------|--------------------------------|
//! | (fixed)                           | `source.connector`             |
//! | API host                          | `source.system`                |
//! | `table_name`                      | `source.table`                 |
//! | `id`                              | `source.pos`                   |
//! | `http_method`                     | `source.channel`               |
//! | `domain`                          | `source.origin`                |
//! | `record_id`                       | `key.id`                       |
//! | `operation`                       | `op`                           |
//! | `old_data` / `new_data`           | `before` / `after`             |
//! | `user_id`, `user_name`, `org_id`  | `actor.id`, `actor.name`, `actor.org` (`kind` = user/system) |
//! | `created_at`                      | `committed_at`                 |
//! | node receive time                 | `captured_at`                  |

use chrono::{DateTime, SecondsFormat, Utc};
use sthira_core::{ActorV2, EventV2, Op, RecordKey, SPEC_VERSION_V2, SourceV2};

use super::types::{AuditLogEntry, id_string};

/// `source.connector` of every event read from the audit API.
pub const CONNECTOR: &str = "dreamtalk-audit-api";

/// The host part of an API URL (`https://host/path?q` → `host`).
pub fn system_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest).to_string()
}

/// RFC 3339 with any offset → UTC with microseconds (spec format). An unparsable value is kept as is,
/// so it is still anchored exactly as received.
pub fn to_spec_time(s: &str) -> String {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Micros, true))
        .unwrap_or_else(|_| s.to_string())
}

fn op_of(operation: &str) -> Op {
    match operation.to_ascii_uppercase().as_str() {
        "INSERT" | "CREATE" => Op::Insert,
        "DELETE" => Op::Delete,
        _ => Op::Update,
    }
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref().filter(|s| !s.is_empty()).cloned()
}

/// Turns one audit row into the spec v2 event that gets hashed.
pub fn normalize_audit(e: &AuditLogEntry, chain: &str, system: &str, captured_at: &str) -> EventV2 {
    let row = e.new_data.as_ref().or(e.old_data.as_ref());
    let key = id_string(&e.record_id).or_else(|| row.and_then(|r| r.get("id")).and_then(id_string)).unwrap_or_default();
    let actor_id = id_string(&e.user_id);
    let actor_name = nonempty(&e.user_name);
    EventV2 {
        v: SPEC_VERSION_V2,
        chain: chain.to_string(),
        source: SourceV2 {
            connector: CONNECTOR.to_string(),
            system: system.to_string(),
            table: e.table_name.clone(),
            pos: e.id.to_string(),
            channel: nonempty(&e.http_method),
            origin: nonempty(&e.domain),
        },
        key: RecordKey { id: key },
        op: op_of(&e.operation),
        before: e.old_data.clone(),
        after: e.new_data.clone(),
        actor: ActorV2 {
            kind: if actor_id.is_some() || actor_name.is_some() { "user" } else { "system" }.to_string(),
            id: actor_id,
            name: actor_name,
            org: id_string(&e.org_id),
        },
        committed_at: to_spec_time(&e.created_at),
        captured_at: captured_at.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn entry(v: Value) -> AuditLogEntry {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn maps_every_field() {
        let e = entry(json!({
            "id": 190850, "user_id": 557, "user_name": "Test User", "org_id": 3, "http_method": "POST",
            "operation": "UPDATE", "table_name": "app_user", "record_id": "724",
            "old_data": {"id": 724, "role": "User"}, "new_data": {"id": 724, "role": "ORG_ADMIN"},
            "domain": "tenant.example", "created_at": "2026-11-18T05:39:01.767051Z"
        }));
        let ev = normalize_audit(&e, "DT", "api.example", "2026-11-18T05:39:05.000000Z");
        assert_eq!(ev.v, 2);
        assert_eq!(ev.source.pos, "190850");
        assert_eq!(ev.source.table, "app_user");
        assert_eq!(ev.source.channel.as_deref(), Some("POST"));
        assert_eq!(ev.source.origin.as_deref(), Some("tenant.example"));
        assert_eq!(ev.key.id, "724");
        assert_eq!(ev.op, Op::Update);
        assert_eq!(ev.actor.id.as_deref(), Some("557"));
        assert_eq!(ev.actor.org.as_deref(), Some("3"));
        assert_eq!(ev.actor.kind, "user");
        assert_eq!(ev.committed_at, "2026-11-18T05:39:01.767051Z");
        assert!(ev.before.is_some() && ev.after.is_some());
    }

    #[test]
    fn system_rows_and_offsets() {
        let e = entry(json!({
            "id": 7, "user_id": null, "user_name": null, "org_id": null, "http_method": null,
            "operation": "DELETE", "table_name": "chat_message", "record_id": null,
            "old_data": {"id": "83ced43d-1460-4ab8-81b7-b134f3a0d251"}, "new_data": null,
            "domain": null, "created_at": "2026-11-18T11:09:01.5+05:30"
        }));
        let ev = normalize_audit(&e, "DT", "api.example", "x");
        assert_eq!(ev.actor.kind, "system");
        assert_eq!((ev.actor.id.clone(), ev.actor.name.clone()), (None, None));
        assert_eq!(ev.op, Op::Delete);
        assert_eq!(ev.key.id, "83ced43d-1460-4ab8-81b7-b134f3a0d251");
        assert_eq!(ev.committed_at, "2026-11-18T05:39:01.500000Z");
        assert_eq!(ev.source.channel, None);
    }

    #[test]
    fn host_from_url() {
        assert_eq!(system_of("https://api.example.com/audit/?page=1"), "api.example.com");
        assert_eq!(system_of("api.example.com"), "api.example.com");
    }
}
