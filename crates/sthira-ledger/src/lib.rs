//! Ledger adapter · [`canton::CantonLedger`] implements [`sthira_core::AuditLedger`] on a Canton
//! participant (JSON Ledger API v2, the `Sthiranet.Audit` Daml templates in `prototype-sandbox/`).
//!
//! What the ledger guarantees, enforced by the Daml model rather than by this code:
//! - Every member (company, auditor, …) signs a chain once; after that only the submitter appends.
//! - An append continues the chain (`seq` = last + 1, `prevHead` = chain head) or is refused, and
//!   its leaves must match its counts. The chain contract is consumed, so it can never fork.
//! - Every `AuditAnchor` is signed by every member and stored on every member's participant node,
//!   so no single party can archive (rewrite) it.
//! - Parties outside the chain see nothing; the synchronizer only orders encrypted payloads.

pub mod canton;

pub use canton::{CantonConfig, CantonLedger};
