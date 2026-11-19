//! Durable state for a [`Node`], as plain files in one data directory.
//!
//! ```text
//! <data>/
//!   chain.json              chain id, members, submitter, genesis head, chain start, source, ledger
//!   node/                   the company's node: its off-ledger audit store
//!     trail.jsonl           one StoredRow per captured event (event, salt, leaf hash)
//!     batches.jsonl         the node's own batch table
//!     receipts.jsonl        the anchors as the ledger returned them (verifiers read the ledger itself)
//!     cursor.json           the last GTID number captured and sealed
//!     tamper-log.jsonl      originals of rows altered by the presenter's `tamper` command
//!     .lock                 serialises writers (the running node and `tamper`)
//! ```
//!
//! Commit order per batch is trail → batches → receipt → cursor. On restart, anything sealed locally
//! but not on the ledger is dropped, and capture resumes after the cursor, so transactions are
//! re-read rather than lost.
//!
//! This module knows nothing about the ledger: the caller says how far the ledger got.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sthira_core::Anchor;

use crate::node::{BatchRecord, Node, Sealed, StoredRow};

/// The chain's identity, fixed when the data directory is created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainInfo {
    pub chain_id: String,
    /// The party whose node captures and appends (the company).
    pub submitter: String,
    /// Every party that co-signs the chain's anchors.
    pub members: Vec<String>,
    pub genesis_head: String,
    pub chain_start: String,
    /// The change log the node captures from (the generated binlog file).
    pub source: String,
    /// JSON Ledger API of the participant the chain is anchored on.
    pub ledger_url: String,
    pub ledger_user: String,
}

/// Where capture stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// The last GTID number captured and sealed.
    pub last_tx: u64,
}

/// The original of a row the presenter altered or deleted, so `untamper` can put it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TamperEntry {
    pub row_id: String,
    pub action: String,
    pub description: String,
    pub original: StoredRow,
}

/// Paths inside a data directory.
#[derive(Debug, Clone)]
pub struct DataDir {
    pub root: PathBuf,
}

/// A held write lock on the node directory; released when dropped.
pub struct WriteLock(#[allow(dead_code)] File);

/// A node rebuilt from disk.
pub struct Restored {
    pub info: ChainInfo,
    pub node: Node,
    pub cursor: Cursor,
}

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn chain(&self) -> PathBuf {
        self.root.join("chain.json")
    }
    pub fn trail(&self) -> PathBuf {
        self.root.join("node/trail.jsonl")
    }
    pub fn batches(&self) -> PathBuf {
        self.root.join("node/batches.jsonl")
    }
    pub fn receipts(&self) -> PathBuf {
        self.root.join("node/receipts.jsonl")
    }
    pub fn cursor(&self) -> PathBuf {
        self.root.join("node/cursor.json")
    }
    pub fn tamper_log(&self) -> PathBuf {
        self.root.join("node/tamper-log.jsonl")
    }

    pub fn exists(&self) -> bool {
        self.chain().exists()
    }

    /// Blocks until no other writer holds the node directory.
    pub fn lock(&self) -> io::Result<WriteLock> {
        fs::create_dir_all(self.root.join("node"))?;
        let f = OpenOptions::new().create(true).truncate(false).write(true).open(self.root.join("node/.lock"))?;
        f.lock()?;
        Ok(WriteLock(f))
    }

    /// Creates a fresh data directory for a new chain. Refuses to overwrite an existing one.
    pub fn init(&self, info: &ChainInfo) -> io::Result<()> {
        if self.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already holds a chain; use another --data directory", self.root.display()),
            ));
        }
        fs::create_dir_all(self.root.join("node"))?;
        write_json(&self.chain(), info)?;
        for p in [self.trail(), self.batches(), self.receipts()] {
            File::create(p)?;
        }
        Ok(())
    }

    pub fn info(&self) -> io::Result<ChainInfo> {
        read_json(&self.chain())
    }

    pub fn load_receipts(&self) -> io::Result<Vec<Anchor>> {
        Ok(read_jsonl(&self.receipts())?.0)
    }

    /// The trail at `path` (default: the node's own), with the number of unreadable lines skipped.
    pub fn load_trail_from(&self, path: Option<&Path>) -> io::Result<(Vec<StoredRow>, usize)> {
        read_jsonl(path.unwrap_or(&self.trail()))
    }

    pub fn load_batches(&self) -> io::Result<Vec<BatchRecord>> {
        Ok(read_jsonl(&self.batches())?.0)
    }

    pub fn load_cursor(&self) -> io::Result<Cursor> {
        if !self.cursor().exists() {
            return Ok(Cursor::default());
        }
        read_json(&self.cursor())
    }

    pub fn load_tamper_log(&self) -> io::Result<Vec<TamperEntry>> {
        Ok(read_jsonl(&self.tamper_log())?.0)
    }

    /// Persists one sealed batch, in commit order, then the cursor.
    pub fn commit(&self, sealed: &Sealed) -> io::Result<()> {
        let _lock = self.lock()?;
        append_jsonl(&self.trail(), &sealed.rows)?;
        append_jsonl(&self.batches(), std::slice::from_ref(&sealed.batch))?;
        append_jsonl(&self.receipts(), std::slice::from_ref(&sealed.anchor))?;
        write_json(&self.cursor(), &Cursor { last_tx: sealed.batch.to_tx })
    }

    /// Rebuilds the node from disk, given the last batch number the ledger holds.
    ///
    /// Batches sealed locally but never anchored (a crash between the two) are dropped from the
    /// node's files; their transactions will be read from the source again.
    pub fn restore(&self, anchored_seq: u64) -> Result<Restored, String> {
        let info = self.info().map_err(|e| format!("reading {}: {e}", self.chain().display()))?;
        let _lock = self.lock().map_err(|e| e.to_string())?;
        let (mut trail, _) = self.load_trail_from(None).map_err(|e| e.to_string())?;
        let mut batches = self.load_batches().map_err(|e| e.to_string())?;
        let saved = batches.last().map_or(0, |b| b.seq);
        if saved < anchored_seq {
            return Err(format!(
                "the ledger holds batches up to #{anchored_seq} but this node only saved up to #{saved}; the node \
                 stopped between anchoring and saving. Start a new chain with another --data directory"
            ));
        }
        let (t0, b0) = (trail.len(), batches.len());
        trail.retain(|r| r.batch_seq <= anchored_seq);
        batches.retain(|b| b.seq <= anchored_seq);
        if trail.len() != t0 || batches.len() != b0 {
            rewrite_jsonl(&self.trail(), &trail).map_err(|e| e.to_string())?;
            rewrite_jsonl(&self.batches(), &batches).map_err(|e| e.to_string())?;
            let mut receipts = self.load_receipts().map_err(|e| e.to_string())?;
            receipts.retain(|a| a.seq <= anchored_seq);
            rewrite_jsonl(&self.receipts(), &receipts).map_err(|e| e.to_string())?;
        }
        let cursor = Cursor { last_tx: batches.iter().map(|b| b.to_tx).max().unwrap_or(0) };
        write_json(&self.cursor(), &cursor).map_err(|e| e.to_string())?;
        let node = Node::new(&info.submitter, &info.chain_id, info.chain_start.clone()).restore(trail, batches);
        Ok(Restored { info, node, cursor })
    }
}

pub fn write_json<T: Serialize>(path: &Path, v: &T) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    fs::rename(tmp, path)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub fn append_jsonl<T: Serialize>(path: &Path, items: &[T]) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut buf = Vec::new();
    for it in items {
        serde_json::to_writer(&mut buf, it)?;
        buf.push(b'\n');
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(&buf)?;
    f.sync_data()
}

/// Replaces a JSONL file atomically (write a temporary file, then rename it over the original).
pub fn rewrite_jsonl<T: Serialize>(path: &Path, items: &[T]) -> io::Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    File::create(&tmp)?;
    append_jsonl(&tmp, items)?;
    fs::rename(tmp, path)
}

/// Reads a JSONL file. Lines that do not parse (a half-written last line, or a hand-edited one) are
/// skipped and counted. A missing file reads as empty.
pub fn read_jsonl<T: DeserializeOwned>(path: &Path) -> io::Result<(Vec<T>, usize)> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    let mut bad = 0;
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(&line) {
            Ok(v) => out.push(v),
            Err(_) => bad += 1,
        }
    }
    Ok((out, bad))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_seal;
    use sthira_core::genesis_head;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sthira-persist-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// A sealed batch as the node would produce it, without a ledger.
    fn sealed(seq: u64, to_tx: u64) -> Sealed {
        let ev = serde_json::json!({ "n": seq });
        let plan = plan_seal("DT", &genesis_head("DT"), "T0", None, &[&ev], Some((to_tx, to_tx, 1)), "T1".into());
        let mut anchor_input = plan.anchor_input();
        anchor_input.seq = seq;
        let batch = BatchRecord {
            seq,
            prev_head: plan.prev_head.clone(),
            head: plan.head.clone(),
            merkle_root: plan.tree.hash.clone(),
            event_count: 1,
            tx_count: 1,
            from_tx: to_tx,
            to_tx,
            from_time: "T0".into(),
            to_time: "T1".into(),
            contract_id: format!("c{seq}"),
            update_id: format!("u{seq}"),
        };
        let anchor = Anchor {
            contract_id: batch.contract_id.clone(),
            chain_id: "DT".into(),
            seq,
            prev_head: batch.prev_head.clone(),
            head: batch.head.clone(),
            merkle_root: batch.merkle_root.clone(),
            leaves: plan.leaves.clone(),
            event_count: 1,
            tx_count: 1,
            from_tx: to_tx,
            to_tx,
            from_time: "T0".into(),
            to_time: "T1".into(),
            signatories: vec!["DT".into(), "AUDITOR".into()],
            update_id: batch.update_id.clone(),
            recorded_at: "T1".into(),
        };
        Sealed { batch, anchor, rows: Vec::new() }
    }

    #[test]
    fn commit_restore_and_drop_unanchored() {
        let dir = DataDir::new(tmpdir("restore"));
        let info = ChainInfo {
            chain_id: "DT".into(),
            submitter: "DT".into(),
            members: vec!["DT".into(), "AUDITOR".into()],
            genesis_head: genesis_head("DT"),
            chain_start: "T0".into(),
            source: "changes.jsonl".into(),
            ledger_url: "http://localhost:7575".into(),
            ledger_user: "sthira-node".into(),
        };
        dir.init(&info).unwrap();
        assert!(dir.init(&info).is_err(), "never overwrites a chain");
        dir.commit(&sealed(1, 4)).unwrap();
        dir.commit(&sealed(2, 9)).unwrap();
        assert_eq!(dir.load_cursor().unwrap().last_tx, 9);

        // The ledger only got batch 1: batch 2 is dropped and capture resumes after tx 4.
        let r = dir.restore(1).unwrap();
        assert_eq!((r.node.last_seq(), r.node.last_tx, r.cursor.last_tx), (1, 4, 4));
        assert_eq!(dir.load_receipts().unwrap().len(), 1);
        assert_eq!(dir.load_cursor().unwrap().last_tx, 4);
        // The ledger is ahead of the node: refuse.
        assert!(dir.restore(3).is_err());
        let _ = fs::remove_dir_all(&dir.root);
    }

    #[test]
    fn unreadable_lines_are_skipped_and_counted() {
        let d = tmpdir("jsonl");
        fs::create_dir_all(&d).unwrap();
        let p = d.join("x.jsonl");
        fs::write(&p, "{\"a\":1}\n{broken\n{\"a\":2}\n{\"a\":").unwrap();
        let (v, bad): (Vec<serde_json::Value>, usize) = read_jsonl(&p).unwrap();
        assert_eq!((v.len(), bad), (2, 2));
        rewrite_jsonl::<serde_json::Value>(&p, &[]).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "");
        let _ = fs::remove_dir_all(&d);
    }
}
