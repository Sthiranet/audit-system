//! End to end on the Canton sandbox, through the binary: generate → run → verify → tamper → verify →
//! untamper → verify → resume, plus a chain that lost GTIDs before capture.
//!
//! Run with `make test` (it boots the sandbox), or by hand with a sandbox on :7575:
//! `cargo test -p sthira-node -- --include-ignored`

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn url() -> String {
    std::env::var("STHIRA_CANTON_URL").unwrap_or_else(|_| "http://localhost:7575".into())
}

fn node(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sthira-node"))
        .current_dir(dir)
        .env("STHIRA_LEDGER_URL", url())
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .expect("sthira-node runs")
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let o = node(dir, args);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{args:?} failed:\n{out}\n{}", String::from_utf8_lossy(&o.stderr));
    out
}

fn workdir(name: &str) -> (PathBuf, String) {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
    let tag = format!("{:06x}", (n ^ std::process::id()) & 0xffffff);
    let d = std::env::temp_dir().join(format!("sthira-e2e-{name}-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    (d, format!("E{tag}"))
}

fn first_record_of_batch(trail: &str, seq: u64, op: &str) -> String {
    trail
        .lines()
        .find(|l| l.starts_with(&format!("#{seq} ")) && l.split_whitespace().nth(3) == Some(op))
        .and_then(|l| l.split_whitespace().nth(2))
        .unwrap_or_else(|| panic!("no {op} record in batch {seq}:\n{trail}"))
        .to_string()
}

#[test]
#[ignore = "needs the Canton sandbox (make test)"]
fn tamper_is_pinpointed_and_restored_and_the_chain_resumes() {
    let (d, chain) = workdir("tamper");
    ok(&d, &["gen", "--tx", "30"]);
    let run = ok(&d, &["run", "--chain", &chain, "--interval", "1", "--rate", "0", "--batches", "3"]);
    assert!(run.contains("anchored"), "{run}");
    assert!(ok(&d, &["verify"]).contains("PASS"));

    // An insider edits an update and covers it by rewriting the stored leaf hash; another deletes a row.
    let trail = ok(&d, &["trail", "--last", "1000"]);
    let edited = first_record_of_batch(&trail, 1, "update");
    let deleted = first_record_of_batch(&trail, 1, "insert");
    ok(&d, &["tamper", "edit", &edited, "--cover"]);
    ok(&d, &["tamper", "delete", &deleted]);
    let v = node(&d, &["verify"]);
    let out = String::from_utf8_lossy(&v.stdout);
    assert_eq!(v.status.code(), Some(1), "{out}");
    assert!(out.contains("ALTERED  L") && out.contains(&edited), "the edit is pinpointed:\n{out}");
    assert!(out.contains("still caught"), "{out}");
    assert!(out.contains("DELETED"), "{out}");
    let p = node(&d, &["proof", &edited]);
    assert_eq!(p.status.code(), Some(1), "the proof of an altered record fails");

    ok(&d, &["untamper"]);
    assert!(ok(&d, &["verify"]).contains("PASS"));

    // The ledger refuses a unilateral rewrite.
    assert!(ok(&d, &["rewrite", "1"]).contains("refused"));

    // A restarted node resumes the same chain on Canton.
    let resumed = ok(&d, &["run", "--interval", "1", "--rate", "0", "--batches", "1"]);
    assert!(resumed.contains("resumed") && resumed.contains("batch #4"), "{resumed}");
    assert!(ok(&d, &["verify"]).contains("PASS"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
#[ignore = "needs the Canton sandbox (make test)"]
fn gtids_lost_before_capture_show_on_the_ledger() {
    let (d, chain) = workdir("gap");
    ok(&d, &["gen", "--tx", "20"]);
    let run = ok(
        &d,
        &["run", "--chain", &chain, "--interval", "1", "--rate", "0", "--batches", "2", "--skip-gtids", "7,8,9"],
    );
    assert!(run.contains("GTIDs 7–9 never reached"), "{run}");
    let v = ok(&d, &["verify", "--json"]);
    let j: serde_json::Value = serde_json::from_str(&v).unwrap();
    assert_eq!(j["report"]["ok"], true, "a gap is a completeness warning, not an integrity failure");
    assert_eq!(j["positions"]["missing"], 3);
    let _ = std::fs::remove_dir_all(&d);
}
