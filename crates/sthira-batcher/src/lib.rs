//! Node layer · capture, batch sealing and anchoring.
//!
//! - [`node::Node`] queues captured transactions, seals them into a batch on demand and anchors the
//!   batch through any [`sthira_core::AuditLedger`]. Its off-ledger audit store keeps each event with
//!   its salt and leaf hash.
//! - [`persist`] keeps the store, the batch table and the capture cursor on disk.
//! - [`plan_seal`] is the sealing rule: a fresh salt and leaf per event, the Merkle tree, the GTID
//!   window and the chained head. An empty batch is sealed and anchored too, so a quiet period is
//!   visible on the ledger.

pub mod node;
pub mod persist;

pub use node::{BatchRecord, Ingest, Node, Sealed, StoredRow};

use serde::Serialize;
use sthira_core::{AnchorInput, BatchMeta, MerkleTree, head_hash, leaf_hash, merkle_tree, new_salt};

/// The end of a chain as the node last sealed it.
#[derive(Debug, Clone, Copy)]
pub struct ChainTip<'a> {
    pub seq: u64,
    pub head: &'a str,
    pub to_time: &'a str,
    pub to_tx: u64,
}

/// Everything computed when a batch is sealed, before anything is anchored or stored.
#[derive(Debug, Clone)]
pub struct SealPlan {
    pub prev_head: String,
    pub head: String,
    pub tree: MerkleTree,
    pub meta: BatchMeta,
    pub salts: Vec<String>,
    pub leaves: Vec<String>,
}

impl SealPlan {
    /// What the submitter appends to the ledger for this batch: the leaves travel with the root.
    pub fn anchor_input(&self) -> AnchorInput {
        AnchorInput {
            seq: self.meta.seq,
            prev_head: self.prev_head.clone(),
            head: self.head.clone(),
            merkle_root: self.tree.hash.clone(),
            leaves: self.leaves.clone(),
            event_count: self.meta.event_count,
            tx_count: self.meta.tx_count,
            from_tx: self.meta.from_tx,
            to_tx: self.meta.to_tx,
            from_time: self.meta.from_time.clone(),
            to_time: self.meta.to_time.clone(),
        }
    }
}

/// The sealing rule: chain position, a fresh salt and leaf per event, the Merkle tree, the GTID
/// window and the chained head. Pure apart from the random salts.
///
/// - `last` is the previous batch (`None` for the first, which chains onto `genesis`).
/// - `txs` is the first and last GTID number and the number of transactions sealed; `None` for an
///   empty batch, which repeats the previous `to_tx` (0 before anything was captured).
/// - Windows are contiguous: `from_time` is the previous `to_time` (or `chain_start`).
pub fn plan_seal<E: Serialize>(
    chain_id: &str,
    genesis: &str,
    chain_start: &str,
    last: Option<ChainTip<'_>>,
    events: &[&E],
    txs: Option<(u64, u64, u64)>,
    to_time: String,
) -> SealPlan {
    let prev_tx = last.map_or(0, |b| b.to_tx);
    let (from_tx, to_tx, tx_count) = txs.unwrap_or((prev_tx, prev_tx, 0));
    let salts: Vec<String> = events.iter().map(|_| new_salt()).collect();
    let leaves: Vec<String> = events.iter().zip(&salts).map(|(e, s)| leaf_hash(s, e)).collect();
    let tree = merkle_tree(&leaves);
    let meta = BatchMeta {
        chain: chain_id.to_string(),
        seq: last.map_or(0, |b| b.seq) + 1,
        from_tx,
        to_tx,
        from_time: last.map_or(chain_start, |b| b.to_time).to_string(),
        to_time,
        event_count: events.len() as u64,
        tx_count,
    };
    let prev_head = last.map_or(genesis, |b| b.head).to_string();
    let head = head_hash(&prev_head, &tree.hash, &meta);
    SealPlan { prev_head, head, tree, meta, salts, leaves }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sthira_core::{EMPTY_ROOT, genesis_head};

    #[test]
    fn seal_chains_and_empty_batches_repeat_the_position() {
        let g = genesis_head("DT");
        let (a, b) = (json!({"a": 1}), json!({"b": 2}));
        let p1 = plan_seal("DT", &g, "T0", None, &[&a, &b], Some((1, 1, 1)), "T1".into());
        assert_eq!((p1.meta.seq, p1.prev_head.as_str(), p1.leaves.len()), (1, g.as_str(), 2));
        assert_eq!(p1.meta.from_time, "T0");
        let input = p1.anchor_input();
        assert_eq!((input.leaves.clone(), input.merkle_root.clone()), (p1.leaves.clone(), p1.tree.hash.clone()));
        let tip = ChainTip { seq: 1, head: &p1.head, to_time: "T1", to_tx: 1 };
        let p2 = plan_seal::<serde_json::Value>("DT", &g, "T0", Some(tip), &[], None, "T2".into());
        assert_eq!((p2.meta.from_tx, p2.meta.to_tx, p2.meta.tx_count), (1, 1, 0));
        assert_eq!(
            (p2.prev_head.as_str(), p2.tree.hash.as_str(), p2.meta.from_time.as_str()),
            (p1.head.as_str(), EMPTY_ROOT, "T1")
        );
    }
}
