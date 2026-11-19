//! `gen` and `run`.
//!
//! The run loop has two tasks joined by a bounded channel:
//!
//! ```text
//!   connector task: ChangeSource::next_tx ──► mpsc (whole transactions) ──► node loop
//!   node loop: select { ctrl-c → stop · tick → seal + anchor + commit · tx → ingest }
//! ```
//!
//! The channel applies back-pressure: if anchoring is slow, the connector waits instead of
//! buffering without bound. A batch is sealed only at a tick and holds whole transactions. On
//! Ctrl-C nothing partial is sealed: pending transactions are not in the cursor, so a restarted node
//! reads them again.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use chrono::DateTime;
use sthira_batcher::persist::{ChainInfo, DataDir};
use sthira_batcher::{Ingest, Node, Sealed};
use sthira_connector::describe::describe;
use sthira_connector::generator::{GenConfig, write_jsonl};
use sthira_connector::replay::{ReplayOptions, ReplaySource};
use sthira_connector::{iso_micros, now_iso};
use sthira_core::{AuditLedger, CapturedTx, ChangeSource, SourceError, genesis_head};
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior};

use crate::cli::{GenArgs, RunArgs};
use crate::{Paint, Res, clock, ledger, short};

pub async fn gen_cmd(a: GenArgs, p: Paint) -> Res<ExitCode> {
    if a.out.exists() && !a.force {
        return Err(format!("{} exists; pass --force to overwrite it", a.out.display()));
    }
    let start = DateTime::parse_from_rfc3339(&a.start).map_err(|e| format!("--start {}: {e}", a.start))?.timestamp();
    let cfg = GenConfig { seed: a.seed, txs: a.tx, start, first_tx: 1 };
    let (txs, rows) = write_jsonl(&cfg, &a.out).await.map_err(|e| format!("{}: {e}", a.out.display()))?;
    println!(
        "{} {txs} transactions ({rows} row changes) of an ERP workload as MySQL binlog entries → {}",
        p.ok("generated"),
        a.out.display()
    );
    println!("  {}", p.dim(format!("seed {} · GTIDs 1–{txs} · first commit {}", a.seed, iso_micros(start * 1000))));
    println!("  {}", p.dim("next: make sandbox (another terminal), then sthira-node run"));
    Ok(ExitCode::SUCCESS)
}

/// The connector task: forwards whole transactions until the log ends or the node stops listening.
async fn feed<S: ChangeSource>(mut source: S, out: mpsc::Sender<CapturedTx>) -> Result<u64, SourceError> {
    let mut n = 0;
    while let Some(tx) = source.next_tx().await? {
        if out.send(tx).await.is_err() {
            break;
        }
        n += 1;
    }
    Ok(n)
}

fn print_batch(s: &Sealed, gaps: &[(u64, u64)], p: Paint) {
    let b = &s.batch;
    let span = if b.tx_count == 0 { "–".to_string() } else { format!("{}–{}", b.from_tx, b.to_tx) };
    println!(
        "{} {}  {:>3} tx {:>3} rows  gtid {:<9}  root {}  head {}  {} {}",
        p.bold(format!("#{:<4}", b.seq)),
        clock(&b.to_time),
        b.tx_count,
        b.event_count,
        span,
        short(&b.merkle_root),
        short(&b.head),
        p.ok(format!("anchored {} leaves", s.anchor.leaves.len())),
        p.dim(format!("signed {}", s.anchor.signatories.join("+"))),
    );
    for r in &s.rows {
        let d = describe(&r.event);
        let line = format!("      L{:<3} {:<11} {:<9} {}", r.index + 1, r.id, d.kind, d.text);
        if d.flagged { println!("{line}  {}", p.warn("⚑")) } else { println!("{line}") }
    }
    for (from, to) in gaps {
        println!(
            "      {} GTIDs {from}–{to} never reached the binlog the node reads ({} missing)",
            p.warn("⚠ gap"),
            to - from + 1
        );
    }
}

pub async fn run(dir: &DataDir, a: RunArgs, p: Paint) -> Res<ExitCode> {
    let fresh = !dir.exists();
    if fresh {
        if !a.input.exists() {
            return Err(format!("{} not found; generate it first with `sthira-node gen`", a.input.display()));
        }
        let source = std::fs::canonicalize(&a.input).map_err(|e| e.to_string())?;
        let info = ChainInfo {
            chain_id: a.chain.clone(),
            submitter: a.company.clone(),
            members: vec![a.company.clone(), a.auditor.clone()],
            genesis_head: genesis_head(&a.chain),
            chain_start: now_iso(),
            source: source.display().to_string(),
            ledger_url: a.ledger_url.clone(),
            ledger_user: a.ledger_user.clone(),
        };
        dir.init(&info).map_err(|e| e.to_string())?;
    }
    let info = dir.info().map_err(|e| e.to_string())?;
    let (canton, anchored) = ledger::open_for_node(dir, &info).await?;
    let st = dir.restore(anchored)?;
    let opts = ReplayOptions {
        chain: info.chain_id.clone(),
        after_tx: st.cursor.last_tx,
        skip: a.skip_gtids.iter().copied().collect(),
        rate: a.rate,
    };
    let source = ReplaySource::open(Path::new(&info.source), opts, now_iso).await.map_err(|e| e.to_string())?;

    println!(
        "{}",
        p.bold(format!("Sthiranet node · chain {} · {}", info.chain_id, if fresh { "new" } else { "resumed" }))
    );
    println!("  source     {} (MySQL binlog, replayed at {} tx/s)", info.source, a.rate);
    println!("  parties    {} (submits), co-signed by {}", info.submitter, info.members[1..].join(", "));
    println!("  ledger     Canton participant at {} (JSON Ledger API v2)", info.ledger_url);
    println!("  store      {}", dir.trail().display());
    println!(
        "  resume at  GTID {} · batch #{} · every {} s{}",
        st.cursor.last_tx + 1,
        st.node.last_seq() + 1,
        a.interval,
        if a.skip_gtids.is_empty() { String::new() } else { p.warn(format!(" · dropping GTIDs {:?}", a.skip_gtids)) }
    );
    println!();
    drive(dir, st.node, canton, source, &a, p).await
}

/// The node loop, generic over the connector and the ledger.
async fn drive<S, L>(dir: &DataDir, mut node: Node, ledger: L, source: S, a: &RunArgs, p: Paint) -> Res<ExitCode>
where
    S: ChangeSource + 'static,
    L: AuditLedger,
{
    let (tx_out, mut rx) = mpsc::channel::<CapturedTx>(256);
    let mut connector = tokio::spawn(feed(source, tx_out));
    let mut connector_done = false;

    let period = Duration::from_secs(a.interval.max(1));
    let mut ticker = tokio::time::interval_at(Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut gaps: Vec<(u64, u64)> = Vec::new();
    let mut sealed = 0u64;
    // One listener for the whole run, so a Ctrl-C between two iterations is not lost.
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);

    loop {
        tokio::select! {
            biased;
            _ = &mut stop => {
                println!("\n{}", p.dim(format!("stopped; {} captured transaction(s) not sealed will be read again", node.pending.len())));
                break;
            }
            _ = ticker.tick() => {
                let s = node.seal(&ledger, now_iso()).await.map_err(|e| format!("ledger refused the batch: {}", e.0))?;
                let (d, s2) = (dir.clone(), s.clone());
                tokio::task::spawn_blocking(move || d.commit(&s2))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| format!("writing {}: {e}", dir.root.display()))?;
                print_batch(&s, &gaps, p);
                gaps.clear();
                sealed += 1;
                if a.batches.is_some_and(|n| sealed >= n) {
                    break;
                }
            }
            // Disabled once the connector has finished, so a closed channel never spins the loop.
            got = rx.recv(), if !connector_done => match got {
                Some(t) => {
                    if let Ingest::QueuedAfterGap { from, to } = node.ingest(t) {
                        gaps.push((from, to));
                    }
                }
                None => {
                    connector_done = true;
                    match (&mut connector).await {
                        Ok(Ok(n)) => println!("      {}", p.dim(format!("end of the change log ({n} transaction(s) replayed this run); batches after the next one stay empty"))),
                        Ok(Err(e)) => return Err(format!("change log: {e}")),
                        Err(e) => return Err(format!("connector task: {e}")),
                    }
                }
            }
        }
    }
    connector.abort();
    Ok(ExitCode::SUCCESS)
}
