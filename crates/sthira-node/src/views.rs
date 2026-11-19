//! Read-only commands, and the ledger-side `rewrite` attempt.
//!
//! Anything that judges integrity (`ledger`, `proof`, `check-proof`, `verify`) reads the anchors
//! from the AUDITOR's own ledger copy on Canton, never from the company's files. `status` and
//! `trail` only describe the node's local state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use sthira_batcher::StoredRow;
use sthira_batcher::persist::{DataDir, read_json, write_json};
use sthira_connector::describe::describe;
use sthira_core::{Anchor, AuditLedger, Event, canonical, leaf_hash, verify_inclusion};
use sthira_verify::{PositionReport, VerifyReport, position_report, proof_from_anchor, verify_records};

use crate::{Paint, Res, auditor_of, clock, ledger, need_chain, short};

/// The trail rows that belong to batches the node has receipts for (rows of a batch being
/// written right now are left out).
fn sealed_rows(dir: &DataDir, path: Option<&Path>, last_seq: u64) -> Res<(Vec<StoredRow>, usize)> {
    let (mut rows, bad) = dir.load_trail_from(path).map_err(|e| e.to_string())?;
    rows.retain(|r| r.batch_seq <= last_seq);
    Ok((rows, bad))
}

fn last_seq(anchors: &[Anchor]) -> u64 {
    anchors.last().map_or(0, |a| a.seq)
}

// ---------------------------------------------------------------------------------------------
// status / ledger / trail

pub fn status(dir: &DataDir, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let receipts = dir.load_receipts().map_err(|e| e.to_string())?;
    let (rows, _) = sealed_rows(dir, None, last_seq(&receipts))?;
    let cursor = dir.load_cursor().map_err(|e| e.to_string())?;
    let tl = dir.load_tamper_log().map_err(|e| e.to_string())?;
    let pos = position_report(&receipts);

    println!("{}", p.bold(format!("Chain {} · source {}", info.chain_id, info.source)));
    println!("  members    {} (submitter {})", info.members.join(", "), info.submitter);
    println!("  ledger     Canton at {} {}", info.ledger_url, p.dim("(counts below are the node's local receipts)"));
    println!("  started    {}", info.chain_start);
    match receipts.last() {
        Some(a) => println!(
            "  batches    {} anchored · last #{} at {} · head {}",
            receipts.len(),
            a.seq,
            a.to_time,
            short(&a.head)
        ),
        None => println!("  batches    none yet"),
    }
    let flagged = rows.iter().filter(|r| describe(&r.event).flagged).count();
    println!("  records    {} row changes ({} flagged for attention)", rows.len(), flagged);
    if let (Some(f), Some(l)) = (pos.first, pos.last) {
        println!(
            "  gtids      {f}–{l} · {}",
            if pos.complete() { p.ok("none missing") } else { p.warn(format!("{} missing", pos.missing)) }
        );
    }
    println!("  cursor     next GTID {}", cursor.last_tx + 1);
    let mut tables: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &rows {
        *tables.entry(r.event.source.table.as_str()).or_default() += 1;
    }
    let mut tv: Vec<_> = tables.into_iter().collect();
    tv.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!("  tables     {}", tv.iter().map(|(t, n)| format!("{t} {n}")).collect::<Vec<_>>().join(" · "));
    if !tl.is_empty() {
        println!(
            "  {}",
            p.warn(format!("tampered   {} record(s) altered by the presenter; `untamper` restores them", tl.len()))
        );
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn ledger(dir: &DataDir, last: usize, leaves: bool, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let auditor = auditor_of(&info);
    let anchors = ledger::anchors_as(&info, auditor).await?;
    println!(
        "{}",
        p.bold(format!(
            "Ledger · chain {} · {} anchors · each signed by {} · read from {auditor}'s copy on Canton",
            info.chain_id,
            anchors.len(),
            info.members.join(" + ")
        ))
    );
    println!("{}", p.dim("  Only fingerprints: one leaf hash per record, the root, the head. No record content."));
    println!(
        "{}",
        p.dim(format!(
            "  {:<5} {:<9} {:>4} {:>5}  {:<9}  {:<12} {:<12} {:<12}",
            "seq", "window", "tx", "rows", "gtids", "prev head", "root", "head"
        ))
    );
    for a in anchors.iter().skip(anchors.len().saturating_sub(last)) {
        let span = if a.tx_count == 0 { "–".to_string() } else { format!("{}–{}", a.from_tx, a.to_tx) };
        println!(
            "  #{:<4} {} {:>4} {:>5}  {:<9}  {} {} {}",
            a.seq,
            clock(&a.to_time),
            a.tx_count,
            a.event_count,
            span,
            short(&a.prev_head),
            short(&a.merkle_root),
            short(&a.head)
        );
        if leaves {
            for (i, l) in a.leaves.iter().enumerate() {
                println!("        {} {l}", p.dim(format!("L{:<3}", i + 1)));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn trail(dir: &DataDir, batch: Option<u64>, flagged: bool, last: usize, p: Paint) -> Res<ExitCode> {
    need_chain(dir)?;
    let receipts = dir.load_receipts().map_err(|e| e.to_string())?;
    let (rows, _) = sealed_rows(dir, None, last_seq(&receipts))?;
    let shown: Vec<(&StoredRow, _)> = rows
        .iter()
        .filter(|r| batch.is_none_or(|b| r.batch_seq == b))
        .map(|r| (r, describe(&r.event)))
        .filter(|(_, d)| !flagged || d.flagged)
        .collect();
    for (r, d) in shown.iter().skip(shown.len().saturating_sub(last)) {
        println!(
            "#{:<4} L{:<3} {:<11} {:<6} {:<9} {}  {} {}",
            r.batch_seq,
            r.index + 1,
            r.id,
            r.event.op.as_str(),
            d.kind,
            d.text,
            p.dim(format!("leaf {}", short(&r.leaf_hash))),
            if d.flagged { p.warn("⚑") } else { String::new() }
        );
    }
    if shown.is_empty() {
        println!("{}", p.dim("no records match"));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------------------------
// proof / check-proof

/// Everything a third party needs to check one record against the ledger, and nothing about any
/// other record.
#[derive(Serialize, Deserialize)]
struct ProofBundle {
    chain: String,
    record_id: String,
    batch_seq: u64,
    index: usize,
    batch_size: u64,
    salt: String,
    event: Event,
    proof: Vec<String>,
    /// The root as the exporting node claims it; the checker uses its own ledger copy instead.
    claimed_root: String,
}

fn find_rows<'a>(rows: &'a [StoredRow], target: &str) -> Vec<&'a StoredRow> {
    match target.split_once(':') {
        Some((table, key)) => {
            rows.iter().filter(|r| r.event.source.table == table && r.event.key_label() == key).collect()
        }
        None => rows.iter().filter(|r| r.id == target).collect(),
    }
}

/// Checks one record two ways: its leaf equals the anchored leaf in its slot, and the inclusion path
/// derived from the anchored leaves leads to the anchored root.
fn check_record(leaf: &str, index: usize, proof: &[String], a: &Anchor) -> (bool, bool) {
    let same_leaf = a.leaves.get(index).is_some_and(|l| l == leaf);
    let included = verify_inclusion(leaf, index, a.leaves.len(), proof, &a.merkle_root);
    (same_leaf, included)
}

pub async fn proof(dir: &DataDir, target: &str, show_event: bool, export: Option<&Path>, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let anchors = ledger::anchors_as(&info, auditor_of(&info)).await?;
    let by_seq: HashMap<u64, &Anchor> = anchors.iter().map(|a| (a.seq, a)).collect();
    let (rows, _) = sealed_rows(dir, None, last_seq(&anchors))?;
    let found = find_rows(&rows, target);
    if found.is_empty() {
        return Err(format!(
            "no captured record matches {target} (use a record id like {}-12.0, or table:key like invoices:id=1042)",
            info.chain_id
        ));
    }
    let mut all_ok = true;
    for r in &found {
        let a = by_seq[&r.batch_seq];
        let leaf = leaf_hash(&r.salt, &r.event);
        let path = proof_from_anchor(a, r.index).unwrap_or_default();
        let (same_leaf, included) = check_record(&leaf, r.index, &path, a);
        all_ok &= same_leaf && included;
        let d = describe(&r.event);
        println!("{}", p.bold(format!("{} · {} · {}", r.id, d.kind, d.text)));
        println!("  committed  {} in GTID {}", r.event.committed_at, r.event.source.gtid);
        println!("  batch      #{} slot L{} of {}", r.batch_seq, r.index + 1, a.event_count);
        println!("  leaf       {}  = H(00 ‖ salt ‖ canonical event)", leaf);
        println!(
            "  on ledger  {}  {}",
            a.leaves.get(r.index).map_or("(no such slot)", String::as_str),
            p.mark(same_leaf)
        );
        println!("  path       {} sibling hashes, derived from the anchored leaves", path.len());
        for (i, h) in path.iter().enumerate() {
            println!("    {}  {}", p.dim(format!("{i:>2}")), h);
        }
        println!(
            "  root       {}  (anchor {}, signed {})",
            a.merkle_root,
            short(&a.contract_id),
            a.signatories.join("+")
        );
        println!(
            "  included   {}",
            if same_leaf && included {
                p.ok("✓ this exact record is the one anchored in that slot")
            } else {
                p.bad("✗ this record is NOT the one anchored: it changed after capture")
            }
        );
        if show_event {
            println!("  event      {}", canonical(&r.event));
        }
        println!();
    }
    if let Some(out) = export {
        let r = found[0];
        let a = by_seq[&r.batch_seq];
        let path = proof_from_anchor(a, r.index).unwrap_or_default();
        let n = path.len();
        let b = ProofBundle {
            chain: info.chain_id.clone(),
            record_id: r.id.clone(),
            batch_seq: r.batch_seq,
            index: r.index,
            batch_size: a.event_count,
            salt: r.salt.clone(),
            event: r.event.clone(),
            proof: path,
            claimed_root: a.merkle_root.clone(),
        };
        write_json(out, &b).map_err(|e| e.to_string())?;
        println!(
            "Proof bundle for {} written to {} (one record, its salt and {n} sibling hashes; nothing else).",
            r.id,
            out.display()
        );
    }
    Ok(if all_ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

pub async fn check_proof(dir: &DataDir, bundle: &Path, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let b: ProofBundle = read_json(bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    let auditor = auditor_of(&info);
    let anchors = ledger::anchors_as(&info, auditor).await?;
    let Some(a) = anchors.iter().find(|a| a.seq == b.batch_seq && a.chain_id == b.chain) else {
        println!("{} the ledger has no anchor #{} on chain {}", p.bad("✗"), b.batch_seq, b.chain);
        return Ok(ExitCode::from(1));
    };
    let leaf = leaf_hash(&b.salt, &b.event);
    let (same_leaf, included) = check_record(&leaf, b.index, &b.proof, a);
    let d = describe(&b.event);
    println!("{} · {} · {}", b.record_id, d.kind, d.text);
    println!(
        "  checked against anchor #{} from {auditor}'s ledger copy (root {}), not the root in the bundle",
        a.seq,
        short(&a.merkle_root)
    );
    println!("  leaf in slot L{}  {}   path to root  {}", b.index + 1, p.mark(same_leaf), p.mark(included));
    if same_leaf && included {
        println!(
            "  {}",
            p.ok(format!("✓ this exact record was anchored in that batch, co-signed by {}", a.signatories.join(" + ")))
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("  {}", p.bad("✗ the record does not match what was anchored"));
        Ok(ExitCode::from(1))
    }
}

// ---------------------------------------------------------------------------------------------
// rewrite

pub async fn rewrite(dir: &DataDir, seq: u64, actor: Option<String>, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let actor = actor.unwrap_or_else(|| info.submitter.clone());
    let canton = ledger::connect(&info).await?;
    println!(
        "{}",
        p.bold(format!("{actor} tries to archive anchor #{seq} of chain {} on Canton, signing alone", info.chain_id))
    );
    let r = canton.attempt_archive(&info.chain_id, &actor, seq).await.map_err(|e| e.0)?;
    if let Some(c) = &r.contract_id {
        println!("  contract   {c}");
    }
    if !r.required.is_empty() {
        println!("  required   {}", r.required.join(" + "));
        println!("  signed     {actor}");
        println!("  missing    {}", p.bad(r.missing.join(" + ")));
    }
    if r.ok {
        println!("  {} the anchor was archived", p.bad("!!"));
        Ok(ExitCode::from(1))
    } else {
        println!("  {} {}", p.ok("refused:"), r.reason);
        println!(
            "  {}",
            p.dim("The anchor is still on every member's ledger copy. History cannot be rewritten by one party.")
        );
        Ok(ExitCode::SUCCESS)
    }
}

// ---------------------------------------------------------------------------------------------
// verify

fn print_completeness(pos: &PositionReport, p: Paint) {
    let (Some(f), Some(l)) = (pos.first, pos.last) else { return };
    if pos.complete() {
        println!("  completeness: GTIDs {f}–{l}, {} transactions anchored, {}", pos.anchored, p.ok("none missing"));
        return;
    }
    println!(
        "  completeness: GTIDs {f}–{l}, {} transactions anchored, {}",
        pos.anchored,
        p.warn(format!(
            "{} transaction(s) never captured (binlog purged or edited before capture, sql_log_bin=0, or capture down)",
            pos.missing
        ))
    );
    for g in &pos.gaps {
        println!(
            "        {} GTIDs {}–{} between batch #{} and #{}",
            p.warn("GAP"),
            g.from,
            g.to,
            g.after_seq,
            g.before_seq
        );
    }
    for h in &pos.holes {
        println!(
            "        {} batch #{} spans GTIDs {}–{} but holds {} fewer",
            p.warn("HOLE"),
            h.seq,
            h.from,
            h.to,
            h.missing
        );
    }
}

pub async fn verify(dir: &DataDir, trail_path: Option<&Path>, all: bool, json: bool, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let auditor = auditor_of(&info).to_string();
    // The anchors come from the AUDITOR's own ledger copy, never from the company's files.
    let anchors = ledger::anchors_as(&info, &auditor).await?;
    let (rows, unreadable) = sealed_rows(dir, trail_path, last_seq(&anchors))?;
    let report = verify_records(&info.chain_id, &info.genesis_head, &anchors, &rows);
    let pos = position_report(&anchors);
    let code = if report.ok { ExitCode::SUCCESS } else { ExitCode::from(1) };
    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            report: &'a VerifyReport,
            positions: &'a PositionReport,
            unreadable_lines: usize,
        }
        let out = Out { report: &report, positions: &pos, unreadable_lines: unreadable };
        println!("{}", serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?);
        return Ok(code);
    }

    let by_id: HashMap<&str, &StoredRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    println!(
        "{}",
        p.bold(format!(
            "{auditor} verifies chain {}: {} records from {} against {} anchors on its own Canton copy",
            info.chain_id,
            rows.len(),
            trail_path.map_or_else(|| dir.trail().display().to_string(), |t| t.display().to_string()),
            anchors.len()
        ))
    );
    if unreadable > 0 {
        println!("  {}", p.warn(format!("{unreadable} line(s) of the trail could not be read and count as missing")));
    }
    let changed: HashSet<u64> = report.changed_seqs.iter().copied().collect();
    for b in &report.batches {
        if !all && b.ok {
            continue;
        }
        let note = if b.ok {
            p.ok("intact")
        } else if !b.ledger_consistent {
            p.bad("ANCHOR INCONSISTENT")
        } else if changed.contains(&b.seq) {
            p.bad("CONTENTS CHANGED")
        } else {
            p.warn("contents intact; head chains on an altered batch")
        };
        println!(
            "  #{:<4} leaves {} root {} head {} count {}  {:>3}/{:<3} records  {note}",
            b.seq,
            p.mark(b.leaf_issues.is_empty() && b.missing_indexes.is_empty()),
            p.mark(b.root_match),
            p.mark(b.head_match),
            p.mark(b.count_match),
            b.recomputed_count,
            b.anchored_count
        );
        for li in &b.leaf_issues {
            let what = by_id.get(li.record_id.as_str()).map(|r| {
                let d = describe(&r.event);
                format!("{} · {}", d.kind, d.text)
            });
            let tag = if li.anchored.is_some() { "ALTERED " } else { "INVENTED" };
            println!(
                "        {} L{} {}  {}{}",
                p.bad(tag),
                li.index + 1,
                li.record_id,
                what.unwrap_or_default(),
                if li.covered {
                    p.dim("  (stored leaf hash was rewritten to match; still caught)")
                } else {
                    String::new()
                }
            );
        }
        for m in &b.missing_indexes {
            println!("        {} L{}  a record anchored in this slot is gone from the trail", p.bad("DELETED "), m + 1);
        }
    }
    if !report.unanchored.is_empty() {
        println!("  {} batches in the trail with no anchor: {:?}", p.bad("UNANCHORED"), report.unanchored);
    }
    print_completeness(&pos, p);

    println!();
    if report.ok {
        println!(
            "{} every record matches its anchored leaf; roots and the chain of {} heads recompute.",
            p.ok("PASS"),
            anchors.len()
        );
    } else {
        let seqs: Vec<String> = report.changed_seqs.iter().map(|s| format!("#{s}")).collect();
        println!(
            "{} {} altered and {} deleted record(s), in batch(es) {}. {} batch(es) fail in total because each head \
             chains on the one before.",
            p.bad("FAIL"),
            report.altered_count,
            report.deleted_count,
            if seqs.is_empty() { "-".into() } else { seqs.join(", ") },
            report.failed_seqs.len()
        );
    }
    Ok(code)
}
