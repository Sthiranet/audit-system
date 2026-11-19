//! [`CantonLedger`]: the `AuditLedger` trait on a real Canton participant, through the JSON Ledger
//! API v2, with the `sthiranet-audit` Daml package (`prototype-sandbox/`) uploaded.
//!
//! | Trait method      | Ledger API calls                                                        |
//! |-------------------|-------------------------------------------------------------------------|
//! | `add_party`       | `GET /v2/parties` (reuse) or `POST /v2/parties` (allocate, hint = id)   |
//! | `create_chain`    | create `ChainProposal` → each member exercises `Sign` → `Finalize`      |
//! | `append`          | exercise `AuditChain.Append` (consuming: one successor only)            |
//! | `attempt_archive` | exercise `Archive` on an `AuditAnchor` acting as one party only         |
//! | `copy` / `view`   | `POST /v2/state/active-contracts` per party, cached until [`CantonLedger::refresh`] |
//!
//! Party ids on Canton look like `DREAMTALK::1220ab…`. The trait and everything above it use the
//! short id (`DREAMTALK`); this module maps between the two.
//!
//! In the sandbox every party is hosted on the one participant, so this client can act as each of
//! them. On a multi-node network each member would sign from its own participant.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Anchor, AnchorInput, ArchiveAttempt, AuditLedger, ChainCopy, Envelope, LedgerError, Party, Result};

/// The Daml package name, as written in `prototype-sandbox/daml.yaml`.
pub const PACKAGE_NAME: &str = "sthiranet-audit";
const MODULE: &str = "Sthiranet.Audit";

/// Where and how to reach the participant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CantonConfig {
    /// JSON Ledger API base URL, e.g. `http://localhost:7575`.
    pub url: String,
    /// Ledger API user id sent with every command.
    pub user_id: String,
}

pub struct CantonLedger {
    cfg: CantonConfig,
    agent: ureq::Agent,
    /// short id → full Canton party id
    parties: BTreeMap<String, String>,
    /// (short party, chain id) → that party's view
    copies: HashMap<(String, String), ChainCopy>,
    /// chain id → contract id of the current `AuditChain`
    chains: HashMap<String, String>,
    commands: u64,
}

fn short(p: &str) -> String {
    p.split("::").next().unwrap_or(p).to_string()
}

fn tid(template: &str) -> String {
    format!("#{PACKAGE_NAME}:{MODULE}:{template}")
}

/// Daml `Int` arrives as a JSON string (`"12"`); accept a number too.
fn int(v: &Value) -> u64 {
    match v {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// A Ledger API error body (`{code, cause, …}`) as one line.
fn api_error(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) if v.get("code").is_some() => format!("{}: {}", text(&v, "code"), text(&v, "cause")),
        _ => body.chars().take(400).collect(),
    }
}

impl CantonLedger {
    /// Connects and checks the participant answers. Parties and chains are loaded by
    /// [`add_party`](AuditLedger::add_party) and [`refresh`](Self::refresh).
    pub fn connect(cfg: CantonConfig) -> Result<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        let me =
            Self { cfg, agent, parties: BTreeMap::new(), copies: HashMap::new(), chains: HashMap::new(), commands: 0 };
        let v = me.get("/v2/version")?;
        if v.get("version").is_none() {
            return Err(LedgerError(format!("{} does not look like a JSON Ledger API v2", me.cfg.url)));
        }
        // The JSON API answers before the participant has joined its synchronizer; parties can only
        // be allocated (and commands submitted) once it has.
        for _ in 0..60 {
            let s = me.get("/v2/state/connected-synchronizers")?;
            if s.get("connectedSynchronizers").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
                return Ok(me);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Err(LedgerError(format!("the participant at {} is not connected to a synchronizer yet", me.cfg.url)))
    }

    /// The participant's Canton version.
    pub fn version(&self) -> Result<String> {
        Ok(text(&self.get("/v2/version")?, "version"))
    }

    pub fn config(&self) -> &CantonConfig {
        &self.cfg
    }

    /// Replaces full party ids (`NAME::1220…`) in a message with their short form.
    fn shorten(&self, msg: &str) -> String {
        self.parties.values().fold(msg.to_string(), |m, full| m.replace(full.as_str(), &short(full)))
    }

    /// The full Canton party id behind a short id.
    pub fn party_id(&self, short_id: &str) -> Option<&str> {
        self.parties.get(short_id).map(String::as_str)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.cfg.url.trim_end_matches('/'))
    }

    fn get(&self, path: &str) -> Result<Value> {
        let mut res = self
            .agent
            .get(&self.url(path))
            .call()
            .map_err(|e| LedgerError(format!("Canton unreachable at {}: {e}", self.cfg.url)))?;
        let status = res.status().as_u16();
        let body = res.body_mut().read_to_string().map_err(|e| LedgerError(e.to_string()))?;
        if status >= 400 {
            return Err(LedgerError(format!("Canton {path}: {}", api_error(&body))));
        }
        serde_json::from_str(&body).map_err(|e| LedgerError(format!("Canton {path}: {e}")))
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let mut res = self
            .agent
            .post(&self.url(path))
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .map_err(|e| LedgerError(format!("Canton unreachable at {}: {e}", self.cfg.url)))?;
        let status = res.status().as_u16();
        let text = res.body_mut().read_to_string().map_err(|e| LedgerError(e.to_string()))?;
        if status >= 400 {
            return Err(LedgerError(api_error(&text)));
        }
        serde_json::from_str(&text).map_err(|e| LedgerError(format!("Canton {path}: {e}")))
    }

    fn full(&self, short_id: &str) -> Result<String> {
        self.parties
            .get(short_id)
            .cloned()
            .ok_or_else(|| LedgerError(format!("party {short_id} is not allocated on the participant")))
    }

    /// Finds an existing party with this hint, or allocates one.
    fn ensure_party(&mut self, id: &str) -> Result<String> {
        if let Some(p) = self.parties.get(id) {
            return Ok(p.clone());
        }
        let listed = self.get("/v2/parties")?;
        let existing = listed
            .get("partyDetails")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| d.get("party").and_then(Value::as_str))
            .find(|p| short(p) == id)
            .map(str::to_string);
        let full = match existing {
            Some(p) => p,
            None => {
                let v = self.post("/v2/parties", &json!({ "partyIdHint": id, "identityProviderId": "" }))?;
                v.pointer("/partyDetails/party")
                    .and_then(Value::as_str)
                    .ok_or_else(|| LedgerError(format!("allocating party {id}: unexpected answer {v}")))?
                    .to_string()
            }
        };
        self.parties.insert(id.to_string(), full.clone());
        Ok(full)
    }

    /// Submits one command acting as `act_as` and returns the resulting transaction.
    fn submit(&mut self, act_as: &[String], command: Value) -> Result<Value> {
        self.commands += 1;
        let command_id = format!("sthira-{}-{}", std::process::id(), rand::random::<u64>());
        let body = json!({
            "commands": {
                "commands": [command],
                "commandId": command_id,
                "userId": self.cfg.user_id,
                "actAs": act_as,
            }
        });
        let v = self.post("/v2/commands/submit-and-wait-for-transaction", &body)?;
        v.get("transaction").cloned().ok_or_else(|| LedgerError(format!("unexpected submit answer: {v}")))
    }

    fn exercise(&mut self, act_as: &[String], template: &str, cid: &str, choice: &str, arg: Value) -> Result<Value> {
        self.submit(
            act_as,
            json!({ "ExerciseCommand": { "templateId": tid(template), "contractId": cid, "choice": choice, "choiceArgument": arg } }),
        )
    }

    /// The contract id of the first created event of `template` in a transaction.
    fn created(tx: &Value, template: &str) -> Option<Value> {
        tx.get("events")?.as_array()?.iter().find_map(|e| {
            let c = e.get("CreatedEvent")?;
            let t = c.get("templateId")?.as_str()?;
            t.ends_with(&format!(":{MODULE}:{template}")).then(|| c.clone())
        })
    }

    fn anchor_of(&self, ev: &Value, update_id: &str, recorded_at: &str) -> Anchor {
        let a = &ev["createArgument"];
        Anchor {
            contract_id: text(ev, "contractId"),
            template: format!("{MODULE}:AuditAnchor"),
            chain_id: text(a, "chainId"),
            seq: int(&a["seq"]),
            prev_head: text(a, "prevHead"),
            head: text(a, "head"),
            merkle_root: text(a, "merkleRoot"),
            event_count: int(&a["eventCount"]),
            from_pos: text(a, "fromPos"),
            to_pos: text(a, "toPos"),
            from_time: text(a, "fromTime"),
            to_time: text(a, "toTime"),
            signatories: ev["signatories"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(short)
                .collect(),
            update_id: update_id.to_string(),
            recorded_at: recorded_at.to_string(),
        }
    }

    /// The active contracts of `templates` that `party` (full id) is a stakeholder of.
    fn active(&self, party: &str, templates: &[&str]) -> Result<Vec<Value>> {
        let end = self.get("/v2/state/ledger-end")?;
        let offset = end.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let filters: Vec<Value> = templates
            .iter()
            .map(|t| json!({ "identifierFilter": { "TemplateFilter": { "value": { "templateId": tid(t), "includeCreatedEventBlob": false } } } }))
            .collect();
        let body = json!({
            "activeAtOffset": offset,
            "eventFormat": { "filtersByParty": { party: { "cumulative": filters } }, "verbose": false }
        });
        let v = self.post("/v2/state/active-contracts", &body)?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.pointer("/contractEntry/JsActiveContract/createdEvent").cloned())
            .collect())
    }

    /// Reloads every known party's view of every chain from the participant.
    pub fn refresh(&mut self) -> Result<()> {
        self.copies.clear();
        let parties: Vec<(String, String)> = self.parties.iter().map(|(s, f)| (s.clone(), f.clone())).collect();
        for (short_id, full) in parties {
            let events = self.active(&full, &["AuditChain", "AuditAnchor"])?;
            let mut anchors: BTreeMap<String, Vec<Anchor>> = BTreeMap::new();
            let mut chains: Vec<&Value> = Vec::new();
            for ev in &events {
                let t = ev["templateId"].as_str().unwrap_or_default();
                if t.ends_with(":AuditAnchor") {
                    let off = ev["offset"].as_i64().unwrap_or_default();
                    let a = self.anchor_of(ev, &format!("offset {off}"), &text(ev, "createdAt"));
                    anchors.entry(a.chain_id.clone()).or_default().push(a);
                } else if t.ends_with(":AuditChain") {
                    chains.push(ev);
                }
            }
            for ev in chains {
                let a = &ev["createArgument"];
                let chain_id = text(a, "chainId");
                let mut list = anchors.remove(&chain_id).unwrap_or_default();
                list.sort_by_key(|x| x.seq);
                self.chains.insert(chain_id.clone(), text(ev, "contractId"));
                let copy = ChainCopy {
                    chain_id: chain_id.clone(),
                    submitter: short(&text(a, "submitter")),
                    members: a["members"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(short)
                        .collect(),
                    genesis_head: text(a, "genesisHead"),
                    last_seq: int(&a["lastSeq"]),
                    last_head: text(a, "lastHead"),
                    anchors: list,
                    created_update: String::new(),
                };
                self.copies.insert((short_id.clone(), chain_id), copy);
            }
        }
        Ok(())
    }
}

impl AuditLedger for CantonLedger {
    fn add_party(&mut self, id: &str) {
        // The trait has no error channel here; a failure surfaces at the next call that needs the party.
        let _ = self.ensure_party(id);
    }

    fn create_chain(&mut self, chain_id: &str, submitter: &str, members: &[Party], genesis_head: &str) -> Result<()> {
        let sub = self.ensure_party(submitter)?;
        let all: Vec<String> = members.iter().map(|m| self.ensure_party(m)).collect::<Result<_>>()?;
        let tx = self.submit(
            std::slice::from_ref(&sub),
            json!({ "CreateCommand": { "templateId": tid("ChainProposal"), "createArguments": {
                "chainId": chain_id, "submitter": sub, "members": all, "signed": [sub], "genesisHead": genesis_head
            } } }),
        )?;
        let mut proposal = Self::created(&tx, "ChainProposal")
            .map(|c| text(&c, "contractId"))
            .ok_or_else(|| LedgerError("ChainProposal was not created".into()))?;
        for m in all.iter().filter(|m| **m != sub) {
            let tx =
                self.exercise(std::slice::from_ref(m), "ChainProposal", &proposal, "Sign", json!({ "signer": m }))?;
            proposal = Self::created(&tx, "ChainProposal")
                .map(|c| text(&c, "contractId"))
                .ok_or_else(|| LedgerError(format!("{} did not sign", short(m))))?;
        }
        let tx =
            self.exercise(std::slice::from_ref(&sub), "ChainProposal", &proposal, "Finalize", json!({ "actor": sub }))?;
        Self::created(&tx, "AuditChain").ok_or_else(|| LedgerError("AuditChain was not created".into()))?;
        self.refresh()
    }

    fn append(&mut self, chain_id: &str, submitter: &str, a: AnchorInput) -> Result<Anchor> {
        if !self.chains.contains_key(chain_id) {
            self.refresh()?;
        }
        let cid = self
            .chains
            .get(chain_id)
            .cloned()
            .ok_or_else(|| LedgerError(format!("no AuditChain {chain_id} on the ledger")))?;
        let sub = self.full(submitter)?;
        let arg = json!({
            "seq": a.seq.to_string(), "prevHead": a.prev_head, "head": a.head, "merkleRoot": a.merkle_root,
            "eventCount": a.event_count.to_string(), "fromPos": a.from_pos, "toPos": a.to_pos,
            "fromTime": a.from_time, "toTime": a.to_time,
        });
        let tx = self
            .exercise(&[sub], "AuditChain", &cid, "Append", arg)
            .map_err(|e| LedgerError(format!("Canton rejected the append: {}", self.shorten(&e.0))))?;
        let update_id = text(&tx, "updateId");
        let recorded_at = text(&tx, "recordTime");
        let anchor_ev =
            Self::created(&tx, "AuditAnchor").ok_or_else(|| LedgerError("no AuditAnchor in the transaction".into()))?;
        let next = Self::created(&tx, "AuditChain").ok_or_else(|| LedgerError("no successor AuditChain".into()))?;
        let anchor = self.anchor_of(&anchor_ev, &update_id, &recorded_at);
        self.chains.insert(chain_id.to_string(), text(&next, "contractId"));
        for ((_, c), copy) in self.copies.iter_mut() {
            if c == chain_id {
                copy.anchors.push(anchor.clone());
                copy.last_seq = anchor.seq;
                copy.last_head = anchor.head.clone();
            }
        }
        Ok(anchor)
    }

    fn add_member(&mut self, chain_id: &str, new_member: &str, _submitter: &str) -> Result<()> {
        Err(LedgerError(format!(
            "adding {new_member} to {chain_id} is not part of the V1 Daml model; create the chain with every member"
        )))
    }

    fn attempt_archive(&mut self, chain_id: &str, actor: &str, seq: u64) -> Result<ArchiveAttempt> {
        self.refresh()?;
        let Some(a) = self.copy(actor, chain_id).and_then(|c| c.anchors.iter().find(|a| a.seq == seq)).cloned() else {
            return Ok(ArchiveAttempt {
                ok: false,
                actor: actor.into(),
                chain_id: chain_id.into(),
                seq,
                contract_id: None,
                required: vec![],
                signed: vec![],
                missing: vec![],
                reason: format!(
                    "{actor} sees no anchor #{seq} of chain {chain_id}: not a stakeholder, so it cannot even reference it"
                ),
            });
        };
        let me = self.full(actor)?;
        let res = self.exercise(&[me], "AuditAnchor", &a.contract_id, "Archive", json!({}));
        let missing: Vec<Party> = a.signatories.iter().filter(|s| *s != actor).cloned().collect();
        Ok(match res {
            Ok(_) => ArchiveAttempt {
                ok: true,
                actor: actor.into(),
                chain_id: chain_id.into(),
                seq,
                contract_id: Some(a.contract_id),
                required: a.signatories.clone(),
                signed: a.signatories,
                missing: vec![],
                reason: "archived".into(),
            },
            Err(e) => ArchiveAttempt {
                ok: false,
                actor: actor.into(),
                chain_id: chain_id.into(),
                seq,
                contract_id: Some(a.contract_id),
                required: a.signatories,
                signed: vec![actor.into()],
                missing,
                reason: format!("Canton refused: {}", self.shorten(&e.0)),
            },
        })
    }

    fn copy(&self, party: &str, chain_id: &str) -> Option<&ChainCopy> {
        self.copies.get(&(party.to_string(), chain_id.to_string()))
    }

    fn view(&self, party: &str) -> Vec<ChainCopy> {
        let mut v: Vec<ChainCopy> =
            self.copies.iter().filter(|((p, _), _)| p == party).map(|(_, c)| c.clone()).collect();
        v.sort_by(|a, b| a.chain_id.cmp(&b.chain_id));
        v
    }

    fn envelopes(&self) -> &[Envelope] {
        // The synchronizer's traffic is not exposed through the Ledger API.
        &[]
    }
}
