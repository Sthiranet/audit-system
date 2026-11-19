# prototype-sandbox: Sthiranet anchoring on Canton (sandbox)

This directory holds the Daml package that the prototype node (`sthira-node run --ledger canton`)
anchors its batches on, and the script that starts a local **Canton sandbox** with it. The
sandbox is one participant node and one synchronizer in a single process, with in-memory storage.
It is not the TestNet and not LocalNet.

| | |
|---|---|
| SDK | 3.6.4 (Canton 3.x), installed with `dpm install 3.6.4`, pinned in `daml.yaml` |
| Package | `sthiranet-audit` 0.1.0, module `Sthiranet.Audit` |
| Ledger access | JSON Ledger API v2 on `http://localhost:7575` (gRPC Ledger API on 6865) |
| Auth | none (sandbox default: every valid request is accepted) |

## Files

```
daml.yaml                         package definition (sdk-version, name, deps)
daml/Sthiranet/Audit.daml         templates: ChainProposal, AuditChain, AuditAnchor
daml/Sthiranet/Audit/Test.daml    Daml Script tests (dpm test)
sandbox.sh                        dpm build + dpm sandbox --json-api-port 7575 --dar …
```

## The model

```
ChainProposal ──Sign (each member)──► ChainProposal ──Finalize──► AuditChain
AuditChain    ──Append (submitter only)──► AuditChain (next) + AuditAnchor
```

- **ChainProposal**: the company proposes a chain with a member list. Each member adds its signature
  (`Sign`), and then any member turns it into the chain (`Finalize`). Signatories are the members who
  have signed so far.
- **AuditChain**: the head of the chain (`lastSeq`, `lastHead`), signed by every member. `Append` is
  controlled by the submitter only. It checks `seq == lastSeq + 1` and `prevHead == lastHead`, so the chain
  can't fork or skip a batch. It also checks `fromTime <= toTime` and `eventCount >= 0`. Append is
  *consuming*, so the old head can never be appended to again.
- **AuditAnchor**: one per batch, with seq, prev head, head, Merkle root, event count, log positions and
  time window. It is signed by every member, so archiving (rewriting) one needs all of them.

Hashes, times and positions are `Text`: they are exactly the strings the node hashed, so a verifier
recomputes from them unchanged. Fixed-format UTC timestamps sort correctly as text.

The model has no contract keys (Canton 3.x / Daml-LF 2) and no record content: only fingerprints.

## Tests

```bash
dpm test      # or: make daml-test (from the repo root)
```

| Test | Checks |
|---|---|
| `test_append_chains` | signed by both members; seq and head advance |
| `test_fork_and_gap_rejected` | wrong `prevHead` and a skipped `seq` are refused |
| `test_no_second_successor` | the consumed chain contract can't be appended to again |
| `test_only_submitter_appends` | the auditor cannot append |
| `test_no_unilateral_archive` | neither party alone can archive an anchor; both together can |
| `test_outsider_sees_nothing` | a third party sees no anchors |
| `test_proposal_rules` | no finalising before everyone signed; non-members can't sign |

## Run

```bash
./sandbox.sh            # or: make sandbox (from the repo root). Ctrl-C stops it.
```

Then, from the repo root in another terminal:

```bash
./target/release/sthira-node --data ./sthira-canton run --ledger canton
```

On first start the node allocates the parties `DREAMTALK` and `AUDITOR` (they become
`DREAMTALK::1220…` and `AUDITOR::1220…`). It then creates the `ChainProposal`, the auditor signs it, the
node finalises the `AuditChain`, and every batch is appended with `Append`. `verify`, `ledger` and
`check-proof` read the anchors from the auditor party's active contracts on the participant, not from
the node's files. `rewrite <seq>` submits an `Archive` on an anchor acting as one party, and Canton
refuses it with `DAML_AUTHORIZATION_ERROR`.

**The sandbox forgets everything when it stops.** A chain created on a sandbox that has since restarted
can't be resumed. The node says so; start a new chain with a new `--data` directory.

## JSON Ledger API calls used (`crates/sthira-ledger/src/canton.rs`)

| Purpose | Call |
|---|---|
| health/version | `GET /v2/version` |
| find or allocate a party | `GET /v2/parties`, `POST /v2/parties {partyIdHint}` |
| create / exercise | `POST /v2/commands/submit-and-wait-for-transaction` with `{commands: {commands: [CreateCommand or ExerciseCommand], commandId, userId, actAs}}` |
| read a party's contracts | `GET /v2/state/ledger-end`, then `POST /v2/state/active-contracts {activeAtOffset, eventFormat: {filtersByParty: {party: {cumulative: [TemplateFilter]}}}}` |

Template ids use the package-name form (`#sthiranet-audit:Sthiranet.Audit:AuditChain`), so the client
doesn't need the package hash. Daml `Int` values travel as JSON strings. The participant's own OpenAPI
description is served at `http://localhost:7575/docs/openapi`.

## Limits of the sandbox setup

- **One participant hosts every party.** The node can act as the auditor to sign the proposal. On
  LocalNet or a real network, the auditor signs from its own participant, and its copy of the anchors lives
  on its own node. The Daml model doesn't change.
- **No authentication.** A real participant needs JWTs (`canton.participants.<name>.ledger-api.auth-services`).
- **In-memory storage.** Use a persistent participant (PostgreSQL) for anything beyond a demo.
- **Members are fixed** at chain creation. Adding an auditor to an existing chain is not in the V1 model.
