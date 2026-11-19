# The Sthiranet audit system prototype, from the bottom up

This is a guided tour of every file on the `prototype` branch. It is written for a programmer who
knows Rust-level concepts (traits, generics, `async`, `Arc`/`Mutex`, closures) and wants to know
**what this code does, why it is shaped the way it is, what it costs, and how the parts talk to
each other**.

The tour follows the dependency graph from the bottom up. Each chapter only uses things that earlier
chapters explained, so the story builds from the hash function up to the command-line tool. By the
end you should be able to open any file, know where it sits in the design, and extend it safely.

**Contents**

0. [The problem and the shape of the solution](#0-the-problem-and-the-shape-of-the-solution)
1. [The workspace](#1-the-workspace)
2. [`sthira-core`: the hashing spec and the ports](#2-sthira-core-the-hashing-spec-and-the-ports)
3. [`sthira-connector`: where change events come from](#3-sthira-connector-where-change-events-come-from)
4. [The Daml model: what the ledger enforces](#4-the-daml-model-what-the-ledger-enforces)
5. [`sthira-ledger`: talking to Canton](#5-sthira-ledger-talking-to-canton)
6. [`sthira-batcher`: sealing, anchoring and the audit store](#6-sthira-batcher-sealing-anchoring-and-the-audit-store)
7. [`sthira-verify`: the auditor's algorithm](#7-sthira-verify-the-auditors-algorithm)
8. [`sthira-node`: wiring it all together](#8-sthira-node-wiring-it-all-together)
9. [Tests](#9-tests)
10. [One row, end to end](#10-one-row-end-to-end)
11. [Building on it](#11-building-on-it)
12. [Known simplifications](#12-known-simplifications)
13. [File index](#13-file-index)

---

## 0. The problem and the shape of the solution

A company's database records every business change: orders, invoices, payments, user roles. An
auditor wants to trust that history. But the company controls the database, its change log, and any
audit copy it keeps, so on its own word the history proves nothing.

Sthiranet runs a **node** next to the database that reads the database's own change log (here, the
MySQL binary log). From each change it computes a **fingerprint**, and every few seconds it seals the
fingerprints into a **batch**. Each batch is **anchored** on a ledger (Canton) that the company and
the auditor both sign. The full records stay with the company, in an **off-ledger audit store**. The
ledger holds only hashes.

Later, the auditor takes the company's audit store and recomputes every fingerprint. It then compares
them with **its own copy** of the ledger. Any record changed, deleted or invented after capture shows
up, and so do transactions that never reached the node.

The trust model in one table:

| Party | Holds | Can do | Cannot do |
|---|---|---|---|
| Company node | full records + salts (audit store) | append anchors | rewrite or delete an anchor (needs the auditor's signature) |
| Auditor | its own ledger copy | verify any trail it is given | read records it was not given (only hashes are on the ledger) |
| Ledger operator (synchronizer) | encrypted, ordered messages | order transactions | read contents |

### The crates and how they depend on each other

```
                          ┌──────────────────────┐
                          │     sthira-core      │  spec v3 event, hashing, Merkle,
                          │  (pure, no I/O)      │  ledger data types, PORT TRAITS
                          └──────────▲───────────┘
          ┌───────────────┬──────────┼───────────────┬───────────────┐
          │               │          │               │               │
 ┌────────┴───────┐ ┌─────┴──────┐ ┌─┴────────────┐ ┌┴─────────────┐  │
 │sthira-connector│ │sthira-ledger│ │sthira-batcher│ │sthira-verify │  │
 │ ChangeSource   │ │ AuditLedger │ │ uses         │ │ uses         │  │
 │ impl (replay)  │ │ impl(Canton)│ │ AuditLedger, │ │ TrailRecord  │  │
 │                │ │             │ │ impl Trail-  │ │              │  │
 │                │ │             │ │ Record       │ │              │  │
 └────────▲───────┘ └─────▲──────┘ └──────▲───────┘ └──────▲───────┘  │
          └───────────────┴───────┬───────┴────────────────┘          │
                          ┌───────┴────────┐                          │
                          │  sthira-node   │──────────────────────────┘
                          │ CLI, composition root: picks the concrete
                          │ ReplaySource + CantonLedger, runs the loop
                          └────────────────┘
```

The rule that shapes everything: **library crates depend only on `sthira-core`**, and they reach
each other only through the three traits defined there (`ChangeSource`, `AuditLedger`,
`TrailRecord`). This is the ports-and-adapters (hexagonal) style. The batcher seals and anchors
through `AuditLedger` without knowing Canton exists, and the verifier checks any record type that
implements `TrailRecord`. `sthira-node` is the only crate that names concrete types.

The payoff: a production MySQL reader, an Oracle connector or another ledger is a new adapter crate
plus a few lines in `sthira-node`. Nothing else changes.

There is one more component outside Rust: the **Daml model** in `prototype-sandbox/`. It runs on the
Canton participant and enforces the chain's rules on the ledger side (chapter 4).

---

## 1. The workspace

| File | Purpose |
|---|---|
| `Cargo.toml` | Workspace: the six member crates, shared package metadata (edition 2024, `rust-version = 1.94`), and every external dependency pinned once under `[workspace.dependencies]`. Member crates write `serde.workspace = true`, so versions never drift between crates. |
| `rust-toolchain.toml` | Stable channel, with `rustfmt` and `clippy`. |
| `rustfmt.toml` | `max_width = 120`, `use_small_heuristics = "Max"`: wide lines, fewer forced breaks. |
| `Makefile` | The operational entry points (below). |
| `.gitignore` | Build output and everything a run generates: `target/`, data directories, `changes.jsonl`, proof bundles. |

**External dependencies** and why each one is there:

| Crate | Used by | Why |
|---|---|---|
| `serde`, `serde_json` | all | every structure is JSON on disk, on the ledger API, or in a hash |
| `serde_json_canonicalizer` | core | RFC 8785 canonical JSON (JCS), so a hash input is byte-identical everywhere |
| `sha2`, `hex` | core | SHA-256; hashes travel as lowercase hex |
| `rand` | core, connector, ledger | salts (OS randomness); the generator's seeded `StdRng`; unique command ids |
| `thiserror` | core | the port error types |
| `chrono` | connector, node | RFC 3339 timestamps |
| `tokio` | connector, ledger, node | async file I/O, timers, channels, signals, the runtime |
| `reqwest` (rustls) | ledger | async HTTP to the JSON Ledger API |
| `clap` | node | the CLI |
| `proptest` | core (dev) | property tests for the Merkle code |

**Makefile targets.** `gen` writes the dummy change log. `node` builds the release binary. `sandbox`
starts Canton. `daml-test` runs the Daml tests. `test-unit` runs the Rust tests that need no ledger.
`test` runs every test against a sandbox, starting and stopping one if none is running. `check` runs
fmt and clippy with `-D warnings`. `clean` deletes all build output and generated data.

The `test` target is worth reading. It starts the sandbox under `setsid`, so the sandbox gets its own
process group. A shell `trap … EXIT` then kills that whole group (`kill -- -$pid`). This matters
because `sandbox.sh` → `dpm` → `java` is a chain of processes: killing only the first would leave
Canton running.

---

## 2. `sthira-core`: the hashing spec and the ports

`crates/sthira-core/` has no I/O and no async runtime. It defines *what* is hashed and *how*, and the
interfaces everyone else implements. A pure crate is easy to test exhaustively and could be compiled
to WebAssembly, for example to let an auditor verify in a browser.

### 2.1 The primitives (`src/lib.rs`)

The module header states the whole spec:

```text
leaf    = H(0x00 || salt || JCS(event))
node    = H(0x01 || left || right)
root    = RFC 6962 Merkle tree hash over the batch's leaves (empty batch: H(""))
meta    = JCS({ chain, seq, from_tx, to_tx, from_time, to_time, event_count, tx_count })
head    = H(0x02 || prev_head || root || meta)
genesis = H(0x03 || "sthiranet/v1/genesis/" || chain)
```

- **`sha256_hex(parts: &[&[u8]])`** feeds each slice into one SHA-256 state and returns lowercase
  hex. Taking slices avoids building one concatenated buffer: O(total bytes) time, O(1) extra space.
- **Domain-separation tags** (`mod tag`: `LEAF=0x00`, `NODE=0x01`, `HEAD=0x02`, `GENESIS=0x03`). A
  hash of one kind can never be passed off as another. Without the tags, an attacker could present
  an inner node as a leaf (the classic second-preimage attack on Merkle trees). RFC 6962 uses the
  same 0x00/0x01 convention.
- **`hex_bytes(s)`** decodes a hex hash into raw bytes before hashing, so hashes are combined as 32
  raw bytes, not 64 ASCII characters. If the string is not valid hex (somebody hand-edited a stored
  value), it falls back to the raw string bytes. Verification still produces *a* hash, just a wrong
  one, and reports a mismatch instead of crashing.
- **`canonical(v)`** applies RFC 8785 JCS: keys sorted by UTF-16 code units, no whitespace,
  ECMAScript number formatting. **This is why hashes are reproducible.** Two `serde_json::Value`s
  with the same content but different key order canonicalise to the same bytes. It costs O(n log k)
  for n bytes, with k keys per object. It panics only on values JSON cannot represent, which our
  types never produce.
- **`new_salt()`** returns 32 bytes from the OS random generator, as hex. The salt is why a leaf
  reveals nothing. Many records are guessable (an invoice status is one of four values), so without a
  salt anyone holding the ledger could hash candidate records and test them against the leaves. With
  a 256-bit secret salt per record, that dictionary attack is impossible. The salt lives only in the
  company's store and in proof bundles the company chooses to hand out.
- **`leaf_hash(salt, event)`** = `H(0x00 ‖ salt_bytes ‖ JCS(event))`. It is generic over
  `Serialize`, so tests can hash plain JSON while the node hashes `Event`. Cost: O(|event|).
- **`node_hash(l, r)`** = `H(0x01 ‖ l ‖ r)`, with exactly 65 bytes of input.
- **`BatchMeta`**: the batch facts folded into the head. Field names are part of the spec, because
  they are serialised into the hash.
- **`head_hash(prev, root, meta)`**: the **chain**. Each head commits to the previous head, so
  changing anything in batch *k* changes every head after it. That is why the verifier reports
  "contents changed" for one batch and "head chains on an altered batch" for all later ones.
- **`genesis_head(chain)`**: the head before batch 1. The tag string still says `v1`. It is a domain
  separator, not the spec version, and changing it would change every hash and every test vector.

### 2.2 The event: spec v3 (`src/event.rs`)

`Event` is the exact structure that becomes a leaf:

```rust
pub struct Event {
    pub v: u32,                 // 3
    pub chain: String,          // which chain it belongs to (binds the event to one chain)
    pub source: Source,         // where in the change log
    pub key: RecordKey,         // BTreeMap<String, String>: primary key columns → value
    pub op: Op,                 // insert | update | delete
    pub before: Option<Value>,  // full row image before (null for insert)
    pub after: Option<Value>,   // full row image after (null for delete)
    pub committed_at: String,   // source commit time, RFC 3339 UTC micros
    pub captured_at: String,    // node receive time
}
pub struct Source { connector, server, db, table, gtid, tx, file, pos, row }
```

Design decisions:

- **Every field is always present.** An absent value is `null`, never a missing key. One change
  therefore has exactly one canonical form. With `skip_serializing_if`, two serialisations of "the
  same" event could hash differently.
- **`source` is shaped after a row-based binary log.** `server` is MySQL's `server_uuid`. `gtid` is
  `server_uuid:tx`. `tx` is the GTID transaction number as a decimal string. `file`/`pos` are the
  binlog file and the rows event's `end_log_pos`. `row` is the row's index within its transaction.
  Together they locate a change uniquely in the source's own log, so the source DBA can find it.
  Oracle (SCN, redo log) or SAP (change document number) map onto the same fields.
- **`key` is a `BTreeMap`**, so composite keys (`order_lines(order_id, line_no)`) work, and its
  iteration order is sorted, which makes `key_label()` stable.
- **Numbers that must not lose precision are strings.** `tx` and `pos` are strings in the event,
  and the generator writes money as `"1250.00"`. JSON floats lose precision and canonicalise
  differently across languages.
- **There is no actor field.** A binlog does not record which application user made a change. To
  anchor that, the application writes it as an ordinary row in the same transaction, and that row
  is anchored like any other.

Helpers: `row()` (the image describing the record), `tx_num()`, `key_label()` (`col=v,col=v`).

### 2.3 Merkle trees (`src/merkle.rs`)

These are RFC 6962 trees (Certificate Transparency's), not "duplicate the last leaf" trees. For n > 1
leaves, split at **k = the largest power of two strictly below n**:
`MTH(D[0:n]) = node(MTH(D[0:k]), MTH(D[k:n]))`. The odd leaf is promoted, never duplicated, so two
different leaf lists can never produce the same root (a known flaw of duplication).

| Function | What | Time | Space |
|---|---|---|---|
| `split(n)` | largest power of two < n | O(log n) | O(1) |
| `merkle_tree(leaves)` | the whole tree as `MerkleTree` nodes (for display) | O(n) hashes | O(n) |
| `merkle_root(leaves)` | the root only | O(n) hashes | O(log n) recursion |
| `inclusion_proof(leaves, i)` | sibling hashes from leaf i up to the root | O(n) hashes (recomputes sibling subtree roots: n/2 + n/4 + … ) | O(log n) |
| `all_inclusion_proofs(leaves)` | every leaf's proof in one pass | O(n) hashes + O(n log n) hash clones | O(n log n) |
| `verify_inclusion(leaf, i, n, proof, root)` | fold the path back to a root and compare | O(log n) | O(log n) |

`verify_inclusion` needs the tree **size** as well as the index, because in an RFC 6962 tree the
path's shape depends on n. Walking down from the root, it decides at each level whether index `m`
is left or right of `k = split(n)`, recurses into that subtree, then combines with the sibling from
the end of the proof. A wrong index, a wrong size or any flipped bit gives a different root. The
property tests check exactly that.

`EMPTY_ROOT` = `H("")`, with no tag: an empty batch still has a well-defined root. `MerkleTree`
serialises compactly (leaves carry `leaf: index`, inner nodes carry `left`/`right`). It exists for
tooling; the node itself uses `merkle_tree(...).hash` and the verifier uses `merkle_root`.

### 2.4 Ledger data types (`src/ledger.rs`)

These describe what any ledger stores. They live in core because the port trait uses them.

- `AnchorInput`: what the submitter sends per batch. It carries `seq`, `prev_head`, `head`,
  `merkle_root`, `leaves: Vec<String>` (**every leaf hash, in batch order**), `event_count`,
  `tx_count`, `from_tx`, `to_tx`, `from_time`, `to_time`.
- `Anchor`: the same fields as stored, plus `contract_id`, `chain_id`, `signatories`, `update_id`
  and `recorded_at`.
- `ChainSpec`: what `create_chain` needs (id, submitter, members, genesis head).
- `ChainView`: one member's view of a chain: head state plus all anchors in seq order.
- `ArchiveAttempt`: the outcome of trying to delete an anchor alone.

**Why anchor the leaves, not only the root?** With only the root on the ledger, a record is checked
through an inclusion proof that sits in the company's own store. An insider who controls that store
can damage the proofs of honest records, and then cannot tell the guilty record from the honest ones
any more: a whole batch "fails" and the result blurs. With every leaf on the ledger, each record is
compared directly with *its own* anchored leaf, so the one altered record stands out. The root is
still anchored and still feeds the head, which keeps the chain compact and lets proofs be derived.
The cost is 64 hex characters per record on the ledger, which is fine for audit volumes.

**Positions.** `from_tx`/`to_tx` are GTID numbers. An empty batch repeats the previous `to_tx` in
both fields, or 0 before anything was captured, so every anchor states where the chain stands in
the log.

### 2.5 The ports (`src/ports.rs`)

```rust
pub struct CapturedTx { pub tx: u64, pub gtid: String, pub events: Vec<Event> }

pub trait ChangeSource: Send {
    fn next_tx(&mut self) -> impl Future<Output = Result<Option<CapturedTx>, SourceError>> + Send;
}

pub trait AuditLedger: Send + Sync {
    fn ensure_parties(&mut self, ids: &[Party]) -> impl Future<Output = Result<(), LedgerError>> + Send;
    fn create_chain(&self, spec: &ChainSpec) -> impl Future<Output = Result<(), LedgerError>> + Send;
    fn append(&self, chain_id: &str, submitter: &str, input: &AnchorInput) -> impl Future<...Anchor...> + Send;
    fn chain_view(&self, party: &str, chain_id: &str) -> impl Future<...Option<ChainView>...> + Send;
    fn attempt_archive(&self, chain_id: &str, actor: &str, seq: u64) -> impl Future<...ArchiveAttempt...> + Send;
}

pub trait TrailRecord {
    fn record_id(&self) -> &str;  fn batch_seq(&self) -> u64;  fn index(&self) -> usize;
    fn stored_leaf(&self) -> &str;  fn recompute_leaf(&self) -> String;
}
```

Why they look like this:

- **The unit of capture is a whole transaction** (`CapturedTx`), never a row. A batch must never split
  a transaction: half a transfer anchored in one batch and half in the next would make "what was
  committed" ambiguous. It also lets completeness be checked per GTID.
- **`impl Future + Send` instead of `async fn`.** Native async-in-traits (Rust ≥ 1.75) works with
  plain `async fn`, but then callers cannot require the returned future to be `Send`. Spelling out
  `+ Send` in the trait means any generic caller can `tokio::spawn` the work on a multi-threaded
  runtime. Implementations still write ordinary `async fn` bodies (see `CantonLedger` and
  `ReplaySource`). The trade-off: these traits are not object-safe, so there is no
  `Box<dyn AuditLedger>`. Callers use generics (`Node::seal<L: AuditLedger>`,
  `drive<S: ChangeSource, L: AuditLedger>`), which also means static dispatch and no allocation per
  call.
- **`&mut self` only where state changes.** `ensure_parties` fills the adapter's party-id map once at
  start-up, so it takes `&mut self`. Everything else takes `&self`, so one ledger handle can serve
  concurrent readers. That is why the trait requires `Sync`, and why the Canton adapter keeps its one
  piece of mutable cache behind a `Mutex` (§5).
- **`TrailRecord` exposes only what verification needs.** The verifier never trusts `stored_leaf`: it
  calls `recompute_leaf`. The stored value is used only to report whether an insider "covered" an
  edit by rewriting it.
- **Errors.** `SourceError { Io, Decode{at, msg} }` and `LedgerError(String)`. The ledger error is a
  string because the useful content is the ledger's own message (for example a Daml assertion like
  "chain fork: prevHead does not match the chain head"), which callers display as is.

### 2.6 Test vectors (`tests/vectors.rs`, `test-vectors/`)

`test-vectors/generate_v3.py` is an **independent** implementation of the spec in about 100 lines of
standard-library Python. It writes `hashing-v3.json`: five realistic events with fixed salts, their
canonical forms and leaves, a batch root with every inclusion path, the genesis head, and two chained
heads (a non-empty batch, then an empty one).

`tests/vectors.rs` checks that the Rust code reproduces every value. If anyone changes a field name,
the hashing order or the canonicalisation, this test fails. Two implementations agreeing is strong
evidence the spec is written down correctly, and it is exactly what an auditor writing their own
verifier needs. The script avoids floats on purpose, because Python's float formatting differs from
ECMAScript's in edge cases.

---

## 3. `sthira-connector`: where change events come from

This crate turns a database's change log into `CapturedTx` values. For the prototype it reads a
**generated** binlog file. The module layout already has the place where a real binlog reader, or an
Oracle connector, will go.

### 3.1 The raw binlog shape (`src/mysql/binlog.rs`)

In a MySQL row-based binary log, a transaction appears as a sequence of events:

```
GTID_EVENT  → BEGIN (QUERY_EVENT)
  → TABLE_MAP_EVENT + WRITE_ROWS / UPDATE_ROWS / DELETE_ROWS   (per table touched)
→ XID_EVENT (commit)
```

A binlog reader collapses that into one `BinlogTx`:

```rust
pub struct BinlogTx { server_uuid, tx: u64, file, timestamp: i64 /* unix seconds */, rows: Vec<RowChange> }
pub struct RowChange { db, table, event: RowsEvent, end_log_pos: u64, pk: Vec<String>,
                       before: Option<Map>, after: Option<Map> }
```

This is the **skeleton spec v3 is derived from**: it has exactly what the binlog provides with
`binlog_format=ROW`, `binlog_row_image=FULL` and `gtid_mode=ON`. The binlog stores column positions,
not names, so a real reader resolves names (and the primary key) from the table-map event plus
`information_schema`. Here the generator fills them in directly. The timestamp is in seconds because
that is the binlog event header's resolution.

### 3.2 Normalisation (`src/mysql/normalize.rs`)

`normalize_tx(&BinlogTx, chain, captured_at) -> CapturedTx` is a pure mapping (the table at the top
of the file lists every field):

- One `Event` per `RowChange`; `source.row` is its index in the transaction.
- `op` comes from the rows-event type (`WRITE_ROWS` → insert, and so on).
- The key comes from the `pk` columns, read from `after` (or `before` for a delete). Values become
  strings: strings stay as they are, everything else becomes its JSON text (`5001` → `"5001"`).
- `committed_at` is `iso_micros(timestamp * 1000)`.

Cost: O(total row bytes), because the row images are cloned into `serde_json::Value::Object`. The
`CONNECTOR` constant (`"mysql-binlog"`) goes into every event, so an event says which connector read
it.

### 3.3 The dummy-data generator (`src/generator.rs`)

`generate(&GenConfig) -> impl Iterator<Item = BinlogTx>` simulates a small ERP. **Determinism is the
point:** `StdRng::seed_from_u64(seed)` drives every choice, including the server UUID, and there is
no wall clock. The same `gen` command therefore always writes the same file, so a presentation can
be rehearsed and test expectations are stable.

How it works:

- `Erp` holds the *current* state of every entity the workload may touch again (users, customers,
  orders, invoices). Each generated update therefore carries a correct full `before` image, just as a
  real `binlog_row_image=FULL` would.
- `next_rows(n)` picks a transaction kind:
  1. The first 3 transactions create users and the next 3 create customers, which seeds the
     database.
  2. After that, kinds are weighted by a roll of 0..100: new order with 1–3 lines (25%), confirm
     an order (18%), invoice an order (18%), pay an invoice (18%), new customer (7%), credit-limit
     change (6%), delete a draft invoice (4%), role change (2%), new user (2%).
- Each kind **falls back** when it has nothing to act on (no `ISSUED` invoice to pay → invoice an
  order instead → confirm one → place one). Every transaction therefore changes at least one row.
- Multi-row transactions mirror real business units of work: an order and its lines; an invoice and
  the order's status change; a payment and the invoice's status change.
- `row(...)` advances a fake `end_log_pos` by a random 180–420 bytes, so positions strictly increase.
  The file name rolls over every 500 transactions (`binlog.000001`, …).
- The `generate` closure advances commit time by 1–6 s per transaction, and GTIDs are consecutive
  from `first_tx`.

`write_jsonl(&cfg, path)` streams the iterator to a file through a `tokio::io::BufWriter`, one JSON
object per line, and returns `(txs, rows)` written.

Complexity: generating T transactions costs O(T × E). The "find a candidate" steps
(`orders.filter(status == …)`) scan the entity vectors, whose length E grows with T. That makes long
runs quadratic. It doesn't matter at hundreds or thousands of transactions; per-status indexes would
fix it if anyone ever generates millions. Output memory is O(1) per transaction, since nothing is
buffered beyond the writer.

### 3.4 Replay as a live source (`src/replay.rs`)

`ReplaySource<C>` implements `ChangeSource`:

```rust
pub struct ReplaySource<C> { lines: Lines<BufReader<File>>, line, opts: ReplayOptions, pace, clock: C, dropped }
pub struct ReplayOptions { chain, after_tx, skip: BTreeSet<u64>, rate: f64 }
```

- **Streaming:** `tokio::fs::File` plus `BufReader::lines()` reads one line at a time, so memory is
  O(longest line) regardless of file size.
- **`next_tx`** loops: read a line, skip blanks, parse a `BinlogTx` (`Decode` error with the line
  number on failure), skip `tx <= after_tx` (resume), drop GTIDs in `skip` (recording them in
  `dropped`), sleep for the pacing interval, then normalise with `captured_at = (self.clock)()`.
- **Pacing:** `rate` transactions per second becomes a `Duration` of `1/rate`, slept *before* each
  release. `rate == 0` means no sleep. Sleeping before the release models a live log: the change
  "arrives" when it is released.
- **`skip`** is the presenter's tool for "this transaction never reached the binlog we read" (a
  purged binlog file, or a session with `sql_log_bin=0`). The node sees a GTID hole, and the
  verifier proves it from the ledger (§7.2).
- **The clock is a generic closure** `C: FnMut() -> String + Send`. Production passes `now_iso`;
  tests pass `|| "T".to_string()` for deterministic output. Injecting the clock is what keeps
  `normalize_tx` and this source testable without time mocking. The tokio test with
  `start_paused = true` checks pacing exactly: 4 transactions at 2/s take exactly 2 s of virtual
  time.

### 3.5 Business meaning (`src/describe.rs`) and time helpers (`src/lib.rs`)

`describe(&Event) -> Description { kind, text, flagged }` turns a change into a sentence, such as
"Invoice #1014 (DRAFT, ₹5578.00) deleted", and **flags** what an auditor should look at: deleted
invoices, credit-limit changes, role *escalations* (rank clerk < accountant < manager < admin), and
any other delete. It is display only and never hashed. That is deliberate: wording can change freely
without touching a single anchor. Unknown tables fall back to a generic diff of the changed fields.

`iso_micros(ms)` and `now_iso()` produce the spec's timestamp format (`2026-11-20T09:00:01.000000Z`).
Fixed-width UTC strings sort correctly as text, which the Daml model relies on (§4).

---

## 4. The Daml model: what the ledger enforces

`prototype-sandbox/daml/Sthiranet/Audit.daml` runs inside Canton. It is the part of the system that
**neither the company's code nor the auditor's code can bypass**. Rust only submits commands; these
rules decide whether they are accepted.

```
ChainProposal ──Sign (each member)──► ChainProposal ──Finalize──► AuditChain
AuditChain    ──Append (submitter only)──► AuditChain (next) + AuditAnchor
```

- **`ChainProposal`**: signatories are the members who have signed so far. Each member exercises
  `Sign` (adding itself), and once everyone has signed, any member can `Finalize`. A multi-party
  contract has to be built this way: Daml only lets a party sign by acting itself, so the auditor
  must consent explicitly.
- **`AuditChain`**: the chain head (`lastSeq`, `lastHead`), signed by **every** member. `Append` is
  controlled by the submitter alone. The members' authority flows into the new anchor because the
  chain contract already carries their signatures, so the auditor doesn't have to sign every batch.
  `Append` asserts:
  - `seq == lastSeq + 1` (no gaps) and `prevHead == lastHead` (no forks);
  - the time window does not run backwards (fixed-format UTC compares correctly as `Text`);
  - `length leaves == eventCount`;
  - `0 <= txCount <= eventCount`, and at least one transaction when there are events;
  - `0 <= fromTx <= toTx`.

  `Append` is a **consuming** choice: it archives the current `AuditChain` and creates the next one.
  Two conflicting appends can never both succeed, because the second would be exercising an
  archived contract. That gives fork resistance with no locking in our code.
- **`AuditAnchor`**: one per batch, signed by every member. Archiving one, the only way to "rewrite"
  it, needs every signatory, so neither party can do it alone. `sthira-node rewrite` demonstrates
  this.

Privacy comes for free from Daml's stakeholder model: only signatories and observers see a
contract, so a party outside the chain sees nothing (`test_outsider_sees_nothing`).

`Test.daml` has 8 Daml Script tests (listed in `prototype-sandbox/README.md`); `appendArgs` builds a
valid `Append` that each test then breaks in one way. `daml.yaml` pins SDK 3.6.4 and names the
package `sthiranet-audit`, the name the Rust client uses in template ids. `sandbox.sh` runs
`dpm build`, then `dpm sandbox --json-api-port 7575 --dar …`: one participant and one synchronizer in
one in-memory process.

---

## 5. `sthira-ledger`: talking to Canton

`src/canton.rs` implements `AuditLedger` over the **JSON Ledger API v2** (`src/lib.rs` only
re-exports it). The module header has a table mapping each trait method to its API calls.

**Structure:**

```rust
pub struct CantonLedger {
    cfg: CantonConfig,                     // url, user_id
    http: reqwest::Client,                 // one connection pool, 30 s timeout
    parties: BTreeMap<String, String>,     // "AUDITOR" → "AUDITOR::1220ab…"
    chains: Mutex<HashMap<String, String>>,// chain id → current AuditChain contract id (cache)
}
```

**The pieces, in call order:**

1. **`connect`** calls `GET /v2/version` to confirm the server is a v2 JSON API. It then polls
   `GET /v2/state/connected-synchronizers` once a second (up to 60 times) with `tokio::time::sleep`.
   The JSON API answers before the participant has joined its synchronizer, and commands fail until
   it has.
2. **`ensure_parties`** lists parties once (`GET /v2/parties`) and **allocates** the missing ones
   with `POST /v2/parties {partyIdHint}`. Canton turns the hint into a full id
   (`NAME::<fingerprint>`); the rest of the system only ever sees short ids, and `short()` and
   `shorten()` translate (`shorten` also rewrites ids inside error messages).

   **The race:** two clients starting together (the two e2e tests on a fresh sandbox) both see a
   party missing and both try to allocate it. Canton refuses the second with
   `REQUEST_ALREADY_IN_FLIGHT`. `allocate` handles this by re-listing for up to 5 s
   (20 × 250 ms) until the other allocation appears, then using it.
3. **`submit`** wraps one command in `POST /v2/commands/submit-and-wait-for-transaction`, with a
   unique `commandId` (pid + random u64) and `actAs`. It returns the resulting transaction, so the
   created contracts can be read back without a second query. `exercise` is a thin wrapper.
4. **Template ids** use the package-name form `#sthiranet-audit:Sthiranet.Audit:AuditChain`, so the
   client never needs the package hash and survives rebuilding the DAR.
5. **`create_chain`**: create `ChainProposal` as the submitter → `Sign` as each other member →
   `Finalize`. That is 2 + (members − 1) round trips. The new `AuditChain` id goes into the cache.
6. **`append`** resolves the chain contract id (`chain_cid`: the cache, or else an active-contract
   query) and exercises `Append` with the batch. **Daml `Int` values are sent as JSON strings**
   (`"12"`) because the JSON API encodes 64-bit integers that way; `int()` accepts either form when
   reading. From the transaction it reads the created `AuditAnchor` and the successor `AuditChain`,
   and caches the successor's id. On failure it **evicts** the cache entry: the cached id may be
   stale, for example if another process appended in the meantime, and the next call re-queries.
   - Cost: one HTTP round trip per batch in the steady state.
   - Payload: O(event count), about 70 bytes per leaf.
7. **`chain_view(party, chain)`** gets the ledger end offset, then calls
   `POST /v2/state/active-contracts` with a filter for that **party** and the two templates. It
   builds the head state and the anchors sorted by `seq`. Because it filters by party, the result is
   genuinely *that party's* view; the verifier reads the auditor's. Cost: O(number of the party's
   active contracts), which grows linearly with the chain. A production version would page through
   it or keep a local mirror updated from the transaction stream.
8. **`attempt_archive`** finds the anchor in the actor's view and exercises the built-in `Archive`
   choice acting only as that party. Canton's answer is the evidence: `DAML_AUTHORIZATION_ERROR …
   requires authorizers AUDITOR,DREAMTALK, but only DREAMTALK were given`.

**Why `std::sync::Mutex` and not `tokio::sync::Mutex`?** The lock is held only to read or write one
`HashMap` entry and is **never held across an `.await`**. A blocking mutex is cheaper and cannot
deadlock in that pattern. A tokio mutex is only needed when a guard must live across an await.

**Error mapping.** `read()` turns non-2xx responses into `LedgerError(api_error(body))`, which
extracts the API's `code: cause` (for example the Daml assertion text) and falls back to the first
400 characters of the body.

---

## 6. `sthira-batcher`: sealing, anchoring and the audit store

This is the company's node logic. It depends only on core: it seals through any `AuditLedger` and
stores rows that implement `TrailRecord`.

### 6.1 The sealing rule (`src/lib.rs`)

```rust
pub fn plan_seal<E: Serialize>(chain_id, genesis, chain_start, last: Option<ChainTip>,
                               events: &[&E], txs: Option<(from_tx, to_tx, tx_count)>, to_time) -> SealPlan
```

It is a **pure function**, apart from drawing salts:

1. `seq` = last seq + 1; `prev_head` = last head (or genesis); `from_time` = last `to_time` (or the
   chain start), so time windows are **contiguous** and a quiet period is still covered by some
   batch.
2. A fresh salt per event, then `leaf_hash(salt, event)`, then `merkle_tree(leaves)`.
3. GTID window: the given `txs`, or for an empty batch the previous `to_tx` repeated with
   `tx_count = 0`.
4. `meta`, then `head_hash(prev_head, root, meta)`.

`SealPlan::anchor_input()` packages it for the ledger, **leaves included**. Cost: O(total event
bytes) for hashing plus O(n) for the tree, where n is the number of events.

`ChainTip` borrows from the last batch (`&'a str`) instead of cloning it. It is a small zero-copy
view.

**Empty batches are sealed and anchored too.** "Nothing happened between 10:00:10 and 10:00:20" then
becomes a signed statement, so an insider cannot later insert a change into a quiet window.

### 6.2 The node (`src/node.rs`)

```rust
pub struct Node { party, chain_id, genesis, chain_start,
                  pending: Vec<CapturedTx>, store: Vec<StoredRow>, batches: Vec<BatchRecord>, last_tx: u64 }
```

- **`ingest(CapturedTx) -> Ingest`** is O(1).
  - The binlog is ordered, so anything with `tx <= last_tx` is a re-read and returns `Duplicate`.
    This is what makes restarts safe (§6.3).
  - A jump (`tx > last_tx + 1`, after the first transaction) returns `QueuedAfterGap{from, to}`, so
    the run loop can tell the presenter immediately.
  - The transaction is queued either way. The gap's *proof* comes later, from the anchored GTID
    windows.
- **`seal(&L, to_time)`** is generic over `L: AuditLedger` and is the key transactional step:
  1. Compute the plan from `pending`, **without mutating anything**.
  2. `ledger.append(...).await?`. If the ledger refuses (fork, gap, a network error), return the
     error with the node unchanged: the transactions are still pending.
  3. Only after the anchor exists, move `pending` into `StoredRow`s (salt, leaf, slot index, id
     `{chain}-{tx}.{row}`), append a `BatchRecord`, and return `Sealed {batch, anchor, rows}` for
     the caller to persist.

  "Anchor first, then mutate" is what keeps the node and the ledger consistent: the node never
  records a batch the ledger does not hold. Cost: O(n) plus one round trip.
- **`StoredRow` implements `TrailRecord`**, and `recompute_leaf` is `leaf_hash(salt, event)`.
  **Proofs are not stored**: since the leaves are on the ledger, any proof can be derived from them
  when it's needed (§7.3). That saves O(n log n) storage per batch and removes a forgeable artefact
  from the store.

### 6.3 Persistence (`src/persist.rs`)

The node is a long-running process that can crash. Its state lives in plain files:

```
<data>/chain.json             ChainInfo: chain id, members, submitter, genesis, chain start, source file, ledger url/user
<data>/node/trail.jsonl       one StoredRow per line: the off-ledger audit store
<data>/node/batches.jsonl     the node's batch table
<data>/node/receipts.jsonl    anchors as the ledger returned them (local receipts; verifiers read the ledger)
<data>/node/cursor.json       { last_tx }
<data>/node/tamper-log.jsonl  originals of rows the presenter altered
<data>/node/.lock             advisory lock serialising writers
```

**JSONL** (one JSON object per line) makes each batch an append, O(rows), with no rewrite of the
file. A torn final line from a crash is just one unparsable line: `read_jsonl` skips and counts bad
lines instead of failing.

**Commit order is trail → batches → receipt → cursor** (`commit`), each step `fsync`ed with
`sync_data`. What a crash leaves behind, and how `restore(anchored_seq)` repairs it:

| Crash point | State on disk | Repair |
|---|---|---|
| before the anchor | nothing for this batch | transactions are re-read from the source (cursor not advanced) |
| after the anchor, before the trail | ledger ahead of node | `restore` refuses: the records are unrecoverable, so start a new chain (loud, by design) |
| after the trail/batches, before the receipt or cursor | node has a batch the ledger has | kept, because `anchored_seq` covers it |
| node sealed locally but the ledger has fewer | node ahead of ledger | rows and batches with `seq > anchored_seq` are dropped, and the cursor is reset to the last kept `to_tx` |

`restore` takes `anchored_seq` as a **parameter**: it doesn't know what a ledger is. The caller
(`sthira-node`) asks Canton and passes the number in, which keeps the batcher independent of the
ledger.

Two helper patterns:

- **Atomic replace:** `write_json` and `rewrite_jsonl` write a temporary file and then `rename` it
  over the target. On POSIX, rename is atomic, so a reader sees either the old file or the new one,
  never a half-written mix.
- **Locking:** `lock()` uses `File::lock()` (an advisory exclusive lock, stable in std since 1.89).
  It serialises the running node's commits against the presenter's `tamper`, which rewrites the
  trail.

---

## 7. `sthira-verify`: the auditor's algorithm

This crate depends only on core. It is generic over `R: TrailRecord`, so the same code can verify a
trail stored in any format.

### 7.1 `verify_records(chain_id, genesis, anchors, rows) -> VerifyReport`

```
group rows by batch_seq                                  O(R)
prev_head = genesis
for each anchor a (from the auditor's ledger copy):      A iterations
    recs = rows of a.seq sorted by index                 O(r log r)
    leaves = recs.map(recompute_leaf)                    O(bytes)
    root = merkle_root(leaves); head = head_hash(prev_head, root, meta(a, |recs|))
    leaf_issues   = recs where leaves[i] != a.leaves[rec.index]   (ALTERED, or INVENTED if no such slot)
    missing       = slots 0..|a.leaves| with no record             (DELETED)
    ledger_consistent = merkle_root(a.leaves) == a.merkle_root && |a.leaves| == a.event_count
    ok = ledger_consistent && root == a.root && head == a.head && |recs| == a.count && no leaf issues
    prev_head = head            ← the RECOMPUTED head, so damage propagates down the chain
```

Total: O(R log R + total bytes) time, O(R) space, with R records and A anchors. The checks are
layered, and each one catches something different:

| Check | Catches |
|---|---|
| leaf vs `a.leaves[index]` | **which** record changed, even if the stored leaf hash was rewritten (`covered: true`) or other rows were vandalised |
| missing slots | records deleted from the store |
| record at a slot the anchor never had | records invented and added to the store |
| recomputed root vs anchored root | any change to the batch as a whole, including order |
| recomputed head vs anchored head | changed metadata, and every batch after an altered one |
| `ledger_consistent` | an anchor whose leaves disagree with its own root or count; it should be impossible, but it is checked rather than trusted |

The report separates **`changed_seqs`** (the batch's own contents differ) from **`failed_seqs`**
(any check failed, including heads that fail only because an earlier batch changed). An auditor needs
both: *where* the tampering is, and *how far* its effect reaches. `unanchored` lists batch numbers in
the trail with no anchor at all.

The verifier receives anchors as a slice. *Where* they come from is the caller's decision, and the
CLI always reads the auditor's own copy (§8.4).

### 7.2 Completeness (`src/positions.rs`)

`position_report(&anchors)` uses **ledger values only**, so the company cannot influence it. For
each non-empty anchor, in order:

- **Gap** between batches: `from_tx > previous to_tx + 1`.
- **Hole** inside a batch: `(to_tx − from_tx + 1) > tx_count`.
- **Total missing**: `(last − first + 1) − Σ tx_count`.

O(A) time. This is why `tx_count` and the GTID window are anchored. GTIDs are dense per server, so
any number that is never anchored is a transaction the node never received. That is reported as a
completeness **warning**, separate from integrity: the batches that *were* anchored can still be
intact.

### 7.3 Proofs from the ledger

`proof_from_anchor(anchor, index)` is `inclusion_proof(&anchor.leaves, index)`. Because the leaves
are on the ledger, a proof can be built at any time from trusted data. O(n) per proof.

---

## 8. `sthira-node`: wiring it all together

The binary crate is the **composition root**: the one place that picks `ReplaySource` and
`CantonLedger`. It also contains the presenter's insider tools. It is written as one async program
(`#[tokio::main]`, multi-threaded runtime).

### 8.1 `main.rs` and `cli.rs`

`cli.rs` is the clap definition: global `--data` (env `STHIRA_DATA`) and `--no-color`, subcommands
`gen run status ledger trail proof check-proof tamper untamper rewrite verify`, and their flags. Every
flag has a doc comment, which clap turns into `--help` text.

`main.rs` parses arguments, decides on colour (a terminal, no `--no-color`, no `NO_COLOR`), and
dispatches. Every command returns `Result<ExitCode, String>`:

- 0 means success or PASS;
- 1 means a negative finding (verify FAIL, an invalid proof);
- 2 means an operational error.

Scripts and tests can therefore tell "tampering found" from "something broke". `Paint` is a tiny
ANSI helper. `auditor_of(info)` picks the last member as the auditor.

### 8.2 Opening the ledger (`ledger.rs`)

- `connect(info)`: `CantonLedger::connect` + `ensure_parties(members)`.
- `open_for_node(dir, info)` reconciles the data directory with the ledger before the node writes
  anything:
  - The ledger has no chain and there are no local receipts: create it.
  - The ledger has no chain but there are local receipts: the sandbox was restarted (it is
    in-memory), so refuse.
  - The ledger has the chain with anchors but there are no receipts: another data directory owns
    it, so refuse.
  - The genesis head differs: refuse.
  - Any local receipt's `(seq, head)` is missing on the ledger: refuse.

  It returns the ledger and `last_seq`, which then feeds `DataDir::restore`.
- `anchors_as(info, party)`: `chain_view(party, …)`. **Every integrity command uses this with the
  auditor**, never the company's receipts.

### 8.3 The run loop (`run.rs`)

`run` creates or loads the data directory, opens the ledger, restores the node, opens
`ReplaySource` positioned after the cursor (`after_tx = cursor.last_tx`), prints a header, then calls
the generic `drive<S: ChangeSource + 'static, L: AuditLedger>`:

```
┌───────────────────── connector task (tokio::spawn) ─────────────────────┐
│ loop { tx = source.next_tx().await?;  sender.send(tx).await }           │
└───────────────┬─────────────────────────────────────────────────────────┘
                │ mpsc::channel<CapturedTx>(256)  ← bounded: back-pressure
┌───────────────▼──────────────── node loop (main task) ──────────────────┐
│ select! (biased) {                                                       │
│   ctrl-c                → stop (pending transactions not sealed)         │
│   interval tick         → node.seal(&ledger).await                       │
│                           spawn_blocking(dir.commit(sealed))             │
│                           print batch                                    │
│   rx.recv(), if !done   → node.ingest(tx)  (record any GTID gap)         │
│ }                                                                        │
└──────────────────────────────────────────────────────────────────────────┘
```

Each decision, and its reason:

- **Two tasks and a channel.** Reading the source and anchoring run concurrently: while a batch is
  being anchored (a network round trip), the connector keeps reading. The channel is **bounded**
  (256 transactions), so if anchoring stalls, `send().await` blocks the connector instead of growing
  memory without limit. That is back-pressure.
- **`biased` select.** Branches are polled in order: Ctrl-C first, then the tick, then data. Under a
  flood of transactions a seal is never starved, and a stop request is never delayed.
- **One pinned Ctrl-C future** (`tokio::pin!(stop)`), created once. Creating a fresh `ctrl_c()` each
  iteration could miss a signal that arrives between iterations.
- **`interval_at(now + period)` with `MissedTickBehavior::Delay`.** The first seal comes one period
  after start (not immediately). If a seal takes longer than the period, the next tick is pushed back
  instead of firing a burst of catch-up ticks.
- **`, if !connector_done` guard.** When the log ends, `recv()` returns `None` forever. Without the
  guard, `select!` would spin on that ready branch. The first `None` awaits the connector task's
  result (surfacing any `SourceError`) and disables the branch. After that, ticks keep sealing empty
  batches, which is a heartbeat proving the node is alive.
- **Commit through `spawn_blocking`.** `commit` does synchronous file I/O with `fsync`. Running it on
  the async worker threads would block other tasks, so it moves to tokio's blocking pool. `DataDir`
  and `Sealed` are cloned into the closure, because `spawn_blocking` needs `'static`.
- **Ctrl-C seals nothing partial.** Pending transactions are not in the cursor, so the next run
  re-reads them, and `ingest` drops anything already stored.

`gen_cmd` parses `--start`, refuses to overwrite without `--force`, and calls `write_jsonl`.
`print_batch` shows the batch summary, each record's description (⚑ when flagged) and any gaps found
on ingest.

### 8.4 Views (`views.rs`)

| Command | Reads | Does |
|---|---|---|
| `status` | local files | summary from receipts and trail: batches, records, flagged count, GTID completeness, cursor, per-table counts, pending tamper entries |
| `ledger` | **auditor's Canton copy** | one line per anchor (window, tx/rows, GTIDs, prev head, root, head); `--leaves` lists every anchored leaf |
| `trail` | local trail | records with op, kind and description; `--batch`, `--flagged`, `--last` |
| `proof` | auditor's copy + trail | for each matching record: recomputed leaf vs the anchored leaf, the path **derived from the anchored leaves**, inclusion check; `--export` writes a `ProofBundle` (one record, its salt, its path; nothing else) |
| `check-proof` | auditor's copy + bundle | checks the bundle's leaf against the **ledger's** leaf and root, ignoring `claimed_root` in the bundle |
| `rewrite` | Canton | `attempt_archive` as one party; prints required, signed and missing signatures and Canton's refusal |
| `verify` | auditor's copy + trail | `verify_records` + `position_report`, printed per batch (failing batches only, unless `--all`) with the record descriptions; `--json` for machines |

`sealed_rows` filters the trail to batches at or below the last known anchor, so a batch that is
being written while you run `verify` is not reported as unanchored. `find_rows` matches a record id
(`DT-23.0`) or `table:key` (`invoices:id=1042`). It is a linear scan, O(R), which is fine at
prototype scale.

### 8.5 The insider (`tamper.rs`)

`tamper edit <id> [--cover]` changes one field of the stored row image:

- **Choosing the field:** it prefers business-sensitive fields (`amount`, `total`, `credit_limit`,
  `unit_price`, `role`, `status`, …) and never touches primary-key columns.
- **Changing the value:** a decimal string gains a digit (`35859.00` → `358590.00`), other strings
  get `" (edited)"`, integers get +1.
- **Repeated edits** keep drifting away from the original and never land back on it (tested).
- **`--cover`** rewrites the stored `leaf_hash` too, which simulates an insider who knows the scheme.

`tamper delete <id>` removes the row. Each action appends the original to `tamper-log.jsonl` under
the write lock. `untamper` restores the first logged original of each row and re-sorts the trail.

---

## 9. Tests

| Where | Count | Needs | What it pins down |
|---|---|---|---|
| `sthira-core` unit | 12 | — | JCS (including RFC 8785 examples), salts, leaf/head behaviour, RFC 6962 reference roots, proof properties (proptest: every valid proof verifies, any flipped bit fails, the all-at-once proofs equal single proofs) |
| `sthira-core/tests/vectors.rs` | 3 | — | Rust matches the independent Python implementation byte for byte |
| `sthira-connector` | 6 | — | normalisation maps every field; the generator is deterministic, dense, well-formed and touches every table; replay resumes, skips, reports bad lines and paces exactly; every generated change has a description |
| `sthira-batcher` | 3 | — | sealing chains and empty batches; commit/restore drops unanchored batches and refuses when the ledger is ahead; JSONL tolerance |
| `sthira-verify` | 6 | — | a covered edit is pinpointed despite vandalised honest rows and a deletion; invented records; inconsistent anchors; proofs from anchored leaves; GTID gaps and holes |
| `sthira-node` unit | 1 | — | tamper never restores the original or touches keys |
| `sthira-ledger/tests/canton_sandbox.rs` | 1 | sandbox | create, append with leaves, fork, gap and leaf-mismatch refused, auditor cannot append, auditor's copy holds the leaves, outsider sees nothing, unilateral archive refused |
| `sthira-node/tests/e2e.rs` | 2 | sandbox | the real binary: gen → run → verify PASS → covered edit and delete → FAIL naming the record → `proof` fails → untamper → PASS → rewrite refused → resume; and `--skip-gtids` → gap reported, integrity still OK |
| `Test.daml` | 8 | `dpm` | the ledger rules themselves |

Sandbox tests are marked `#[ignore = "needs the Canton sandbox (make test)"]`, so plain `cargo test`
stays fast and offline. `make test` passes `--include-ignored`. Each sandbox test uses fresh random
chain ids, because the sandbox keeps state until it restarts. The e2e tests drive the compiled
binary via `env!("CARGO_BIN_EXE_sthira-node")` in a temporary working directory, so they test exactly
what a user runs.

---

## 10. One row, end to end

Follow one payment through the whole system:

1. **Generate.** `pay_invoice` emits a `BinlogTx` for GTID 23 with two rows:
   - `payments` (`WRITE_ROWS`, `after = {id: 1017, amount: "35859.00", …}`);
   - `invoices` (`UPDATE_ROWS`, `before.status = ISSUED`, `after.status = PAID`).

   It is written as one line of `changes.jsonl`.
2. **Capture.** `ReplaySource::next_tx` reads the line, waits 0.5 s (rate 2), stamps `captured_at`,
   and `normalize_tx` makes two `Event`s, `source.row` 0 and 1. The connector task sends the
   `CapturedTx` into the channel.
3. **Queue.** The node loop receives it; `Node::ingest` checks that `23 == last_tx + 1` and queues
   it.
4. **Seal.** At the tick, `plan_seal` gives each event a fresh salt. The payment row lands at slot 4
   (shown as `L5`): `leaf = H(00 ‖ salt ‖ JCS(event))`. The batch's 31 leaves give a root, and with
   `meta` and the previous head, a new head.
5. **Anchor.** `CantonLedger::append` exercises `AuditChain.Append` with the 31 leaves. Daml checks
   the sequence, prev head, counts and window, archives the old chain contract, and creates the next
   one plus an `AuditAnchor` signed by DREAMTALK and AUDITOR. A copy lands on every member's
   participant.
6. **Store.** Only now does the node create `StoredRow { id: "DT-23.0", batch_seq: 2, index: 4, salt,
   leaf_hash, event }`. `commit` appends it to `trail.jsonl` in commit order.
7. **Tamper.** An insider changes `amount` to `"358590.00"` and recomputes `leaf_hash` (`--cover`),
   so the row looks self-consistent.
8. **Verify.** The auditor reads anchor #2 **from its own copy**. It recomputes the leaf of `DT-23.0`
   from the stored salt and the edited event, compares it with `anchor.leaves[4]`, and the two
   differ: `ALTERED L5 DT-23.0 … (stored leaf hash was rewritten to match; still caught)`. The
   recomputed root and head of batch 2 differ too, and every later head fails as well.
9. **Rewrite?** Deleting anchor #2 needs both signatures; Canton refuses.
10. **Untamper.** The original row comes back, and every check passes again.

---

## 11. Building on it

**A real MySQL connector.** Add a module next to `mysql/binlog.rs` that reads the binlog (as a
replica, using a binlog-protocol library) and produces `BinlogTx` values. Reuse `normalize_tx`.
Implement `ChangeSource` on it. Persist the GTID set as the resume point instead of `after_tx`.
Then change the one line in `sthira-node/src/run.rs` that constructs the source; nothing else
changes. Things to handle:

- reading the table map plus `information_schema` for names and keys;
- multiple servers (several `server_uuid`s), which means per-server GTID windows;
- DDL events.

**Oracle or SAP.** Add `src/oracle/` (for example LogMiner: SCN as `tx`, redo position as
`file/pos`) or `src/sap/` (change documents) with its own raw types and normaliser to `Event`. The
spec fields were chosen to fit.

**Another ledger.** Implement `AuditLedger` in a new crate and enforce the same rules on its side:
sequence, prev head, leaf count, multi-party signatures. Pick the adapter in `sthira-node/src/ledger.rs`.
The batcher and verifier don't change.

**A new table's wording.** Add a match arm in `describe.rs`. Hashes are unaffected.

**Changing the hashed structure.** Bump `SPEC_VERSION`, update `generate_v3.py` (or add a v4
script), regenerate the vectors, and keep the Rust test green against them. Never change hashing
silently: existing anchors would no longer verify.

**Scaling.** The places that grow linearly:

- `chain_view` reads all active contracts. Replace it with a mirror fed from the ledger's update
  stream.
- The trail is one JSONL file read whole. Replace it with an indexed store such as SQLite or
  Postgres keyed by `(batch_seq, index)`, implementing `TrailRecord` on its row type.
- `find_rows` scans the trail. Use an index.
- The generator's entity scans (§3.3).

None of these change an interface.

---

## 12. Known simplifications

- **One participant hosts every party** in the sandbox, so the node can sign as the auditor during
  `create_chain`. On a real network the auditor signs from its own participant.
- **No authentication** on the JSON API (the sandbox default); production needs JWTs.
- **In-memory sandbox:** restarting it forgets every chain, and `open_for_node` detects that and
  refuses.
- **Members are fixed** when the chain is created; there is no "add auditor later" choice yet.
- **Salts and records live in one file** with no encryption at rest.
- **Single source server:** completeness assumes one dense GTID sequence.

---

## 13. File index

| File | Chapter |
|---|---|
| `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `Makefile`, `.gitignore` | 1 |
| `crates/sthira-core/src/lib.rs` | 2.1 |
| `crates/sthira-core/src/event.rs` | 2.2 |
| `crates/sthira-core/src/merkle.rs` | 2.3 |
| `crates/sthira-core/src/ledger.rs` | 2.4 |
| `crates/sthira-core/src/ports.rs` | 2.5 |
| `crates/sthira-core/tests/vectors.rs`, `test-vectors/*` | 2.6 |
| `crates/sthira-connector/src/mysql/binlog.rs` | 3.1 |
| `crates/sthira-connector/src/mysql/normalize.rs`, `mysql/mod.rs` | 3.2 |
| `crates/sthira-connector/src/generator.rs` | 3.3 |
| `crates/sthira-connector/src/replay.rs` | 3.4 |
| `crates/sthira-connector/src/describe.rs`, `lib.rs` | 3.5 |
| `prototype-sandbox/daml/Sthiranet/Audit.daml`, `Audit/Test.daml`, `daml.yaml`, `sandbox.sh`, `README.md` | 4 |
| `crates/sthira-ledger/src/canton.rs`, `lib.rs` | 5 |
| `crates/sthira-batcher/src/lib.rs` | 6.1 |
| `crates/sthira-batcher/src/node.rs` | 6.2 |
| `crates/sthira-batcher/src/persist.rs` | 6.3 |
| `crates/sthira-verify/src/lib.rs` | 7.1, 7.3 |
| `crates/sthira-verify/src/positions.rs` | 7.2 |
| `crates/sthira-node/src/main.rs`, `cli.rs` | 8.1 |
| `crates/sthira-node/src/ledger.rs` | 8.2 |
| `crates/sthira-node/src/run.rs` | 8.3 |
| `crates/sthira-node/src/views.rs` | 8.4 |
| `crates/sthira-node/src/tamper.rs` | 8.5 |
| `crates/sthira-ledger/tests/canton_sandbox.rs`, `crates/sthira-node/tests/e2e.rs` | 9 |
| `README.md` | how to run the prototype, step by step |
