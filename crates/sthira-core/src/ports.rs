//! The ports: the only way the crates talk to each other.
//!
//! ```text
//!   sthira-connector ──impl──► ChangeSource ──used by──► sthira-node (run loop) ──► sthira-batcher
//!   sthira-ledger    ──impl──► AuditLedger  ──used by──► sthira-batcher (seal), sthira-node
//!   sthira-batcher   ──impl──► TrailRecord  ──used by──► sthira-verify
//! ```
//!
//! The async methods return `impl Future + Send`, so callers stay generic (no boxing, no `dyn`) and
//! their futures can still run on a multi-threaded runtime.

use std::future::Future;

use crate::event::Event;
use crate::ledger::{Anchor, AnchorInput, ArchiveAttempt, ChainSpec, ChainView, Party};

/// One committed source transaction, normalised: every row change it made, in log order.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedTx {
    /// GTID transaction number.
    pub tx: u64,
    pub gtid: String,
    pub events: Vec<Event>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("reading the change log: {0}")]
    Io(String),
    #[error("change log entry {at}: {msg}")]
    Decode { at: String, msg: String },
}

/// A connector: yields whole committed transactions in log order, `None` once the log is exhausted.
/// A transaction is never split.
pub trait ChangeSource: Send {
    fn next_tx(&mut self) -> impl Future<Output = Result<Option<CapturedTx>, SourceError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LedgerError(pub String);

/// The ledger operations Sthiranet needs. Every member's node keeps its own copy of the chain; an
/// append succeeds only when every member's copy accepts it.
pub trait AuditLedger: Send + Sync {
    /// Makes sure every party exists on the ledger (allocating it when needed).
    fn ensure_parties(&mut self, ids: &[Party]) -> impl Future<Output = Result<(), LedgerError>> + Send;
    /// Creates a chain co-signed by every member.
    fn create_chain(&self, spec: &ChainSpec) -> impl Future<Output = Result<(), LedgerError>> + Send;
    /// Appends one batch's anchor. Refused on a sequence gap, a fork, or inconsistent counts.
    fn append(
        &self,
        chain_id: &str,
        submitter: &str,
        input: &AnchorInput,
    ) -> impl Future<Output = Result<Anchor, LedgerError>> + Send;
    /// `party`'s own copy of the chain, read from its node; `None` if it is not a stakeholder.
    fn chain_view(
        &self,
        party: &str,
        chain_id: &str,
    ) -> impl Future<Output = Result<Option<ChainView>, LedgerError>> + Send;
    /// Tries to archive anchor `seq` acting as `actor` alone.
    fn attempt_archive(
        &self,
        chain_id: &str,
        actor: &str,
        seq: u64,
    ) -> impl Future<Output = Result<ArchiveAttempt, LedgerError>> + Send;
}

/// One record of an off-ledger trail. The verifier needs only these facts and recomputes the leaf
/// itself; it never trusts `stored_leaf`.
pub trait TrailRecord {
    fn record_id(&self) -> &str;
    fn batch_seq(&self) -> u64;
    /// Slot in the batch as sealed, 0-based.
    fn index(&self) -> usize;
    fn stored_leaf(&self) -> &str;
    /// `H(0x00 || salt || JCS(event))` from the stored salt and event.
    fn recompute_leaf(&self) -> String;
}
