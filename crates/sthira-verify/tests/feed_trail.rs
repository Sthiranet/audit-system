//! End to end on the prototype path: audit-API rows → FeedNode batches anchored on the ledger →
//! independent verification of the trail, and every insider action the CLI offers is caught.

use sthira_batcher::feed_node::{FeedNode, FeedRow};
use sthira_core::leaf_hash;
use sthira_ledger::{Anchor, AuditLedger, SimLedger};
use sthira_profiles::audit_api::{
    AuditCursor, AuditEnvelope, AuditFeed, AuditLogEntry, AuditPage, FeedError, MemoryFeed, reattribute_audit,
    tamper_audit,
};
use sthira_verify::{position_report, verify_records};

fn fixture() -> Vec<AuditLogEntry> {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../sthira-profiles/tests/fixtures/audit_api/page.json");
    serde_json::from_str::<AuditEnvelope>(&std::fs::read_to_string(p).unwrap()).unwrap().response.results
}

struct Hide(MemoryFeed, Vec<u64>);
impl AuditFeed for Hide {
    fn page(&mut self, n: u32) -> Result<AuditPage, FeedError> {
        let mut p = self.0.page(n)?;
        p.results.retain(|r| !self.1.contains(&r.id));
        Ok(p)
    }
}

/// Feeds the fixture through the cursor in four ticks of five rows and returns the trail and anchors.
fn build(hide: Vec<u64>) -> (FeedNode, Vec<FeedRow>, Vec<Anchor>) {
    let all = fixture();
    let mut ledger = SimLedger::new();
    ledger.add_party("DREAMTALK");
    ledger.add_party("AUDITOR");
    let mut node = FeedNode::new("DREAMTALK", "DT", "api.example", "2026-11-18T05:00:00.000000Z".into());
    ledger.create_chain("DT", "DREAMTALK", &["DREAMTALK".into(), "AUDITOR".into()], &node.genesis).unwrap();
    let mut feed = Hide(MemoryFeed::new(Vec::new(), 7), hide);
    let mut cursor = AuditCursor::after(5000);
    cursor.grace_ms = 0;
    let mut sorted = all.clone();
    sorted.sort_by_key(|r| r.id);
    let mut rows = Vec::new();
    for (tick, chunk) in sorted.chunks(5).enumerate() {
        feed.0.rows.extend(chunk.iter().cloned());
        let p = cursor.poll(&mut feed, tick as i64 * 10_000).unwrap();
        node.ingest(&p.rows, &p.late, "2026-11-18T06:00:00.000000Z");
        let s = node.seal(&mut ledger, format!("2026-11-18T06:00:{:02}.000000Z", tick * 10)).unwrap();
        rows.extend(s.rows);
    }
    let anchors = ledger.copy("AUDITOR", "DT").unwrap().anchors.clone();
    (node, rows, anchors)
}

#[test]
fn an_untouched_trail_verifies_and_is_complete() {
    let (node, rows, anchors) = build(vec![]);
    assert_eq!(rows.len(), 20);
    let r = verify_records("DT", &node.genesis, &anchors, &rows);
    assert!(r.ok, "{:?}", r.failed_seqs);
    let pos = position_report(&anchors);
    assert!(pos.complete());
    assert_eq!((pos.first, pos.last, pos.anchored), (Some(5001), Some(5020), 20));
}

#[test]
fn edits_covered_edits_reattribution_and_deletion_are_pinpointed() {
    let (node, mut rows, anchors) = build(vec![]);
    // Batch 2, slot 1: content edit. Batch 3, slot 2: edit with the stored leaf rewritten.
    let i = rows.iter().position(|r| r.batch_seq == 2 && r.index == 0).unwrap();
    tamper_audit(&mut rows[i].event);
    let j = rows.iter().position(|r| r.batch_seq == 3 && r.index == 1).unwrap();
    tamper_audit(&mut rows[j].event);
    rows[j].leaf_hash = leaf_hash(&rows[j].salt, &rows[j].event);
    // Batch 3, slot 4: blame someone else.
    let k = rows.iter().position(|r| r.batch_seq == 3 && r.index == 3).unwrap();
    reattribute_audit(&mut rows[k].event);
    // Batch 4, slot 3: deleted.
    let d = rows.iter().position(|r| r.batch_seq == 4 && r.index == 2).unwrap();
    rows.remove(d);

    let r = verify_records("DT", &node.genesis, &anchors, &rows);
    assert!(!r.ok);
    assert_eq!(r.changed_seqs, vec![2, 3, 4]);
    assert_eq!((r.altered_count, r.deleted_count), (3, 1));
    let b3 = &r.batches[2];
    let mut slots: Vec<(usize, bool)> = b3.leaf_issues.iter().map(|l| (l.index, l.covered)).collect();
    slots.sort();
    assert_eq!(slots, vec![(1, true), (3, false)]);
    assert_eq!(r.batches[3].missing_indexes, vec![2]);
    // Batch 1 untouched.
    assert!(r.batches[0].ok);
}

#[test]
fn ids_the_source_never_delivered_show_as_a_completeness_gap() {
    let (node, rows, anchors) = build(vec![5008, 5009]);
    assert_eq!(rows.len(), 18);
    // The trail itself is intact: the missing ids were never anchored.
    assert!(verify_records("DT", &node.genesis, &anchors, &rows).ok);
    let pos = position_report(&anchors);
    assert_eq!(pos.missing, 2);
    assert_eq!(pos.gaps.len() + pos.holes.len(), 1);
}
