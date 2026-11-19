//! [`ReplaySource`]: a generated binlog file played back as a live change stream.
//!
//! It reads the JSONL written by [`crate::generator::write_jsonl`] line by line (async, so a large
//! file is never loaded at once), releases at most `rate` transactions per second, and stamps each
//! transaction with the capture time from its clock closure.
//!
//! - `after_tx` resumes a stopped node: transactions up to and including it are passed over.
//! - `skip` drops transactions as if they never reached the binary log the node reads (a purged log
//!   file, or a session with `sql_log_bin=0`). The node then sees a hole in the GTID sequence, which
//!   the anchors make visible to every member.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use sthira_core::{CapturedTx, ChangeSource, SourceError};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};

use crate::mysql::{BinlogTx, normalize_tx};

#[derive(Debug, Clone, Default)]
pub struct ReplayOptions {
    /// Chain id written into every event.
    pub chain: String,
    /// Resume after this GTID number (0: from the start).
    pub after_tx: u64,
    /// GTID numbers to drop before capture.
    pub skip: BTreeSet<u64>,
    /// Transactions per second; 0 releases them as fast as they are read.
    pub rate: f64,
}

pub struct ReplaySource<C> {
    lines: Lines<BufReader<File>>,
    line: u64,
    opts: ReplayOptions,
    pace: Option<Duration>,
    clock: C,
    dropped: Vec<u64>,
}

impl<C> ReplaySource<C>
where
    C: FnMut() -> String + Send,
{
    /// Opens a generated file. `clock` gives the capture time of each transaction.
    pub async fn open(path: &Path, opts: ReplayOptions, clock: C) -> Result<Self, SourceError> {
        let f = File::open(path).await.map_err(|e| SourceError::Io(format!("{}: {e}", path.display())))?;
        let pace = (opts.rate > 0.0).then(|| Duration::from_secs_f64(1.0 / opts.rate));
        Ok(Self { lines: BufReader::new(f).lines(), line: 0, opts, pace, clock, dropped: Vec::new() })
    }

    /// GTID numbers dropped so far because of `skip`.
    pub fn dropped(&self) -> &[u64] {
        &self.dropped
    }
}

impl<C> ChangeSource for ReplaySource<C>
where
    C: FnMut() -> String + Send,
{
    async fn next_tx(&mut self) -> Result<Option<CapturedTx>, SourceError> {
        loop {
            let Some(text) = self.lines.next_line().await.map_err(|e| SourceError::Io(e.to_string()))? else {
                return Ok(None);
            };
            self.line += 1;
            if text.trim().is_empty() {
                continue;
            }
            let t: BinlogTx = serde_json::from_str(&text)
                .map_err(|e| SourceError::Decode { at: format!("line {}", self.line), msg: e.to_string() })?;
            if t.tx <= self.opts.after_tx {
                continue;
            }
            if self.opts.skip.contains(&t.tx) {
                self.dropped.push(t.tx);
                continue;
            }
            if let Some(p) = self.pace {
                tokio::time::sleep(p).await;
            }
            let captured_at = (self.clock)();
            return Ok(Some(normalize_tx(&t, &self.opts.chain, &captured_at)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::{GenConfig, write_jsonl};

    async fn all<C: FnMut() -> String + Send>(mut s: ReplaySource<C>) -> (Vec<u64>, Vec<u64>) {
        let mut got = Vec::new();
        while let Some(t) = s.next_tx().await.unwrap() {
            assert!(t.events.iter().all(|e| e.captured_at == "T" && e.chain == "DT"));
            got.push(t.tx);
        }
        (got, s.dropped().to_vec())
    }

    #[tokio::test]
    async fn resumes_skips_and_reports_bad_lines() {
        let dir = std::env::temp_dir().join(format!("sthira-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("changes.jsonl");
        assert_eq!(write_jsonl(&GenConfig { txs: 20, ..GenConfig::default() }, &path).await.unwrap().0, 20);

        let opts = ReplayOptions { chain: "DT".into(), after_tx: 5, skip: [8, 9].into_iter().collect(), rate: 0.0 };
        let (got, dropped) = all(ReplaySource::open(&path, opts, || "T".to_string()).await.unwrap()).await;
        assert_eq!(got, [6, 7, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]);
        assert_eq!(dropped, [8, 9]);

        std::fs::write(&path, "{\"not\": \"a tx\"}\n").unwrap();
        let mut s = ReplaySource::open(&path, ReplayOptions::default(), || "T".to_string()).await.unwrap();
        assert!(matches!(s.next_tx().await, Err(SourceError::Decode { .. })));
        assert!(ReplaySource::open(&dir.join("missing"), ReplayOptions::default(), String::new).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(start_paused = true)]
    async fn paces_at_the_given_rate() {
        let dir = std::env::temp_dir().join(format!("sthira-replay-pace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("changes.jsonl");
        write_jsonl(&GenConfig { txs: 4, ..GenConfig::default() }, &path).await.unwrap();
        let opts = ReplayOptions { chain: "DT".into(), rate: 2.0, ..Default::default() };
        let start = tokio::time::Instant::now();
        let (got, _) = all(ReplaySource::open(&path, opts, || "T".to_string()).await.unwrap()).await;
        assert_eq!(got.len(), 4);
        assert_eq!(start.elapsed(), Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
