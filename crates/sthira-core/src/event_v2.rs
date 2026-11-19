//! Spec v2: the normalised change event, the exact structure that is canonicalised and hashed into a
//! leaf. It is shaped for sources that publish their own change log over an API (the first one is an
//! application's audit-log endpoint).
//!
//! Every field is always present: an absent value is an explicit JSON `null`, never a missing key,
//! so one change has exactly one canonical form.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kind of change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Insert,
    Update,
    Delete,
}

/// The changed record's primary key in the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RecordKey {
    pub id: String,
}

/// Spec version written into every v2 event.
pub const SPEC_VERSION_V2: u32 = 2;

/// Where the change came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SourceV2 {
    /// The connector that read the change, e.g. `dreamtalk-audit-api`.
    pub connector: String,
    /// The system the change log belongs to (the API host).
    pub system: String,
    pub table: String,
    /// Position in the source's change log: the audit row id, as a decimal string.
    pub pos: String,
    /// How the change was triggered, e.g. the HTTP method of the request; `null` for background jobs.
    pub channel: Option<String>,
    /// The tenant domain the request came through, if the source reports one.
    pub origin: Option<String>,
}

/// Who made the change, as the source reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ActorV2 {
    /// The source's user id, as a string.
    pub id: Option<String>,
    pub name: Option<String>,
    /// The source's organisation id, as a string.
    pub org: Option<String>,
    /// `user` when the source names a user, `system` otherwise.
    pub kind: String,
}

/// A normalised change event (spec v2).
///
/// - `before` is `null` for inserts. On an update it is `null` when the source did not supply the
///   previous row image; the event is anchored exactly as the source reported it.
/// - Timestamps are RFC 3339 UTC with microseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EventV2 {
    pub v: u32,
    pub chain: String,
    pub source: SourceV2,
    pub key: RecordKey,
    pub op: Op,
    #[schema(value_type = Option<Object>)]
    pub before: Option<Value>,
    #[schema(value_type = Option<Object>)]
    pub after: Option<Value>,
    pub actor: ActorV2,
    /// When the source wrote the change to its log.
    pub committed_at: String,
    /// When the Sthiranet node received it.
    pub captured_at: String,
}

impl EventV2 {
    /// The row image that describes the record: `after`, or `before` for a delete.
    pub fn row(&self) -> Option<&Value> {
        self.after.as_ref().or(self.before.as_ref())
    }

    /// The log position as a number, when it is one.
    pub fn pos_num(&self) -> Option<u64> {
        self.source.pos.parse().ok()
    }
}
