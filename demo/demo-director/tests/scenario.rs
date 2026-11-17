//! Runs the narrated story against the backend and checks each claim (port of the old `test/scenario.js`).

use demo_director::views::{TamperMode, TamperRequest};
use demo_director::{World, set_auto};

fn tamper(w: &mut World, held_in: &str, chain: &str, event_id: &str, mode: TamperMode, cover: bool) {
    w.tamper(&TamperRequest { held_in: held_in.into(), chain: chain.into(), event_id: event_id.into(), mode, cover })
        .expect("tamper");
}

#[tokio::test]
async fn the_story() {
    let mut w = World::new();

    // 1. Normal operation: capture, seal, anchor co-signed by S1 + HQ, chained to #3.
    w.capture("S1", Some(10)).unwrap();
    let b = w.seal("S1").unwrap();
    assert_eq!(b.seq, 4);
    assert_eq!(b.batch.event_count, 10);
    assert_eq!(b.anchor.signatories, ["S1", "HQ"]);
    assert_eq!(b.anchor.merkle_root, b.batch.merkle_root);
    assert_eq!(b.anchor.prev_head, w.batch_detail("S1", 3).unwrap().batch.head);
    // Spec v1: the anchor carries source log positions, and they match the batch.
    assert_eq!((&b.anchor.from_pos, &b.anchor.to_pos), (&b.batch.from_pos, &b.batch.to_pos));
    assert_eq!(b.batch.items[0].salt.len(), 64, "32-byte salts");

    // 2. HQ holds copies of all three chains; S1 sees only its own; S2 cannot see S1.
    let hq = w.ledger_view("HQ").unwrap();
    assert_eq!(hq.chains.len(), 3);
    assert_eq!(hq.chains.iter().find(|c| c.chain_id == "S1").unwrap().last_seq, 4);
    assert_eq!(w.ledger_view("S1").unwrap().chains.len(), 1);
    assert!(w.ledger_view("S2").unwrap().chains.iter().all(|c| c.chain_id != "S1"));

    // 3. An empty batch is still anchored (silence is visible).
    let empty = w.seal("S2").unwrap();
    assert_eq!(empty.batch.event_count, 0);

    // 4. HQ verifies S1's pushed trail: all 4 batches match.
    w.push("S1").unwrap();
    let v = w.verify("HQ", "S1").unwrap();
    assert!(v.report.ok, "clean trail should verify");
    assert_eq!(v.report.batches.len(), 4);

    // 5. Edit batch #2 at S1, push to HQ: root #2 mismatches, the leaf is pinpointed, heads #3+ cascade.
    let target = w.trail("sub", "S1").unwrap().rows.into_iter().find(|r| r.batch_seq == 2).unwrap();
    tamper(&mut w, "sub", "S1", &target.id, TamperMode::Edit, false);
    w.push("S1").unwrap();
    let v = w.verify("HQ", "S1").unwrap().report;
    assert!(!v.ok);
    assert_eq!(v.first_bad_seq, Some(2));
    let b2 = v.batches.iter().find(|x| x.seq == 2).unwrap();
    assert!(!b2.root_match && !b2.head_match);
    assert_eq!(b2.leaf_issues[0].event_id, target.id);
    assert!(v.batches.iter().find(|x| x.seq == 1).unwrap().ok);
    let b3 = v.batches.iter().find(|x| x.seq == 3).unwrap();
    assert!(b3.root_match && !b3.head_match, "later heads cascade");
    assert!(!w.state().last_verify["S1"].ok);

    // 6. Tamper with the leaf hash recomputed: the stored hash agrees, but the record still does not
    //    prove into the anchored root, so the leaf is pinpointed all the same.
    w.reset();
    let t2 = w.trail("sub", "S1").unwrap().rows.into_iter().find(|r| r.batch_seq == 3).unwrap();
    tamper(&mut w, "sub", "S1", &t2.id, TamperMode::Edit, true);
    w.push("S1").unwrap();
    let v = w.verify("HQ", "S1").unwrap().report;
    assert_eq!(v.first_bad_seq, Some(3));
    let b3 = v.batches.iter().find(|x| x.seq == 3).unwrap();
    assert_eq!(b3.leaf_issues.len(), 1);
    assert_eq!((b3.leaf_issues[0].event_id.as_str(), b3.leaf_issues[0].index), (t2.id.as_str(), t2.index));
    assert!(b3.leaf_issues[0].covered);

    // 7. Deleted record: event count and root mismatch, missing position reported.
    w.reset();
    let t3 = w.trail("sub", "S2").unwrap().rows.into_iter().find(|r| r.batch_seq == 1).unwrap();
    tamper(&mut w, "sub", "S2", &t3.id, TamperMode::Delete, false);
    w.push("S2").unwrap();
    let v = w.verify("HQ", "S2").unwrap().report;
    let d1 = v.batches.iter().find(|x| x.seq == 1).unwrap();
    assert!(!d1.count_match && !d1.root_match);
    assert_eq!(d1.missing_indexes, [t3.index]);

    // 8. Nobody can rewrite the ledger alone; S2 cannot even see S1's anchors.
    let r = w.rewrite("S1", "S1", 2).unwrap();
    assert!(!r.ok);
    assert_eq!(r.missing, ["HQ"]);
    assert_eq!(w.rewrite("HQ", "S1", 2).unwrap().missing, ["S1"]);
    assert!(w.rewrite("S2", "S1", 2).unwrap().required.is_empty());

    // 9. Auditor joins: new anchors carry 3 signatures; the auditor catches an edit made to HQ's copy.
    assert!(w.verify("AUDITOR", "S1").is_err());
    w.add_auditor().unwrap();
    w.capture("S1", Some(3)).unwrap();
    let b5 = w.seal("S1").unwrap();
    assert_eq!(b5.anchor.signatories, ["S1", "HQ", "AUDITOR"]);
    assert_eq!(w.ledger_view("AUDITOR").unwrap().chains.len(), 3);
    w.push("S1").unwrap();
    assert!(w.verify("AUDITOR", "S1").unwrap().report.ok);
    let hq_row = w.trail("hq", "S1").unwrap().rows.remove(0);
    tamper(&mut w, "hq", "S1", &hq_row.id, TamperMode::Edit, false);
    assert!(!w.verify("AUDITOR", "S1").unwrap().report.ok);
    assert_eq!(w.rewrite("HQ", "S1", b5.seq as i64).unwrap().missing, ["S1", "AUDITOR"]);

    // 10. Auto-anchor toggles on and off.
    let shared = World::shared();
    assert!(set_auto(&shared, true).auto);
    assert!(!set_auto(&shared, false).auto);
}

/// Several manipulations at once, in one batch and across batches: every one is reported, each at
/// the leaf slot it had when the batch was sealed.
#[test]
fn every_manipulation_is_pinpointed() {
    let mut w = World::new();
    w.capture("S1", Some(8)).unwrap();
    let seq = w.seal("S1").unwrap().seq;
    let rows = w.trail("sub", "S1").unwrap().rows;
    let slot = |i: usize| rows.iter().find(|r| r.batch_seq == seq && r.index == i).unwrap().id.clone();
    let earlier = rows.iter().find(|r| r.batch_seq == 2).unwrap().clone();

    // Delete L2, alter L1, alter L4 and L7 (L7 with its stored hash rewritten), alter one record of batch #2.
    tamper(&mut w, "sub", "S1", &slot(1), TamperMode::Delete, false);
    tamper(&mut w, "sub", "S1", &slot(0), TamperMode::Edit, false);
    tamper(&mut w, "sub", "S1", &slot(3), TamperMode::Edit, false);
    tamper(&mut w, "sub", "S1", &slot(6), TamperMode::Edit, true);
    tamper(&mut w, "sub", "S1", &earlier.id, TamperMode::Edit, false);
    // Altering the same record again must not put the original value back.
    tamper(&mut w, "sub", "S1", &slot(3), TamperMode::Edit, false);
    w.push("S1").unwrap();

    let v = w.verify("HQ", "S1").unwrap().report;
    assert_eq!(v.changed_seqs, [2, seq]);
    assert_eq!((v.altered_count, v.deleted_count), (4, 1));
    let b = v.batches.iter().find(|x| x.seq == seq).unwrap();
    assert_eq!(b.missing_indexes, [1]);
    let found: Vec<(usize, usize, &str, bool)> =
        b.leaf_issues.iter().map(|x| (x.index, x.position, x.event_id.as_str(), x.covered)).collect();
    let (s0, s3, s6) = (slot(0), slot(3), slot(6));
    assert_eq!(found, [(0, 0, s0.as_str(), false), (3, 2, s3.as_str(), false), (6, 5, s6.as_str(), true)]);
    // The tree is drawn over the 7 records left; each leaf keeps its original slot number.
    assert_eq!(b.leaves.iter().map(|l| l.index).collect::<Vec<_>>(), [0, 2, 3, 4, 5, 6, 7]);
    let b2 = v.batches.iter().find(|x| x.seq == 2).unwrap();
    assert_eq!(b2.leaf_issues.len(), 1);
    assert_eq!((b2.leaf_issues[0].event_id.as_str(), b2.leaf_issues[0].index), (earlier.id.as_str(), earlier.index));
    // Batches in between kept their contents; only their heads moved.
    let b3 = v.batches.iter().find(|x| x.seq == 3).unwrap();
    assert!(b3.root_match && !b3.head_match && b3.leaf_issues.is_empty() && b3.missing_indexes.is_empty());
}

#[test]
fn errors_are_reported_with_status() {
    let mut w = World::new();
    assert_eq!(w.seal("S9").unwrap_err().status, 404);
    assert_eq!(w.trail("elsewhere", "S1").unwrap_err().status, 400);
    assert_eq!(w.verify("S1", "S1").unwrap_err().status, 400);
    assert_eq!(w.batch_detail("S1", 99).unwrap_err().status, 404);
    assert_eq!(w.rewrite("S1", "S1", 99).unwrap_err().status, 400);
}

#[test]
fn auto_tick_anchors_every_chain() {
    let mut w = World::new();
    w.auto_tick();
    assert!(w.state().subs.iter().all(|s| s.last_seq == 4 && s.pending == 0));
}
