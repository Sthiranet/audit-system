//! Command-line interface.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "sthira-node",
    version,
    about = "Sthiranet prototype: capture a database change log (MySQL binlog, generated offline), anchor leaves and \
             Merkle roots on Canton, verify independently"
)]
pub struct Cli {
    /// Data directory holding the chain's identity and the node's off-ledger audit store.
    #[arg(long, global = true, default_value = "./sthira-data", env = "STHIRA_DATA")]
    pub data: PathBuf,
    /// Plain output without colours.
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Generate dummy MySQL binlog transactions (an ERP workload) into a JSONL file, offline.
    Gen(GenArgs),
    /// Run the company's node: replay the change log, seal and anchor a batch every interval.
    Run(RunArgs),
    /// Summary of the chain, the trail and the node's capture state.
    Status,
    /// What the auditor's ledger copy holds: one anchor per batch, hashes and counts only.
    Ledger {
        /// Show only the last N anchors.
        #[arg(long, default_value_t = 20)]
        last: usize,
        /// Also list every anchored leaf hash.
        #[arg(long)]
        leaves: bool,
    },
    /// List captured records with what each change means.
    Trail {
        /// Only this batch.
        #[arg(long)]
        batch: Option<u64>,
        /// Only changes flagged for an auditor's attention.
        #[arg(long)]
        flagged: bool,
        /// Only the last N records shown.
        #[arg(long, default_value_t = 40)]
        last: usize,
    },
    /// Prove one record is in its anchored batch, without revealing any other record.
    Proof {
        /// A record id (`DT-118.1`) or `table:key` (`invoices:id=1042`) for every captured version.
        target: String,
        /// Print the full event as hashed.
        #[arg(long)]
        event: bool,
        /// Write a self-contained proof bundle for a third party to check with `check-proof`.
        #[arg(long)]
        export: Option<PathBuf>,
    },
    /// Check an exported proof bundle against the auditor's ledger copy only.
    CheckProof { bundle: PathBuf },
    /// Presenter: act as an insider with write access to the node's audit store.
    Tamper {
        #[arg(value_enum)]
        action: TamperAction,
        /// Record id, e.g. DT-118.1 (see `trail`).
        record: String,
        /// After an edit, also rewrite the stored leaf hash so the row looks self-consistent.
        #[arg(long)]
        cover: bool,
    },
    /// Presenter: put back every record changed by `tamper`.
    Untamper,
    /// Try to archive (rewrite) an anchor on the ledger acting as ONE party. Canton refuses: every
    /// anchor is signed by every member.
    Rewrite {
        /// Batch number of the anchor to archive.
        seq: u64,
        /// The party attempting it (default: the company).
        #[arg(long = "as")]
        actor: Option<String>,
    },
    /// Auditor: recompute the trail and check every record against its anchored leaf.
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
}

#[derive(clap::Args)]
pub struct GenArgs {
    /// Output file.
    #[arg(long, default_value = "changes.jsonl")]
    pub out: PathBuf,
    /// Number of transactions.
    #[arg(long, default_value_t = 200)]
    pub tx: u64,
    /// RNG seed: the same seed always produces the same file.
    #[arg(long, default_value_t = 7)]
    pub seed: u64,
    /// Commit time of the first transaction (RFC 3339).
    #[arg(long, default_value = "2026-11-20T09:00:00Z")]
    pub start: String,
    /// Overwrite an existing file.
    #[arg(long)]
    pub force: bool,
}

#[derive(clap::Args)]
pub struct RunArgs {
    /// The change log to replay (written by `gen`; only used when the data directory is new).
    #[arg(long, default_value = "changes.jsonl")]
    pub input: PathBuf,
    /// Chain id (new data directory only).
    #[arg(long, default_value = "DT")]
    pub chain: String,
    /// The company's party (submitter).
    #[arg(long, default_value = "DREAMTALK")]
    pub company: String,
    /// The co-signing party that keeps its own copy of every anchor.
    #[arg(long, default_value = "AUDITOR")]
    pub auditor: String,
    /// Seconds per batch.
    #[arg(long, default_value_t = 10)]
    pub interval: u64,
    /// Transactions released per second from the change log (0: as fast as possible).
    #[arg(long, default_value_t = 2.0)]
    pub rate: f64,
    /// Stop after this many batches (default: run until Ctrl-C).
    #[arg(long)]
    pub batches: Option<u64>,
    /// Presenter: drop these GTID numbers before capture, as if the binlog lost them (comma separated).
    #[arg(long, value_delimiter = ',')]
    pub skip_gtids: Vec<u64>,
    /// JSON Ledger API v2 of the Canton participant (new data directory only).
    #[arg(long, default_value = "http://localhost:7575", env = "STHIRA_LEDGER_URL")]
    pub ledger_url: String,
    /// Ledger API user id sent with each command (new data directory only).
    #[arg(long, default_value = "sthira-node")]
    pub ledger_user: String,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TamperAction {
    /// Change a field of the record's row image (an amount, a status, a role, …).
    Edit,
    /// Remove the record from the store.
    Delete,
}
