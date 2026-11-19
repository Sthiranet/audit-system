//! Connector layer · the application audit-log API profile (first source: the Dreamtalk backend's
//! `/audit/` endpoint).
//!
//! The application writes one audit row per insert, update and delete into its own change log and
//! publishes it over HTTP. This module replaces Debezium for such sources:
//!
//! 1. [`types`]: the API's JSON exactly as served ([`AuditLogEntry`], [`AuditPage`]).
//! 2. [`feed`]: reading the newest-first, page-numbered endpoint into an ordered stream
//!    ([`AuditCursor`]), with gap and late-arrival handling.
//! 3. [`normalize`]: turning an audit row into the spec v2 event that gets hashed ([`normalize_audit`]).
//! 4. [`describe`]: what a change means, for people ([`describe_audit`], [`audit_flags`]).
//! 5. [`tamper`]: the presenter's "corrupt insider" edits on a stored v2 event.

pub mod describe;
pub mod feed;
pub mod normalize;
pub mod tamper;
pub mod types;

pub use describe::{audit_flags, describe_audit, is_noise};
pub use feed::{AuditCursor, AuditFeed, FeedError, GapRange, HttpFeed, MemoryFeed, Polled, Start};
pub use normalize::{CONNECTOR, normalize_audit, system_of};
pub use tamper::{reattribute_audit, tamper_audit};
pub use types::{AuditEnvelope, AuditLogEntry, AuditPage};
