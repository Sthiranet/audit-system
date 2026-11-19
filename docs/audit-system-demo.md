# The Sthiranet audit system web demo, from the bottom up

This is a guided tour of every file on the `demo` branch. It is written for a programmer who knows
Rust and TypeScript at an everyday level (traits, generics, `async`, `Arc`/`Mutex`, closures, React
hooks) and wants to know **what this code does, why it is shaped the way it is, what it costs, and
how the parts talk to each other**.

The tour follows the dependency graph from the bottom up: from the hash function, through the
simulated ERP and ledger, to the HTTP server and the React screens. Each chapter only uses things that
earlier chapters explained. By the end you should be able to open any file, know where it sits in the
design, and change it safely.

The demo is a **presentation tool**. Its job is to tell a story correctly and convincingly in front
of an audience. The hashing is the real spec v1. The ERP, Debezium and the ledger are simulated in
memory, and the code is shaped around a presenter clicking through tabs. The `prototype` branch
carries the production direction; the differences are summarised in [chapter 12](#12-known-simplifications-and-what-the-prototype-changed).

**Contents**

0. [The story and the shape of the solution](#0-the-story-and-the-shape-of-the-solution)
1. [The workspace](#1-the-workspace)
2. [`sthira-core`: hashing spec v1](#2-sthira-core-hashing-spec-v1)
3. [`sthira-ledger`: the simulated ledger](#3-sthira-ledger-the-simulated-ledger)
4. [`sthira-profiles`: the simulated ERP and Debezium](#4-sthira-profiles-the-simulated-erp-and-debezium)
5. [`sthira-batcher`: a subsidiary's node](#5-sthira-batcher-a-subsidiarys-node)
6. [`sthira-verify`: the verifier](#6-sthira-verify-the-verifier)
7. [`demo-director`: the server](#7-demo-director-the-server)
8. [The UI](#8-the-ui)
9. [Tests](#9-tests)
10. [One change, end to end](#10-one-change-end-to-end)
11. [Building on it](#11-building-on-it)
12. [Known simplifications, and what the prototype changed](#12-known-simplifications-and-what-the-prototype-changed)
13. [File index](#13-file-index)

---

## 0. The story and the shape of the solution

A corporate group has a headquarters (**HQ**) and three subsidiaries (**S1**, **S2**, **S3**) in
different regions. Each subsidiary runs its own ERP database, with journal entries and vendors. HQ
and, later, an external **auditor** need to trust each subsidiary's audit trail without trusting the
subsidiary.

Each subsidiary runs a Sthiranet **node**:

1. It captures every change from its ERP's change log (Debezium-shaped events).
2. It seals the changes into **batches**, hashes them into a **Merkle root**, and chains each batch's
   **head** to the previous one.
3. It **anchors** the root and head on a ledger, where every anchor is co-signed by the subsidiary and
   HQ.
4. The records stay in the subsidiary's own **off-ledger audit store**. The subsidiary **pushes** a
   copy of its trail upstream to HQ.

HQ, or the auditor, **verifies** a trail by recomputing every hash and comparing the result with its
**own** ledger copy.

Trust model:

| Party | Holds | Can do | Cannot do |
|---|---|---|---|
| Subsidiary | its records + salts, its ledger copy | append anchors to its own chain | rewrite or archive an anchor alone |
| HQ | ledger copies of every chain, pushed trails | co-sign, verify | change anchors alone; read another party's records it wasn't given |
| Auditor (optional) | its own ledger copies | co-sign new anchors, verify HQ's copies | change anything alone |
| Synchronizer (Sthiranet) | encrypted envelopes | order and deliver | read contents |

### The crates and how they depend on each other

```
                           ┌───────────────┐
                           │  sthira-core  │  spec v1 hashing, Merkle trees, proofs (pure)
                           └───────▲───────┘
            ┌──────────────────────┼─────────────────────┐
   ┌────────┴────────┐    ┌────────┴────────┐            │
   │  sthira-ledger  │    │ sthira-profiles │            │
   │ AuditLedger +   │    │ ERP sim,        │            │
   │ SimLedger       │    │ Debezium, norm. │            │
   └────────▲────────┘    └────────▲────────┘            │
            └──────────┬───────────┘                     │
              ┌────────┴────────┐                        │
              │ sthira-batcher  │  SubsidiaryNode: capture, seal, anchor
              └────────▲────────┘                        │
              ┌────────┴────────┐                        │
              │  sthira-verify  │  verify_trail ─────────┘
              └────────▲────────┘
              ┌────────┴────────┐        HTTP + SSE        ┌──────────────┐
              │  demo-director  │ ◄──────────────────────► │   demo/ui    │
              │ World, axum API │   OpenAPI → TS types     │ React + Vite │
              └─────────────────┘                          └──────────────┘
```

This branch has a simpler layering than the prototype. Crates depend on each other **directly**:
the batcher names `FinanceSource` and the verifier names `StoreRow`. Only the ledger is behind a
trait (`AuditLedger`). For a self-contained demo that is the cheapest shape that works; the prototype
moved every crate boundary behind traits so real adapters can be swapped in (chapter 12).

`demo-director` is the only crate that knows about presentation. The UI holds **no audit logic at
all**: it renders what the server computes. The server's OpenAPI spec generates the UI's TypeScript
types, so the two can't silently drift apart.

---

## 1. The workspace

| File | Purpose |
|---|---|
| `Cargo.toml` | Workspace: the five `sthira-*` crates plus `demo/demo-director`. Shared metadata (edition 2024, `rust-version = 1.94`), and every external dependency pinned once under `[workspace.dependencies]`. |
| `rust-toolchain.toml`, `rustfmt.toml` | Stable toolchain with rustfmt and clippy; 120-column formatting. |
| `Makefile` | Entry points: `install`, `api`, `build`, `start`, `dev`, `test`, `check`, `stop`, `clean`, `distclean`. |
| `Dockerfile` | Three stages: Node builds the UI, Rust builds `demo-director`, and a Debian slim runtime runs both as an unprivileged `demo` user. |
| `render.yaml` | Render Blueprint: one free Docker web service, pinned to `branch: demo`, health check on `/api/state`. |
| `.gitignore`, `.dockerignore` | Build output, `node_modules`, `dist`, the generated `openapi.json`. |

**External dependencies**, and why each one is there:

| Crate / package | Used by | Why |
|---|---|---|
| `serde`, `serde_json` | all | every structure is JSON: in hashes, over HTTP, in the UI |
| `serde_json_canonicalizer` | core | RFC 8785 canonical JSON, so a hash input is byte-identical everywhere |
| `sha2`, `hex` | core | SHA-256; hashes travel as lowercase hex |
| `rand` | core, ledger, profiles, director | salts; random ids and fake ciphertext; the ERP simulation |
| `chrono` | ledger, profiles, director | RFC 3339 timestamps |
| `thiserror` | ledger | `LedgerError` |
| `utoipa` | all Rust crates | derives an OpenAPI schema from Rust types (`ToSchema`); the UI's types are generated from it |
| `axum`, `tokio`, `tokio-stream`, `tower-http` | director | HTTP server, runtime, the SSE stream, static files, response headers |
| `tower`, `http-body-util` | director (dev) | calling the router directly in HTTP tests |
| `proptest` | core (dev) | Merkle property tests |
| React 19, TanStack Query 5, `openapi-fetch` | UI | rendering, server-state cache, a typed fetch client |
| Vite 8, TypeScript 6, `openapi-typescript` | UI (dev) | bundling and dev server, type checking, OpenAPI → `schema.d.ts` |

**How the pieces build.** `make build` first runs `make api`. That runs `demo-director --openapi`,
which prints the spec to `demo/ui/openapi.json`, and `openapi-typescript` turns the spec into
`src/api/schema.d.ts`. It then runs `npm run build` (`tsc` type check + `vite build` into
`demo/ui/dist`) and `cargo build --release -p demo-director`.

`schema.d.ts` is **committed**, so the Docker UI stage needs no Rust toolchain. `make start` runs the
release binary, which serves both the API and `dist`. `make dev` runs the API on :3000 and Vite on
:5173; `vite.config.ts` proxies `/api` (including the SSE stream) to :3000, so the browser sees one
origin.

`make stop` uses `pkill -x demo-director`, and `make clean` depends on `stop`, because all demo state
lives in that process.

---

## 2. `sthira-core`: hashing spec v1

`crates/sthira-core/` is pure: no I/O, no runtime. It defines what is hashed and how.

### 2.1 The primitives (`src/lib.rs`)

```text
leaf    = H(0x00 || salt || JCS(event))
node    = H(0x01 || left || right)
root    = RFC 6962 Merkle tree hash over the batch's leaves (empty batch: H(""))
meta    = JCS({ chain, seq, from_pos, to_pos, from_time, to_time, event_count })
head    = H(0x02 || prev_head || root || meta)
genesis = H(0x03 || "sthiranet/v1/genesis/" || chain)
```

- **`sha256_hex(parts)`** feeds slices into one SHA-256 state, so no concatenated buffer is built:
  O(total bytes) time, O(1) extra space.
- **Domain-separation tags** (`0x00`–`0x03`) make each hash kind distinct. An inner node can never be
  presented as a leaf, which defeats the classic second-preimage attack on Merkle trees. RFC 6962 uses
  the same 0x00/0x01 convention.
- **`hex_bytes`**: hashes are combined as raw 32-byte values. A string that isn't valid hex (a
  hand-edited value) is hashed as its raw bytes instead, so verification reports a mismatch rather
  than crashing.
- **`canonical(v)`** applies RFC 8785 JCS: sorted keys, no whitespace, ECMAScript number format. This
  is what makes a hash reproducible regardless of key order. O(n log k).
- **`new_salt()`** returns 32 OS-random bytes. A ledger full of hashes of guessable records (an
  amount, a status) would let anyone test candidate values. A secret 256-bit salt per record makes
  that impossible. The salt stays in the off-ledger store.
- **`leaf_hash`**, **`node_hash`**, **`head_hash`**, **`genesis_head`** implement the formulas.
  `leaf_hash` is generic over `Serialize`, so tests can hash plain JSON. Each costs O(input).
- **`BatchMeta`**: the batch facts in the head. Its field names are part of the spec, because they are
  serialised into the hash.
- **The chain:** each head includes the previous head, so changing anything in batch *k* changes every
  head after it. That is the "cascade" the Verify tab explains.

### 2.2 The event: spec v1 (`src/event.rs`)

```rust
pub struct Event {
    pub v: u32,                       // 1
    pub chain: String,                // the subsidiary's chain id, e.g. "S1"
    pub source: EventSource,          // connector, db, schema, table, pos (LSN or SCN), tx
    pub key: RecordKey,               // { id }
    pub op: Op,                       // insert | update | delete
    pub before: Option<Value>,        // row before (null for inserts)
    pub after: Option<Value>,         // row after (null for deletes)
    pub actor: Actor,                 // { name, kind: service | user }
    pub committed_at: String,         // source commit time
    pub captured_at: String,          // when Debezium delivered it
}
```

- The shape follows **Debezium's source block**: PostgreSQL reports a log sequence number (`lsn`, e.g.
  `0/1A3F2C8`) and Oracle a system change number (`scn`). Both land in `source.pos`.
- Every field is always present (`null` when empty), so one change has one canonical form.
- Money is a **string** in the row images (`"1250.00"`), never a JSON float, because floats lose
  precision and canonicalise differently across languages.
- `Op::from_debezium` maps Debezium's `c`/`r` (create, snapshot read), `u` and `d`.
- Every type derives `utoipa::ToSchema`, because these structures are sent to the UI as they are.

### 2.3 Merkle trees (`src/merkle.rs`)

These are RFC 6962 trees. For n > 1 leaves, split at **k = the largest power of two below n**; an odd
leaf is promoted, never duplicated, so different leaf lists can't share a root.

| Function | What | Time | Space |
|---|---|---|---|
| `merkle_tree(leaves)` | the whole tree as nested `MerkleTree` nodes, so the UI can draw it | O(n) hashes | O(n) |
| `merkle_root(leaves)` | the root only | O(n) hashes | O(log n) |
| `inclusion_proof(leaves, i)` | sibling hashes from leaf i up to the root | O(n) hashes (sibling subtree roots: n/2 + n/4 + …) | O(log n) |
| `verify_inclusion(leaf, i, n, proof, root)` | fold the path back to a root and compare | O(log n) | O(log n) |

`MerkleTree` is shaped for drawing. A leaf has `leaf: index`, an inner node has `left`/`right`, and an
empty batch is `{ empty: true }`. `#[schema(no_recursion)]` keeps the recursive type valid in OpenAPI.
`verify_inclusion` needs the tree size, because in RFC 6962 the shape of the path depends on n.

### 2.4 Test vectors (`tests/vectors.rs`, `test-vectors/`)

`test-vectors/generate.py` is an independent standard-library Python implementation of spec v1. It
writes `hashing-v1.json`: events with fixed salts, their canonical forms and leaves, a batch root with
every inclusion path, the genesis head, and two chained heads (a batch, then an empty one).
`vectors.rs` checks that the Rust code reproduces every value byte for byte, so an accidental spec
change fails the build. An auditor can also write their own verifier against these vectors.

---

## 3. `sthira-ledger`: the simulated ledger

`src/lib.rs` defines the ledger interface and **`SimLedger`**, an in-memory stand-in that reproduces
the parts of Canton's model the story depends on.

**Types:**
- `AnchorInput`: what a subsidiary submits per batch: seq, prev head, head, Merkle root, event count,
  log positions and time window.
- `Anchor`: the same, plus `contract_id`, `template`, `signatories`, `update_id` and `recorded_at`.
- `ChainCopy`: **one party's** copy of a chain: members, submitter, genesis, last seq and head, and its
  anchors.
- `Envelope`: what the synchronizer sees of one transaction: recipients, a byte size, and a random
  "ciphertext".
- `ArchiveAttempt`: the outcome of a rewrite attempt: required, signed and missing signatures, plus a
  reason.

**The trait** is synchronous and takes `&mut self`: `add_party`, `create_chain`, `append`,
`add_member`, `attempt_archive`, `copy`, `view` and `envelopes`. It is object-safe, so the world holds
it as `Box<dyn AuditLedger>`. In-memory operations need nothing more.

**`SimLedger`'s data model** is the point of the simulation:

```rust
parties: HashMap<Party, BTreeMap<ChainId, ChainCopy>>   // each party's OWN copies
envelopes: Vec<Envelope>                                 // the synchronizer's log
```

Every party has its own independent `ChainCopy` of each chain it belongs to. Nothing is shared. This
is what makes "HQ sees three chains, S1 sees one, S2 cannot see S1's" literally true in the data,
rather than a filter applied at display time.

- **`create_chain`** checks that every member exists, sequences one envelope, and gives each member its
  own copy.
- **`append`** checks that the submitter is the chain's submitter. Then **each member's copy** checks
  continuity on its own (`seq == last_seq + 1`, `prev_head == last_head`), and any member can refuse
  ("HQ refused to sign: …"). Only if every member agrees is an envelope sequenced and the anchor
  pushed to every member's copy.
  - Cost: O(m) for m members, plus O(1) amortised pushes.
  - The anchor's signatories are all the members, which simulates Canton's co-signing.
- **`add_member`** (the auditor joining) appends the new member to every existing copy and gives the new
  member a copy of the **full history**. Earlier anchors keep their original signatories, and only new
  anchors carry three signatures.
- **`attempt_archive`** looks the anchor up in the actor's copy. A non-stakeholder gets "cannot even see
  it". Otherwise it computes the missing signatures, sequences an envelope (the rejected attempt is
  still traffic), and returns `ok: false` with an explanation. **It never archives**: the demo only
  shows single-party attempts, and the rule "archiving needs every signatory" is modelled by always
  refusing a one-signature attempt.
- **Envelopes** are capped at `MAX_ENVELOPES = 300` with `Vec::remove(0)`, which is O(n) per removal.
  That's negligible at 300; a `VecDeque` would make it O(1).

---

## 4. `sthira-profiles`: the simulated ERP and Debezium

A "profile" is industry-specific: here, finance. It produces source events, normalises them into
spec v1, and explains them to people.

### 4.1 Debezium's event shape (`src/debezium.rs`)

`DebeziumEvent { before, after, source, op, ts_ms, transaction }` and `DebeziumSource { version,
connector, name, ts_ms, db, schema, table, txId, lsn?, scn? }` mirror what Debezium Server's HTTP sink
delivers as JSON with schemas off. `lsn` and `scn` are optional and skipped when absent, because each
connector sends only one of them. The UI shows this raw event next to the normalised one.

### 4.2 The simulated ERP (`src/source.rs`)

`FinanceSource` stands in for one subsidiary's database plus its Debezium connector.

- **State:**
  - `jes: Vec<JournalEntry>`, `vendors: Vec<Vendor>`: the current rows, so updates can carry correct
    `before` images.
  - Id counters: `JE-1001…`, `V-201…`.
  - The log position (`pos`): PostgreSQL starts at `0x1a3f000` and is rendered as an LSN `0/…`; Oracle
    starts at `48_210_000` and is rendered as a decimal SCN.
  - A transaction counter starting at a random 5000–9000.
  - `service = erp_svc_<id>`, which goes into `modified_by`.
- **Construction:** `new` seeds the RNG from the OS, so every demo run differs. `with_seed` is
  deterministic, for tests. Four vendors exist before capture starts (master data).
- **`next(ts)`** picks a change kind from one random draw: post a journal entry (42%, and always when
  there are none yet), amend an entry by ±1–8% (16%), create a vendor (12%), change a vendor's bank
  account (18%), or toggle a vendor's status (12%). It applies the change to its state, advances the
  position by 40–400 and the transaction id by 1, and returns a `DebeziumEvent`. The capture time
  (`ts_ms`) is 40–900 ms after the commit, to mimic connector lag.
- Amounts are computed as `f64` and immediately rounded and formatted to a 2-decimal **string** by
  `money()`, so floats never reach a hash.

Cost: O(1) per change, except `new_vendor`, which scans the 16 vendor names (O(16·v)).

### 4.3 Normalisation, meaning and tampering (`src/lib.rs`)

- **`normalize(raw, chain)`** maps Debezium to spec v1:
  - `pos` = `lsn`, or else `scn`;
  - `key.id` = the row's `id`;
  - `actor.name` = `modified_by`, and `kind` is `service` when it starts with `erp_svc`;
  - `committed_at` from `source.ts_ms`, `captured_at` from the event's `ts_ms`.
- **`describe(ev)`** returns a business sentence with a stable `kind` tag (`je-posted`, `je-amended`,
  `vendor-bank-change`, …) and a `label`, for example "Vendor bank account changed · V-203 · ACCT … →
  ACCT …". It is display only and never hashed.
- **`fmt_money`** groups thousands for display.
- **`tamper(ev)`** is what the presenter's corrupt insider does to a stored event:
  - A journal entry's amount changes roughly tenfold: ×10, or /10 + 1 when it is already large. The
    "+1" makes sure a second edit can never land back on the original.
  - A vendor's bank account is swapped to `ACCT 9999-0042-NNNN`, with a counter that also never
    repeats the original.

  A test checks that six repeated edits never restore the original, because a restored original would
  make the verifier "miss" a tamper during a live demo.

---

## 5. `sthira-batcher`: a subsidiary's node

`src/lib.rs` defines `SubsidiaryNode`, the company-side logic, plus the shapes the UI needs.

```rust
pub struct SubsidiaryNode {
    id, chain_id, genesis, chain_start,
    source: FinanceSource,
    pending: Vec<PendingRecord>,   // captured, not yet sealed
    store: Vec<StoreRow>,          // the off-ledger audit store
    batches: Vec<Batch>,           // the node's own batch table (with the tree, for the UI)
    event_seq: u64,                // record ids S1-00001, S1-00002, …
}
```

- **`capture(n, now_ms)`** pulls n changes from the source, spaced 300 ms apart and ending at `now_ms`,
  normalises them, and queues them. O(n).
- **`seal(&mut dyn AuditLedger, now_ms)`** follows the key rule, *anchor before mutating*:
  1. Compute everything from `pending` without changing state: `seq`, `prev_head`, a contiguous time
     window (`from_time` = previous `to_time`), salts, leaves, the tree, the positions (an empty batch
     repeats the previous `to_pos`), the meta, and the head.
  2. `ledger.append(...)`. If any member's copy refuses, return the error with the node unchanged.
  3. Only then compute every leaf's **inclusion proof**, move `pending` into `StoreRow`s (event, raw
     Debezium event, salt, leaf hash, proof, slot index), and push a `Batch` with its `items` and full
     `tree` for the Capture tab.

  Cost: O(n) hashes for the leaves and tree, plus **O(n²)** for the proofs, because each
  `inclusion_proof` call is O(n). Batches hold 1–20 changes, so this is irrelevant here. The
  prototype replaced it with an O(n log n) all-proofs pass.
- **`StoreRow` carries its proof.** In this design only the root is anchored, so each record needs its
  own path to prove that it belongs to the root. The verifier then checks records one by one (§6).
- **Empty batches** are sealed and anchored too, so a quiet period is a signed statement.

`PendingRecord`, `BatchItem` and `Batch` exist largely for the UI (raw event, normalised event,
description, salt, leaf). `Batch.tree` is the drawable `MerkleTree`.

---

## 6. `sthira-verify`: the verifier

`verify_trail(chain_id, genesis, anchors, rows) -> VerifyReport`, where `anchors` must come from the
**verifier's own** ledger copy:

```
group rows by batch_seq; prev_head = genesis
for each anchor a:
    evs = rows of a.seq sorted by index; leaves = leaf_hash(salt, event) for each
    tree = merkle_tree(leaves); head = head_hash(prev_head, tree.hash, meta(a, |evs|))
    leaf_issues = evs where !verify_inclusion(leaf, row.index, a.event_count, row.proof, a.merkle_root)
    missing_indexes = 0..a.event_count not present among row.index
    root_match, head_match, count_match → ok
    prev_head = head                      ← the RECOMPUTED head: damage cascades
```

O(R log R) for sorting, O(R log n) for proofs and O(total bytes) for hashing, with R records and n
per batch.

What makes it pinpoint the right record:
- **Each record is checked on its own, at its original slot.** Its recomputed leaf, with the proof it
  carries, must lead to the **anchored** root at `row.index`. Deleting other records shifts *positions*
  in the recomputed tree but not slots. That is why `LeafIssue` reports both `index` (the slot when
  sealed) and `position` (in the recomputed tree), and why the UI draws leaves by slot.
- **Covering tracks doesn't help.** If the insider recomputes and stores a new leaf hash
  (`covered: true`), the recomputed leaf still doesn't fold to the anchored root, because the root was
  co-signed before the edit.
- **`changed_seqs`** (root or count mismatch: this batch's own contents changed) is reported apart from
  **`failed_seqs`** (any mismatch, including heads broken by an earlier batch). `first_bad_seq` is the
  first changed batch.
- `BatchCheck` also returns the recomputed `tree` and `leaves` with descriptions, so the Verify tab can
  draw "recomputed vs anchored" without a second request.

**The known weakness.** Proofs live in the subsidiary's own store. An insider who controls the store
can also damage the proofs of *honest* records, and a verifier then can't separate the guilty record
from the honest ones in that batch. The batch still fails, but the pinpoint blurs. This is why the
prototype anchors **every leaf hash** on the ledger (chapter 12).

---

## 7. `demo-director`: the server

`demo/demo-director` wires everything into one process and serves the UI. Its lib (`src/lib.rs`)
exposes `api`, `views` and `world`, so tests can drive the world and the router directly.

### 7.1 The world (`src/world.rs`)

**The cast** is constants: `HQ`, `AUDITOR` and `SUBSIDIARIES` (id, name, region, database, connector,
currency). S1 runs PostgreSQL/USD, S2 Oracle/EUR, S3 PostgreSQL/AED. `SEED_BATCHES = 3` and
`AUTO_INTERVAL = 10 s`. Renaming the cast means editing these lines.

```rust
pub type Shared = Arc<Mutex<World>>;
pub struct World {
    ledger: Box<dyn AuditLedger>,
    subs: Vec<Sub>,                                  // each: config + SubsidiaryNode
    hq_received: HashMap<String, Received>,          // trails pushed to HQ (pushed_at, rows)
    auditor_enabled: bool,
    auto_task: Option<JoinHandle<()>>,
    last_verify: BTreeMap<String, LastVerify>,       // drives the network-strip lights
    activity: VecDeque<Activity>,                    // newest first, capped at 100
    version: u64, version_tx: watch::Sender<u64>,    // change notification for SSE
}
```

**Actions**, which the HTTP API calls one-to-one:

| Method | Does |
|---|---|
| `reset()` | aborts auto-anchoring and builds a fresh `SimLedger` with all five parties. For each subsidiary it creates a node with chain members `[sub, HQ]` and seals 3 batches of 5–9 changes, back-dated 10 s apart so the window looks real. It then pushes every trail to HQ, clears logs and verifications, and logs "Started". The version keeps counting up, so browsers notice the reset. |
| `state()` | the `StateView` snapshot: parties, per-subsidiary counters, members, HQ copy summary, `unpushed` (the store differs from HQ's copy), last verifications, latest 50 activities, envelope count |
| `pending`, `capture(id, count)` | capture 1–20 changes (default 5) |
| `seal(id)` | `node.seal(ledger)`; a refusal becomes HTTP 409 |
| `batch_detail(id, seq)` | the node's batch plus the anchor **from the subsidiary's own ledger copy** |
| `ledger_view(party)` | that party's `ChainCopy`s plus the latest 12 envelopes |
| `rewrite(actor, chain, seq)` | `attempt_archive` + a log line explaining the refusal |
| `trail(where, chain)` | the subsidiary's store (`sub`) or HQ's received copy (`hq`) |
| `push(chain)` | copies the subsidiary's whole store into `hq_received` (a snapshot: later edits on either side don't propagate) |
| `tamper(req)` | edit (optionally `cover` = recompute the stored leaf) or delete a row, in either the subsidiary's store or HQ's copy |
| `verify(verifier, chain)` | HQ or AUDITOR (only once added). It reads **HQ's received copy** of the trail and the anchors from **the verifier's own** ledger copy, then runs `verify_trail`, records `last_verify` and logs a plain-language result. |
| `add_auditor()` | `ledger.add_member(chain, AUDITOR)` for every chain |
| `auto_tick()` | each subsidiary captures 1–4 changes and seals |

**Errors.** `DemoError { status, message }` carries an HTTP status (400, 404 or 409) from the action
that failed, so actions decide their own status codes.

**Change notification.** Every `log()` bumps `version` and calls `version_tx.send_replace(version)` on
a `tokio::sync::watch` channel. A watch channel keeps only the **latest** value: a slow browser that
misses ten versions is woken once, with the newest. That is exactly right for "something changed, so
refetch".

**Auto-anchoring.** `set_auto(&Shared, on)` spawns a tokio task with `interval_at(now + 10 s)`. Each
tick upgrades a **`Weak`** reference to the world, locks it and calls `auto_tick()`. Two consequences
of holding a `Weak`:
- the task never keeps the world alive on its own: if the world is dropped, `upgrade()` fails and the
  task ends;
- there is no reference cycle between the world and its own task's handle.

Turning it off, or `reset()`, aborts the task.

**Locking.** `Shared` is an `Arc<std::sync::Mutex<World>>`. Every handler locks, calls one synchronous
action, and unlocks. **No `.await` happens while the lock is held**, so a blocking mutex is correct and
cheaper than `tokio::sync::Mutex`. It also serialises every action, which keeps the world consistent
trivially. That's fine for one presenter; it is the main reason this design doesn't scale (chapter
12).

### 7.2 The API contract (`src/views.rs`)

Every JSON shape the UI reads or sends: `StateView`, `SubView`, `HqCopy`, `PendingRow`,
`BatchDetail`, `LedgerView`, `TrailView`, `VerifyResponse`, `ErrorBody`, and the request bodies
(`CaptureRequest`, `RewriteRequest`, `TamperRequest`, `VerifyRequest`, `AutoRequest`).

- `#[serde(rename_all = "camelCase")]` makes the JSON idiomatic for TypeScript.
- Every type derives `ToSchema`, so it appears in the OpenAPI spec and therefore in `schema.d.ts`.
- `#[serde(flatten)]` merges `Description` and `VerifyReport` into their parents.
- Request bodies use `#[serde(default)]`: a missing field falls back to a default, and the action
  validates the value.

### 7.3 HTTP, SSE and OpenAPI (`src/api.rs`)

- **Routes** sit under `/api`, one handler per action. Each handler is two lines: lock, call, wrap in
  `Json`. `ApiResult<T> = Result<Json<T>, DemoError>`, and `impl IntoResponse for DemoError` turns
  errors into `{ "error": "…" }` with the action's status.
- **`JsonBody<T>`** is a custom extractor. It accepts an empty body as `T::default()` (so
  `POST /api/seal/S1` needs no body) and returns JSON errors instead of axum's plain-text rejections.
- **`/api/stream`** subscribes to the watch channel, wraps it in a `WatchStream`, and maps each value to
  an SSE event `version: <n>`. `KeepAlive` stops proxies from closing an idle stream.
- **`no-store`** is set on every API response, so no browser or CDN caches state.
- The **fallback** for unknown `/api/*` paths is a JSON 404. Every other path is served from the UI
  `dist` folder, with `index.html` as the fallback for a single-page app.
- **`#[derive(OpenApi)] ApiDoc`** lists every handler (each annotated with `#[utoipa::path]`).
  `demo-director --openapi` prints it, which is the start of the type-generation pipeline.

### 7.4 Startup (`src/main.rs`)

`--openapi` prints the spec and exits. Otherwise it reads `PORT` (default 3000) and `STHIRA_UI_DIST`
(default `../ui/dist` relative to the crate), warns if `index.html` is missing (the API still works),
binds `0.0.0.0` (needed inside Docker), and serves with graceful shutdown on Ctrl-C.

---

## 8. The UI

`demo/ui` is React 19 + TypeScript, built with Vite. It contains **no audit logic**: every hash,
verdict and explanation comes from the server.

### 8.1 Plumbing

- **`index.html`, `main.tsx`.** These mount `<App>` inside a TanStack `QueryClientProvider` (with
  `staleTime: 0`, so data is always refetched when asked; `retry: 1`; no refetch on window focus) and
  a `ToastProvider`.
- **`api/schema.d.ts`.** This is generated and should not be edited. It holds `paths`, `operations` and
  `components.schemas`, mirroring the Rust types.
- **`api/client.ts`.** `openapi-fetch`'s `createClient<paths>` makes each call typed by URL: a wrong
  path, parameter or body is a **compile error**, so a backend change breaks `npm run build` instead of
  the live demo. `call()` unwraps the response or throws the server's `error` message. `api` exposes
  one function per endpoint, and `keys` holds the query-cache keys.
- **`ui.tsx`** holds the shared pieces:
  - formatters (`fmtTime`, `plural`, `partyName`);
  - `Hash` (short hash, full value in a tooltip), `OpBadge`, `Sigs`, `AnchorCard`;
  - the toast context;
  - `useRun()`, which wraps any action so a failure becomes a toast instead of an unhandled promise.

### 8.2 `App.tsx`: state and live updates

- **`useLiveUpdates()`** opens an `EventSource('/api/stream')`. When the `version` changes, it calls
  `queryClient.invalidateQueries()`, so **everything on screen refetches**. That one rule keeps every
  open browser in sync without per-screen subscriptions. If SSE drops, the state query falls back to
  polling every 1.5 s until the browser reconnects.
- **UI state lives in `App`**, not in the tabs: the selected subsidiary, batch per chain, ledger party,
  rewrite form and result, trail holder, cover checkbox, verifier and verify result. Switching tabs
  therefore keeps each tab's state, which matters mid-presentation.
- The top-bar actions put the returned `StateView` straight into the cache (`setQueryData`) for an
  instant update. **Reset** also clears every UI selection and drops all cached non-state queries.

### 8.3 Always on screen (`components/Chrome.tsx`)

- **`TopBar`**: the auto-anchor switch, Add auditor, and **Reset**, which needs two clicks within 3 s
  (an "armed" state with a timer) so a presenter can't reset by accident.
- **`NetworkStrip`**: HQ, the private synchronizer link, one card per subsidiary (database, chain head,
  pending count, and a verification light from `lastVerify`), and the auditor. Clicking a card selects
  that subsidiary.
- **`ActivityPanel`**: the server's activity log, coloured by level.

### 8.4 The Merkle tree drawing (`components/MerkleTree.tsx`)

The tree is SVG, laid out from the server's nested `MerkleTree`:

1. Walk the tree to collect the leaves. Each leaf's column is its **original slot** (`slots[position]`,
   passed by the Verify tab). Deleted slots (`missing`) are merged in, and the whole row is sorted by
   slot. A deleted record therefore shows as a dashed "ghost" box exactly where it used to be, and the
   remaining leaves keep their numbers.
2. `height(n)` (memoised in a `Map`) puts leaves at height 0 and an inner node one above its taller
   child. y = (tree height − node height) × row height.
3. Leaves get x from their column. An inner node's x is the midpoint of its children's x.
4. Bad leaves are flagged by position. `isBad` propagates up, so the path from a bad leaf to the root
   turns red.
5. With `animate` (a freshly sealed batch), each node's CSS animation delay is proportional to its
   height, so the tree "builds" from the leaves up.

Layout is O(n) with memoisation; the SVG scales to its container through `viewBox`.

### 8.5 The tabs

- **`CaptureTab`**:
  - the pending list, where each change expands to show the raw Debezium event next to the
    normalised event that gets hashed;
  - capture and seal buttons;
  - a three-step explanation of the batch (leaves with salts → the Merkle tree → the `AnchorCard` from
    the ledger), with previous/next navigation between batches.

  After a seal, `freshSeq` marks the new batch, so its tree animates and scrolls into view.
- **`LedgerTab`**:
  - party chips choose *whose node* the ledger is read from;
  - each chain is a horizontal row of blocks (genesis → #1 → #2 …) with root, head, previous head and
    signatures;
  - a module-level `seenSeq` map remembers the last sequence number shown per party and chain, so newly
    anchored blocks slide in, even across tab switches;
  - **Try to rewrite history** shows the ledger's refusal and the missing signatures;
  - *What the synchronizer sees* lists envelopes: time, update id, recipients, size, ciphertext.
- **`TrailTab`**:
  - the records held in the subsidiary's store or in HQ's received copy;
  - Alter and Delete per row, plus the *cover tracks* checkbox;
  - **Push to HQ**, labelled "changes not pushed" when `unpushed`.

  Each change clears the old verify result, so a stale verdict is never shown.
- **`VerifyTab`**:
  - the verifier chips (HQ, or the auditor once added);
  - a staged six-step animation (read trail → recompute leaves → rebuild roots → chain heads → read own
    ledger copy → compare). **The API is called only at the last step**, so the activity log and the
    network light can't reveal the verdict early;
  - the result: a banner, findings per changed batch, and a table of recomputed and anchored roots and
    heads;
  - a side-by-side **recomputed tree vs anchored values** for the selected batch. A changed batch is
    shown together with every other changed batch;
  - `Explain`, which turns a `BatchCheck` into prose: altered leaf, covered hash, deleted slot, count
    mismatch, or "intact but its head follows an altered batch".

### 8.6 Styling (`style.css`)

Colours are CSS custom properties on `:root`: a cappuccino palette with a navy accent, gold for
anchored values, red and green for mismatch and match, and purple for the auditor. Components use the
variables, so restyling the demo means editing the tokens only.

---

## 9. Tests

| Where | Count | What it pins down |
|---|---|---|
| `sthira-core` unit | 11 (incl. 2 property tests) | JCS, including the RFC 8785 examples; salts; leaf and head behaviour; the RFC 6962 reference roots; tree shape; every valid proof verifies, and any flipped bit fails |
| `sthira-core/tests/vectors.rs` | 3 | Rust matches the independent Python implementation of spec v1 |
| `sthira-ledger` | 5 | co-signed copies per member; fork, skip and wrong-submitter refused; archive needs every signatory; a new member receives the history and co-signs; the envelope cap |
| `sthira-profiles` | 4 | money formatting; normalised events follow spec v1 (positions, schema, actor, string money) for PostgreSQL and Oracle; tampering changes the leaf and never restores the original |
| `sthira-batcher` | 1 | sealing anchors, chains, and seals empty batches |
| `demo-director/tests/scenario.rs` | 4 | **the narrated 10-step story**, step by step (`the_story`); several tampers at once, each at its original slot; error status codes; auto-tick anchors every chain |
| `demo-director/tests/http.rs` | 5 | through the real router (`tower::ServiceExt::oneshot`): the state's JSON shape, seal and batch detail, trail → tamper → verify over HTTP, JSON error bodies, rewrite rejected |
| UI | `tsc` | `make test` and `make check` type-check the UI against the generated API types |

The scenario test is the demo's script in code form. If a claim the presenter makes stops being true,
`make test` fails.

---

## 10. One change, end to end

Follow one vendor bank-account change at S1. The ids, slots and positions below are illustrative,
since the simulated ERP is random on every run.

1. **Capture.** The presenter clicks *Capture* (`POST /api/capture/S1`). `World::capture` →
   `SubsidiaryNode::capture` → `FinanceSource::next` draws "vendor bank change": it updates `V-203`'s
   `bank_account` and returns a `DebeziumEvent` (`op: "u"`, `before`/`after`, `lsn: "0/1A3F2C8"`).
   `normalize` makes a spec v1 `Event`, which is queued as `S1-00031`. `log()` bumps the version, SSE
   tells every browser, and the pending list refetches.
2. **Seal.** `POST /api/seal/S1`:
   - the node computes salts and leaves; `V-203`'s leaf sits at slot 2 (shown as L3);
   - it builds the tree and the head over the previous head #3, and calls `SimLedger::append`;
   - S1's and HQ's copies each check continuity, an envelope is sequenced, and both copies store anchor
     #4, signed `S1 + HQ`;
   - only then do the proofs get computed and the rows enter S1's store;
   - the UI animates the tree.
3. **Push.** `POST /api/push/S1` snapshots S1's store into `hq_received["S1"]`.
4. **Tamper.** The insider alters `S1-00031` in S1's store with *cover tracks* on. The bank account
   becomes `ACCT 9999-0042-7731`, and the stored `leaf_hash` is recomputed. Then S1 pushes again.
5. **Verify.** `POST /api/verify {HQ, S1}`:
   - HQ's anchors come from **HQ's copy**;
   - batch 4's recomputed leaf at slot 2 doesn't fold through its stored proof to the anchored root,
     so a `LeafIssue { index: 2, covered: true }` is reported;
   - the root and head mismatch, nothing after batch 4 exists yet, and the banner reads "Tamper
     detected: 1 record altered";
   - the tree shows L3 and its path in red.
6. **Rewrite?** S1 tries to archive anchor #4 alone. `attempt_archive` reports HQ's missing signature,
   and the anchor is unchanged on both copies.

---

## 11. Building on it

- **Rename or re-cast.** Edit `HQ`, `AUDITOR` and `SUBSIDIARIES` in `world.rs`. Adding a fourth
  subsidiary is one more array entry; the UI renders whatever `state` returns.
- **A different industry.** Write a new profile like `sthira-profiles` (a source, `normalize`,
  `describe`, `tamper`). Today the batcher names `FinanceSource` directly, so you would also make
  `SubsidiaryNode` generic over a source trait. That is the step the prototype took with
  `ChangeSource`.
- **Changing the API.** Edit `views.rs` or `api.rs`, then run `make api`. TypeScript errors in the UI
  show exactly what to update. Commit the regenerated `schema.d.ts`.
- **A new UI view.** Add a route in `api.rs` (annotated for OpenAPI), a method on `World`, `make api`,
  then a function in `client.ts` and a component. Live updates come for free, as long as the action
  calls `log()`.
- **A real ledger.** Implement `AuditLedger` for it (the prototype's `CantonLedger` is the reference)
  and construct it in `World::reset`.

---

## 12. Known simplifications, and what the prototype changed

Simplifications that are fine for a presentation:

- **In-memory ledger.** No consensus, no cryptography; `attempt_archive` never archives anything.
- **Simulated ERP and Debezium.** Data is random on every run (`FinanceSource::new` seeds from the OS).
- **One process, one mutex.** Every action is serialised. That's right for one presenter; a hosted
  multi-user demo would share one world among all viewers (that's how Render behaves).
- **No persistence and no authentication.** Anyone who can reach the URL can reset the demo.
- **O(n²) proof generation and an O(n) envelope cap.** Both are negligible at demo sizes.

How the `prototype` branch evolved the same ideas:

| Demo (spec v1) | Prototype (spec v3) |
|---|---|
| Debezium-shaped events from a simulated finance ERP | MySQL row-based binlog transactions (GTIDs), generated offline and replayed |
| `actor` in the event | No actor: a database log doesn't record the application user |
| Only the root is anchored; proofs live in the store | **Every leaf** is anchored with the root, so each record is checked against its own leaf on the ledger, and proofs are derived from the ledger |
| In-memory `SimLedger` | A Daml model on a real Canton sandbox (JSON Ledger API v2) |
| Direct crate dependencies | Ports and adapters: `ChangeSource`, `AuditLedger` and `TrailRecord` traits in core |
| Synchronous | Async (tokio): connector task → bounded channel → node loop |
| Log positions per event | GTID windows and transaction counts anchored, so missing transactions are visible from the ledger |

---

## 13. File index

| File | Chapter |
|---|---|
| `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml`, `Makefile`, `Dockerfile`, `render.yaml`, `.gitignore`, `.dockerignore` | 1 |
| `crates/sthira-core/src/lib.rs` | 2.1 |
| `crates/sthira-core/src/event.rs` | 2.2 |
| `crates/sthira-core/src/merkle.rs` | 2.3 |
| `crates/sthira-core/tests/vectors.rs`, `test-vectors/*` | 2.4 |
| `crates/sthira-ledger/src/lib.rs` | 3 |
| `crates/sthira-profiles/src/debezium.rs` | 4.1 |
| `crates/sthira-profiles/src/source.rs` | 4.2 |
| `crates/sthira-profiles/src/lib.rs` | 4.3 |
| `crates/sthira-batcher/src/lib.rs` | 5 |
| `crates/sthira-verify/src/lib.rs` | 6 |
| `demo/demo-director/src/world.rs` | 7.1 |
| `demo/demo-director/src/views.rs` | 7.2 |
| `demo/demo-director/src/api.rs` | 7.3 |
| `demo/demo-director/src/main.rs`, `lib.rs` | 7.4 |
| `demo/ui/index.html`, `package.json`, `tsconfig.json`, `vite.config.ts`, `src/main.tsx`, `src/api/*`, `src/ui.tsx` | 8.1 |
| `demo/ui/src/App.tsx` | 8.2 |
| `demo/ui/src/components/Chrome.tsx` | 8.3 |
| `demo/ui/src/components/MerkleTree.tsx` | 8.4 |
| `demo/ui/src/tabs/*.tsx` | 8.5 |
| `demo/ui/src/style.css`, `src/assets/*` | 8.6 |
| `demo/demo-director/tests/*.rs` | 9 |
| `README.md` | how to run and present the demo, step by step |
