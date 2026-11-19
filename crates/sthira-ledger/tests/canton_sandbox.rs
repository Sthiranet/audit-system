//! `CantonLedger` against a running sandbox (`prototype-sandbox/sandbox.sh`).
//!
//! Skipped unless `STHIRA_CANTON_URL` is set, e.g.
//! `STHIRA_CANTON_URL=http://localhost:7575 cargo test -p sthira-ledger --test canton_sandbox -- --nocapture`

use sthira_ledger::canton::{CantonConfig, CantonLedger};
use sthira_ledger::{AnchorInput, AuditLedger};

fn input(seq: u64, prev: &str, head: &str) -> AnchorInput {
    AnchorInput {
        seq,
        prev_head: prev.into(),
        head: head.into(),
        merkle_root: format!("root-{head}"),
        event_count: 3,
        from_pos: "1".into(),
        to_pos: "3".into(),
        from_time: "2026-11-18T00:00:00.000000Z".into(),
        to_time: "2026-11-18T00:00:10.000000Z".into(),
    }
}

#[test]
fn chain_append_fork_and_unilateral_archive_on_canton() {
    let Ok(url) = std::env::var("STHIRA_CANTON_URL") else {
        eprintln!("STHIRA_CANTON_URL not set; skipping");
        return;
    };
    // Fresh party and chain names per run, since the sandbox keeps state until it restarts.
    let tag = format!("{:08x}", rand_tag());
    let (co, au, out, chain) = (format!("Co{tag}"), format!("Au{tag}"), format!("Out{tag}"), format!("T{tag}"));
    let mut l = CantonLedger::connect(CantonConfig { url, user_id: "sthira-test".into() }).unwrap();
    for p in [&co, &au, &out] {
        l.add_party(p);
    }
    l.create_chain(&chain, &co, &[co.clone(), au.clone()], "g0").unwrap();
    assert_eq!(l.copy(&au, &chain).unwrap().last_seq, 0);

    let a1 = l.append(&chain, &co, input(1, "g0", "h1")).unwrap();
    assert_eq!((a1.seq, a1.event_count), (1, 3));
    assert_eq!(a1.signatories.len(), 2);
    l.append(&chain, &co, input(2, "h1", "h2")).unwrap();

    // Fork and gap are refused by the Daml model.
    assert!(l.append(&chain, &co, input(3, "not-h2", "x")).unwrap_err().0.contains("fork"));
    assert!(l.append(&chain, &co, input(5, "h2", "x")).unwrap_err().0.contains("sequence gap"));

    // The auditor's own view, read back from the participant.
    l.refresh().unwrap();
    let copy = l.copy(&au, &chain).unwrap();
    assert_eq!(copy.anchors.iter().map(|a| a.seq).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(copy.last_head, "h2");
    assert!(l.copy(&out, &chain).is_none(), "an outsider sees nothing");

    // Neither party can archive alone.
    let r = l.attempt_archive(&chain, &co, 1).unwrap();
    assert!(!r.ok && r.reason.contains("DAML_AUTHORIZATION_ERROR"), "{}", r.reason);
    let r = l.attempt_archive(&chain, &out, 1).unwrap();
    assert!(!r.ok && r.required.is_empty());
    l.refresh().unwrap();
    assert_eq!(l.copy(&au, &chain).unwrap().anchors.len(), 2);
}

fn rand_tag() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() ^ std::process::id()
}
