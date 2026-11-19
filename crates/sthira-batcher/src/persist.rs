//! Durable state for a [`FeedNode`], as plain files in one data directory.
//!
//! ```text
//! <data>/
//!   ledger/                 stand-in for the ledger (what every member's participant node holds)
//!     chain.json            chain id, members, submitter, genesis head, chain start, source API
//!     anchors.jsonl         one AuditAnchor per line, in seq order
//!   node/                   the company's node: its off-ledger audit store
//!     trail.jsonl           one FeedRow per captured event (event, salt, leaf hash, proof, raw row)
//!     batches.jsonl         the node's own batch table
//!     cursor.json           position in the source's log, declared gaps
//!     tamper-log.jsonl      originals of rows altered by the presenter's `tamper` command
//!     .lock                 serialises writers (the running node and `tamper`)
//! ```
//!
//! With `ledger.kind = canton` in `chain.json`, the anchors live on a Canton participant and
//! `anchors.jsonl` is only the node's local receipt; verifiers read the ledger itself.
//!
//! Commit order per batch is trail → batches → anchor → cursor. On restart, anything sealed locally
//! but not anchored is dropped, and the cursor resumes from its last saved position, so rows are
//! re-read rather than lost; the node's dedupe makes the re-read harmless. With Canton, `ledger/` is
//! replaced by the participant nodes; the node directory stays as it is.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sthira_ledger::canton::{CantonConfig, CantonLedger};
use sthira_ledger::{Anchor, AnchorInput, AuditLedger, SimLedger};
use sthira_profiles::audit_api::AuditCursor;

use crate::feed_node::{FeedBatch, FeedNode, FeedRow, Sealed};

/// The chain's identity, fixed at `init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainInfo {
    pub chain_id: String,
    /// The party whose node captures and appends (the company).
    pub submitter: String,
    /// Every party that co-signs the chain's anchors.
    pub members: Vec<String>,
    pub genesis_head: String,
    pub chain_start: String,
    pub api_url: String,
    pub system: String,
    /// Where the anchors live. Older data directories have no field and use the SimLedger.
    #[serde(default)]
    pub ledger: LedgerConfig,
}

/// The ledger a chain is anchored on, fixed when the chain is created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LedgerConfig {
    /// In-memory stand-in, replayed from `ledger/anchors.jsonl` on every start.
    #[default]
    Sim,
    /// A Canton participant through the JSON Ledger API v2 (the sandbox in `prototype-sandbox/`).
    Canton(CantonConfig),
}

/// The original of a row the presenter altered or deleted, so `untamper` can put it back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TamperEntry {
    pub row_id: String,
    pub action: String,
    pub description: String,
    pub original: FeedRow,
}

/// Paths inside a data directory.
#[derive(Debug, Clone)]
pub struct DataDir {
    pub root: PathBuf,
}

/// A held write lock on the node directory; released when dropped.
pub struct WriteLock(#[allow(dead_code)] File);

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn chain(&self) -> PathBuf {
        self.root.join("ledger/chain.json")
    }
    pub fn anchors(&self) -> PathBuf {
        self.root.join("ledger/anchors.jsonl")
    }
    pub fn trail(&self) -> PathBuf {
        self.root.join("node/trail.jsonl")
    }
    pub fn batches(&self) -> PathBuf {
        self.root.join("node/batches.jsonl")
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
        fs::create_dir_all(self.root.join("ledger"))?;
        fs::create_dir_all(self.root.join("node"))?;
        write_json(&self.chain(), info)?;
        for p in [self.anchors(), self.trail(), self.batches()] {
            File::create(p)?;
        }
        Ok(())
    }

    pub fn info(&self) -> io::Result<ChainInfo> {
        read_json(&self.chain())
    }

    pub fn load_anchors(&self) -> io::Result<Vec<Anchor>> {
        Ok(read_jsonl(&self.anchors())?.0)
    }

    /// The trail at `path` (default: the node's own), with the number of unreadable lines skipped.
    pub fn load_trail_from(&self, path: Option<&Path>) -> io::Result<(Vec<FeedRow>, usize)> {
        read_jsonl(path.unwrap_or(&self.trail()))
    }

    pub fn load_batches(&self) -> io::Result<Vec<FeedBatch>> {
        Ok(read_jsonl(&self.batches())?.0)
    }

    pub fn load_cursor(&self) -> io::Result<Option<AuditCursor>> {
        if !self.cursor().exists() {
            return Ok(None);
        }
        read_json(&self.cursor()).map(Some)
    }

    pub fn load_tamper_log(&self) -> io::Result<Vec<TamperEntry>> {
        if !self.tamper_log().exists() {
            return Ok(Vec::new());
        }
        Ok(read_jsonl(&self.tamper_log())?.0)
    }

    /// Persists one sealed batch, in commit order, then the cursor.
    pub fn commit(&self, sealed: &Sealed, cursor: &AuditCursor) -> io::Result<()> {
        let _lock = self.lock()?;
        append_jsonl(&self.trail(), &sealed.rows)?;
        append_jsonl(&self.batches(), std::slice::from_ref(&sealed.batch))?;
        append_jsonl(&self.anchors(), std::slice::from_ref(&sealed.anchor))?;
        write_json(&self.cursor(), cursor)
    }

    /// Opens the chain's ledger without touching the node's files: the SimLedger is replayed from
    /// `ledger/anchors.jsonl`; Canton is connected and every member's view is loaded. A Canton
    /// chain that does not exist yet is created when `create` is set and nothing was anchored before.
    pub fn open_ledger(&self, create: bool) -> Result<(ChainInfo, Box<dyn AuditLedger>), String> {
        let info = self.info().map_err(|e| format!("reading {}: {e}", self.chain().display()))?;
        let local = self.load_anchors().map_err(|e| e.to_string())?;
        match &info.ledger {
            LedgerConfig::Sim => {
                let mut ledger = SimLedger::new();
                for m in &info.members {
                    ledger.add_party(m);
                }
                ledger
                    .create_chain(&info.chain_id, &info.submitter, &info.members, &info.genesis_head)
                    .map_err(|e| e.0)?;
                for a in &local {
                    ledger.append(&info.chain_id, &info.submitter, input_of(a)).map_err(|e| {
                        format!("replaying anchor #{} from {}: {}", a.seq, self.anchors().display(), e.0)
                    })?;
                }
                Ok((info, Box::new(ledger)))
            }
            LedgerConfig::Canton(cfg) => {
                let mut ledger = CantonLedger::connect(cfg.clone())
                    .map_err(|e| format!("{}. Is the sandbox running? (prototype-sandbox/sandbox.sh)", e.0))?;
                for m in &info.members {
                    ledger.add_party(m);
                }
                ledger.refresh().map_err(|e| e.0)?;
                let on_ledger = ledger.copy(&info.submitter, &info.chain_id).cloned();
                match on_ledger {
                    None if local.is_empty() && create => {
                        ledger
                            .create_chain(&info.chain_id, &info.submitter, &info.members, &info.genesis_head)
                            .map_err(|e| format!("creating chain {} on Canton: {}", info.chain_id, e.0))?;
                    }
                    None => {
                        return Err(format!(
                            "Canton at {} has no chain {}. The sandbox keeps its state in memory, so a restarted \
                             sandbox has forgotten it; start a new chain with another --data directory",
                            cfg.url, info.chain_id
                        ));
                    }
                    Some(c) => {
                        if local.is_empty() && !c.anchors.is_empty() {
                            return Err(format!(
                                "chain {} already exists on this ledger with {} anchors from another data directory; \
                                 pick another chain id with `run --chain …` in a new --data directory",
                                info.chain_id,
                                c.anchors.len()
                            ));
                        }
                        if c.genesis_head != info.genesis_head {
                            return Err(format!("Canton's chain {} has a different genesis head", info.chain_id));
                        }
                        // The ledger must hold the same history this directory recorded.
                        for a in &local {
                            match c.anchors.iter().find(|x| x.seq == a.seq) {
                                Some(x) if x.head == a.head => {}
                                _ => {
                                    return Err(format!(
                                        "Canton's chain {} does not match this data directory at batch #{} \
                                         (was the sandbox restarted and the chain created again?)",
                                        info.chain_id, a.seq
                                    ));
                                }
                            }
                        }
                    }
                }
                Ok((info, Box::new(ledger)))
            }
        }
    }

    /// The anchors as `party`'s ledger copy holds them, and a label saying where they came from.
    /// This is what a verifier compares against: never the node's own files when Canton is used.
    pub fn anchors_as(&self, party: &str) -> Result<(Vec<Anchor>, String), String> {
        let info = self.info().map_err(|e| e.to_string())?;
        match &info.ledger {
            LedgerConfig::Sim => Ok((
                self.load_anchors().map_err(|e| e.to_string())?,
                format!("SimLedger stand-in ({})", self.anchors().display()),
            )),
            LedgerConfig::Canton(_) => {
                let (info, ledger) = self.open_ledger(false)?;
                let copy = ledger
                    .copy(party, &info.chain_id)
                    .ok_or_else(|| format!("{party} holds no copy of chain {} on Canton", info.chain_id))?;
                Ok((copy.anchors.clone(), format!("{party}'s view on Canton")))
            }
        }
    }

    /// Rebuilds the node and its ledger from disk, ready to keep appending.
    ///
    /// Batches sealed locally but never anchored (a crash between the two) are dropped from the
    /// node's files; their rows will be read from the source again.
    pub fn restore(&self) -> Result<Restored, String> {
        let (info, ledger) = self.open_ledger(true)?;
        let anchors = ledger.copy(&info.submitter, &info.chain_id).map(|c| c.anchors.clone()).unwrap_or_default();
        let last = anchors.last().map_or(0, |a| a.seq);

        let _lock = self.lock().map_err(|e| e.to_string())?;
        let (mut trail, _) = self.load_trail_from(None).map_err(|e| e.to_string())?;
        let mut batches = self.load_batches().map_err(|e| e.to_string())?;
        let saved = batches.last().map_or(0, |b| b.seq);
        if saved < last {
            return Err(format!(
                "the ledger holds batches up to #{last} but this node only saved up to #{saved}; the node stopped \
                 between anchoring and saving. Start a new chain with another --data directory"
            ));
        }
        let (t0, b0) = (trail.len(), batches.len());
        trail.retain(|r| r.batch_seq <= last);
        batches.retain(|b| b.seq <= last);
        if trail.len() != t0 || batches.len() != b0 {
            rewrite_jsonl(&self.trail(), &trail).map_err(|e| e.to_string())?;
            rewrite_jsonl(&self.batches(), &batches).map_err(|e| e.to_string())?;
        }
        let cursor = self.load_cursor().map_err(|e| e.to_string())?;
        let node = FeedNode::new(&info.submitter, &info.chain_id, &info.system, info.chain_start.clone())
            .restore(trail, batches);
        Ok(Restored { info, node, ledger, cursor })
    }
}

/// A node and its ledger, rebuilt from disk.
pub struct Restored {
    pub info: ChainInfo,
    pub node: FeedNode,
    pub ledger: Box<dyn AuditLedger>,
    pub cursor: Option<AuditCursor>,
}

/// The submission that produced an anchor.
pub fn input_of(a: &Anchor) -> AnchorInput {
    AnchorInput {
        seq: a.seq,
        prev_head: a.prev_head.clone(),
        head: a.head.clone(),
        merkle_root: a.merkle_root.clone(),
        event_count: a.event_count,
        from_pos: a.from_pos.clone(),
        to_pos: a.to_pos.clone(),
        from_time: a.from_time.clone(),
        to_time: a.to_time.clone(),
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
    let _ = fs::remove_file(&tmp);
    append_jsonl(&tmp, items)?;
    if items.is_empty() {
        File::create(&tmp)?;
    }
    fs::rename(tmp, path)
}

/// Reads a JSONL file. Lines that do not parse (a half-written last line, or a hand-edited one) are
/// skipped and counted.
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
    use sthira_core::genesis_head;
    use sthira_profiles::audit_api::AuditLogEntry;

    fn row(id: u64) -> AuditLogEntry {
        serde_json::from_value(serde_json::json!({
            "id": id, "operation": "INSERT", "table_name": "t", "record_id": id.to_string(),
            "new_data": {"id": id}, "created_at": "2026-11-18T00:00:00Z"
        }))
        .unwrap()
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sthira-persist-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn commit_restore_and_drop_unanchored() {
        let dir = DataDir::new(tmpdir("restore"));
        let info = ChainInfo {
            chain_id: "DT".into(),
            submitter: "DT".into(),
            members: vec!["DT".into(), "AUDITOR".into()],
            genesis_head: genesis_head("DT"),
            chain_start: "2026-11-18T00:00:00.000000Z".into(),
            api_url: "memory".into(),
            system: "api.example".into(),
            ledger: LedgerConfig::Sim,
        };
        dir.init(&info).unwrap();
        assert!(dir.init(&info).is_err(), "never overwrites a chain");

        let mut r = dir.restore().unwrap();
        let mut cursor = AuditCursor::after(0);
        r.node.ingest(&(1..=4).map(row).collect::<Vec<_>>(), &[], "t");
        let s = r.node.seal(r.ledger.as_mut(), "2026-11-18T00:00:10.000000Z".into()).unwrap();
        cursor.contiguous = 4;
        dir.commit(&s, &cursor).unwrap();

        // A batch sealed and written locally, but whose anchor never reached the ledger file.
        r.node.ingest(&[row(5)], &[], "t");
        let s2 = r.node.seal(r.ledger.as_mut(), "2026-11-18T00:00:20.000000Z".into()).unwrap();
        append_jsonl(&dir.trail(), &s2.rows).unwrap();
        append_jsonl(&dir.batches(), &[s2.batch]).unwrap();

        let mut r = dir.restore().unwrap();
        assert_eq!(r.node.store.len(), 4);
        assert_eq!(r.node.last_seq(), 1);
        assert_eq!(r.cursor.unwrap().contiguous, 4);
        // Row 5 is read again and sealed as batch 2; rows 1-4 are not stored twice.
        assert_eq!(r.node.ingest(&(1..=5).map(row).collect::<Vec<_>>(), &[], "t"), 1);
        let s = r.node.seal(r.ledger.as_mut(), "2026-11-18T00:00:30.000000Z".into()).unwrap();
        assert_eq!(s.batch.seq, 2);
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
        let _ = fs::remove_dir_all(&d);
    }
}
