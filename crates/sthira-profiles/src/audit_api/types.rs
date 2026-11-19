//! The audit API's JSON, field for field. Nothing here is interpreted; that is [`super::normalize`]'s job.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One row of the application's audit log, exactly as the API serves it.
///
/// Ids the API may send as numbers or strings (`user_id`, `org_id`, `record_id`) are kept as raw JSON
/// values so the row round-trips unchanged. Fields the API adds later land in `extra`; they are kept
/// with the raw row but are not part of the hashed event until the profile maps them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditLogEntry {
    /// The audit row id: a dense, increasing sequence. Used as the log position.
    pub id: u64,
    #[serde(default)]
    pub user_id: Value,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub org_id: Value,
    #[serde(default)]
    pub http_method: Option<String>,
    /// `INSERT`, `UPDATE` or `DELETE`.
    pub operation: String,
    pub table_name: String,
    #[serde(default)]
    pub record_id: Value,
    #[serde(default)]
    pub old_data: Option<Value>,
    #[serde(default)]
    pub new_data: Option<Value>,
    #[serde(default)]
    pub domain: Option<String>,
    /// When the application wrote the audit row.
    pub created_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One page of the paginated list (Django REST framework style).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditPage {
    pub count: u64,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub previous: Option<String>,
    pub results: Vec<AuditLogEntry>,
}

/// The envelope the endpoint wraps every page in. `session_data` and `appstate` are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEnvelope {
    pub response: AuditPage,
}

/// A JSON id (number or string) as a string; `None` for null or anything else.
pub fn id_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
