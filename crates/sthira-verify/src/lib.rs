//! Verifier · any member (for example the auditor) takes an off-ledger audit trail, recomputes every
//! leaf, Merkle root and head, and compares them with the anchors on ITS OWN ledger copy. It needs
//! nothing from the party that produced the trail except the trail.
//!
//! Because every leaf hash is anchored, each record is checked on its own against its leaf on the
//! ledger: an altered record is found at its exact slot even if the rest of the store was forged
//! around it (other records edited or deleted, stored leaf hashes rewritten to cover the edit).

use std::collections::{BTreeMap, BTreeSet};

pub mod positions;

pub use positions::{PositionReport, TxGap, TxHole, position_report};

use serde::{Deserialize, Serialize};
use sthira_core::{Anchor, BatchMeta, TrailRecord, head_hash, merkle_root};

/// A record that is no longer the one anchored at capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeafIssue {
    /// The record's slot in the batch as sealed.
    pub index: usize,
    pub record_id: String,
    /// The leaf the ledger holds for this slot; `None` if the batch never had such a slot.
    pub anchored: Option<String>,
    /// The leaf recomputed from the stored salt and event.
    pub recomputed: String,
    /// The leaf hash stored next to the record.
    pub recorded: String,
    /// The stored leaf hash was rewritten to agree with the altered record.
    pub covered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCheck {
    pub seq: u64,
    pub ok: bool,
    /// The anchor agrees with itself: its leaves hash to its root and match its count.
    pub ledger_consistent: bool,
    pub root_match: bool,
    pub head_match: bool,
    pub count_match: bool,
    pub anchored_root: String,
    pub recomputed_root: String,
    pub anchored_head: String,
    pub recomputed_head: String,
    pub anchored_count: u64,
    pub recomputed_count: u64,
    pub signatories: Vec<String>,
    pub contract_id: String,
    /// Every altered or fabricated record in this batch.
    pub leaf_issues: Vec<LeafIssue>,
    /// Slots the anchor committed to that have no record in the trail: deleted records.
    pub missing_indexes: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub ok: bool,
    pub batches: Vec<BatchCheck>,
    /// Batches with any mismatch.
    pub failed_seqs: Vec<u64>,
    /// Batches whose own contents changed, as opposed to batches that fail only because an earlier
    /// head changed.
    pub changed_seqs: Vec<u64>,
    /// Batch numbers present in the trail with no anchor on the ledger.
    pub unanchored: Vec<u64>,
    pub altered_count: usize,
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
        let mut recs: Vec<&R> = by_seq.get(&a.seq).cloned().unwrap_or_default();
        recs.sort_by_key(|r| r.index());
        let leaves: Vec<String> = recs.iter().map(|r| r.recompute_leaf()).collect();
        let root = merkle_root(&leaves);
        let meta = BatchMeta {
            chain: chain_id.to_string(),
            seq: a.seq,
            from_tx: a.from_tx,
            to_tx: a.to_tx,
            from_time: a.from_time.clone(),
            to_time: a.to_time.clone(),
            event_count: recs.len() as u64,
            tx_count: a.tx_count,
        };
        let head = head_hash(&prev_head, &root, &meta);

        let leaf_issues: Vec<LeafIssue> = recs
            .iter()
            .zip(&leaves)
            .filter(|(r, l)| a.leaves.get(r.index()) != Some(*l))
            .map(|(r, l)| LeafIssue {
                index: r.index(),
                record_id: r.record_id().to_string(),
                anchored: a.leaves.get(r.index()).cloned(),
                recomputed: l.clone(),
                recorded: r.stored_leaf().to_string(),
                covered: r.stored_leaf() == l.as_str(),
            })
            .collect();
        let present: BTreeSet<usize> = recs.iter().map(|r| r.index()).collect();
        let missing_indexes: Vec<usize> = (0..a.leaves.len()).filter(|i| !present.contains(i)).collect();

        let ledger_consistent = merkle_root(&a.leaves) == a.merkle_root && a.leaves.len() as u64 == a.event_count;
        let root_match = root == a.merkle_root;
        let head_match = head == a.head;
        let count_match = recs.len() as u64 == a.event_count;
        batches.push(BatchCheck {
            seq: a.seq,
            ok: ledger_consistent && root_match && head_match && count_match && leaf_issues.is_empty(),
            ledger_consistent,
            root_match,
            head_match,
            count_match,
            anchored_root: a.merkle_root.clone(),
            recomputed_root: root,
            anchored_head: a.head.clone(),
            recomputed_head: head.clone(),
            anchored_count: a.event_count,
            recomputed_count: recs.len() as u64,
            signatories: a.signatories.clone(),
            contract_id: a.contract_id.clone(),
            leaf_issues,
            missing_indexes,
        });
        prev_head = head; // heads chain: one altered batch breaks every head after it
    }

    let anchored: BTreeSet<u64> = anchors.iter().map(|a| a.seq).collect();
    let unanchored: Vec<u64> = by_seq.keys().copied().filter(|s| !anchored.contains(s)).collect();
    let failed_seqs: Vec<u64> = batches.iter().filter(|b| !b.ok).map(|b| b.seq).collect();
    let changed_seqs: Vec<u64> = batches
        .iter()
        .filter(|b| !b.leaf_issues.is_empty() || !b.missing_indexes.is_empty() || !b.root_match || !b.count_match)
        .map(|b| b.seq)
        .collect();
    VerifyReport {
        ok: failed_seqs.is_empty() && unanchored.is_empty(),
        altered_count: batches.iter().map(|b| b.leaf_issues.len()).sum(),
        deleted_count: batches.iter().map(|b| b.missing_indexes.len()).sum(),
        batches,
        failed_seqs,
        changed_seqs,
        unanchored,
        event_count: rows.len(),
    }
}

/// The RFC 6962 inclusion path of slot `index` in an anchored batch, derived from the leaves on the
/// ledger. `None` if the batch has no such slot.
pub fn proof_from_anchor(anchor: &Anchor, index: usize) -> Option<Vec<String>> {
    sthira_core::inclusion_proof(&anchor.leaves, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sthira_core::{genesis_head, leaf_hash, merkle_root, verify_inclusion};

    #[derive(Clone)]
    struct Rec {
        id: String,
        seq: u64,
        index: usize,
        salt: String,
        body: String,
        leaf: String,
    }

    impl TrailRecord for Rec {
        fn record_id(&self) -> &str {
            &self.id
        }
        fn batch_seq(&self) -> u64 {
            self.seq
        }
        fn index(&self) -> usize {
            self.index
        }
        fn stored_leaf(&self) -> &str {
            &self.leaf
        }
        fn recompute_leaf(&self) -> String {
            leaf_hash(&self.salt, &self.body)
        }
    }

    /// Two batches of four records, anchored as the node would.
    fn chain() -> (Vec<Anchor>, Vec<Rec>) {
        let g = genesis_head("DT");
        let mut prev = g.clone();
        let (mut anchors, mut rows) = (Vec::new(), Vec::new());
        for seq in 1..=2u64 {
            let recs: Vec<Rec> = (0..4)
                .map(|i| {
                    let salt = format!("{:02x}", seq * 10 + i as u64).repeat(32);
                    let body = format!("record {seq}.{i}");
                    let leaf = leaf_hash(&salt, &body);
                    Rec { id: format!("DT-{seq}.{i}"), seq, index: i, salt, body, leaf }
                })
                .collect();
            let leaves: Vec<String> = recs.iter().map(|r| r.leaf.clone()).collect();
            let root = merkle_root(&leaves);
            let (from_tx, to_tx) = (seq * 2 - 1, seq * 2);
            let meta = BatchMeta {
                chain: "DT".into(),
                seq,
                from_tx,
                to_tx,
                from_time: format!("T{}", seq - 1),
                to_time: format!("T{seq}"),
                event_count: 4,
                tx_count: 2,
            };
            let head = head_hash(&prev, &root, &meta);
            let mut a = positions::tests::a(seq, from_tx, to_tx, 2);
            (a.prev_head, a.head, a.merkle_root, a.leaves, a.event_count) =
                (prev.clone(), head.clone(), root, leaves, 4);
            (a.from_time, a.to_time) = (meta.from_time, meta.to_time);
            anchors.push(a);
            rows.extend(recs);
            prev = head;
        }
        (anchors, rows)
    }

    #[test]
    fn untouched_trail_passes() {
        let (anchors, rows) = chain();
        let r = verify_records("DT", &genesis_head("DT"), &anchors, &rows);
        assert!(r.ok, "{:?}", r.failed_seqs);
        assert_eq!(r.event_count, 8);
    }

    #[test]
    fn covered_edit_is_pinpointed_even_with_other_damage() {
        let (anchors, mut rows) = chain();
        // Insider edits record 1.2 and rewrites its stored leaf hash, deletes record 1.0, and
        // vandalises the stored leaf of an honest record 1.3.
        rows[2].body = "record 1.2 (edited)".into();
        rows[2].leaf = rows[2].recompute_leaf();
        rows[3].leaf = "00".repeat(32);
        rows.remove(0);
        let r = verify_records("DT", &genesis_head("DT"), &anchors, &rows);
        assert!(!r.ok);
        assert_eq!(r.changed_seqs, vec![1]);
        assert_eq!(r.failed_seqs, vec![1, 2], "batch 2's head chains over batch 1");
        let b = &r.batches[0];
        assert_eq!(b.leaf_issues.len(), 1, "only the edited record, not the honest one");
        assert_eq!((b.leaf_issues[0].record_id.as_str(), b.leaf_issues[0].covered), ("DT-1.2", true));
        assert_eq!(b.missing_indexes, vec![0]);
        assert!(b.ledger_consistent && r.batches[1].leaf_issues.is_empty());
    }

    #[test]
    fn fabricated_record_and_inconsistent_anchor() {
        let (mut anchors, mut rows) = chain();
        let mut fake = rows[7].clone();
        (fake.id, fake.index, fake.body) = ("DT-2.9".into(), 9, "invented".into());
        rows.push(fake);
        anchors[1].leaves[0] = "ff".repeat(32);
        let r = verify_records("DT", &genesis_head("DT"), &anchors, &rows);
        let b = &r.batches[1];
        assert!(!b.ledger_consistent);
        assert!(b.leaf_issues.iter().any(|i| i.record_id == "DT-2.9" && i.anchored.is_none()));
    }

    #[test]
    fn proofs_come_from_the_anchored_leaves() {
        let (anchors, rows) = chain();
        let a = &anchors[1];
        let p = proof_from_anchor(a, 3).unwrap();
        assert!(verify_inclusion(&rows[7].leaf, 3, a.leaves.len(), &p, &a.merkle_root));
        assert!(proof_from_anchor(a, 4).is_none());
    }
}
