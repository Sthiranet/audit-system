//! `CantonLedger` against a running sandbox. Run with `make test` (it boots the sandbox), or by hand:
//! `STHIRA_CANTON_URL=http://localhost:7575 cargo test -p sthira-ledger -- --include-ignored`

use sthira_core::{AnchorInput, AuditLedger, ChainSpec};
use sthira_ledger::{CantonConfig, CantonLedger};

fn url() -> String {
    std::env::var("STHIRA_CANTON_URL").unwrap_or_else(|_| "http://localhost:7575".into())
}

fn input(seq: u64, prev: &str, head: &str) -> AnchorInput {
    AnchorInput {
        seq,
        prev_head: prev.into(),
        head: head.into(),
        merkle_root: format!("root-{head}"),
        leaves: vec![format!("{head}-l1"), format!("{head}-l2"), format!("{head}-l3")],
        event_count: 3,
        tx_count: 2,
        from_tx: seq * 2 - 1,
        to_tx: seq * 2,
        from_time: "2026-11-20T00:00:00.000000Z".into(),
        to_time: "2026-11-20T00:00:10.000000Z".into(),
    }
}

fn tag() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
    format!("{:08x}", n ^ std::process::id())
}

#[tokio::test]
#[ignore = "needs the Canton sandbox (make test)"]
async fn chain_append_with_leaves_fork_mismatch_and_unilateral_archive() {
    // Fresh party and chain names per run, since the sandbox keeps state until it restarts.
    let t = tag();
    let (co, au, out, chain) = (format!("Co{t}"), format!("Au{t}"), format!("Out{t}"), format!("T{t}"));
    let mut l = CantonLedger::connect(CantonConfig { url: url(), user_id: "sthira-test".into() }).await.unwrap();
    l.ensure_parties(&[co.clone(), au.clone(), out.clone()]).await.unwrap();
    let spec = ChainSpec {
        chain_id: chain.clone(),
        submitter: co.clone(),
        members: vec![co.clone(), au.clone()],
        genesis_head: "g0".into(),
    };
    l.create_chain(&spec).await.unwrap();
    assert_eq!(l.chain_view(&au, &chain).await.unwrap().unwrap().last_seq, 0);

    let a1 = l.append(&chain, &co, &input(1, "g0", "h1")).await.unwrap();
    assert_eq!((a1.seq, a1.event_count, a1.tx_count, a1.leaves.len()), (1, 3, 2, 3));
    assert_eq!(a1.signatories.len(), 2);
    l.append(&chain, &co, &input(2, "h1", "h2")).await.unwrap();

    // Fork, gap and inconsistent leaves are refused by the Daml model.
    let err = |r: Result<_, sthira_core::LedgerError>| r.unwrap_err().0;
    assert!(err(l.append(&chain, &co, &input(3, "not-h2", "x")).await).contains("fork"));
    assert!(err(l.append(&chain, &co, &input(5, "h2", "x")).await).contains("sequence gap"));
    let mut short = input(3, "h2", "h3");
    short.leaves.pop();
    assert!(err(l.append(&chain, &co, &short).await).contains("leaf count"));
    // The auditor cannot append.
    assert!(l.append(&chain, &au, &input(3, "h2", "h3")).await.is_err());

    // The auditor's own copy, read back from the participant, holds the leaves.
    let copy = l.chain_view(&au, &chain).await.unwrap().unwrap();
    assert_eq!(copy.anchors.iter().map(|a| a.seq).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(copy.anchors[1].leaves, input(2, "h1", "h2").leaves);
    assert_eq!(copy.last_head, "h2");
    assert!(l.chain_view(&out, &chain).await.unwrap().is_none(), "an outsider sees nothing");

    // Neither party can archive alone.
    let r = l.attempt_archive(&chain, &co, 1).await.unwrap();
    assert!(!r.ok && r.reason.contains("DAML_AUTHORIZATION_ERROR"), "{}", r.reason);
    assert_eq!(r.missing, vec![au.clone()]);
    let r = l.attempt_archive(&chain, &out, 1).await.unwrap();
    assert!(!r.ok && r.required.is_empty());
    assert_eq!(l.chain_view(&au, &chain).await.unwrap().unwrap().anchors.len(), 2);
}
