//! Completeness from the anchors alone: did the chain anchor every transaction in the source's log?
//!
//! GTID transaction numbers are dense per server, and the binary log is read in order. Each anchor's
//! `from_tx..=to_tx` and `tx_count` say how many transactions the batch spanned and how many it holds.
//! Only ledger values are used, so the party that produced the trail cannot influence the result.
//!
//! A missing number means the source committed a transaction the node never received: a purged or
//! edited binary log, a session with `sql_log_bin=0`, or capture that was down. It is a completeness
//! warning, not an integrity failure: the anchored batches can still be intact.

use serde::{Deserialize, Serialize};
use sthira_core::Anchor;

/// Transactions between two consecutive non-empty batches that no batch covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxGap {
    pub after_seq: u64,
    pub before_seq: u64,
    pub from: u64,
    pub to: u64,
}

/// A batch whose window spans more transactions than it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxHole {
    pub seq: u64,
    pub from: u64,
    pub to: u64,
    pub missing: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PositionReport {
    /// First and last anchored GTID number.
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// Transactions anchored across all batches.
    pub anchored: u64,
    /// GTID numbers in `first..=last` that no anchor holds.
    pub missing: u64,
    pub gaps: Vec<TxGap>,
    pub holes: Vec<TxHole>,
}

impl PositionReport {
    pub fn complete(&self) -> bool {
        self.missing == 0
    }
}

/// Checks the anchored GTID windows for missing transactions.
pub fn position_report(anchors: &[Anchor]) -> PositionReport {
    let mut r = PositionReport::default();
    let mut prev: Option<(u64, u64)> = None; // (seq, to_tx)
    for a in anchors.iter().filter(|a| a.tx_count > 0) {
        let (lo, hi) = (a.from_tx, a.to_tx);
        r.first.get_or_insert(lo);
        r.anchored += a.tx_count;
        if let Some((pseq, ptx)) = prev
            && lo > ptx + 1
        {
            r.gaps.push(TxGap { after_seq: pseq, before_seq: a.seq, from: ptx + 1, to: lo - 1 });
        }
        let window = hi.saturating_sub(lo) + 1;
        if a.tx_count < window {
            r.holes.push(TxHole { seq: a.seq, from: lo, to: hi, missing: window - a.tx_count });
        }
        prev = Some((a.seq, hi));
        r.last = Some(hi);
    }
    if let (Some(f), Some(l)) = (r.first, r.last) {
        r.missing = (l - f + 1).saturating_sub(r.anchored);
    }
    r
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn a(seq: u64, from: u64, to: u64, txs: u64) -> Anchor {
        Anchor {
            contract_id: String::new(),
            chain_id: "DT".into(),
            seq,
            prev_head: String::new(),
            head: String::new(),
            merkle_root: String::new(),
            leaves: vec![],
            event_count: txs,
            tx_count: txs,
            from_tx: from,
            to_tx: to,
            from_time: String::new(),
            to_time: String::new(),
            signatories: vec![],
            update_id: String::new(),
            recorded_at: String::new(),
        }
    }

    #[test]
    fn complete_chain() {
        let r = position_report(&[a(1, 1, 5, 5), a(2, 5, 5, 0), a(3, 6, 9, 4)]);
        assert!(r.complete());
        assert_eq!((r.first, r.last, r.anchored), (Some(1), Some(9), 9));
        assert!(r.gaps.is_empty() && r.holes.is_empty());
    }

    #[test]
    fn gaps_between_and_holes_inside() {
        let r = position_report(&[a(1, 1, 5, 5), a(2, 8, 12, 4)]);
        assert_eq!(r.gaps, vec![TxGap { after_seq: 1, before_seq: 2, from: 6, to: 7 }]);
        assert_eq!(r.holes, vec![TxHole { seq: 2, from: 8, to: 12, missing: 1 }]);
        assert_eq!(r.missing, 3);
    }
}
