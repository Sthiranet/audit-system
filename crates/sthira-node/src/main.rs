//! `sthira-node`: the prototype, run from a terminal, and the composition root of the workspace.
//!
//! This is the only crate that picks concrete types: the connector's [`ReplaySource`] and the
//! ledger's [`CantonLedger`]. Everything else talks through the ports in `sthira-core`.
//!
//! One binary plays every role, each touching only what that party would hold:
//!
//! - the generator (`gen`): writes an offline MySQL binlog workload;
//! - the company's node (`run`): replays the change log, seals batches, anchors them on Canton and
//!   keeps the off-ledger audit store under `<data>/node/`;
//! - the auditor (`verify`, `check-proof`, `ledger`): recomputes from a trail and compares with its
//!   own ledger copy only;
//! - the presenter's insider (`tamper`, `untamper`), and a party trying to rewrite history on the
//!   ledger itself (`rewrite`).
//!
//! [`ReplaySource`]: sthira_connector::replay::ReplaySource
//! [`CantonLedger`]: sthira_ledger::CantonLedger

mod cli;
mod ledger;
mod run;
mod tamper;
mod views;

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use sthira_batcher::persist::{ChainInfo, DataDir};

use cli::{Cli, Cmd};

pub type Res<T> = Result<T, String>;

/// Terminal colours, off when not a terminal, with `--no-color` or `NO_COLOR`.
#[derive(Clone, Copy)]
pub struct Paint(bool);

impl Paint {
    fn wrap(&self, code: &str, s: impl AsRef<str>) -> String {
        if self.0 { format!("\x1b[{code}m{}\x1b[0m", s.as_ref()) } else { s.as_ref().to_string() }
    }
    pub fn ok(&self, s: impl AsRef<str>) -> String {
        self.wrap("32", s)
    }
    pub fn bad(&self, s: impl AsRef<str>) -> String {
        self.wrap("31;1", s)
    }
    pub fn warn(&self, s: impl AsRef<str>) -> String {
        self.wrap("33", s)
    }
    pub fn dim(&self, s: impl AsRef<str>) -> String {
        self.wrap("2", s)
    }
    pub fn bold(&self, s: impl AsRef<str>) -> String {
        self.wrap("1", s)
    }
    pub fn mark(&self, b: bool) -> String {
        if b { self.ok("✓") } else { self.bad("✗") }
    }
}

/// The first characters of a hash.
pub fn short(h: &str) -> String {
    h.chars().take(10).collect::<String>() + "…"
}

/// `HH:MM:SS` of an RFC 3339 timestamp.
pub fn clock(iso: &str) -> &str {
    iso.get(11..19).unwrap_or(iso)
}

/// The co-signer that plays the auditor: the last member.
pub fn auditor_of(info: &ChainInfo) -> &str {
    info.members.last().map_or("", String::as_str)
}

pub fn need_chain(dir: &DataDir) -> Res<ChainInfo> {
    if !dir.exists() {
        return Err(format!(
            "no chain in {} yet; start one with `sthira-node --data {} run`",
            dir.root.display(),
            dir.root.display()
        ));
    }
    dir.info().map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let p = Paint(!cli.no_color && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none());
    let dir = DataDir::new(&cli.data);
    let r = match cli.cmd {
        Cmd::Gen(a) => run::gen_cmd(a, p).await,
        Cmd::Run(a) => run::run(&dir, a, p).await,
        Cmd::Status => views::status(&dir, p),
        Cmd::Ledger { last, leaves } => views::ledger(&dir, last, leaves, p).await,
        Cmd::Trail { batch, flagged, last } => views::trail(&dir, batch, flagged, last, p),
        Cmd::Proof { target, event, export } => views::proof(&dir, &target, event, export.as_deref(), p).await,
        Cmd::CheckProof { bundle } => views::check_proof(&dir, &bundle, p).await,
        Cmd::Tamper { action, record, cover } => tamper::tamper(&dir, action, &record, cover, p),
        Cmd::Untamper => tamper::untamper(&dir, p),
        Cmd::Rewrite { seq, actor } => views::rewrite(&dir, seq, actor, p).await,
        Cmd::Verify { trail, all, json } => views::verify(&dir, trail.as_deref(), all, json, p).await,
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e}", p.bad("error:"));
            ExitCode::from(2)
        }
    }
}
