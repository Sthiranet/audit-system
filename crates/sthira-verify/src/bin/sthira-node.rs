//! `sthira-node`: the prototype, run from a terminal.
//!
//! One binary plays every role, each touching only what that party would hold:
//!
//! - the company's node (`run`): reads the source's audit API, seals 10-second batches, anchors them
//!   and keeps the off-ledger audit store under `<data>/node/`;
//! - the ledger: a Canton participant (`run --ledger canton`, the sandbox in `prototype-sandbox/`)
//!   or the in-memory SimLedger stand-in (`--ledger sim`, replayed from `<data>/ledger/anchors.jsonl`);
//! - the auditor (`verify`, `check-proof`, `ledger`): recomputes from a trail and compares with the
//!   ledger only;
//! - the presenter's insider (`tamper`, `untamper`), and a party trying to rewrite history on the
//!   ledger itself (`rewrite`).
//!
//! Run `sthira-node --help` or see `prototype_tutorial.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sthira_batcher::feed_node::FeedRow;
use sthira_batcher::persist::{ChainInfo, DataDir, LedgerConfig, TamperEntry, append_jsonl, rewrite_jsonl, write_json};
use sthira_core::{EventV2, canonical, genesis_head, leaf_hash, verify_inclusion};
use sthira_ledger::Anchor;
use sthira_ledger::canton::CantonConfig;
use sthira_profiles::audit_api::{
    AuditCursor, AuditEnvelope, AuditFeed, AuditLogEntry, AuditPage, FeedError, HttpFeed, MemoryFeed, Start,
    audit_flags, describe_audit, is_noise, normalize_audit, reattribute_audit, system_of, tamper_audit,
};
use sthira_profiles::iso_micros;
use sthira_verify::{VerifyReport, position_report, verify_records};

const DEFAULT_API: &str = "https://dreamtalk-backend.vishvena.com/audit/";

#[derive(Parser)]
#[command(
    name = "sthira-node",
    version,
    about = "Sthiranet audit-trail prototype: capture an application's audit API, anchor Merkle roots, verify independently"
)]
struct Cli {
    /// Data directory holding the node's audit store and the ledger stand-in.
    #[arg(long, global = true, default_value = "./sthira-data", env = "STHIRA_DATA")]
    data: PathBuf,
    /// Plain output without colours.
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the company's node: poll the audit API, seal and anchor a batch every interval.
    Run(Box<RunArgs>),
    /// Summary of the chain, the trail and the node's capture state.
    Status,
    /// What the ledger holds: one anchor per batch, hashes and counts only (the auditor's view).
    Ledger {
        /// Show only the last N anchors.
        #[arg(long, default_value_t = 20)]
        last: usize,
    },
    /// List captured records (request-log rows hidden unless --all).
    Trail {
        /// Only this batch.
        #[arg(long)]
        batch: Option<u64>,
        /// Include the source's own request-log rows.
        #[arg(long)]
        all: bool,
        /// Only the last N records shown.
        #[arg(long, default_value_t = 40)]
        last: usize,
    },
    /// Prove one record is in its anchored batch, without revealing any other record.
    Proof {
        /// A record id (`DT-190850`) or `table:key` for every captured version of a record.
        target: String,
        /// Print the full event as hashed.
        #[arg(long)]
        event: bool,
        /// Write a self-contained proof bundle for a third party to check with `check-proof`.
        #[arg(long)]
        export: Option<PathBuf>,
    },
    /// Check an exported proof bundle against this data directory's ledger only.
    CheckProof { bundle: PathBuf },
    /// Presenter: act as an insider with write access to the node's audit store.
    Tamper {
        #[arg(value_enum)]
        action: TamperAction,
        /// Record id, e.g. DT-190850 (see `trail`).
        record: String,
        /// After an edit, also rewrite the stored leaf hash so the row looks self-consistent.
        #[arg(long)]
        cover: bool,
    },
    /// Presenter: put back every record changed by `tamper`.
    Untamper,
    /// Try to archive (rewrite) an anchor on the ledger acting as ONE party. The ledger refuses:
    /// every anchor is signed by every member.
    Rewrite {
        /// Batch number of the anchor to archive.
        seq: u64,
        /// The party attempting it (default: the company).
        #[arg(long = "as")]
        actor: Option<String>,
    },
    /// Auditor: recompute the trail and compare it with the anchors on the ledger.
    Verify {
        /// Verify this trail file instead of the node's own (e.g. a copy the company handed over).
        #[arg(long)]
        trail: Option<PathBuf>,
        /// Print every batch, not only the ones that fail.
        #[arg(long)]
        all: bool,
        /// Machine-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Re-read the newest API pages and compare them with what was anchored: shows audit rows the
    /// source changed or removed after capture.
    Reconcile {
        #[arg(long, default_value_t = 3)]
        pages: u32,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum TamperAction {
    /// Change a field of the record's row image.
    Edit,
    /// Change who the record says made the change.
    Actor,
    /// Remove the record from the store.
    Delete,
}

#[derive(clap::Args)]
struct RunArgs {
    /// The audit API list endpoint (only used when the data directory is new).
    #[arg(long, default_value = DEFAULT_API)]
    api: String,
    /// Replay rows from a JSON file (an API page or a list of rows) instead of calling the API.
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// Chain id (new data directory only).
    #[arg(long, default_value = "DT")]
    chain: String,
    /// The company's party (submitter).
    #[arg(long, default_value = "DREAMTALK")]
    company: String,
    /// The co-signing party that keeps its own copy of every anchor.
    #[arg(long, default_value = "AUDITOR")]
    auditor: String,
    /// Seconds per batch.
    #[arg(long, default_value_t = 10)]
    interval: u64,
    /// Where a new chain starts: `head` (only new rows), `pages:N` (backfill the newest N pages) or `id:N`.
    #[arg(long, default_value = "pages:1")]
    start: String,
    /// Seconds to wait for a missing audit id before declaring a gap.
    #[arg(long, default_value_t = 30)]
    grace: i64,
    /// Stop after this many batches (default: run until Ctrl-C).
    #[arg(long)]
    batches: Option<u64>,
    /// Presenter: pretend the source deleted these audit ids before capture (comma separated).
    #[arg(long, value_delimiter = ',')]
    hide_ids: Vec<u64>,
    /// Where to anchor (new data directory only): `canton` (the sandbox) or `sim` (in-memory stand-in).
    #[arg(long, value_enum, default_value_t = LedgerKind::Sim)]
    ledger: LedgerKind,
    /// JSON Ledger API v2 of the Canton participant.
    #[arg(long, default_value = "http://localhost:7575", env = "STHIRA_LEDGER_URL")]
    ledger_url: String,
    /// Ledger API user id sent with each command.
    #[arg(long, default_value = "sthira-node")]
    ledger_user: String,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LedgerKind {
    Sim,
    Canton,
}

// ---------------------------------------------------------------------------------------------
// Output helpers

#[derive(Clone, Copy)]
struct Paint(bool);

impl Paint {
    fn wrap(&self, code: &str, s: impl AsRef<str>) -> String {
        if self.0 { format!("\x1b[{code}m{}\x1b[0m", s.as_ref()) } else { s.as_ref().to_string() }
    }
    fn ok(&self, s: impl AsRef<str>) -> String {
        self.wrap("32", s)
    }
    fn bad(&self, s: impl AsRef<str>) -> String {
        self.wrap("31;1", s)
    }
    fn warn(&self, s: impl AsRef<str>) -> String {
        self.wrap("33", s)
    }
    fn dim(&self, s: impl AsRef<str>) -> String {
        self.wrap("2", s)
    }
    fn bold(&self, s: impl AsRef<str>) -> String {
        self.wrap("1", s)
    }
    fn mark(&self, b: bool) -> String {
        if b { self.ok("✓") } else { self.bad("✗") }
    }
}

fn short(h: &str) -> String {
    h.chars().take(10).collect::<String>() + "…"
}

fn clock(iso: &str) -> &str {
    iso.get(11..19).unwrap_or(iso)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn actor_label(e: &EventV2) -> String {
    match (&e.actor.name, &e.actor.id) {
        (Some(n), _) => n.clone(),
        (None, Some(id)) => format!("user {id}"),
        _ => "system".to_string(),
    }
}

type Res<T> = Result<T, String>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let p = Paint(!cli.no_color && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none());
    let dir = DataDir::new(&cli.data);
    let r = match cli.cmd {
        Cmd::Run(a) => run(&dir, *a, p),
        Cmd::Status => status(&dir, p),
        Cmd::Ledger { last } => ledger(&dir, last, p),
        Cmd::Trail { batch, all, last } => trail(&dir, batch, all, last, p),
        Cmd::Proof { target, event, export } => proof(&dir, &target, event, export.as_deref(), p),
        Cmd::CheckProof { bundle } => check_proof(&dir, &bundle, p),
        Cmd::Tamper { action, record, cover } => tamper(&dir, action, &record, cover, p),
        Cmd::Untamper => untamper(&dir, p),
        Cmd::Rewrite { seq, actor } => rewrite(&dir, seq, actor, p),
        Cmd::Verify { trail, all, json } => verify(&dir, trail.as_deref(), all, json, p),
        Cmd::Reconcile { pages } => reconcile(&dir, pages, p),
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e}", p.bad("error:"));
            ExitCode::from(2)
        }
    }
}

/// The co-signer that plays the auditor: the last member.
fn auditor_of(info: &ChainInfo) -> &str {
    info.members.last().map_or("", String::as_str)
}

fn need_chain(dir: &DataDir) -> Res<ChainInfo> {
    if !dir.exists() {
        return Err(format!(
            "no chain in {} yet; start one with `sthira-node --data {} run`",
            dir.root.display(),
            dir.root.display()
        ));
    }
    dir.info().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------------
// run

/// A feed that hides some ids, as if the source had deleted those audit rows before capture.
struct Hiding<F> {
    inner: F,
    hide: HashSet<u64>,
}

impl<F: AuditFeed> AuditFeed for Hiding<F> {
    fn page(&mut self, n: u32) -> Result<AuditPage, FeedError> {
        let mut p = self.inner.page(n)?;
        p.results.retain(|r| !self.hide.contains(&r.id));
        Ok(p)
    }
}

fn load_fixture(path: &Path) -> Res<Vec<AuditLogEntry>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Ok(e) = serde_json::from_str::<AuditEnvelope>(&text) {
        return Ok(e.response.results);
    }
    if let Ok(p) = serde_json::from_str::<AuditPage>(&text) {
        return Ok(p.results);
    }
    serde_json::from_str::<Vec<AuditLogEntry>>(&text)
        .map_err(|e| format!("{}: not an audit page or row list: {e}", path.display()))
}

fn parse_start(s: &str) -> Res<Start> {
    match s.split_once(':') {
        None if s == "head" => Ok(Start::Head),
        Some(("pages", n)) => n.parse().map(Start::BackfillPages).map_err(|_| format!("bad --start {s}")),
        Some(("id", n)) => n.parse().map(Start::FromId).map_err(|_| format!("bad --start {s}")),
        _ => Err(format!("--start must be head, pages:N or id:N (got {s})")),
    }
}

fn run(dir: &DataDir, a: RunArgs, p: Paint) -> Res<ExitCode> {
    let fresh = !dir.exists();
    let api = match &a.fixture {
        Some(f) => format!("file://{}", f.display()),
        None => a.api.clone(),
    };
    if fresh {
        let info = ChainInfo {
            chain_id: a.chain.clone(),
            submitter: a.company.clone(),
            members: vec![a.company.clone(), a.auditor.clone()],
            genesis_head: genesis_head(&a.chain),
            chain_start: iso_micros(now_ms()),
            system: if a.fixture.is_some() { "fixture".into() } else { system_of(&api) },
            api_url: api.clone(),
            ledger: match a.ledger {
                LedgerKind::Sim => LedgerConfig::Sim,
                LedgerKind::Canton => {
                    LedgerConfig::Canton(CantonConfig { url: a.ledger_url.clone(), user_id: a.ledger_user.clone() })
                }
            },
        };
        dir.init(&info).map_err(|e| e.to_string())?;
    }
    let mut st = dir.restore()?;
    let info = st.info.clone();

    let inner: Box<dyn AuditFeed> = match &a.fixture {
        Some(f) => Box::new(MemoryFeed::new(load_fixture(f)?, 100)),
        None => Box::new(HttpFeed::new(&info.api_url, 100)),
    };
    let mut feed = Hiding { inner, hide: a.hide_ids.iter().copied().collect() };

    let mut cursor = match st.cursor.take() {
        Some(c) => c,
        None => match st.node.max_pos() {
            Some(m) => AuditCursor::after(m),
            None => AuditCursor::start(&mut feed, parse_start(&a.start)?).map_err(|e| e.to_string())?,
        },
    };
    cursor.grace_ms = a.grace * 1000;

    println!(
        "{}",
        p.bold(format!("Sthiranet node · chain {} · {}", info.chain_id, if fresh { "new" } else { "resumed" }))
    );
    println!("  source     {}", info.api_url);
    println!("  parties    {} (submits), co-signed by {}", info.submitter, info.members[1..].join(", "));
    println!("  store      {}", dir.trail().display());
    match &info.ledger {
        LedgerConfig::Sim => println!(
            "  ledger     SimLedger stand-in, replayed from {}  {}",
            dir.anchors().display(),
            p.dim("(use --ledger canton for the sandbox)")
        ),
        LedgerConfig::Canton(c) => println!(
            "  ledger     Canton participant at {} (JSON Ledger API v2) · parties {}",
            c.url,
            info.members.join(", ")
        ),
    }
    println!(
        "  resume at  audit id {} · batch #{} · every {} s{}",
        cursor.contiguous + 1,
        st.node.last_seq() + 1,
        a.interval,
        if a.hide_ids.is_empty() { String::new() } else { p.warn(format!(" · hiding ids {:?}", a.hide_ids)) }
    );
    println!();

    let interval = Duration::from_secs(a.interval.max(1));
    let mut sealed = 0u64;
    let mut next = std::time::Instant::now();
    loop {
        // Read and seal at the end of each window, so a batch covers the interval that just ended.
        next += interval;
        if let Some(wait) = next.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(wait);
        }
        let now = now_ms();
        let captured_at = iso_micros(now);
        let polled = match cursor.poll(&mut feed, now) {
            Ok(x) => x,
            Err(e) => {
                // Still seal: an empty batch records that the node was alive but the source was unreachable.
                println!("  {} {e}", p.warn("source unreachable:"));
                Default::default()
            }
        };
        st.node.ingest(&polled.rows, &polled.late, &captured_at);
        let s = st
            .node
            .seal(st.ledger.as_mut(), captured_at.clone())
            .map_err(|e| format!("ledger refused the batch: {}", e.0))?;
        dir.commit(&s, &cursor).map_err(|e| format!("writing {}: {e}", dir.root.display()))?;
        print_batch(&s.batch, &s.rows, &s.anchor, &polled, p);
        sealed += 1;
        if a.batches.is_some_and(|n| sealed >= n) {
            return Ok(ExitCode::SUCCESS);
        }
    }
}

fn print_batch(
    b: &sthira_batcher::feed_node::FeedBatch,
    rows: &[FeedRow],
    anchor: &Anchor,
    polled: &sthira_profiles::audit_api::Polled,
    p: Paint,
) {
    let noise = rows.iter().filter(|r| is_noise(&r.event)).count();
    let span =
        if b.event_count == 0 { "        –        ".to_string() } else { format!("{}–{}", b.from_pos, b.to_pos) };
    println!(
        "{} {}  {:>3} events {}  pos {:<15}  root {}  head {}  {} {} {}",
        p.bold(format!("#{:<4}", b.seq)),
        clock(&b.to_time),
        b.event_count,
        p.dim(format!("({noise:>2} req-log)")),
        span,
        short(&b.merkle_root),
        short(&b.head),
        p.ok("anchored"),
        p.dim(format!("signed {}", anchor.signatories.join("+"))),
        p.dim(format!("cid {}", short(&anchor.contract_id))),
    );
    for r in rows.iter().filter(|r| !is_noise(&r.event)) {
        let flags = audit_flags(&r.event);
        let d = describe_audit(&r.event);
        let line = format!(
            "      L{:<3} {}  {:<28} {}  {}",
            r.index + 1,
            r.id,
            d.label,
            d.text,
            p.dim(format!("by {}", actor_label(&r.event)))
        );
        if flags.iter().any(|f| f == "privilege-change") {
            println!("{}  {}", line, p.warn("⚑ privilege change"));
        } else if r.late {
            println!("{}  {}", line, p.warn("late"));
        } else {
            println!("{line}");
        }
    }
    for g in &polled.new_gaps {
        println!(
            "      {} audit ids {}–{} never arrived ({} missing); declared after the grace period",
            p.warn("⚠ gap"),
            g.from,
            g.to,
            g.len()
        );
    }
    if polled.held > 0 {
        println!("      {}", p.dim(format!("{} rows held back waiting for a missing id", polled.held)));
    }
}

// ---------------------------------------------------------------------------------------------
// status / ledger / trail

/// The trail rows that belong to anchored batches (rows of a batch being written right now are left out).
fn anchored_rows(dir: &DataDir, path: Option<&Path>, anchors: &[Anchor]) -> Res<(Vec<FeedRow>, usize)> {
    let (mut rows, bad) = dir.load_trail_from(path).map_err(|e| e.to_string())?;
    let last = anchors.last().map_or(0, |a| a.seq);
    rows.retain(|r| r.batch_seq <= last);
    Ok((rows, bad))
}

fn status(dir: &DataDir, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let anchors = dir.load_anchors().map_err(|e| e.to_string())?;
    let (rows, _) = anchored_rows(dir, None, &anchors)?;
    let cursor = dir.load_cursor().map_err(|e| e.to_string())?;
    let tl = dir.load_tamper_log().map_err(|e| e.to_string())?;
    let noise = rows.iter().filter(|r| is_noise(&r.event)).count();
    let pos = position_report(&anchors);

    println!("{}", p.bold(format!("Chain {} · {}", info.chain_id, info.system)));
    println!("  members    {} (submitter {})", info.members.join(", "), info.submitter);
    match &info.ledger {
        LedgerConfig::Sim => println!("  ledger     SimLedger stand-in ({})", dir.anchors().display()),
        LedgerConfig::Canton(c) => {
            println!("  ledger     Canton at {} {}", c.url, p.dim("(counts below are the node's local receipts)"))
        }
    }
    println!("  started    {}", info.chain_start);
    match anchors.last() {
        Some(a) => {
            println!(
                "  batches    {} anchored · last #{} at {} · head {}",
                anchors.len(),
                a.seq,
                a.to_time,
                short(&a.head)
            )
        }
        None => println!("  batches    none yet"),
    }
    println!("  records    {} ({} business changes, {} request-log rows)", rows.len(), rows.len() - noise, noise);
    if let (Some(f), Some(l)) = (pos.first, pos.last) {
        println!(
            "  positions  audit ids {f}–{l} · {}",
            if pos.complete() { p.ok("none missing") } else { p.warn(format!("{} missing", pos.missing)) }
        );
    }
    if let Some(c) = cursor {
        println!("  cursor     next audit id {} · {} declared gap(s) open", c.contiguous + 1, c.gaps.len());
    }

    let mut tables: BTreeMap<&str, usize> = BTreeMap::new();
    let mut flags: BTreeMap<String, usize> = BTreeMap::new();
    for r in &rows {
        *tables.entry(r.event.source.table.as_str()).or_default() += 1;
        for f in audit_flags(&r.event) {
            *flags.entry(f).or_default() += 1;
        }
    }
    let mut tv: Vec<_> = tables.into_iter().collect();
    tv.sort_by_key(|a| std::cmp::Reverse(a.1));
    println!("  tables     {}", tv.iter().map(|(t, n)| format!("{t} {n}")).collect::<Vec<_>>().join(" · "));
    if !flags.is_empty() {
        println!("  flags      {}", flags.iter().map(|(f, n)| format!("{f} {n}")).collect::<Vec<_>>().join(" · "));
    }
    if !tl.is_empty() {
        println!(
            "  {}",
            p.warn(format!("tampered   {} record(s) altered by the presenter; `untamper` restores them", tl.len()))
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn ledger(dir: &DataDir, last: usize, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let (anchors, source) = dir.anchors_as(auditor_of(&info))?;
    println!("{}", p.dim(format!("  read from: {source}")));
    println!(
        "{}",
        p.bold(format!(
            "Ledger · chain {} · {} anchors · each signed by {}",
            info.chain_id,
            anchors.len(),
            info.members.join(" + ")
        ))
    );
    println!("{}", p.dim("  Only fingerprints: no record content, no personal data."));
    println!(
        "{}",
        p.dim(format!(
            "  {:<5} {:<9} {:>6}  {:<15}  {:<12} {:<12} {:<12}",
            "seq", "window", "count", "positions", "prev head", "root", "head"
        ))
    );
    for a in anchors.iter().skip(anchors.len().saturating_sub(last)) {
        println!(
            "  #{:<4} {} {:>6}  {:<15}  {} {} {}",
            a.seq,
            clock(&a.to_time),
            a.event_count,
            format!("{}–{}", a.from_pos, a.to_pos),
            short(&a.prev_head),
            short(&a.merkle_root),
            short(&a.head)
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn trail(dir: &DataDir, batch: Option<u64>, all: bool, last: usize, p: Paint) -> Res<ExitCode> {
    need_chain(dir)?;
    let anchors = dir.load_anchors().map_err(|e| e.to_string())?;
    let (rows, _) = anchored_rows(dir, None, &anchors)?;
    let shown: Vec<&FeedRow> =
        rows.iter().filter(|r| batch.is_none_or(|b| r.batch_seq == b)).filter(|r| all || !is_noise(&r.event)).collect();
    let hidden = rows.iter().filter(|r| batch.is_none_or(|b| r.batch_seq == b)).count() - shown.len();
    for r in shown.iter().skip(shown.len().saturating_sub(last)) {
        let d = describe_audit(&r.event);
        let flags = audit_flags(&r.event);
        println!(
            "#{:<4} L{:<3} {:<12} {:<7} {:<28} {}  {} {}",
            r.batch_seq,
            r.index + 1,
            r.id,
            format!("{:?}", r.event.op).to_lowercase(),
            d.label,
            d.text,
            p.dim(format!("by {} · leaf {}", actor_label(&r.event), short(&r.leaf_hash))),
            p.warn(flags.join(" "))
        );
    }
    if hidden > 0 {
        println!("{}", p.dim(format!("({hidden} request-log rows hidden; --all shows them)")));
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
    event: EventV2,
    proof: Vec<String>,
    /// The root as the exporting node claims it; the checker uses its own ledger copy instead.
    claimed_root: String,
}

fn find_rows<'a>(rows: &'a [FeedRow], target: &str) -> Vec<&'a FeedRow> {
    match target.split_once(':') {
        Some((table, key)) => rows.iter().filter(|r| r.event.source.table == table && r.event.key.id == key).collect(),
        None => rows.iter().filter(|r| r.id == target).collect(),
    }
}

fn proof(dir: &DataDir, target: &str, show_event: bool, export: Option<&Path>, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let (anchors, _) = dir.anchors_as(&info.submitter)?;
    let by_seq: HashMap<u64, &Anchor> = anchors.iter().map(|a| (a.seq, a)).collect();
    let (rows, _) = anchored_rows(dir, None, &anchors)?;
    let found = find_rows(&rows, target);
    if found.is_empty() {
        return Err(format!(
            "no captured record matches {target} (use a record id like {}-123, or table:key)",
            info.chain_id
        ));
    }
    let mut all_ok = true;
    for r in &found {
        let a = by_seq[&r.batch_seq];
        let leaf = leaf_hash(&r.salt, &r.event);
        let ok = verify_inclusion(&leaf, r.index, a.event_count as usize, &r.proof, &a.merkle_root);
        all_ok &= ok;
        let d = describe_audit(&r.event);
        println!("{}", p.bold(format!("{} · {} · {}", r.id, d.label, d.text)));
        println!("  committed  {} by {}", r.event.committed_at, actor_label(&r.event));
        println!("  batch      #{} slot L{} of {}", r.batch_seq, r.index + 1, a.event_count);
        println!("  leaf       {}  = H(00 ‖ salt ‖ canonical event)", leaf);
        println!("  path       {} sibling hashes", r.proof.len());
        for (i, h) in r.proof.iter().enumerate() {
            println!("    {}  {}", p.dim(format!("{i:>2}")), h);
        }
        println!(
            "  root       {}  (anchor {} on the ledger, signed {})",
            a.merkle_root,
            a.contract_id,
            a.signatories.join("+")
        );
        println!(
            "  included   {}",
            if ok {
                p.ok("✓ the path leads from this record to the anchored root")
            } else {
                p.bad("✗ the path does NOT lead to the anchored root: this record is not the one anchored")
            }
        );
        if show_event {
            println!("  event      {}", canonical(&r.event));
        }
        println!();
    }
    if let Some(path) = export {
        let r = found[0];
        let a = by_seq[&r.batch_seq];
        let b = ProofBundle {
            chain: info.chain_id.clone(),
            record_id: r.id.clone(),
            batch_seq: r.batch_seq,
            index: r.index,
            batch_size: a.event_count,
            salt: r.salt.clone(),
            event: r.event.clone(),
            proof: r.proof.clone(),
            claimed_root: a.merkle_root.clone(),
        };
        write_json(path, &b).map_err(|e| e.to_string())?;
        println!(
            "Proof bundle for {} written to {} (one record, its salt and {} sibling hashes; nothing else).",
            r.id,
            path.display(),
            r.proof.len()
        );
    }
    Ok(if all_ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

fn check_proof(dir: &DataDir, bundle: &Path, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let b: ProofBundle =
        sthira_batcher::persist::read_json(bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    let (anchors, source) = dir.anchors_as(auditor_of(&info))?;
    println!("{}", p.dim(format!("  anchors read from: {source}")));
    let Some(a) = anchors.iter().find(|a| a.seq == b.batch_seq && a.chain_id == b.chain) else {
        println!("{} the ledger has no anchor #{} on chain {}", p.bad("✗"), b.batch_seq, b.chain);
        return Ok(ExitCode::from(1));
    };
    let leaf = leaf_hash(&b.salt, &b.event);
    let ok = a.event_count == b.batch_size
        && verify_inclusion(&leaf, b.index, a.event_count as usize, &b.proof, &a.merkle_root);
    let d = describe_audit(&b.event);
    println!("{} · {} · {}", b.record_id, d.label, d.text);
    println!(
        "  checked against anchor #{} from the ledger (root {}), not the root in the bundle",
        a.seq,
        short(&a.merkle_root)
    );
    if ok {
        println!(
            "  {}",
            p.ok("✓ this exact record was anchored in that batch, co-signed by ".to_string()
                + &a.signatories.join(" + "))
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("  {}", p.bad("✗ the record does not match what was anchored"));
        Ok(ExitCode::from(1))
    }
}

// ---------------------------------------------------------------------------------------------
// tamper / untamper

fn tamper(dir: &DataDir, action: TamperAction, record: &str, cover: bool, p: Paint) -> Res<ExitCode> {
    need_chain(dir)?;
    let _lock = dir.lock().map_err(|e| e.to_string())?;
    let (mut rows, _) = dir.load_trail_from(None).map_err(|e| e.to_string())?;
    let i = rows
        .iter()
        .position(|r| r.id == record)
        .ok_or_else(|| format!("no record {record} in the node's trail (see `trail`)"))?;
    let original = rows[i].clone();
    let (name, description) = match action {
        TamperAction::Delete => {
            let r = rows.remove(i);
            ("delete", format!("deleted {} ({})", r.id, describe_audit(&r.event).text))
        }
        TamperAction::Edit | TamperAction::Actor => {
            let r = &mut rows[i];
            let d = match action {
                TamperAction::Edit => tamper_audit(&mut r.event),
                _ => reattribute_audit(&mut r.event),
            };
            if cover {
                r.leaf_hash = leaf_hash(&r.salt, &r.event);
            }
            (if matches!(action, TamperAction::Edit) { "edit" } else { "actor" }, d)
        }
    };
    rewrite_jsonl(&dir.trail(), &rows).map_err(|e| e.to_string())?;
    append_jsonl(
        &dir.tamper_log(),
        &[TamperEntry { row_id: record.to_string(), action: name.into(), description: description.clone(), original }],
    )
    .map_err(|e| e.to_string())?;
    println!("{} {description}", p.bad("Insider altered the audit store:"));
    if cover && !matches!(action, TamperAction::Delete) {
        println!("  {}", p.dim("The stored leaf hash was recomputed too, so the row looks self-consistent."));
    }
    println!("  {}", p.dim("Nothing on the ledger changed. Run `verify` to see whether the auditor notices."));
    Ok(ExitCode::SUCCESS)
}

fn untamper(dir: &DataDir, p: Paint) -> Res<ExitCode> {
    need_chain(dir)?;
    let _lock = dir.lock().map_err(|e| e.to_string())?;
    let log = dir.load_tamper_log().map_err(|e| e.to_string())?;
    if log.is_empty() {
        println!("Nothing to restore.");
        return Ok(ExitCode::SUCCESS);
    }
    let (mut rows, _) = dir.load_trail_from(None).map_err(|e| e.to_string())?;
    let mut done = HashSet::new();
    for e in &log {
        if !done.insert(e.row_id.clone()) {
            continue; // the first entry holds the true original
        }
        match rows.iter().position(|r| r.id == e.row_id) {
            Some(i) => rows[i] = e.original.clone(),
            None => rows.push(e.original.clone()),
        }
    }
    rows.sort_by_key(|r| (r.batch_seq, r.index));
    rewrite_jsonl(&dir.trail(), &rows).map_err(|e| e.to_string())?;
    std::fs::remove_file(dir.tamper_log()).map_err(|e| e.to_string())?;
    println!("{} {} record(s) restored to what was captured.", p.ok("✓"), done.len());
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------------------------
// rewrite

fn rewrite(dir: &DataDir, seq: u64, actor: Option<String>, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    let actor = actor.unwrap_or_else(|| info.submitter.clone());
    let (info, mut ledger) = dir.open_ledger(false)?;
    let kind = match &info.ledger {
        LedgerConfig::Sim => "the SimLedger",
        LedgerConfig::Canton(_) => "Canton",
    };
    println!(
        "{}",
        p.bold(format!("{actor} tries to archive anchor #{seq} of chain {} on {kind}, signing alone", info.chain_id))
    );
    let r = ledger.attempt_archive(&info.chain_id, &actor, seq).map_err(|e| e.0)?;
    if let Some(c) = &r.contract_id {
        println!("  contract   {c}");
    }
    if !r.required.is_empty() {
        println!("  required   {}", r.required.join(" + "));
        println!("  signed     {}", r.signed.join(" + "));
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

fn verify(dir: &DataDir, trail_path: Option<&Path>, all: bool, json: bool, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    // The anchors come from the AUDITOR's own ledger copy, never from the company's files.
    let (anchors, source) = dir.anchors_as(auditor_of(&info))?;
    let (rows, unreadable) = anchored_rows(dir, trail_path, &anchors)?;
    let report = verify_records(&info.chain_id, &info.genesis_head, &anchors, &rows);
    let pos = position_report(&anchors);
    if json {
        #[derive(Serialize)]
        struct Out<'a> {
            report: &'a VerifyReport,
            positions: &'a sthira_verify::PositionReport,
            unreadable_lines: usize,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out { report: &report, positions: &pos, unreadable_lines: unreadable })
                .unwrap()
        );
        return Ok(if report.ok { ExitCode::SUCCESS } else { ExitCode::from(1) });
    }

    let auditor = auditor_of(&info).to_string();
    let by_id: HashMap<&str, &FeedRow> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    println!(
        "{}",
        p.bold(format!(
            "{auditor} verifies chain {}: {} records from {} against {} anchors on its own ledger copy",
            info.chain_id,
            rows.len(),
            trail_path.map_or_else(|| dir.trail().display().to_string(), |t| t.display().to_string()),
            anchors.len()
        ))
    );
    println!("{}", p.dim(format!("  anchors read from: {source}")));
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
        } else if changed.contains(&b.seq) {
            p.bad("CONTENTS CHANGED")
        } else {
            p.warn("contents intact; head chains on an altered batch")
        };
        println!(
            "  #{:<4} root {} head {} count {}  {:>3}/{:<3} records  {note}",
            b.seq,
            p.mark(b.root_match),
            p.mark(b.head_match),
            p.mark(b.count_match),
            b.recomputed.event_count,
            b.anchored.event_count
        );
        for li in &b.leaf_issues {
            let what = by_id.get(li.event_id.as_str()).map(|r| {
                let d = describe_audit(&r.event);
                format!("{} · {} · by {}", d.label, d.text, actor_label(&r.event))
            });
            println!(
                "        {} L{} {}  {}{}",
                p.bad("ALTERED"),
                li.index + 1,
                li.event_id,
                what.unwrap_or_default(),
                if li.covered {
                    p.dim("  (stored leaf hash was rewritten to match; still caught)")
                } else {
                    String::new()
                }
            );
        }
        for m in &b.missing_indexes {
            println!("        {} L{}  a record anchored in this slot is gone from the trail", p.bad("DELETED"), m + 1);
        }
    }
    if !report.unanchored.is_empty() {
        println!("  {} batches in the trail with no anchor: {:?}", p.bad("UNANCHORED"), report.unanchored);
    }

    // Completeness, from the ledger alone.
    if let (Some(f), Some(l)) = (pos.first, pos.last) {
        if pos.complete() {
            println!("  completeness: audit ids {f}–{l}, {} anchored, {}", pos.anchored, p.ok("none missing"));
        } else {
            println!(
                "  completeness: audit ids {f}–{l}, {} anchored, {}",
                pos.anchored,
                p.warn(format!(
                    "{} id(s) never captured (removed at the source before capture, or capture was down)",
                    pos.missing
                ))
            );
            for g in &pos.gaps {
                println!(
                    "        {} ids {}–{} between batch #{} and #{}",
                    p.warn("GAP"),
                    g.from,
                    g.to,
                    g.after_seq,
                    g.before_seq
                );
            }
            for h in &pos.holes {
                println!(
                    "        {} batch #{} covers ids {}–{} but holds {} fewer",
                    p.warn("HOLE"),
                    h.seq,
                    h.from,
                    h.to,
                    h.missing
                );
            }
        }
    }

    println!();
    if report.ok {
        println!(
            "{} every record recomputes to the anchored roots; the chain of {} heads is unbroken.",
            p.ok("PASS"),
            anchors.len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        let seqs: Vec<String> = report.changed_seqs.iter().map(|s| format!("#{s}")).collect();
        println!(
            "{} {} altered and {} deleted record(s), in batch(es) {}. {} batch(es) fail in total because each head chains on the one before.",
            p.bad("FAIL"),
            report.altered_count,
            report.deleted_count,
            if seqs.is_empty() { "-".into() } else { seqs.join(", ") },
            report.failed_seqs.len()
        );
        Ok(ExitCode::from(1))
    }
}

// ---------------------------------------------------------------------------------------------
// reconcile

fn reconcile(dir: &DataDir, pages: u32, p: Paint) -> Res<ExitCode> {
    let info = need_chain(dir)?;
    if !info.api_url.starts_with("http") {
        return Err("reconcile needs a chain captured from the live API".into());
    }
    let anchors = dir.load_anchors().map_err(|e| e.to_string())?;
    let (rows, _) = anchored_rows(dir, None, &anchors)?;
    let mine: HashMap<u64, &FeedRow> = rows.iter().filter_map(|r| r.event.pos_num().map(|n| (n, r))).collect();

    let mut feed = HttpFeed::new(&info.api_url, 100);
    let mut fetched: BTreeMap<u64, AuditLogEntry> = BTreeMap::new();
    for n in 1..=pages.max(1) {
        let page = feed.page(n).map_err(|e| e.to_string())?;
        let more = page.next.is_some();
        for r in page.results {
            fetched.entry(r.id).or_insert(r);
        }
        if !more {
            break;
        }
    }
    let (Some(&lo), Some(&hi)) = (fetched.keys().next(), fetched.keys().last()) else {
        println!("The API returned no rows.");
        return Ok(ExitCode::SUCCESS);
    };
    let mut compared = 0;
    let mut changed = Vec::new();
    for (id, src) in &fetched {
        if let Some(r) = mine.get(id) {
            compared += 1;
            let now = normalize_audit(src, &info.chain_id, &info.system, &r.event.captured_at);
            if now != r.event {
                changed.push((*id, r));
            }
        }
    }
    let removed: Vec<&&FeedRow> =
        mine.iter().filter(|(id, _)| (lo..=hi).contains(*id) && !fetched.contains_key(id)).map(|(_, r)| r).collect();

    println!(
        "{}",
        p.bold(format!(
            "Reconcile: the source now serves audit ids {lo}–{hi}; {compared} of them were captured and anchored"
        ))
    );
    for (id, r) in &changed {
        let d = describe_audit(&r.event);
        println!(
            "  {} audit id {id} ({} {}) differs from the version anchored in batch #{}",
            p.bad("CHANGED AT SOURCE"),
            d.label,
            d.text,
            r.batch_seq
        );
    }
    for r in &removed {
        let d = describe_audit(&r.event);
        println!(
            "  {} audit id {} ({} {}) is no longer served; the anchored copy is in batch #{}",
            p.bad("REMOVED AT SOURCE"),
            r.event.source.pos,
            d.label,
            d.text,
            r.batch_seq
        );
    }
    if changed.is_empty() && removed.is_empty() {
        println!("  {} the source's audit log still matches what was anchored.", p.ok("✓"));
        Ok(ExitCode::SUCCESS)
    } else {
        println!("  The anchored copy is the evidence: `proof DT-<id>` proves what the row said when it was captured.");
        Ok(ExitCode::from(1))
    }
}
