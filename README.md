# Sthiranet · Audit Trail Demo

A small web app for live presentations. It shows how a corporate group's audit trail is captured,
batched, hashed into Merkle roots and co-signed on a ledger. It also shows how recomputing the hashes
catches a trail that was altered, and names the exact record that changed.

The cast follows the story. **HQ** sits in Region R, subsidiaries **S1, S2 and S3** sit in Regions
R1–R3, and an external **auditor** can optionally join.

> **Branches.** This is the `demo` branch. It holds only the web demo, which is self-contained with a
> simulated ERP and an in-memory ledger. The **prototype** is on the `prototype` branch, and `main`
> follows it. The prototype is `sthira-node`, a CLI that anchors a real application's audit log on a
> Canton sandbox: `git switch prototype`.

## Contents

1. [Run it](#1-run-it)
2. [What you see on screen](#2-what-you-see-on-screen)
3. [Presenting it: a 10-step story](#3-presenting-it)
4. [Configuration and commands](#4-configuration-and-commands)
5. [Share it (free hosting on Render)](#5-share-it-free-hosting-on-render)
6. [What is simplified](#6-what-is-simplified)
7. [Development](#7-development)

---

## 1. Run it

You need Rust (stable, 1.94 or newer) and Node.js 22.12 or newer. Node is only used to build the UI.

```bash
make install         # once: installs the UI dependencies
make start           # builds the UI and the server, then serves http://localhost:3000
```

Open http://localhost:3000. The demo opens on a running network: each subsidiary already has 3
anchored batches, and HQ already holds a copy of each trail.

To use a different port, run `PORT=4000 make start`. To stop the server, run `make stop`. All demo state
lives in memory, so stopping the server also clears it.

---

## 2. What you see on screen

**The top bar**

| Control | What it does |
|---|---|
| **Auto-anchor every 10 s** | When on, each subsidiary captures a few changes and seals a batch every 10 seconds, without any clicks. Turn it off to drive every step by hand. |
| **Add auditor** | An external auditor joins the network. From then on, new anchors carry three signatures, and the auditor keeps its own ledger copy. |
| **Reset demo** | Click twice to confirm. Restores the starting point: 3 anchored batches per subsidiary. |
| Network panel | Shows who co-signs, who holds ledger copies, and whose trails each party holds. |

**The four tabs**

| Tab | What it shows |
|---|---|
| **1 Capture & batch** | A subsidiary captures ERP changes (Debezium events). Each change is normalised, salted and hashed into a leaf. Leaves are paired up into a Merkle root, and the root is anchored and chained to the previous batch. Click a row to see the raw event. An empty batch is still anchored, so silence is visible. |
| **2 Ledger** | The ledger as stored on the node of a chosen party: only fingerprints (root, head, previous head), signatures, and "delivered to". This tab also shows *what the synchronizer sees*, and has **Try to rewrite history**, which the ledger rejects. |
| **3 Off-ledger audit trail** | The records themselves, held by a subsidiary or by HQ. **Simulate tampering** lets you **Alter** or **Delete** a record, optionally also rewriting its stored leaf hash. |
| **4 Verify** | The verifier (HQ, or the auditor) recomputes every leaf, rebuilds each Merkle root, chains the heads from genesis, and compares the result with *its own* ledger copy. The verifier pinpoints the altered or deleted record, and explains the cascade into later batches. |

An activity panel on the side logs every action. You can show or hide it.

---

## 3. Presenting it

These steps follow the narrated story that `make test` checks automatically
(`demo/demo-director/tests/scenario.rs`). Turn **auto-anchor off** for full control, or leave it on
for a "live" feel.

1. **Normal operation.** In *Capture & batch*, capture changes at S1 and seal a batch. The batch is
   anchored, co-signed by S1 and HQ, and chained to batch #3.
2. **Who sees what.** In *Ledger*, switch the node: HQ holds copies of all three chains, S1 sees only its
   own, and S2 cannot see S1's.
3. **Silence is visible.** Seal a batch with no changes. It is still anchored.
4. **A clean audit.** In *Verify*, HQ verifies S1's pushed trail, and every batch matches.
5. **The insider.** In *Off-ledger audit trail*, alter a record in batch #2 at S1, then push the trail to
   HQ and verify. Root #2 mismatches, the leaf is pinpointed, and heads #3 onward fail as a cascade.
6. **Covering tracks.** Alter a record and also recompute its leaf hash. The stored hash now agrees with
   the record, but the record is still caught against the anchored root.
7. **A deleted record.** Delete one. The event count and the root mismatch, and the missing position is
   reported.
8. **Rewriting the ledger.** In *Ledger*, use **Try to rewrite history**. The ledger rejects it, because
   nobody can rewrite an anchor alone. S2 cannot even see S1's anchors.
9. **The auditor joins.** Click **Add auditor**. New anchors now carry 3 signatures, and the auditor
   catches an edit made to HQ's own copy of a trail.
10. **Reset.** Click **Reset demo** twice before the next audience.

---

## 4. Configuration and commands

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
| `make test` | Rust tests (the 10-step story, HTTP smoke tests, RFC 6962/8785 and spec v1 vectors, Merkle property tests), plus the UI type-check. |
| `make check` | Formatting, clippy with `-D warnings`, and the UI type-check. |
| `make stop` | Stops a running demo server, which clears its in-memory state. |
| `make clean` | Runs `make stop`, then deletes every build output: `target/`, `demo/ui/dist`, Vite's cache and the generated `openapi.json`. |
| `make distclean` | Runs `make clean`, and also deletes `demo/ui/node_modules`. Run `make install` again afterwards. |

The generated TypeScript API types (`demo/ui/src/api/schema.d.ts`) are committed, because the UI is built
from them. `make build` regenerates them anyway.

**HTTP API** (served under `/api`, and the UI uses nothing else): `state`, `pending/{id}`,
`capture/{id}`, `seal/{id}`, `batch/{id}/{seq}`, `ledger/{party}`, `ledger/rewrite`,
`trail/{where}/{chain}`, `push/{id}`, `tamper`, `verify`, `auditor`, `auto`, `reset`, and `stream`
(server-sent events that push every state change to the UI).

---

## 5. Share it (free hosting on Render)

The `demo` branch includes a `Dockerfile` (UI and server in one image of about 120 MB) and a
`render.yaml` Blueprint for a free Render web service. The Blueprint is pinned to `branch: demo`.

1. In Render, choose **New → Blueprint** and pick the repository. Render reads `render.yaml` and creates
   the `sthiranet-audit-system-demo` service on the free plan.
2. Once the first deploy finishes, share the `https://sthiranet-audit-system-demo-….onrender.com` link.

To try the image locally:

```bash
docker build -t sthiranet-demo . && docker run --rm -p 3000:3000 sthiranet-demo
```

For a temporary live session, run `cloudflared tunnel --url http://localhost:3000`.

---

## 6. What is simplified

This is a presentation demo, not the production backend.

- **No real Canton.** The ledger is an in-memory simulation that reproduces the behaviour the story relies
  on: each party keeps its own copy, every member co-signs, a fork or a skipped sequence is refused, and
  archiving an anchor needs every signatory. The prototype branch anchors on a real Canton sandbox.
- **No real Debezium or databases.** Changes are generated in memory, in Debezium's event shape.
- **The hashing is real.** It follows hashing spec v1 exactly: RFC 8785 canonical JSON, 32-byte salts,
  RFC 6962 Merkle trees, and heads over `seq`, log positions, time window and event count.
- **No persistence.** State lives in memory and resets when the server restarts.

---

## 7. Development

| Path | What it holds |
|---|---|
| `crates/sthira-core` | the hashing kernel: events, canonical JSON, Merkle trees and proofs |
| `crates/sthira-ledger` | the `AuditLedger` trait and the in-memory `SimLedger` |
| `crates/sthira-profiles` | the simulated ERP, Debezium events, normalisation, descriptions and tampering |
| `crates/sthira-batcher` | capture, batch sealing and anchoring |
| `crates/sthira-verify` | the verifier |
| `demo/demo-director` | the demo server: the in-memory world, scenarios, auto-anchoring and the HTTP API |
| `demo/ui` | the React + TypeScript UI (Vite) |
| `test-vectors/` | the independent Python reference implementation of spec v1 and its vectors |

The `sthira-*` crates on this branch are the v1 versions that the demo was built on. They now evolve
separately on the `prototype` branch.
