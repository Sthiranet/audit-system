//! What an audit-API change means, in words a partner or auditor reads without opening JSON.
//!
//! Descriptions are display only: they are computed from the event and never hashed.

use serde_json::Value;
use sthira_core::{EventV2, Op};

use crate::Description;

/// Tables whose rows are the application's own request log. They are anchored like everything else,
/// so the log position stays complete, but views hide them by default. Polling the API writes rows
/// here too, so this table grows with every poll.
pub const NOISE_TABLES: &[&str] = &["api_request_log"];

/// Fields on a user or access row whose change alters what someone may do.
const PRIVILEGE_FIELDS: &[&str] = &["role", "is_staff", "is_superuser", "is_active", "can_edit", "organization"];

pub fn is_noise(ev: &EventV2) -> bool {
    NOISE_TABLES.contains(&ev.source.table.as_str())
}

fn noun(table: &str) -> String {
    let known = match table {
        "api_request_log" => "API request",
        "app_user" => "User account",
        "app_useractivity" => "User activity",
        "app_organizationuser" => "Organisation membership",
        "app_usermicroappaccess" => "App access",
        "app_organization" => "Organisation",
        "dreamreviews_survey" => "Survey",
        "dreamreviews_surveyquestions" => "Survey question",
        "dreamaudio_speaker" => "Recording speaker",
        "dreamaudio_transcriptsegment" => "Transcript segment",
        "dreamaudio_recordinganalysis" => "Recording analysis",
        "archive_archiveentry" => "Archive entry",
        "chat_message" => "Chat message",
        "chat_messagereceipt" => "Message receipt",
        "chat_membership" => "Chat membership",
        "dreamaudio_recording" => "Meeting recording",
        "dreamaudio_bot_session" => "Meeting bot session",
        "dreamaudio_recordingpresence" => "Meeting presence",
        "dreamreviews_media" => "Review media",
        "dreamreviews_surveysubmissions" => "Survey submission",
        "dreamreviews_surveyquestionsubmissions" => "Survey answer",
        "dreamreviews_surveyusers" => "Survey participant",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    // `app_some_table` → `Some table`.
    let base = table.split_once('_').map_or(table, |(_, r)| r).replace('_', " ");
    let mut c = base.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_else(|| table.to_string())
}

fn verb(op: Op) -> &'static str {
    match op {
        Op::Insert => "created",
        Op::Update => "updated",
        Op::Delete => "deleted",
    }
}

fn s(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(x)) => x.clone(),
        Some(x) => x.to_string(),
    }
}

/// Fields that changed between `before` and `after` (empty when either image is missing).
pub fn changed_fields(ev: &EventV2) -> Vec<String> {
    let (Some(Value::Object(b)), Some(Value::Object(a))) = (&ev.before, &ev.after) else {
        return Vec::new();
    };
    let mut keys: Vec<String> =
        a.keys().chain(b.keys()).filter(|k| a.get(*k) != b.get(*k)).cloned().collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys
}

/// Review flags: `privilege-change`, `no-before-image`, `system-actor`.
pub fn audit_flags(ev: &EventV2) -> Vec<String> {
    let mut f = Vec::new();
    let t = ev.source.table.as_str();
    let privileged_table = matches!(t, "app_organizationuser" | "app_usermicroappaccess");
    let user_privilege = t == "app_user"
        && (changed_fields(ev).iter().any(|k| PRIVILEGE_FIELDS.contains(&k.as_str()))
            || (ev.op == Op::Insert
                && ev.after.as_ref().is_some_and(|a| a.get("is_superuser") == Some(&Value::Bool(true)))));
    if privileged_table || user_privilege {
        f.push("privilege-change".to_string());
    }
    if ev.op == Op::Update && ev.before.is_none() {
        f.push("no-before-image".to_string());
    }
    if ev.actor.kind == "system" && !is_noise(ev) {
        f.push("system-actor".to_string());
    }
    f
}

/// The business meaning of a change.
pub fn describe_audit(ev: &EventV2) -> Description {
    let t = ev.source.table.as_str();
    let row = ev.row();
    let g = |k: &str| s(row.and_then(|r| r.get(k)));
    let key = &ev.key.id;
    let kind = format!("{t}-{}", verb(ev.op));
    let label = if is_noise(ev) { "API request logged".to_string() } else { format!("{} {}", noun(t), verb(ev.op)) };
    let text = match t {
        "api_request_log" => format!("{} {} → {}", g("http_method"), g("endpoint"), g("status_code")),
        "app_user" => {
            let changed = changed_fields(ev);
            if !changed.is_empty() {
                let b = |k: &str| s(ev.before.as_ref().and_then(|r| r.get(k)));
                let parts: Vec<String> = changed
                    .iter()
                    .filter(|k| PRIVILEGE_FIELDS.contains(&k.as_str()))
                    .map(|k| format!("{k} {} → {}", b(k), g(k)))
                    .collect();
                if parts.is_empty() {
                    format!("#{key} · changed {}", changed.join(", "))
                } else {
                    format!("#{key} · {}", parts.join(", "))
                }
            } else {
                format!("#{key} · {} · role {}", g("username"), g("role"))
            }
        }
        "app_useractivity" => format!("user {} · {}", g("user"), g("activity_type")),
        "app_organizationuser" => format!("user {} in org {} as {}", g("user"), g("organization"), g("role")),
        "app_usermicroappaccess" => {
            format!("org user {} · app {} · can_edit {}", g("organization_user"), g("micro_app"), g("can_edit"))
        }
        "archive_archiveentry" => format!("{} (deleted at {})", g("object_repr"), g("deleted_at")),
        _ => {
            let status = ["status", "submission_status", "activity_type", "media_type"]
                .iter()
                .map(|k| g(k))
                .find(|v| !v.is_empty());
            match status {
                Some(st) => format!("#{key} · {st}"),
                None => format!("#{key}"),
            }
        }
    };
    Description { kind, label, text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_api::{AuditLogEntry, normalize_audit};
    use serde_json::json;

    fn ev(v: Value) -> EventV2 {
        let e: AuditLogEntry = serde_json::from_value(v).unwrap();
        normalize_audit(&e, "DT", "api.example", "2026-11-18T00:00:00.000000Z")
    }

    #[test]
    fn privilege_change_is_flagged_and_described() {
        let e = ev(json!({
            "id": 1, "user_id": 9, "user_name": "Ops", "operation": "UPDATE", "table_name": "app_user",
            "record_id": "724", "old_data": {"id": 724, "role": "User", "last_login": null},
            "new_data": {"id": 724, "role": "ORG_ADMIN", "last_login": null}, "created_at": "2026-11-18T00:00:00Z"
        }));
        assert!(audit_flags(&e).contains(&"privilege-change".to_string()));
        let d = describe_audit(&e);
        assert_eq!(d.label, "User account updated");
        assert!(d.text.contains("role User → ORG_ADMIN"), "{}", d.text);
    }

    #[test]
    fn missing_before_image_and_system_actor() {
        let e = ev(json!({
            "id": 2, "operation": "UPDATE", "table_name": "dreamaudio_bot_session", "record_id": "44",
            "new_data": {"id": 44, "status": "RECORDING"}, "created_at": "2026-11-18T00:00:00Z"
        }));
        let f = audit_flags(&e);
        assert!(f.contains(&"no-before-image".to_string()) && f.contains(&"system-actor".to_string()));
        assert_eq!(describe_audit(&e).text, "#44 · RECORDING");
    }

    #[test]
    fn request_log_is_noise() {
        let e = ev(json!({
            "id": 3, "http_method": "GET", "operation": "INSERT", "table_name": "api_request_log", "record_id": "5",
            "new_data": {"id": 5, "http_method": "GET", "endpoint": "/audit/", "status_code": 200},
            "created_at": "2026-11-18T00:00:00Z"
        }));
        assert!(is_noise(&e));
        assert!(audit_flags(&e).is_empty());
        assert_eq!(describe_audit(&e).text, "GET /audit/ → 200");
    }

    #[test]
    fn unknown_tables_get_a_readable_noun() {
        assert_eq!(noun("billing_invoice_line"), "Invoice line");
    }
}
