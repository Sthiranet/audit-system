# Sthiranet audit system: prototype

Sthiranet's Audit System, provides **integrity-protection** of an organization's audit trails. A node next to
the database captures every committed row change. It seals the changes into batches every few seconds
and anchors fingerprints of each batch on a **Canton** ledger, co-signed by the company and its
auditor. Later, the auditor can take the company's audit trail and prove, record by record, that
nothing was changed, deleted or invented since capture. The auditor needs only its own copy of the
ledger to do this.

> The web demo lives on the `demo` branch. This branch (`prototype`, followed by `main`) is the
> prototype and the foundation for the production code.

## What the prototype does today

| Feature | What it means |
|---|---|
| **Offline MySQL binlog data** | `gen` writes a deterministic ERP workload (users, customers, orders, invoices, payments) as row-based MySQL binlog transactions with GTIDs. `run` replays it as a live change stream. No database or network source is needed. |
| **Spec v3 events** | Every row change becomes one canonical event: server, db, table, GTID, binlog file and position, primary key, operation, and full before/after images. It is hashed with a 32-byte salt into a **leaf**. |
| **Leaves + Merkle root on the ledger** | Each batch anchors every leaf hash, the RFC 6962 Merkle root and a **chained head**. The head covers the previous head, the root and the batch metadata. No record content reaches the ledger. |
| **Co-signed, unforkable chain** | The Daml model on Canton accepts an anchor only if it continues the chain: next sequence number, matching previous head, leaves consistent with the counts. Every anchor is signed by every member, so no single party can rewrite it. |
| **Record-level verification** | The auditor recomputes each record's leaf and compares it with the leaf **on its own ledger copy**. An edited record is pinpointed to its exact slot, even when the insider also rewrote the stored hash. Deleted and invented records are reported too. |
| **Completeness from the ledger** | GTID windows and transaction counts are anchored. Transactions that never reached the binlog the node reads (purged log, `sql_log_bin=0`, capture down) show up as gaps, using ledger values alone. |
| **Selective disclosure** | `proof` exports one record with its salt and inclusion path. A third party checks it against the ledger without seeing any other record. |

For a guided tour of the code itself (every crate, file and design decision, in dependency
order), read [`docs/audit-system-prototype.md`](docs/audit-system-prototype.md).

## Layout

```
crates/
  sthira-core       spec v3 event, canonical JSON, leaves, Merkle trees, heads, and the port traits
                    (ChangeSource, AuditLedger, TrailRecord) every other crate talks through. Pure.
  sthira-connector  database change-log capture: MySQL binlog types, the dummy-data generator, ReplaySource
  sthira-ledger     CantonLedger: AuditLedger over the JSON Ledger API v2 (async)
  sthira-batcher    the node: sealing, anchoring, the off-ledger audit store on disk
  sthira-verify     the verifier: record, root, head and GTID-completeness checks
  sthira-node       the CLI and composition root (the only crate that picks concrete types)
prototype-sandbox/  the Daml model (Sthiranet.Audit) and the Canton sandbox script; see its README
test-vectors/       hashing spec v3 vectors from an independent Python implementation
```

## Prerequisites

- Rust (stable, 1.94 or newer).
- `dpm`, the Digital Asset package manager, with SDK 3.6.4 installed (`dpm install 3.6.4`). It runs the Canton sandbox.

## Run it, step by step

Use two terminals from the repository root. All commands below run in terminal 2 unless stated.

**1. Start the ledger** (terminal 1)

```bash
make sandbox
```

This builds the Daml package and starts a Canton sandbox (one participant, one synchronizer) on
`http://localhost:7575`. Leave it running. *Showcases:* the real ledger the anchors live on. The
sandbox keeps its state in memory, so a restart forgets every chain.

**2. Build the CLI and generate the change log**

```bash
make node
alias sthira-node=./target/release/sthira-node
sthira-node gen                     # → changes.jsonl: 200 transactions, seed 7
```

*Showcases:* the capture input. Each line is one committed MySQL transaction, as a binlog reader would
deliver it: GTID, commit time, and every changed row with its full images. Options: `--tx N`,
`--seed N`, `--start <RFC 3339>`, `--out <file>`, `--force`.

**3. Run the company's node**

```bash
sthira-node run --batches 6
```

On first start the node does three things:
1. It allocates the parties `DREAMTALK` (the company) and `AUDITOR` on Canton.
2. Both sign the chain.
3. It replays the change log at 2 transactions per second and seals a batch every 10 seconds.

Each batch line shows the GTID window, the root, the head and `anchored N leaves, signed
AUDITOR+DREAMTALK`. Below it is every record in plain words; changes worth an auditor's attention
(privilege raise, credit-limit raise, deleted invoice) are marked ⚑.

*Showcases:* continuous capture, sealing and co-signed anchoring.

Useful options:
- `--interval` seconds per batch.
- `--rate` transactions per second (0 means as fast as possible).
- `--chain`, `--company`, `--auditor` to name the chain and parties.
- `--data <dir>` for a separate chain.

Ctrl-C stops the node cleanly, and running `run` again resumes the same chain.

**4. Look at what the ledger holds**

```bash
sthira-node ledger --last 5          # add --leaves to list every anchored leaf hash
sthira-node status
sthira-node trail --flagged
```

*Showcases:* the ledger holds only fingerprints (leaf hashes, root, head, counts, GTID window), and it
is read from the **auditor's own copy** on Canton. `status` and `trail` show the company's side:
records, tables and flagged changes.

**5. Verify as the auditor**

```bash
sthira-node verify                   # PASS
```

*Showcases:* the auditor recomputes every leaf, root and head from the company's trail, compares them
with its ledger copy, and checks that no GTID is missing.

**6. Tamper as an insider, and get caught**

```bash
sthira-node trail --last 10                          # pick record ids, e.g. DT-23.0
sthira-node tamper edit DT-23.0 --cover              # change an amount AND rewrite its stored hash
sthira-node tamper delete DT-5.0                     # remove a record
sthira-node verify                                   # FAIL
```

The report names the exact record and its slot (`ALTERED L<slot> DT-23.0 … still caught`), the
deleted slot (`DELETED L<slot>`), and the later batches whose heads no longer chain.

*Showcases:* someone with full write access to the company's audit store can't hide an edit, even by
recomputing hashes, because each record is checked against its own leaf on the ledger.

Then put everything back:

```bash
sthira-node untamper && sthira-node verify           # PASS again
```

**7. Prove a single record to a third party**

```bash
sthira-node proof DT-14.0 --export proof.json
sthira-node check-proof proof.json
```

*Showcases:* selective disclosure. The bundle carries one record, its salt and its inclusion path. The
checker uses the root and leaf from its own ledger copy, not the ones in the bundle.

**8. Try to rewrite history on the ledger**

```bash
sthira-node rewrite 1                # as the company
sthira-node rewrite 1 --as AUDITOR   # as the auditor
```

*Showcases:* Canton refuses both attempts with `DAML_AUTHORIZATION_ERROR`, because every anchor needs
every member's signature to be archived. Neither the company nor the auditor can rewrite history alone.

**9. Lose transactions before capture**

```bash
sthira-node --data ./sthira-gap run --chain GAP --skip-gtids 40,41,42 --batches 3
sthira-node --data ./sthira-gap verify
```

The node reports `⚠ gap GTIDs 40–42 never reached the binlog`. The auditor's report shows the
missing GTIDs under *completeness*, computed from ledger values only.

*Showcases:* deletions at the source before capture can't be hidden either. The dense GTID sequence is
anchored, so the gap stays visible to every member.

## Tests

```bash
make test-unit    # hashing vectors, generator, replay, sealing, persistence, verifier (no ledger)
make daml-test    # the Daml model's rules
make test         # everything, including the Canton tests (starts and stops a sandbox if none is running)
make check        # rustfmt + clippy -D warnings
```

To reset a chain, stop the node and delete its data directory (`./sthira-data` by default). After a
sandbox restart, always start a new chain this way. `make clean` removes everything the steps above
generate (build output, `changes.jsonl`, `sthira-data/`, `sthira-gap/`, `proof.json`, sandbox logs).
