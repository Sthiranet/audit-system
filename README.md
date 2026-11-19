# Sthiranet · Audit Trail Prototype

`sthira-node` is a command-line node that takes a **live application's audit log**, anchors it on a
**Canton ledger**, and lets an independent auditor prove that the log was not altered afterwards. If it
was, the node names the exact record.

It reads the Dreamtalk backend's `/audit/` API. Every 10 seconds it seals the new rows into a batch, and
each batch is fingerprinted with a Merkle root and chained to the batch before it. Each batch's
fingerprint becomes a Daml contract co-signed by the company and its auditor on a **Canton 3.6
sandbox**. Only fingerprints go on the ledger, never record content or personal data. Anyone holding
the trail and their own copy of the ledger can recompute everything. An edited row, a deleted row, a row
blamed on someone else, or a row that never reached the node all show up.

This is a V1. It is not production-ready, but it shows the value proposition end to end from the CLI.

> **Branches.** `main` always follows the latest line of work, which is currently the prototype, so
> `main` and `prototype` are identical. The **web demo** (a browser app that tells the same story with a
> simulated ERP and an in-memory ledger) lives on its own branch, `demo`:
> `git switch demo`.

## Contents

1. [Quick start](#1-quick-start)
2. [How it works](#2-how-it-works)
3. [Command reference: every command and flag](#3-command-reference)
4. [Customising a presentation](#4-customising-a-presentation)
5. [Presenting it to partners: a 15-minute script](#5-presenting-it-to-partners)
6. [Limits and likely questions](#6-limits-and-likely-questions)
7. [Development](#7-development)

---

## 1. Quick start

**Requirements**

- Rust, stable 1.94 or newer. `rust-toolchain.toml` pins it.
- For Canton anchoring: the Daml package manager `dpm` with SDK **3.6.4** (`dpm install 3.6.4`). Canton
  also needs a Java runtime.
- Internet access to the audit API. If you have none, use the bundled offline fixture (see
  [`--fixture`](#run-capture-seal-and-anchor)).

```bash
make node                                        # builds ./target/release/sthira-node
make sandbox                                     # terminal T0: wait for "Canton sandbox is ready." (20–40 s)
alias N='./target/release/sthira-node --data ./sthira-data'

N run --ledger canton                            # terminal T1: the company's node, Ctrl-C to stop
N verify                                         # terminal T2: the auditor's check
```

If Canton is unavailable, leave out `--ledger canton`. The node then anchors on an in-memory stand-in
ledger, and every command works the same way.

All state lives in the data directory (`./sthira-data` by default). For a fresh chain, delete that
directory and restart the sandbox: `rm -rf ./sthira-data`.

---

## 2. How it works

### 2.1 The parties and what each one holds

```
 Dreamtalk backend                   Company's node (sthira-node run)              Ledger (co-signed)
 ─────────────────                   ────────────────────────────────              ──────────────────
 its DB → its audit table  ──GET──►  cursor → normalise → batch → seal ──anchor──► one AuditAnchor per batch
 /audit/?page=N (JSON)               off-ledger audit store:                       {seq, prev head, root,
                                       <data>/node/trail.jsonl                       head, count, positions,
                                       (event, salt, leaf, proof)                    window} signed by
                                                                                     DREAMTALK + AUDITOR

 Auditor (sthira-node verify / check-proof): takes a trail + ITS OWN ledger copy, recomputes, compares.
```

| Party | Holds | Never needs |
|---|---|---|
| Source app (Dreamtalk) | its database and its own audit table | to change anything; the node only reads its API |
| Company node (`DREAMTALK`) | the audit store: every captured row, plus its salt, leaf hash and inclusion proof | n/a |
| Ledger | anchors: hashes, counts, positions and time windows | record content or personal data |
| Auditor (`AUDITOR`) | its own copy of every anchor (it co-signed them) | the company's cooperation, or trust in its admins |

### 2.2 From an audit row to a hashed event

The API serves rows like this one (newest first, 100 per page):

```json
{ "id": 190850, "user_id": null, "user_name": null, "org_id": null, "http_method": "GET",
  "operation": "INSERT", "table_name": "api_request_log", "record_id": "44459",
  "old_data": null, "new_data": { "...": "the row after the change" },
  "domain": null, "created_at": "2026-11-18T05:39:01.767051Z" }
```

Each row is mapped to a **spec v2 event**, which is the exact structure that gets hashed:

| API field | Event v2 field | Why it matters |
|---|---|---|
| `id` | `source.pos` | the audit log's own sequence number, which lets missing rows be spotted |
| `table_name`, `record_id` | `source.table`, `key.id` | which record changed |
| `operation` | `op` | insert, update or delete |
| `old_data`, `new_data` | `before`, `after` | what the record was and what it became |
| `user_id`, `user_name`, `org_id` | `actor.{id, name, org}`, `actor.kind` | who made the change (`system` when nobody is named) |
| `http_method`, `domain` | `source.channel`, `source.origin` | how the change was triggered |
| `created_at` | `committed_at` | when the app logged it |
| (the node's clock) | `captured_at` | when the node received it |
| API host | `source.system` | which system the trail belongs to |

Every field is always present (`null` when it is empty), so each change has exactly one canonical form.
The hashing is spec v1's: RFC 8785 canonical JSON, 32-byte salts and RFC 6962 Merkle trees. An
independent Python implementation (`test-vectors/generate_v2.py`) produces reference values that the
Rust code must match.

### 2.3 Reading the API without missing anything

The endpoint is awkward for a capture tool. It serves rows newest first, in numbered pages, and has no
"everything after id X" filter. While the node pages backwards, new rows arrive at the top and push
older rows down. The node's cursor handles this in four ways:

1. **It walks back to where it was.** It reads page 1, 2, 3… until it meets an id it already has. A row
   that shifts between pages is seen twice and de-duplicated by id, so no row can be skipped.
2. **It emits rows in id order only.** If id 1006 is missing but 1007–1010 have arrived, 1007–1010 are
   **held**, because a transaction that took id 1006 may still be committing.
3. **It waits for a grace period, then declares a gap.** If 1006 hasn't arrived after `--grace` seconds,
   the node prints `⚠ gap` and moves on. If 1006 turns up later, it is still captured and marked
   **late**.
4. **It never stores a row twice**, even after a restart.

Every request the node makes causes the app to write about two `api_request_log` rows, so the chain
always has traffic while the node runs. These rows are anchored like every other row, which keeps the id
sequence complete, but they are hidden in listings.

### 2.4 Sealing a batch

At the end of each window (`--interval`), the node seals everything pending. It seals an **empty**
batch too, so silence is recorded as well:

```
salt    = 32 random bytes per event (kept in the store, never on the ledger)
leaf    = SHA-256(0x00 ‖ salt ‖ canonical JSON of the event)
root    = RFC 6962 Merkle tree over the batch's leaves
meta    = {chain, seq, from_pos, to_pos, from_time, to_time, event_count}
head    = SHA-256(0x02 ‖ previous head ‖ root ‖ canonical meta)
proof   = for each leaf, the sibling hashes from it up to the root
```

- The **salt** stops anyone from guessing a record from its hash.
- The **root** commits to every record in the batch and to their order.
- The **head** commits to every earlier batch, so changing one batch breaks every head after it.
- The **proof** lets one record be checked against the root on its own, without revealing any other
  record.

On Canton, the anchor is written by exercising the Daml choice `AuditChain.Append`. This choice refuses
an append whose `seq` or `prev_head` doesn't continue the chain, so the chain cannot fork or skip a
batch.

### 2.5 What is on disk

```
sthira-data/
  ledger/chain.json        chain id, members, genesis head, start time, source URL, ledger kind (canton/sim)
  ledger/anchors.jsonl     one anchor per batch: the node's receipt (Canton) or the stand-in ledger (sim)
  node/trail.jsonl         one row per captured event               ← the company's audit store
  node/batches.jsonl       the node's own batch table
  node/cursor.json         the next audit id to read, and any open gaps
  node/tamper-log.jsonl    originals of rows changed by `tamper` (so `untamper` can restore them)
```

On Canton, `anchors.jsonl` is only the node's local receipt. `verify`, `ledger` and `check-proof` read the
anchors from the **auditor party's own view on the participant**, so editing or deleting this file
changes nothing for the auditor.

Killing the node at any moment is safe. On restart it drops anything that never reached the ledger,
resumes from the saved cursor and re-reads the source, so nothing is lost or duplicated.

### 2.6 How verification works

| Question | Evidence | Shown as |
|---|---|---|
| Is batch #N intact? | the recomputed root vs the anchored root | `root ✓/✗` |
| Is the chain unbroken? | the recomputed head vs the anchored head | `head ✓/✗` |
| Are all records there? | the trail's count vs the anchored count | `count ✓/✗` |
| **Which** record changed? | each record's recomputed leaf, plus its proof, vs the anchored root | `ALTERED L7 DT-191033 …` |
| **Which** slot is empty? | slots the anchor committed to that no record claims | `DELETED L50` |
| Were audit rows missed? | anchored positions vs counts, **from the ledger only** | `completeness … GAP ids …` |

- **Covering tracks doesn't help.** Suppose an insider edits a record and also recomputes its stored
  leaf hash (`tamper --cover`). The record is still caught, because it is checked against the root on
  the ledger.
- **The cascade is explained.** One edit in batch #2 makes #3, #4… fail their head check too. The report
  separates "contents changed" from "intact, but chains on an altered batch".
- **Completeness comes from the ledger.** The company cannot hide a gap by editing its own files.

### 2.7 Descriptions and flags

Each change gets a one-line label for non-technical viewers, for example
`User account updated · #724 · role User → ORG_ADMIN`. Some changes are also flagged:

- `privilege-change`: a role, staff or superuser flag, active status or app access changed. The live
  output shows this as `⚑ privilege change`.
- `no-before-image`: an update that the source logged without its previous values.
- `system-actor`: a business change with no user attached.

The labels and flags are for display only and are never hashed.

### 2.8 Anchoring on Canton

The Daml package `sthiranet-audit` is in `prototype-sandbox/`. See
[`prototype-sandbox/README.md`](prototype-sandbox/README.md) for details.

```
ChainProposal ──Sign (each member)──► ChainProposal ──Finalize──► AuditChain
AuditChain    ──Append (company only)──► AuditChain (next) + AuditAnchor
```

The auditor signs **once**, when the chain is set up. From then on, its authority flows into every anchor
through the chain contract. The company appends batches alone, but no single party can archive an
anchor. Over the JSON Ledger API v2, the node does the following:

1. **First run:** allocate the company and auditor parties, propose the chain, have the auditor sign it,
   and finalise it.
2. **Every batch:** exercise `Append`. The new contract id is printed as `cid …`.
3. **Auditor commands:** read the active `AuditAnchor` contracts visible to the auditor party.
4. **`rewrite`:** try to archive an anchor as one party. Canton answers with `DAML_AUTHORIZATION_ERROR`.
5. **Resume:** find the chain, and check that every anchor recorded locally is on the ledger with the
   same head.

---

## 3. Command reference

```
sthira-node [--data DIR] [--no-color] <command> [options]
```

Run `sthira-node --help` or `sthira-node <command> --help` for the built-in help.

**Exit codes:** `0` means OK. `1` means verification found a problem. `2` means a usage or I/O error.
Scripts can rely on these codes.

### Global options (work with every command)

| Flag | Default | What it does |
|---|---|---|
| `--data DIR` | `./sthira-data`, or the `STHIRA_DATA` environment variable | The data directory: one chain, its audit store and its ledger receipts. Use a different directory per chain to keep several chains side by side, for example a rehearsal chain and a live chain. |
| `--no-color` | colours on | Plain output. Use it for projectors with poor contrast, for screen readers, or when piping into a file. |

### `run`: capture, seal and anchor

Starts the company's node. On a new data directory it creates a chain. On an existing one it resumes
that chain, and the options marked *new chain only* are ignored.

| Flag | Default | What it does and when to use it |
|---|---|---|
| `--ledger canton\|sim` | `sim` | *New chain only.* `canton` anchors every batch as a Daml contract on the sandbox (use it for presentations). `sim` uses the in-memory stand-in, which is replayed from `ledger/anchors.jsonl`. |
| `--ledger-url URL` | `http://localhost:7575`, or the `STHIRA_LEDGER_URL` environment variable | The Canton participant's JSON Ledger API v2. Change it if the sandbox runs on another port or machine. |
| `--ledger-user ID` | `sthira-node` | The Ledger API user id sent with each command. |
| `--api URL` | `https://dreamtalk-backend.vishvena.com/audit/` | *New chain only.* The audit API endpoint to capture from. |
| `--fixture FILE` | none | Replay rows from a JSON file (one API page, or a list of rows) instead of calling the API. Use it offline or for a fully predictable run: `crates/sthira-profiles/tests/fixtures/audit_api/page.json` has 20 synthetic rows, ids 5001–5020, covering every table and operation, including a privilege escalation (`DT-5011`). |
| `--start head\|pages:N\|id:N` | `pages:1` | *New chain only.* Where capture begins. `head` takes only rows that appear from now on. `pages:N` first backfills the newest N pages (100 rows each). `id:N` starts at audit id N, which lets you open on a known stretch of business activity (a cold start far back costs about one request per 100 rows). |
| `--interval S` | `10` | Seconds per batch. Use `5` for a fast-moving demo with many batches. Use `30` or more so each batch carries more rows. |
| `--grace S` | `30` | How long to wait for a missing audit id before declaring a gap. Lower it (for example to `10`) so a gap appears quickly on stage. |
| `--batches N` | run until Ctrl-C | Stop after N batches. Use it for scripted rehearsals, or to build a chain of known length before a meeting. |
| `--hide-ids a,b,…` | none | *Presenter.* Pretend the source deleted these audit ids before capture, so they never arrive and a gap is anchored. |
| `--chain ID` | `DT` | *New chain only.* The chain id, which prefixes record ids (`DT-190850`). Use a new id (for example `DT2`) when you start a second chain on the same running sandbox. |
| `--company NAME` | `DREAMTALK` | *New chain only.* The company party, which submits batches. Rename it to match the audience's organisation. |
| `--auditor NAME` | `AUDITOR` | *New chain only.* The co-signing party that keeps its own copy, for example `KPMG` or `REGULATOR`. |

### Read-only views

| Command | Options | What it shows |
|---|---|---|
| `status` | none | Chain, members, batches, record counts, positions, cursor, tables, flags and tamper state: the company's overview. |
| `ledger` | `--last N` (default `20`) | The anchors as the **auditor** holds them: hashes, counts, positions and times, with no content. On Canton they are read from the participant. |
| `trail` | `--batch S` only batch S · `--all` include request-log rows · `--last N` (default `40`) | Captured records with their description, actor, leaf hash and flags. Use it to pick a record to tamper with or prove. |

### Proofs

| Command | Options | What it does |
|---|---|---|
| `proof <target>` | `--event` print the full event as hashed · `--export FILE` write a proof bundle | Proves that one record is in its anchored batch. `<target>` is a record id (`DT-191035`), or `table:key` (`app_user:724`) for every captured version of a record. |
| `check-proof FILE` | none | Checks an exported bundle against **this** data directory's ledger. The root written inside the bundle is ignored. |

### The insider (presenter tools)

| Command | Options | What it does |
|---|---|---|
| `tamper edit <DT-id>` | `--cover` also rewrites the stored leaf hash so the row looks self-consistent | Changes a field of the stored record. |
| `tamper actor <DT-id>` | `--cover` | Changes who the record says made the change. |
| `tamper delete <DT-id>` | none | Removes the record from the store. |
| `untamper` | none | Puts back every record changed by `tamper`. |
| `rewrite <seq>` | `--as PARTY` (default: the company) | Tries to archive anchor `<seq>` on the ledger as **one** party. The ledger refuses. |

`tamper` and `untamper` work while `run` is running.

### The auditor

| Command | Options | What it does |
|---|---|---|
| `verify` | `--trail FILE` verify a copy someone handed over instead of the node's own trail · `--all` list every batch, not only failing ones · `--json` machine-readable full report | Recomputes the trail and compares it with the anchors. Exits `1` if anything is wrong. |
| `reconcile` | `--pages N` (default `3`) | Re-reads the newest N API pages and compares them with what was anchored. It shows rows that the source changed or removed **after** capture. |

---

## 4. Customising a presentation

| You want… | Use |
|---|---|
| The audience's own names on the parties | `N run --ledger canton --company ACME --auditor REGULATOR --chain AC` |
| Batches scrolling faster | `--interval 5` |
| Real business changes on screen in the first minute | `--start id:<id>`; see the command below to find a good id |
| Only what happens during the meeting | `--start head` |
| A fully predictable, offline run | `--fixture crates/sthira-profiles/tests/fixtures/audit_api/page.json --start id:5001` |
| A chain prepared before the meeting | `N run --ledger canton --batches 12`, then resume with `N run` during the meeting |
| A second chain without restarting Canton | a new `--data` directory and a new `--chain` id |
| A quick gap demonstration | `--hide-ids <id> --grace 10` |
| Every batch in the verify output, not just failures | `N verify --all` |
| A report to hand over or diff | `N verify --json > report.json` |
| Clean output for a projector or a recording | `--no-color` |

To find a starting id just before the latest business activity:

```bash
curl -s 'https://dreamtalk-backend.vishvena.com/audit/?page_size=1&table_name=app_user' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["response"]["results"][0]["id"] - 50)'
```

---

## 5. Presenting it to partners

This takes about 15 minutes and needs three terminals with a large font:

- **T0** runs the Canton sandbox. You can minimise it once it is ready.
- **T1** runs the company's node.
- **T2** is the auditor, and the insider.

### 5.0 Before the meeting

```bash
make node
make sandbox                                # T0: wait for "Canton sandbox is ready."
alias N='./target/release/sthira-node --data ./sthira-data'
rm -rf ./sthira-data                        # a fresh chain
```

Start the sandbox before the meeting and leave it running. Its storage is in memory, so if it restarts,
also `rm -rf ./sthira-data`.

### Act 1 · Live capture (T1, 2 min)

```bash
N run --ledger canton --start id:<start id>
```

The audience sees one line per batch. Each line shows the batch number, the event count, the audit ids
covered, the root and the head, `anchored signed AUDITOR+DREAMTALK`, and the Canton contract id.
Business changes appear underneath with their meaning.

> "This is Dreamtalk's own audit log, read live through its API. We changed nothing in their app. Every
> ten seconds the new rows become one fingerprint, chained to the previous one, and written to Canton as
> a contract co-signed by the company and the auditor. Even when nobody does anything, a batch is
> sealed. Silence is recorded too."

Leave T1 running.

### Act 2 · What the auditor holds (T2, 1 min)

```bash
N ledger --last 8
N status
```

> "This is the auditor's view, read from the ledger, not from our files: sequence numbers, hashes,
> counts, positions and times. No names, no emails, no content. Yet it's enough to check every record."

### Act 3 · A clean audit (T2, 1 min)

```bash
N verify
```

The output is `PASS … the chain of N heads is unbroken` and `audit ids A–B, none missing`.

### Act 4 · The insider (T2, 4 min)

```bash
N trail --last 15                    # pick a business record, ideally one flagged privilege-change
N tamper edit DT-191035
N verify
```

The output is `#k root ✗ head ✗ … CONTENTS CHANGED`, then `ALTERED L7 DT-191035 · …`, and every later
batch is marked *intact, but chains on an altered batch*.

> "One record changed in one batch, and the auditor names it. Every batch after it fails too, because
> history is chained."

Escalate by making the insider smarter:

```bash
N tamper edit DT-<another> --cover   # also rewrites the stored fingerprint
N tamper actor DT-<another>          # pins the change on someone else
N tamper delete DT-<another>         # removes a record entirely
N verify
N untamper && N verify               # PASS again
```

All three are caught. The covered edit, the re-attributed record and the deletion (`DELETED L4`) each
show up.

### Act 5 · Prove one record, reveal nothing else (T2, 2 min)

```bash
N proof DT-191035 --export proof.json
N check-proof proof.json
```

> "A regulator asks: did this change happen, and is this what it said? We hand over one file, with one
> record and a few hashes. They check it against their own ledger copy and never see another record."

### Act 5b · Rewrite history on the ledger itself (T2, 1 min)

```bash
N rewrite 3
N rewrite 3 --as AUDITOR
```

Both attempts are refused with `DAML_AUTHORIZATION_ERROR … requires authorizers AUDITOR,DREAMTALK`.

> "This refusal isn't our code. It's Canton enforcing the contract. Neither the company nor the auditor
> can remove an anchor alone."

### Act 6 · Rows that never arrive (T1 + T2, 3 min)

1. In T1, press Ctrl-C. Stopping the node is safe.
2. In T2, run `N status` and note `cursor next audit id X`.
3. In T1, run `N run --hide-ids $((X+3)) --grace 10`. Within about 20 seconds it prints
   `⚠ gap audit ids …`.
4. In T2, run `N verify`. Integrity is `PASS`, and the output adds
   `completeness: … 1 id(s) never captured`.

> "Everything we anchored is intact. But the app's own log numbering has a hole, and that hole is written
> into co-signed anchors. Note too that the node picked up exactly where it stopped."

### Act 7 · Has the source changed since? (T2, 1 min)

```bash
N reconcile --pages 3
```

> "If anyone edits or deletes an audit row in Dreamtalk's own table after we captured it, it shows up
> here, and our anchored copy proves the original."

### Close

> "This ran on Dreamtalk's real audit log with no change to their system, anchored on Canton. In this
> sandbox, both parties share one node. The next step gives the auditor its own participant, so its copy
> physically lives with the auditor. The contracts and the node don't change."

### Reset after a rehearsal

```bash
N untamper             # if anything is still altered
rm -rf ./sthira-data
# Ctrl-C in T0, then `make sandbox` again for an empty ledger (or keep it and use a new --chain id)
```

---

## 6. Limits and likely questions

**Limits:**

- **Integrity starts at capture.** If a row was altered in the app's audit table before the node read it,
  the altered version is what gets anchored. Gaps and `reconcile` make later removals and edits
  visible.
- **It runs on a sandbox, not a network.** One participant hosts both parties, so the auditor's copy
  lives on the same node. On a real network, the auditor's participant runs elsewhere.
- **The sandbox forgets.** Its storage is in memory, so a chain can't be resumed on a restarted sandbox.
- **It proves integrity, not truth.** A false change made by a legitimate user is anchored faithfully.

**Findings about the source:**

- Most business changes are logged twice, about 5 ms apart.
- Many updates carry no before image.
- Most rows have no user.
- The audit endpoint is public and serves personal data without authentication.

| Question | Answer |
|---|---|
| Does any personal data leave the company? | No. The ledger holds hashes, counts, positions and times, and salts stop anyone guessing a record from its hash. |
| What if the company deletes its whole audit store? | The anchors still prove what existed, and verification fails for every batch with missing records. |
| Why a ledger and not a signed file? | The auditor holds its own co-signed copy, and no single party can rewrite it. |
| Is this a blockchain? | It's Canton: each party's node keeps only the contracts it's party to. There is no public chain and no token. |
| How much load does this put on the app? | About one request per 10 s when caught up, plus one per 100 new rows. |

---

## 7. Development

```bash
make node            # release binary at target/release/sthira-node
make sandbox         # Canton 3.6.4 sandbox with the sthiranet-audit DAR (JSON Ledger API :7575)
make daml-test       # the Daml Script tests of the anchoring model
make test            # all Rust tests
make check           # fmt + clippy -D warnings
make clean           # cargo clean (./sthira-data is kept)
STHIRA_CANTON_URL=http://localhost:7575 cargo test -p sthira-ledger --test canton_sandbox   # needs the sandbox
```

| Path | What it holds |
|---|---|
| `crates/sthira-core` | the hashing kernel: events v1 and v2, canonical JSON, Merkle trees and proofs |
| `crates/sthira-ledger` | the `AuditLedger` trait, `SimLedger`, and `CantonLedger` (JSON Ledger API v2) |
| `crates/sthira-profiles` | source connectors; `audit_api/` holds the Dreamtalk profile (cursor, normalise, describe, tamper) |
| `crates/sthira-batcher` | the node: capture, sealing, anchoring, and on-disk persistence |
| `crates/sthira-verify` | the verifier, completeness checks, and the `sthira-node` binary |
| `prototype-sandbox/` | the Daml package and the sandbox script |
| `test-vectors/` | independent Python reference implementations and their vectors (v1 and v2) |
