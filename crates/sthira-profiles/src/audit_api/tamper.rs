//! Presenter actions on a STORED v2 event: what an insider with write access to the node's audit
//! store might do after the batch was anchored. Detecting it is the verifier's job, not this module's.
//!
//! Like the finance profile, repeated edits walk away from the original and never restore it.

use serde_json::{Map, Value};
use sthira_core::EventV2;

/// Fields an insider would most plausibly rewrite, tried in this order.
const PREFERRED: &[&str] = &[
    "role",
    "is_superuser",
    "status",
    "submission_status",
    "activity_type",
    "text",
    "title",
    "full_name",
    "endpoint",
    "status_code",
];

const IMPOSTOR: &str = "sthira-demo-impostor";

fn edit_value(v: &Value) -> Option<Value> {
    match v {
        Value::String(s) => Some(Value::String(format!("{s} (edited)"))),
        Value::Bool(b) => Some(Value::Bool(!b)),
        Value::Number(n) => n.as_i64().map(|i| Value::from(i + 1)).or_else(|| n.as_f64().map(|f| Value::from(f + 1.0))),
        _ => None,
    }
}

fn pick(row: &Map<String, Value>) -> Option<String> {
    for k in PREFERRED {
        if let Some(v) = row.get(*k) {
            // A boolean flips back on a second edit, so it is only used once.
            if matches!(v, Value::String(_) | Value::Number(_)) || (v.is_boolean() && v == &Value::Bool(false)) {
                return Some((*k).to_string());
            }
        }
    }
    let mut keys: Vec<&String> = row.keys().filter(|k| k.as_str() != "id").collect();
    keys.sort();
    keys.iter()
        .find(|k| row[k.as_str()].is_string())
        .or_else(|| keys.iter().find(|k| row[k.as_str()].is_number()))
        .map(|k| (*k).clone())
}

/// Alters one field of the stored row image (`after`, or `before` for a delete). Returns what changed.
pub fn tamper_audit(ev: &mut EventV2) -> String {
    let table = ev.source.table.clone();
    let key = ev.key.id.clone();
    let Some(Value::Object(row)) = ev.after.as_mut().or(ev.before.as_mut()) else {
        // No row image at all: give it one, which still changes the hashed content.
        ev.after = Some(serde_json::json!({ "edited": true }));
        return format!("{table} #{key}: row image added");
    };
    let Some(field) = pick(row) else {
        let n = row.get("_edited").and_then(Value::as_i64).unwrap_or(0) + 1;
        row.insert("_edited".into(), Value::from(n));
        return format!("{table} #{key}: field _edited set to {n}");
    };
    let old = row[&field].clone();
    let new = edit_value(&old).unwrap_or(Value::String("edited".into()));
    row.insert(field.clone(), new.clone());
    format!("{table} #{key}: {field} {old} → {new}")
}

/// Rewrites who made the change: the insider pins the change on someone else.
pub fn reattribute_audit(ev: &mut EventV2) -> String {
    let old = ev.actor.name.clone().or(ev.actor.id.clone()).unwrap_or_else(|| "system".into());
    let n = if ev.actor.name.as_deref() == Some(IMPOSTOR) {
        ev.actor.id.as_deref().and_then(|i| i.parse::<u64>().ok()).unwrap_or(0) + 1
    } else {
        1
    };
    ev.actor.id = Some(n.to_string());
    ev.actor.name = Some(IMPOSTOR.into());
    ev.actor.kind = "user".into();
    format!("{} #{}: actor {old} → {IMPOSTOR} (id {n})", ev.source.table, ev.key.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_api::{AuditLogEntry, normalize_audit};
    use serde_json::json;

    fn sample() -> Vec<EventV2> {
        let rows = [
            json!({"id": 1, "operation": "UPDATE", "table_name": "app_user", "record_id": "1",
                   "new_data": {"id": 1, "role": "User", "is_superuser": false}, "created_at": "2026-11-18T00:00:00Z"}),
            json!({"id": 2, "operation": "DELETE", "table_name": "chat_message", "record_id": "u",
                   "old_data": {"id": "u", "text": "hi"}, "created_at": "2026-11-18T00:00:00Z"}),
            json!({"id": 3, "operation": "INSERT", "table_name": "x_counts", "record_id": "3",
                   "new_data": {"id": 3, "n": 4}, "created_at": "2026-11-18T00:00:00Z"}),
            json!({"id": 4, "operation": "INSERT", "table_name": "x_empty", "record_id": "4",
                   "new_data": {"id": 4}, "created_at": "2026-11-18T00:00:00Z"}),
        ];
        rows.into_iter()
            .map(|r| normalize_audit(&serde_json::from_value::<AuditLogEntry>(r).unwrap(), "DT", "h", "t"))
            .collect()
    }

    #[test]
    fn repeated_edits_never_restore_the_original() {
        for original in sample() {
            let mut ev = original.clone();
            for round in 0..6 {
                tamper_audit(&mut ev);
                assert_ne!(ev, original, "round {round}");
            }
            let mut ev = original.clone();
            for _ in 0..4 {
                reattribute_audit(&mut ev);
                assert_ne!(ev, original);
            }
        }
    }
}
