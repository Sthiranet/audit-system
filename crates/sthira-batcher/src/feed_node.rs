//! Node layer · a node fed by a live audit-log API (the prototype).
//!
//! Rows read by the connector's [`AuditCursor`](sthira_profiles::audit_api::AuditCursor) are
//! normalised into spec v2 events and wait in `pending`. [`FeedNode::seal`] closes a batch with the
//! shared sealing rule ([`crate::plan_seal`]), anchors it, and moves the rows into the
//! node's off-ledger audit store with their salt, leaf hash and inclusion proof.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sthira_core::{BatchMeta, EventV2, genesis_head};
use sthira_ledger::{Anchor, AuditLedger, LedgerError};
use sthira_profiles::audit_api::{AuditLogEntry, normalize_audit};

use crate::{ChainTip, SealPlan, plan_seal};

/// A captured audit row waiting for the next batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedRecord {
    /// `{chain}-{audit row id}`, e.g. `DT-190850`.
    pub id: String,
    pub raw: AuditLogEntry,
    pub event: EventV2,
    /// It arrived after the node had declared its id missing.
    pub late: bool,
}

/// One row of the node's off-ledger audit store (`node/trail.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedRow {
    pub id: String,
    pub chain: String,
    pub batch_seq: u64,
    /// Slot in the batch as sealed, 0-based (shown as `L{index + 1}`).
    pub index: usize,
    /// The hashed event.
    pub event: EventV2,
    /// The API row as received. Kept for reference; not hashed.
    pub raw: AuditLogEntry,
    pub salt: String,
    pub leaf_hash: String,
    /// RFC 6962 audit path of this leaf in its batch.
    pub proof: Vec<String>,
    #[serde(default)]
    pub late: bool,
}

/// A row of the node's own batch table (`node/batches.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedBatch {
    pub seq: u64,
    pub prev_head: String,
    pub head: String,
    pub merkle_root: String,
    pub event_count: u64,
    pub from_time: String,
    pub to_time: String,
    pub from_pos: String,
    pub to_pos: String,
    pub meta: BatchMeta,
    pub contract_id: String,
    pub update_id: String,
}

/// What one seal produced, for the caller to persist.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub batch: FeedBatch,
    pub anchor: Anchor,
    pub rows: Vec<FeedRow>,
}

pub struct FeedNode {
    /// The party that submits this chain's anchors.
    pub party: String,
    pub chain_id: String,
    /// Host of the source API, written into every event as `source.system`.
    pub system: String,
    pub genesis: String,
    pub chain_start: String,
    pub pending: Vec<FeedRecord>,
    pub store: Vec<FeedRow>,
    pub batches: Vec<FeedBatch>,
    /// Log positions already captured (pending or stored): a re-read row is never stored twice.
    seen: HashSet<String>,
}

impl FeedNode {
    pub fn new(party: &str, chain_id: &str, system: &str, chain_start: String) -> Self {
        Self {
            party: party.to_string(),
            chain_id: chain_id.to_string(),
            system: system.to_string(),
            genesis: genesis_head(chain_id),
            chain_start,
            pending: Vec::new(),
            store: Vec::new(),
            batches: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Rebuilds a node from its persisted store and batch table.
    pub fn restore(mut self, store: Vec<FeedRow>, batches: Vec<FeedBatch>) -> Self {
        self.seen = store.iter().map(|r| r.event.source.pos.clone()).collect();
        self.store = store;
        self.batches = batches;
        self
    }

    pub fn last_batch(&self) -> Option<&FeedBatch> {
        self.batches.last()
    }

    pub fn last_seq(&self) -> u64 {
        self.last_batch().map_or(0, |b| b.seq)
    }

    /// Highest log position in the store.
    pub fn max_pos(&self) -> Option<u64> {
        self.store.iter().filter_map(|r| r.event.pos_num()).max()
    }

    /// Normalises and queues rows. Rows whose position was already captured are skipped.
    /// Returns how many were queued.
    pub fn ingest(&mut self, rows: &[AuditLogEntry], late: &[u64], captured_at: &str) -> usize {
        let mut n = 0;
        for raw in rows {
            let event = normalize_audit(raw, &self.chain_id, &self.system, captured_at);
            if !self.seen.insert(event.source.pos.clone()) {
                continue;
            }
            self.pending.push(FeedRecord {
                id: format!("{}-{}", self.chain_id, raw.id),
                raw: raw.clone(),
                event,
                late: late.contains(&raw.id),
            });
            n += 1;
        }
        n
    }

    /// Seals every pending row into the next batch (an empty one if there are none) and anchors it.
    /// Fails without changing anything if the ledger refuses the append.
    pub fn seal(&mut self, ledger: &mut dyn AuditLedger, to_time: String) -> Result<Sealed, LedgerError> {
        let last =
            self.last_batch().map(|b| ChainTip { seq: b.seq, head: &b.head, to_time: &b.to_time, to_pos: &b.to_pos });
        let events: Vec<&EventV2> = self.pending.iter().map(|p| &p.event).collect();
        // Late rows can sit below earlier batches, so the window is the lowest and highest position.
        let nums: Vec<u64> = events.iter().filter_map(|e| e.pos_num()).collect();
        let span = nums.iter().min().zip(nums.iter().max()).map(|(a, b)| (a.to_string(), b.to_string()));
        let plan = plan_seal(&self.chain_id, &self.genesis, &self.chain_start, last, &events, span, to_time);
        let anchor = ledger.append(&self.chain_id, &self.party, plan.anchor_input())?;
        let proofs = plan.proofs();
        let SealPlan { seq, prev_head, head, tree, meta, salts, leaves, from_time, to_time, from_pos, to_pos } = plan;

        let pending = std::mem::take(&mut self.pending);
        let mut rows = Vec::with_capacity(pending.len());
        for (i, (((rec, salt), leaf), proof)) in pending.into_iter().zip(salts).zip(leaves).zip(proofs).enumerate() {
            rows.push(FeedRow {
                id: rec.id,
                chain: self.chain_id.clone(),
                batch_seq: seq,
                index: i,
                event: rec.event,
                raw: rec.raw,
                salt,
                leaf_hash: leaf,
                proof,
                late: rec.late,
            });
        }
        let batch = FeedBatch {
            seq,
            prev_head,
            head,
            merkle_root: tree.hash,
            event_count: meta.event_count,
            from_time,
            to_time,
            from_pos,
            to_pos,
            meta,
            contract_id: anchor.contract_id.clone(),
            update_id: anchor.update_id.clone(),
        };
        self.store.extend(rows.iter().cloned());
        self.batches.push(batch.clone());
        Ok(Sealed { batch, anchor, rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sthira_ledger::SimLedger;

    pub(crate) fn row(id: u64, table: &str) -> AuditLogEntry {
        serde_json::from_value(json!({
            "id": id, "operation": "INSERT", "table_name": table, "record_id": (id * 10).to_string(),
            "new_data": {"id": id * 10, "status": "OK", "duration_ms": 1.5}, "created_at": "2026-11-18T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn seal_dedupes_chains_and_handles_late_rows() {
        let mut ledger = SimLedger::new();
        ledger.add_party("DT");
        ledger.add_party("AUDITOR");
        let mut node = FeedNode::new("DT", "DT", "api.example", "2026-11-18T00:00:00.000000Z".into());
        ledger.create_chain("DT", "DT", &["DT".into(), "AUDITOR".into()], &node.genesis).unwrap();

        let rows: Vec<_> = (1..=5).map(|i| row(i, "app_user")).collect();
        assert_eq!(node.ingest(&rows, &[], "t"), 5);
        assert_eq!(node.ingest(&rows, &[], "t"), 0, "re-read rows are skipped");
        let s1 = node.seal(&mut ledger, "2026-11-18T00:00:10.000000Z".into()).unwrap();
        assert_eq!((s1.batch.seq, s1.batch.event_count, s1.rows.len()), (1, 5, 5));
        assert_eq!((s1.batch.from_pos.as_str(), s1.batch.to_pos.as_str()), ("1", "5"));
        assert_eq!(s1.rows[2].id, "DT-3");
        assert_eq!(s1.batch.prev_head, node.genesis, "the first batch chains onto genesis");

        let s2 = node.seal(&mut ledger, "2026-11-18T00:00:20.000000Z".into()).unwrap();
        assert_eq!(s2.batch.event_count, 0);
        assert_eq!((s2.batch.from_pos.as_str(), s2.batch.to_pos.as_str()), ("5", "5"));
        assert_eq!(s2.batch.prev_head, s1.batch.head);

        node.ingest(&[row(9, "x"), row(7, "x")], &[7], "t");
        let s3 = node.seal(&mut ledger, "2026-11-18T00:00:30.000000Z".into()).unwrap();
        assert_eq!((s3.batch.from_pos.as_str(), s3.batch.to_pos.as_str()), ("7", "9"));
        assert!(s3.rows[1].late && !s3.rows[0].late);
        assert_eq!(ledger.copy("AUDITOR", "DT").unwrap().last_seq, 3);
        assert_eq!(node.max_pos(), Some(9));
    }
}
