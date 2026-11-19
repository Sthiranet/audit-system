//! Completeness from the anchors alone: did the chain anchor every position in the source's log?
//!
//! For a source whose log positions are a dense sequence (the audit API's row ids), each anchor's
//! `from_pos..=to_pos` and `event_count` say how many ids the batch covered and how many it holds.
//! Only ledger values are used, so the party that produced the trail cannot influence the result.
//!
//! A missing id means the source has (or had) a change log row the node never received: deleted at
//! the source before capture, or lost while capture was down. It is a completeness warning, not an
//! integrity failure: the anchored batches can still be intact.
//!
//! Late rows (ids that arrive after later ids were already sealed) make a batch's window reach back
//! below the previous batch. Such a batch is not checked for holes on its own, but the totals stay
//! exact, because every id is anchored at most once.

use serde::{Deserialize, Serialize};
use sthira_ledger::Anchor;

/// Ids between two consecutive non-empty batches that no batch covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosGap {
    pub after_seq: u64,
    pub before_seq: u64,
    pub from: u64,
    pub to: u64,
}

/// A batch whose window has more ids than it holds events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosHole {
    pub seq: u64,
    pub from: u64,
    pub to: u64,
    pub missing: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PositionReport {
    /// First and last anchored position.
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// Events anchored across all batches.
    pub anchored: u64,
    /// Ids in `first..=last` that no anchor holds.
    pub missing: u64,
    pub gaps: Vec<PosGap>,
    pub holes: Vec<PosHole>,
    /// Batches whose window reaches below an earlier batch (late arrivals).
    pub late_batches: Vec<u64>,
}

impl PositionReport {
    pub fn complete(&self) -> bool {
        self.missing == 0
    }
}

/// Checks the anchored log positions of a dense-sequence source for missing ids.
pub fn position_report(anchors: &[Anchor]) -> PositionReport {
    let mut r = PositionReport::default();
    let mut prev: Option<(u64, u64)> = None; // (seq, highest position so far)
    for a in anchors.iter().filter(|a| a.event_count > 0) {
        let (Ok(lo), Ok(hi)) = (a.from_pos.parse::<u64>(), a.to_pos.parse::<u64>()) else {
            continue;
        };
        r.first = Some(r.first.map_or(lo, |f| f.min(lo)));
        r.anchored += a.event_count;
        let window = hi.saturating_sub(lo) + 1;
        match prev {
            Some((_, max)) if lo <= max => r.late_batches.push(a.seq),
            _ => {
                if let Some((pseq, max)) = prev
                    && lo > max + 1
                {
                    r.gaps.push(PosGap { after_seq: pseq, before_seq: a.seq, from: max + 1, to: lo - 1 });
                }
                if a.event_count < window {
                    r.holes.push(PosHole { seq: a.seq, from: lo, to: hi, missing: window - a.event_count });
                }
            }
        }
        let max = prev.map_or(hi, |(_, m)| m.max(hi));
        prev = Some((a.seq, max));
        r.last = Some(max);
    }
    if let (Some(f), Some(l)) = (r.first, r.last) {
        r.missing = (l - f + 1).saturating_sub(r.anchored);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(seq: u64, from: &str, to: &str, n: u64) -> Anchor {
        Anchor {
            contract_id: String::new(),
            template: String::new(),
            chain_id: "DT".into(),
            seq,
            prev_head: String::new(),
            head: String::new(),
            merkle_root: String::new(),
            event_count: n,
            from_pos: from.into(),
            to_pos: to.into(),
            from_time: String::new(),
            to_time: String::new(),
            signatories: vec![],
            update_id: String::new(),
            recorded_at: String::new(),
        }
    }

    #[test]
    fn complete_chain() {
        let r = position_report(&[a(1, "1", "5", 5), a(2, "5", "5", 0), a(3, "6", "9", 4)]);
        assert!(r.complete());
        assert_eq!((r.first, r.last, r.anchored), (Some(1), Some(9), 9));
        assert!(r.gaps.is_empty() && r.holes.is_empty());
    }

    #[test]
    fn gaps_between_and_holes_inside() {
        let r = position_report(&[a(1, "1", "5", 5), a(2, "8", "12", 4)]);
        assert_eq!(r.gaps, vec![PosGap { after_seq: 1, before_seq: 2, from: 6, to: 7 }]);
        assert_eq!(r.holes, vec![PosHole { seq: 2, from: 8, to: 12, missing: 1 }]);
        assert_eq!(r.missing, 3);
    }

    #[test]
    fn late_rows_fill_the_total() {
        // Id 6 was declared missing, then arrived late in batch 3.
        let r = position_report(&[a(1, "1", "5", 5), a(2, "7", "9", 3), a(3, "6", "10", 2)]);
        assert_eq!(r.gaps.len(), 1);
        assert_eq!(r.late_batches, vec![3]);
        assert!(r.complete());
    }
}
