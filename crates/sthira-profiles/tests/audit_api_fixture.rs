//! The synthetic fixture page (every table and operation shape seen on the live endpoint) parses,
//! and every row normalises into a well-formed spec v2 event.

use sthira_core::{Op, leaf_hash};
use sthira_profiles::audit_api::{AuditEnvelope, audit_flags, describe_audit, normalize_audit};

fn page() -> AuditEnvelope {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/audit_api/page.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn every_fixture_row_normalises() {
    let rows = page().response.results;
    assert_eq!(rows.len(), 20);
    let mut ops = std::collections::BTreeSet::new();
    for r in &rows {
        let ev = normalize_audit(r, "DT", "api.example", "2026-11-18T06:00:00.000000Z");
        assert_eq!(ev.v, 2);
        assert_eq!(ev.source.pos, r.id.to_string());
        assert!(!ev.key.id.is_empty(), "row {} has a key", r.id);
        assert!(ev.committed_at.ends_with('Z') && ev.committed_at.len() == 27, "{}", ev.committed_at);
        match ev.op {
            Op::Insert => assert!(ev.before.is_none() && ev.after.is_some()),
            Op::Delete => assert!(ev.after.is_none() && ev.before.is_some()),
            Op::Update => assert!(ev.after.is_some()),
        }
        assert!(!describe_audit(&ev).label.is_empty());
        let _ = audit_flags(&ev);
        // Hashable (floats included) and stable.
        let s = "00".repeat(32);
        assert_eq!(leaf_hash(&s, &ev), leaf_hash(&s, &ev.clone()));
        ops.insert(format!("{:?}", ev.op));
    }
    assert_eq!(ops.len(), 3);
}

#[test]
fn the_privilege_escalation_in_the_fixture_is_flagged() {
    let rows = page().response.results;
    let flagged: Vec<u64> = rows
        .iter()
        .filter(|r| {
            let ev = normalize_audit(r, "DT", "h", "t");
            r.table_name == "app_user" && audit_flags(&ev).contains(&"privilege-change".to_string())
        })
        .map(|r| r.id)
        .collect();
    assert_eq!(flagged.len(), 1);
}
