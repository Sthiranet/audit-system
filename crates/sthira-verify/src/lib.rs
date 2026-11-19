//! Node layer · sthira-verify. Any member (for example the auditor) takes an off-ledger audit trail,
//! recomputes every leaf, Merkle root and head, and compares them with the anchors stored on
//! ITS OWN ledger copy. It needs nothing from the party that produced the trail except the trail.

use std::collections::{BTreeMap, BTreeSet};

pub mod positions;

pub use positions::{PosGap, PosHole, PositionReport, position_report};

use serde::{Deserialize, Serialize};
use sthira_batcher::feed_node::FeedRow;
use sthira_core::{BatchMeta, MerkleTree, head_hash, leaf_hash, merkle_tree, verify_inclusion};
use sthira_ledger::Anchor;
use sthira_profiles::Description;
use sthira_profiles::audit_api::describe_audit;

/// One record of an off-ledger trail, whatever profile produced it. The verifier needs only these
/// facts, and recomputes the leaf itself; it never trusts `stored_leaf`.
pub trait TrailRecord {
    fn record_id(&self) -> &str;
    fn key(&self) -> &str;
    fn batch_seq(&self) -> u64;
    fn index(&self) -> usize;
    fn stored_leaf(&self) -> &str;
    fn proof(&self) -> &[String];
    /// `H(0x00 || salt || JCS(event))` from the stored salt and event.
    fn recompute_leaf(&self) -> String;
    fn description(&self) -> Description;
}

impl TrailRecord for FeedRow {
    fn record_id(&self) -> &str {
        &self.id
    }
    fn key(&self) -> &str {
        &self.event.key.id
    }
    fn batch_seq(&self) -> u64 {
        self.batch_seq
    }
    fn index(&self) -> usize {
        self.index
    }
    fn stored_leaf(&self) -> &str {
        &self.leaf_hash
    }
    fn proof(&self) -> &[String] {
        &self.proof
    }
    fn recompute_leaf(&self) -> String {
        leaf_hash(&self.salt, &self.event)
    }
    fn description(&self) -> Description {
        describe_audit(&self.event)
    }
}

/// A record that is no longer the one anchored at capture: its recomputed leaf, with the inclusion
/// proof it carries, does not lead to the Merkle root on the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeafIssue {
    /// The leaf's slot in the batch as sealed (what the trail calls `L{index + 1}`).
    pub index: usize,
    /// The leaf's position in the recomputed tree; lower than `index` once earlier records are deleted.
    pub position: usize,
    pub event_id: String,
    pub key: String,
    /// The leaf hash stored next to the record.
    pub recorded: String,
    pub recomputed: String,
    /// The stored leaf hash was rewritten to agree with the altered record.
    pub covered: bool,
}

/// The values committed on the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnchoredValues {
    pub root: String,
    pub head: String,
    pub prev_head: String,
    pub event_count: u64,
    pub from_time: String,
    pub to_time: String,
    pub signatories: Vec<String>,
    pub contract_id: String,
}

/// The values recomputed from the trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecomputedValues {
    pub root: String,
    pub head: String,
    pub prev_head: String,
    pub event_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeafView {
    pub id: String,
    pub key: String,
    pub index: usize,
    pub leaf: String,
    #[serde(flatten)]
    pub description: Description,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchCheck {
    pub seq: u64,
    pub ok: bool,
    pub root_match: bool,
    pub head_match: bool,
    pub count_match: bool,
    pub anchored: AnchoredValues,
    pub recomputed: RecomputedValues,
    /// Every altered record in this batch, however many there are.
    pub leaf_issues: Vec<LeafIssue>,
    /// Slots the anchor committed to that have no record in the trail: deleted records.
    pub missing_indexes: Vec<usize>,
    pub tree: MerkleTree,
    pub leaves: Vec<LeafView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub ok: bool,
    pub batches: Vec<BatchCheck>,
    /// Batches whose root, head or count does not match.
    pub failed_seqs: Vec<u64>,
    /// Batch numbers present in the trail with no anchor on the ledger.
    pub unanchored: Vec<u64>,
    /// Every batch whose own contents changed (root or count mismatch), as opposed to batches that
    /// fail only because an earlier head changed.
    pub changed_seqs: Vec<u64>,
    /// The first of `changed_seqs`.
    pub first_bad_seq: Option<u64>,
    /// Records altered across the whole trail.
    pub altered_count: usize,
    /// Records deleted across the whole trail.
    pub deleted_count: usize,
    pub event_count: usize,
}

/// Recomputes any trail and compares it with `anchors` from the verifier's own ledger copy.
pub fn verify_records<R: TrailRecord>(
    chain_id: &str,
    genesis_head: &str,
    anchors: &[Anchor],
    rows: &[R],
) -> VerifyReport {
    let mut by_seq: BTreeMap<u64, Vec<&R>> = BTreeMap::new();
    for r in rows {
        by_seq.entry(r.batch_seq()).or_default().push(r);
    }

    let mut prev_head = genesis_head.to_string();
    let mut batches = Vec::with_capacity(anchors.len());
    for a in anchors {
        let mut evs: Vec<&R> = by_seq.get(&a.seq).cloned().unwrap_or_default();
        evs.sort_by_key(|r| r.index());
        let leaves: Vec<String> = evs.iter().map(|r| r.recompute_leaf()).collect();
        let tree = merkle_tree(&leaves);
        let meta = BatchMeta {
            chain: chain_id.to_string(),
            seq: a.seq,
            from_pos: a.from_pos.clone(),
            to_pos: a.to_pos.clone(),
            from_time: a.from_time.clone(),
            to_time: a.to_time.clone(),
            event_count: evs.len() as u64,
        };
        let head = head_hash(&prev_head, &tree.hash, &meta);

        // Each record is checked on its own against the ANCHORED root, through the proof it carries.
        // So an altered record is found at its original slot whatever else happened to the batch:
        // other edits, deletions that shift positions, or a stored leaf hash rewritten to cover it.
        let size = a.event_count as usize;
        let leaf_issues: Vec<LeafIssue> = evs
            .iter()
            .zip(&leaves)
            .enumerate()
            .filter(|(_, (r, l))| !verify_inclusion(l, r.index(), size, r.proof(), &a.merkle_root))
            .map(|(i, (r, l))| LeafIssue {
                index: r.index(),
                position: i,
                event_id: r.record_id().to_string(),
                key: r.key().to_string(),
                recorded: r.stored_leaf().to_string(),
                recomputed: l.clone(),
                covered: r.stored_leaf() == l.as_str(),
            })
            .collect();
        let present: BTreeSet<usize> = evs.iter().map(|r| r.index()).collect();
        let missing_indexes: Vec<usize> = (0..size).filter(|i| !present.contains(i)).collect();

        let root_match = tree.hash == a.merkle_root;
        let head_match = head == a.head;
        let count_match = evs.len() as u64 == a.event_count;
        batches.push(BatchCheck {
            seq: a.seq,
            ok: root_match && head_match && count_match,
            root_match,
            head_match,
            count_match,
            anchored: AnchoredValues {
                root: a.merkle_root.clone(),
                head: a.head.clone(),
                prev_head: a.prev_head.clone(),
                event_count: a.event_count,
                from_time: a.from_time.clone(),
                to_time: a.to_time.clone(),
                signatories: a.signatories.clone(),
                contract_id: a.contract_id.clone(),
            },
            recomputed: RecomputedValues {
                root: tree.hash.clone(),
                head: head.clone(),
                prev_head: prev_head.clone(),
                event_count: evs.len() as u64,
            },
            leaf_issues,
            missing_indexes,
            leaves: evs
                .iter()
                .zip(&leaves)
                .map(|(r, l)| LeafView {
                    id: r.record_id().to_string(),
                    key: r.key().to_string(),
                    index: r.index(),
                    leaf: l.clone(),
                    description: r.description(),
                })
                .collect(),
            tree,
        });
        prev_head = head; // heads chain: one altered batch breaks every head after it
    }

    let anchored: BTreeSet<u64> = anchors.iter().map(|a| a.seq).collect();
    let unanchored: Vec<u64> = by_seq.keys().copied().filter(|s| !anchored.contains(s)).collect();
    let failed_seqs: Vec<u64> = batches.iter().filter(|b| !b.ok).map(|b| b.seq).collect();
    let changed_seqs: Vec<u64> = batches.iter().filter(|b| !b.root_match || !b.count_match).map(|b| b.seq).collect();
    VerifyReport {
        first_bad_seq: changed_seqs.first().copied(),
        changed_seqs,
        altered_count: batches.iter().map(|b| b.leaf_issues.len()).sum(),
        deleted_count: batches.iter().map(|b| b.missing_indexes.len()).sum(),
        ok: failed_seqs.is_empty() && unanchored.is_empty(),
        batches,
        failed_seqs,
        unanchored,
        event_count: rows.len(),
    }
}
