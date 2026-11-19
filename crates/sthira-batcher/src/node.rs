//! The company's node: captured transactions wait in `pending`; [`Node::seal`] closes a batch with
//! the sealing rule ([`crate::plan_seal`]), anchors it on the ledger, and only then moves the events
//! into the node's off-ledger audit store with their salt and leaf hash.
//!
//! Inclusion proofs are not stored: the leaves are on the ledger, so a proof for any record is
//! derived from the anchored leaves whenever one is needed.

use serde::{Deserialize, Serialize};
use sthira_core::{Anchor, AuditLedger, CapturedTx, Event, LedgerError, TrailRecord, genesis_head, leaf_hash};

use crate::{ChainTip, SealPlan, plan_seal};

/// One row of the node's off-ledger audit store (`node/trail.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRow {
    /// `{chain}-{tx}.{row}`, e.g. `DT-118.1`.
    pub id: String,
    pub chain: String,
    pub batch_seq: u64,
    /// Slot in the batch as sealed, 0-based (shown as `L{index + 1}`).
    pub index: usize,
    /// The hashed event.
    pub event: Event,
    pub salt: String,
    pub leaf_hash: String,
}

impl TrailRecord for StoredRow {
    fn record_id(&self) -> &str {
        &self.id
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
    fn recompute_leaf(&self) -> String {
        leaf_hash(&self.salt, &self.event)
    }
}

/// A row of the node's own batch table (`node/batches.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    pub seq: u64,
    pub prev_head: String,
    pub head: String,
    pub merkle_root: String,
    pub event_count: u64,
    pub tx_count: u64,
    pub from_tx: u64,
    pub to_tx: u64,
    pub from_time: String,
    pub to_time: String,
    pub contract_id: String,
    pub update_id: String,
}

/// What one seal produced, for the caller to persist.
#[derive(Debug, Clone)]
pub struct Sealed {
    pub batch: BatchRecord,
    pub anchor: Anchor,
    pub rows: Vec<StoredRow>,
}

/// What [`Node::ingest`] did with a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    Queued,
    /// Queued, but GTID numbers `from..=to` never arrived before it.
    QueuedAfterGap {
        from: u64,
        to: u64,
    },
    /// Already captured; ignored.
    Duplicate,
}

pub struct Node {
    /// The party that submits this chain's anchors.
    pub party: String,
    pub chain_id: String,
    pub genesis: String,
    pub chain_start: String,
    pub pending: Vec<CapturedTx>,
    pub store: Vec<StoredRow>,
    pub batches: Vec<BatchRecord>,
    /// Highest GTID number captured (pending or stored).
    pub last_tx: u64,
}

impl Node {
    pub fn new(party: &str, chain_id: &str, chain_start: String) -> Self {
        Self {
            party: party.to_string(),
            chain_id: chain_id.to_string(),
            genesis: genesis_head(chain_id),
            chain_start,
            pending: Vec::new(),
            store: Vec::new(),
            batches: Vec::new(),
            last_tx: 0,
        }
    }

    /// Rebuilds a node from its persisted store and batch table.
    pub fn restore(mut self, store: Vec<StoredRow>, batches: Vec<BatchRecord>) -> Self {
        self.last_tx = batches.iter().map(|b| b.to_tx).max().unwrap_or(0);
        self.store = store;
        self.batches = batches;
        self
    }

    pub fn last_batch(&self) -> Option<&BatchRecord> {
        self.batches.last()
    }

    pub fn last_seq(&self) -> u64 {
        self.last_batch().map_or(0, |b| b.seq)
    }

    /// Queues one captured transaction. The binlog is ordered, so anything at or below the highest
    /// GTID already captured is a re-read and is skipped.
    pub fn ingest(&mut self, tx: CapturedTx) -> Ingest {
        if tx.tx <= self.last_tx {
            return Ingest::Duplicate;
        }
        let (prev, n) = (self.last_tx, tx.tx);
        self.last_tx = n;
        self.pending.push(tx);
        // The very first transaction of a chain may start anywhere in the log.
        if prev > 0 && n > prev + 1 { Ingest::QueuedAfterGap { from: prev + 1, to: n - 1 } } else { Ingest::Queued }
    }

    fn plan(&self, to_time: String) -> SealPlan {
        let last =
            self.last_batch().map(|b| ChainTip { seq: b.seq, head: &b.head, to_time: &b.to_time, to_tx: b.to_tx });
        let events: Vec<&Event> = self.pending.iter().flat_map(|t| &t.events).collect();
        let txs = match (self.pending.first(), self.pending.last()) {
            (Some(a), Some(b)) => Some((a.tx, b.tx, self.pending.len() as u64)),
            _ => None,
        };
        plan_seal(&self.chain_id, &self.genesis, &self.chain_start, last, &events, txs, to_time)
    }

    /// Seals every pending transaction into the next batch (an empty one if there are none) and
    /// anchors it. If the ledger refuses, nothing changes and the transactions stay pending.
    pub async fn seal<L: AuditLedger>(&mut self, ledger: &L, to_time: String) -> Result<Sealed, LedgerError> {
        let plan = self.plan(to_time);
        let anchor = ledger.append(&self.chain_id, &self.party, &plan.anchor_input()).await?;
        let SealPlan { prev_head, head, tree, meta, salts, leaves } = plan;

        let events = std::mem::take(&mut self.pending).into_iter().flat_map(|t| t.events);
        let rows: Vec<StoredRow> = events
            .zip(salts)
            .zip(leaves)
            .enumerate()
            .map(|(i, ((event, salt), leaf))| StoredRow {
                id: format!("{}-{}.{}", self.chain_id, event.source.tx, event.source.row),
                chain: self.chain_id.clone(),
                batch_seq: meta.seq,
                index: i,
                event,
                salt,
                leaf_hash: leaf,
            })
            .collect();
        let batch = BatchRecord {
            seq: meta.seq,
            prev_head,
            head,
            merkle_root: tree.hash,
            event_count: meta.event_count,
            tx_count: meta.tx_count,
            from_tx: meta.from_tx,
            to_tx: meta.to_tx,
            from_time: meta.from_time,
            to_time: meta.to_time,
            contract_id: anchor.contract_id.clone(),
            update_id: anchor.update_id.clone(),
        };
        self.store.extend(rows.iter().cloned());
        self.batches.push(batch.clone());
        Ok(Sealed { batch, anchor, rows })
    }
}
