# Sthiranet audit system: web demo

Sthiranet's Audit System protects the **integrity** of an organization's audit trails. This demo is a
small web app for live presentations. It walks through one story: a corporate group's financial
changes are captured, batched, hashed into Merkle roots and co-signed on a ledger. Later, recomputing
the hashes catches a trail that was altered and names the exact record that changed.

The cast: **HQ** in Region R, subsidiaries **S1, S2 and S3** in Regions R1–R3, and an external
**auditor** who can join at any time.

> This is the `demo` branch. It holds only the web demo, which is self-contained: a simulated ERP and
> an in-memory ledger. The prototype, a CLI that anchors on a real Canton sandbox, is on the
> `prototype` branch, and `main` follows it.

## What the demo does today

| Feature | What it means |
|---|---|
| **Simulated ERPs with Debezium events** | Each subsidiary has an in-memory ERP (journal entries and vendors on PostgreSQL or Oracle) that emits change events in Debezium's JSON shape. No database is needed. |
| **Spec v1 events** | Every change becomes one canonical event: connector, db, schema, table, log position (LSN or SCN), transaction, key, operation, before/after images and actor. It is hashed with a 32-byte salt into a **leaf**. |
| **Merkle roots on a co-signed ledger** | Each batch anchors its RFC 6962 Merkle root and a **chained head** (previous head, root and batch metadata). Every anchor is co-signed by the subsidiary and HQ (and the auditor, once it joins). No record content reaches the ledger. |
| **Every party keeps its own copy** | The in-memory ledger reproduces Canton's model: each member's node stores its own copy of the chains it signed, non-members see nothing, a fork or a skipped sequence is refused, and archiving an anchor needs every signatory. |
| **Verification against your own copy** | HQ (or the auditor) recomputes every leaf, root and head from a trail and compares them with **its own** ledger copy. An altered record is pinpointed through its inclusion proof, even when the insider also rewrote the stored hash. Deleted records and the cascade into later batches are reported. |
| **What the operator sees** | A view of the synchronizer's traffic: recipients, sizes and ciphertext, never contents. |
| **Live, shared UI** | A React UI driven by a Rust server. Every change is pushed to all open browsers over server-sent events, so a presenter and an audience screen stay in sync. |

For a guided tour of the code itself (every crate, file and design decision, in dependency
order), read [`docs/audit-system-demo.md`](docs/audit-system-demo.md).

## Layout

```
crates/
  sthira-core       spec v1 event, canonical JSON, leaves, Merkle trees, inclusion proofs, heads. Pure.
  sthira-ledger     the AuditLedger trait and SimLedger, an in-memory stand-in for Canton
  sthira-profiles   the simulated ERP and Debezium events, normalisation, descriptions, tampering
  sthira-batcher    a subsidiary's node: capture, batch sealing, anchoring, the off-ledger audit store
  sthira-verify     the verifier: leaf, root and head checks against the verifier's own ledger copy
demo/
  demo-director     the server: the in-memory world, scenarios, auto-anchoring, the HTTP + SSE API
  ui/               the React + TypeScript UI (Vite), typed from the server's OpenAPI spec
test-vectors/       hashing spec v1 vectors from an independent Python implementation
Dockerfile, render.yaml   one-image build and the Render Blueprint for free hosting
```

## Prerequisites

- Rust (stable, 1.94 or newer).
- Node.js 22.12 or newer. It is only used to build the UI.

## Run it, step by step

**1. Start the demo**

```bash
make install         # once: installs the UI dependencies
make start           # builds the UI and the server, then serves http://localhost:3000
```

Open http://localhost:3000. Use `PORT=4000 make start` for another port, and `make stop` to stop the
server.

*Showcases:* the network opens already running. Each subsidiary has 3 anchored batches, and HQ already
holds a copy of each trail. The top bar has **Auto-anchor every 10 s**, **Add auditor** and **Reset
demo**. The strip at the bottom shows who co-signs, who holds ledger copies and whose trails each party
holds. The activity panel on the side logs every action.

For full control, turn **auto-anchor off**. Leave it on for a "live" feel: each subsidiary then
captures a few changes and seals a batch every 10 seconds. The steps below follow the story that
`make test` checks automatically (`demo/demo-director/tests/scenario.rs`).

**2. Normal operation** (tab *1 Capture & batch*)

Pick S1, capture changes, then seal a batch. Click any change to see the raw Debezium event next to
the normalised event that gets hashed.

*Showcases:* each change is salted and hashed into a leaf. The Merkle tree builds up to one root on
screen, and the root is anchored, co-signed by S1 and HQ, and chained to batch #3.

**3. Who sees what** (tab *2 Ledger*)

Switch between parties' nodes.

*Showcases:* HQ holds copies of all three chains, S1 sees only its own, and S2 cannot see S1's. The
ledger holds only fingerprints: root, head, previous head, counts, time window and signatures. *What
the synchronizer sees* lists only recipients, sizes and ciphertext.

**4. Silence is visible** (tab *1*)

Seal a batch with nothing pending.

*Showcases:* an empty batch is anchored too, so a quiet period is a signed statement. Nobody can later
insert a change into it.

**5. A clean audit** (tab *4 Verify*)

With HQ as the verifier, verify S1.

*Showcases:* HQ recomputes every leaf, rebuilds each Merkle root, chains the heads from genesis, and
compares the result with **its own** ledger copy. Every batch matches.

**6. The insider** (tab *3 Off-ledger audit trail*, then tab *4*)

In S1's audit store, **Alter** a record in batch #2. **Push S1's trail to HQ**, then verify again.

*Showcases:*
- Root #2 mismatches, and the altered leaf is named.
- Heads #3 onward fail as a cascade, because each head includes the one before it.
- The tree view marks the bad leaf and the path up to the root.

**7. Covering tracks** (tabs *3* and *4*)

The demo has no "undo" for tampering, so first click **Reset demo** (twice) to start from a clean
trail. Then tick *also recompute the stored leaf hash*, alter a record, push, and verify.

*Showcases:* the stored hash now agrees with the altered record, but the record is still caught,
because its inclusion proof no longer leads to the root co-signed on the ledger.

**8. A deleted record** (tabs *3* and *4*)

Reset again, then **Delete** a record (for example in S2's batch #1), push, and verify.

*Showcases:* the event count and the root mismatch. The tree draws a dashed box in the deleted record's
original slot.

**9. Rewriting the ledger** (tab *2*)

Use **Try to rewrite history**, as S1, then as HQ, then as S2.

*Showcases:* every attempt is rejected. Archiving an anchor needs every signatory, so neither S1 nor HQ
can do it alone. S2 is not a stakeholder of S1's chain and cannot even see its anchors.

**10. The auditor joins** (top bar, then tabs *3* and *4*)

Reset first, so the earlier tampering is gone. Click **Add auditor**, then in tab *1* capture changes
and seal a batch at S1. In tab *3*, push S1's trail and verify it as the auditor: it passes. Then
switch tab *3* to *HQ's received copy*, alter a record there, and verify as the auditor again.

*Showcases:* new anchors carry 3 signatures, and the auditor keeps its own copy of every chain. The
auditor catches an edit made to **HQ's** copy of a trail, so HQ can't quietly change what it received
either.

**11. Reset**

Click **Reset demo** twice before the next audience. All state lives in memory, so stopping the server
also resets it.

## Configuration and commands

**Environment variables** (read by `demo-director`):

| Variable | Default | What it does |
|---|---|---|
| `PORT` | `3000` | The port the server listens on. |
| `STHIRA_UI_DIST` | `demo/ui/dist` | Where the built UI is served from. |

**Make targets:**

| Command | What it does |
|---|---|
| `make install` | Installs the UI dependencies (`npm ci`). |
| `make start` | Builds everything, then serves on `http://localhost:${PORT:-3000}`. |
| `make dev` | Runs the API on :3000 plus Vite with hot reload on http://localhost:5173 (open :5173). Use this for UI work. |
| `make build` | Regenerates the API types, then builds the UI and the release server. |
| `make api` | Regenerates the UI's TypeScript API types from the Rust OpenAPI spec. |
| `make stop` | Stops a running demo server, which clears its in-memory state. |

**HTTP API** (under `/api`; the UI uses nothing else): `state`, `pending/{id}`, `capture/{id}`,
`seal/{id}`, `batch/{id}/{seq}`, `ledger/{party}`, `ledger/rewrite`, `trail/{where}/{chain}`,
`push/{id}`, `tamper`, `verify`, `auditor`, `auto`, `reset`, and `stream` (server-sent events that
announce every state change). `demo-director --openapi` prints the full spec.

## Share it (free hosting on Render)

The `Dockerfile` builds the UI and the server into one image of about 120 MB. `render.yaml` is a
Render Blueprint for a free web service, pinned to `branch: demo`.

1. In Render, choose **New → Blueprint** and pick the repository. Render reads `render.yaml` and creates
   the `sthiranet-audit-system-demo` service on the free plan.
2. Once the first deploy finishes, share the `https://sthiranet-audit-system-demo-….onrender.com` link.

To try the image locally, run
`docker build -t sthiranet-demo . && docker run --rm -p 3000:3000 sthiranet-demo`. For a temporary
live session from your machine, run `cloudflared tunnel --url http://localhost:3000`.

## What is simplified

- **No real Canton.** The ledger is an in-memory simulation of the behaviour the story relies on. The
  prototype branch anchors on a real Canton sandbox.
- **No real Debezium or databases.** Changes are generated in memory, in Debezium's event shape.
- **The hashing is real.** It follows hashing spec v1 exactly: RFC 8785 canonical JSON, 32-byte salts,
  RFC 6962 Merkle trees, and chained heads.
- **No persistence.** State lives in memory and resets when the server restarts.

## Tests

```bash
make test         # Rust tests (the narrated story, HTTP smoke tests, spec v1 vectors, Merkle properties) + UI type-check
make check        # rustfmt, clippy -D warnings, UI type-check
```

The generated TypeScript API types (`demo/ui/src/api/schema.d.ts`) are committed, because the UI and
the Docker image are built from them. `make clean` stops the server and removes every build output:
`target/`, `demo/ui/dist`, Vite's cache and the generated `openapi.json`. `make distclean` also removes
`demo/ui/node_modules`; run `make install` again after it.
