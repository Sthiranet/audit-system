//! What the ledger stores and returns, independent of which ledger implements [`crate::AuditLedger`].
//!
//! Positions are GTID transaction numbers. An empty batch repeats the previous batch's `to_tx` in
//! both `from_tx` and `to_tx` (0 before anything was captured), so every anchor states where in the
//! source's log the chain stands.

use serde::{Deserialize, Serialize};

/// A party's short id (`DREAMTALK`, `AUDITOR`). Ledgers map it to their own identifiers.
pub type Party = String;

/// What the submitter appends for one sealed batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorInput {
    pub seq: u64,
    pub prev_head: String,
    pub head: String,
    pub merkle_root: String,
    /// Every leaf hash of the batch, in batch order. Anchoring them lets a verifier check each
    /// record on its own against the ledger, whatever happened to the rest of the store.
    pub leaves: Vec<String>,
    pub event_count: u64,
    /// Number of source transactions in the batch.
    pub tx_count: u64,
    pub from_tx: u64,
    pub to_tx: u64,
    pub from_time: String,
    pub to_time: String,
}

/// An anchor as one member's ledger copy holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    pub contract_id: String,
    pub chain_id: String,
    pub seq: u64,
    pub prev_head: String,
    pub head: String,
    pub merkle_root: String,
    pub leaves: Vec<String>,
    pub event_count: u64,
    pub tx_count: u64,
    pub from_tx: u64,
    pub to_tx: u64,
    pub from_time: String,
    pub to_time: String,
    pub signatories: Vec<Party>,
    /// The ledger transaction that created it.
    pub update_id: String,
    pub recorded_at: String,
}

/// A chain to create: its members co-sign every anchor, only the submitter appends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSpec {
    pub chain_id: String,
    pub submitter: Party,
    /// Every co-signer, the submitter included.
    pub members: Vec<Party>,
    pub genesis_head: String,
}

/// One member's view of a chain: the chain head and every anchor, in seq order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainView {
    pub chain_id: String,
    pub submitter: Party,
    pub members: Vec<Party>,
    pub genesis_head: String,
    pub last_seq: u64,
    pub last_head: String,
    pub anchors: Vec<Anchor>,
}

/// The outcome of one party trying to archive (rewrite) an anchor on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveAttempt {
    pub ok: bool,
    pub actor: Party,
    pub chain_id: String,
    pub seq: u64,
    pub contract_id: Option<String>,
    pub required: Vec<Party>,
    pub missing: Vec<Party>,
    pub reason: String,
}
