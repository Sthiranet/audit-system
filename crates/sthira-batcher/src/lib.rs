//! Node layer · capture, batch sealing and anchoring. [`feed_node::FeedNode`] is the node fed by a
//! live audit-log API (spec v2 events); [`persist`] keeps its trail, batches and cursor on disk.
//!
//! [`plan_seal`] is the sealing rule: a fresh salt and leaf per event, the Merkle tree, the log
//! positions and the chained head. An empty batch is sealed and anchored too, so a quiet period is
//! visible on the ledger.

pub mod feed_node;
pub mod persist;

use serde::Serialize;
use sthira_core::{BatchMeta, MerkleTree, all_inclusion_proofs, head_hash, leaf_hash, merkle_tree, new_salt};
use sthira_ledger::AnchorInput;

/// The end of a chain as the node last sealed it.
#[derive(Debug, Clone, Copy)]
pub struct ChainTip<'a> {
    pub seq: u64,
    pub head: &'a str,
    pub to_time: &'a str,
    pub to_pos: &'a str,
}

/// Everything computed when a batch is sealed, before anything is anchored or stored.
#[derive(Debug, Clone)]
pub struct SealPlan {
    pub seq: u64,
    pub prev_head: String,
    pub head: String,
    pub tree: MerkleTree,
    pub meta: BatchMeta,
    pub salts: Vec<String>,
    pub leaves: Vec<String>,
    pub from_time: String,
    pub to_time: String,
    pub from_pos: String,
    pub to_pos: String,
}

impl SealPlan {
    /// What the subsidiary submits to the ledger for this batch.
    pub fn anchor_input(&self) -> AnchorInput {
        AnchorInput {
            seq: self.seq,
            prev_head: self.prev_head.clone(),
            head: self.head.clone(),
            merkle_root: self.tree.hash.clone(),
            event_count: self.leaves.len() as u64,
            from_pos: self.from_pos.clone(),
            to_pos: self.to_pos.clone(),
            from_time: self.from_time.clone(),
            to_time: self.to_time.clone(),
        }
    }

    /// The RFC 6962 audit path of every leaf. Computed at seal time, while all leaves are in hand.
    pub fn proofs(&self) -> Vec<Vec<String>> {
        all_inclusion_proofs(&self.leaves)
    }
}

/// The sealing rule shared by every profile: chain position, a fresh salt and leaf per event, the
/// Merkle tree, the log positions and the chained head. Pure apart from the random salts.
///
/// - `last` is the previous batch (`None` for the first, which chains onto `genesis`).
/// - `span` is the first and last log position of the events; an empty batch repeats the previous
///   batch's `to_pos`, so it still states where in the log the chain stands.
/// - Windows are contiguous: `from_time` is the previous `to_time` (or `chain_start`).
pub fn plan_seal<E: Serialize>(
    chain_id: &str,
    genesis: &str,
    chain_start: &str,
    last: Option<ChainTip<'_>>,
    events: &[&E],
    span: Option<(String, String)>,
    to_time: String,
) -> SealPlan {
    let seq = last.map_or(0, |b| b.seq) + 1;
    let prev_head = last.map_or(genesis, |b| b.head).to_string();
    let from_time = last.map_or(chain_start, |b| b.to_time).to_string();
    let prev_pos = last.map(|b| b.to_pos.to_string()).unwrap_or_default();
    let salts: Vec<String> = events.iter().map(|_| new_salt()).collect();
    let leaves: Vec<String> = events.iter().zip(&salts).map(|(e, s)| leaf_hash(s, e)).collect();
    let tree = merkle_tree(&leaves);
    let (from_pos, to_pos) = span.unwrap_or_else(|| (prev_pos.clone(), prev_pos));
    let meta = BatchMeta {
        chain: chain_id.to_string(),
        seq,
        from_pos: from_pos.clone(),
        to_pos: to_pos.clone(),
        from_time: from_time.clone(),
        to_time: to_time.clone(),
        event_count: events.len() as u64,
    };
    let head = head_hash(&prev_head, &tree.hash, &meta);
    SealPlan { seq, prev_head, head, tree, meta, salts, leaves, from_time, to_time, from_pos, to_pos }
}
