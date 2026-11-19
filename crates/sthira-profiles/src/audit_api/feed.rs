//! Reading the audit API as an ordered, gap-aware stream.
//!
//! The endpoint lists rows **newest first**, by page number, at most 100 per page, with no "after id"
//! filter. New rows push older ones onto later pages while we read, so offsets drift. [`AuditCursor`]
//! turns that into an ascending stream:
//!
//! - It walks pages from 1 until it reaches a row it already resolved. Drift only ever repeats rows
//!   (they are deduplicated by id); it cannot skip one.
//! - It emits rows strictly in id order. If an id is missing, rows above it are held for a grace
//!   period, because a transaction that took its id earlier may commit later.
//! - After the grace period the missing ids are declared a **gap**. A row from a declared gap that
//!   shows up later is still emitted, marked **late**.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::{AuditEnvelope, AuditLogEntry, AuditPage};

#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("audit API request failed: {0}")]
    Http(String),
    #[error("audit API returned something that is not an audit page: {0}")]
    Decode(String),
}

/// Something that serves audit pages, newest first. Page numbers start at 1.
pub trait AuditFeed {
    fn page(&mut self, n: u32) -> Result<AuditPage, FeedError>;
}

impl<F: AuditFeed + ?Sized> AuditFeed for Box<F> {
    fn page(&mut self, n: u32) -> Result<AuditPage, FeedError> {
        (**self).page(n)
    }
}

/// The live endpoint, over HTTPS.
pub struct HttpFeed {
    agent: ureq::Agent,
    base: String,
    page_size: u32,
    /// Pause between page requests within one poll, to be polite to the source.
    pub delay: Duration,
    /// Number of HTTP requests made (each one also writes rows into the source's request log).
    pub requests: u64,
}

impl HttpFeed {
    /// `url` is the list endpoint. Any `page` / `page_size` parameters in it are replaced.
    pub fn new(url: &str, page_size: u32) -> Self {
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        let kept: Vec<&str> = query
            .split('&')
            .filter(|p| !p.is_empty() && !p.starts_with("page=") && !p.starts_with("page_size="))
            .collect();
        let base = if kept.is_empty() { format!("{path}?") } else { format!("{path}?{}&", kept.join("&")) };
        let agent: ureq::Agent =
            ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
        Self { agent, base, page_size: page_size.clamp(1, 100), delay: Duration::from_millis(150), requests: 0 }
    }

    pub fn page_url(&self, n: u32) -> String {
        format!("{}page={n}&page_size={}", self.base, self.page_size)
    }
}

impl AuditFeed for HttpFeed {
    fn page(&mut self, n: u32) -> Result<AuditPage, FeedError> {
        if n > 1 && !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.requests += 1;
        let url = self.page_url(n);
        let res = self.agent.get(&url).header("Accept", "application/json").call();
        let mut res = match res {
            Ok(r) => r,
            // Past the last page the endpoint answers 404 ("Invalid page").
            Err(ureq::Error::StatusCode(404)) => {
                return Ok(AuditPage { count: 0, next: None, previous: None, results: Vec::new() });
            }
            Err(e) => return Err(FeedError::Http(format!("{url}: {e}"))),
        };
        let body = res.body_mut().read_to_string().map_err(|e| FeedError::Http(e.to_string()))?;
        serde_json::from_str::<AuditEnvelope>(&body)
            .map(|e| e.response)
            .or_else(|_| serde_json::from_str::<AuditPage>(&body))
            .map_err(|e| FeedError::Decode(e.to_string()))
    }
}

/// An in-memory source for tests and offline runs: holds rows, serves them newest first.
/// `before_page` runs before each page is served, so a test can insert rows mid-walk (drift).
pub struct MemoryFeed {
    pub rows: Vec<AuditLogEntry>,
    pub page_size: usize,
    #[allow(clippy::type_complexity)]
    pub before_page: Option<Box<dyn FnMut(u32, &mut Vec<AuditLogEntry>)>>,
}

impl MemoryFeed {
    pub fn new(rows: Vec<AuditLogEntry>, page_size: usize) -> Self {
        Self { rows, page_size, before_page: None }
    }
}

impl AuditFeed for MemoryFeed {
    fn page(&mut self, n: u32) -> Result<AuditPage, FeedError> {
        if let Some(f) = self.before_page.as_mut() {
            f(n, &mut self.rows);
        }
        let mut sorted = self.rows.clone();
        sorted.sort_by_key(|r| std::cmp::Reverse(r.id));
        let start = (n.max(1) as usize - 1) * self.page_size;
        let results: Vec<AuditLogEntry> = sorted.iter().skip(start).take(self.page_size).cloned().collect();
        let next = (start + self.page_size < sorted.len()).then(|| format!("page={}", n + 1));
        Ok(AuditPage { count: sorted.len() as u64, next, previous: None, results })
    }
}

/// Ids the source's log skipped: declared after the grace period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapRange {
    pub from: u64,
    pub to: u64,
    /// Unix ms when the gap was declared.
    pub declared_ms: i64,
}

impl GapRange {
    pub fn len(&self) -> u64 {
        self.to - self.from + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Where a fresh cursor starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Only rows written after the first poll.
    Head,
    /// The newest `n` pages as a backfill, then everything after.
    BackfillPages(u32),
    /// Every row with an id of at least this.
    FromId(u64),
}

/// The result of one poll.
#[derive(Debug, Clone, Default)]
pub struct Polled {
    /// New rows in ascending id order, followed by late rows.
    pub rows: Vec<AuditLogEntry>,
    /// Ids among `rows` that arrived after their gap had been declared.
    pub late: Vec<u64>,
    /// Gaps declared during this poll.
    pub new_gaps: Vec<GapRange>,
    /// Pages read.
    pub pages: u32,
    /// Rows received but held back behind a missing id.
    pub held: usize,
}

/// A position in the audit log, persisted by the node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditCursor {
    /// Every id up to and including this one is resolved: emitted, or part of a declared gap.
    pub contiguous: u64,
    /// Declared gaps whose ids have not (all) arrived.
    pub gaps: Vec<GapRange>,
    /// How long to wait for a missing id before declaring a gap.
    pub grace_ms: i64,
    /// Rows received above a missing id, waiting for it.
    #[serde(skip)]
    held: BTreeMap<u64, AuditLogEntry>,
    /// When the current lowest missing id was first noticed.
    #[serde(skip)]
    hole_since: Option<i64>,
}

impl AuditCursor {
    /// A cursor that will emit every id strictly greater than `id`.
    pub fn after(id: u64) -> Self {
        Self { contiguous: id, gaps: Vec::new(), grace_ms: 30_000, held: BTreeMap::new(), hole_since: None }
    }

    /// Positions a fresh cursor by looking at the newest page(s).
    pub fn start(feed: &mut dyn AuditFeed, start: Start) -> Result<Self, FeedError> {
        Ok(match start {
            Start::FromId(id) => Self::after(id.saturating_sub(1)),
            Start::Head => {
                let p = feed.page(1)?;
                Self::after(p.results.iter().map(|r| r.id).max().unwrap_or(0))
            }
            Start::BackfillPages(n) => {
                let mut min = None;
                for i in 1..=n.max(1) {
                    let p = feed.page(i)?;
                    if let Some(m) = p.results.iter().map(|r| r.id).min() {
                        min = Some(min.map_or(m, |x: u64| x.min(m)));
                    }
                    if p.next.is_none() {
                        break;
                    }
                }
                Self::after(min.map_or(0, |m| m.saturating_sub(1)))
            }
        })
    }

    fn in_gap(&self, id: u64) -> Option<usize> {
        self.gaps.iter().position(|g| g.from <= id && id <= g.to)
    }

    /// Removes `id` from the declared gap at `i`, splitting it if needed.
    fn fill(&mut self, i: usize, id: u64) {
        let g = self.gaps.remove(i);
        if g.from < id {
            self.gaps.push(GapRange { from: g.from, to: id - 1, declared_ms: g.declared_ms });
        }
        if id < g.to {
            self.gaps.push(GapRange { from: id + 1, to: g.to, declared_ms: g.declared_ms });
        }
        self.gaps.sort_by_key(|g| g.from);
    }

    /// Reads every row newer than the cursor and returns what may be sealed now.
    pub fn poll(&mut self, feed: &mut dyn AuditFeed, now_ms: i64) -> Result<Polled, FeedError> {
        let mut out = Polled::default();
        let mut late: BTreeMap<u64, AuditLogEntry> = BTreeMap::new();
        let mut n = 1;
        loop {
            let page = feed.page(n)?;
            out.pages += 1;
            let mut reached = page.results.is_empty();
            for r in page.results {
                if r.id > self.contiguous {
                    self.held.entry(r.id).or_insert(r);
                } else {
                    reached = true;
                    if self.in_gap(r.id).is_some() {
                        late.entry(r.id).or_insert(r);
                    }
                }
            }
            if reached || page.next.is_none() {
                break;
            }
            n += 1;
        }

        loop {
            // Emit everything that continues the sequence.
            while let Some(r) = self.held.remove(&(self.contiguous + 1)) {
                self.contiguous += 1;
                out.rows.push(r);
            }
            let Some(&next) = self.held.keys().next() else {
                self.hole_since = None;
                break;
            };
            let since = *self.hole_since.get_or_insert(now_ms);
            if now_ms - since < self.grace_ms {
                break;
            }
            let gap = GapRange { from: self.contiguous + 1, to: next - 1, declared_ms: now_ms };
            self.contiguous = next - 1;
            self.gaps.push(gap.clone());
            out.new_gaps.push(gap);
            self.hole_since = None;
        }

        for (id, r) in late {
            if let Some(i) = self.in_gap(id) {
                self.fill(i, id);
                out.late.push(id);
                out.rows.push(r);
            }
        }
        out.held = self.held.len();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: u64) -> AuditLogEntry {
        serde_json::from_value(json!({
            "id": id, "operation": "INSERT", "table_name": "api_request_log", "record_id": id.to_string(),
            "new_data": {"id": id}, "created_at": "2026-11-18T00:00:00Z"
        }))
        .unwrap()
    }

    fn ids(p: &Polled) -> Vec<u64> {
        p.rows.iter().map(|r| r.id).collect()
    }

    #[test]
    fn walks_back_to_the_cursor_and_emits_ascending() {
        let mut feed = MemoryFeed::new((1..=250).map(row).collect(), 100);
        let mut c = AuditCursor::after(20);
        let p = c.poll(&mut feed, 0).unwrap();
        assert_eq!(ids(&p), (21..=250).collect::<Vec<_>>());
        assert_eq!(p.pages, 3);
        assert_eq!(c.contiguous, 250);
        // Nothing new: one page, nothing emitted.
        let p = c.poll(&mut feed, 1).unwrap();
        assert!(p.rows.is_empty() && p.pages == 1);
    }

    #[test]
    fn drift_while_paging_repeats_rows_but_never_skips_one() {
        let mut feed = MemoryFeed::new((1..=300).map(row).collect(), 100);
        let mut next = 301;
        // Before each page, 37 new rows arrive at the top, pushing everything down.
        feed.before_page = Some(Box::new(move |_, rows| {
            for _ in 0..37 {
                rows.push(row(next));
                next += 1;
            }
        }));
        let mut c = AuditCursor::after(0);
        let mut seen = Vec::new();
        for t in 0..5 {
            seen.extend(ids(&c.poll(&mut feed, t).unwrap()));
        }
        let expect: Vec<u64> = (1..=*seen.last().unwrap()).collect();
        assert_eq!(seen, expect, "every id exactly once, in order");
    }

    #[test]
    fn a_missing_id_is_held_then_declared_a_gap_and_late_arrivals_are_marked() {
        let mut feed = MemoryFeed::new((1..=10).filter(|i| *i != 6).map(row).collect(), 100);
        let mut c = AuditCursor::after(0);
        c.grace_ms = 30_000;
        let p = c.poll(&mut feed, 0).unwrap();
        assert_eq!(ids(&p), vec![1, 2, 3, 4, 5]);
        assert_eq!(p.held, 4);
        // Within the grace period: still waiting.
        assert!(c.poll(&mut feed, 10_000).unwrap().rows.is_empty());
        // After it: 6 is a gap and the rest flows.
        let p = c.poll(&mut feed, 31_000).unwrap();
        assert_eq!(ids(&p), vec![7, 8, 9, 10]);
        assert_eq!(p.new_gaps.len(), 1);
        assert_eq!((p.new_gaps[0].from, p.new_gaps[0].to), (6, 6));
        // 6 commits after all: emitted as late, gap closed.
        feed.rows.push(row(6));
        let p = c.poll(&mut feed, 40_000).unwrap();
        assert_eq!(ids(&p), vec![6]);
        assert_eq!(p.late, vec![6]);
        assert!(c.gaps.is_empty());
    }

    #[test]
    fn a_hole_that_fills_within_the_grace_period_is_not_a_gap() {
        let mut feed = MemoryFeed::new(vec![row(1), row(3)], 100);
        let mut c = AuditCursor::after(0);
        assert_eq!(ids(&c.poll(&mut feed, 0).unwrap()), vec![1]);
        feed.rows.push(row(2));
        let p = c.poll(&mut feed, 5_000).unwrap();
        assert_eq!(ids(&p), vec![2, 3]);
        assert!(p.new_gaps.is_empty() && c.gaps.is_empty());
    }

    #[test]
    fn start_positions() {
        let mut feed = MemoryFeed::new((1..=250).map(row).collect(), 100);
        assert_eq!(AuditCursor::start(&mut feed, Start::Head).unwrap().contiguous, 250);
        assert_eq!(AuditCursor::start(&mut feed, Start::BackfillPages(2)).unwrap().contiguous, 50);
        assert_eq!(AuditCursor::start(&mut feed, Start::FromId(7)).unwrap().contiguous, 6);
    }

    #[test]
    fn url_keeps_filters_and_replaces_paging() {
        let f = HttpFeed::new("https://h.example/audit/?page=3&page_size=100&table_name=app_user", 100);
        assert_eq!(f.page_url(1), "https://h.example/audit/?table_name=app_user&page=1&page_size=100");
        let f = HttpFeed::new("https://h.example/audit/", 50);
        assert_eq!(f.page_url(2), "https://h.example/audit/?page=2&page_size=50");
    }
}
