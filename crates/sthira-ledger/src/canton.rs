//! [`CantonLedger`]: [`AuditLedger`] on a Canton participant through the JSON Ledger API v2, with the
//! `sthiranet-audit` Daml package (`prototype-sandbox/`) uploaded.
//!
//! | Trait method      | Ledger API calls                                                        |
//! |-------------------|-------------------------------------------------------------------------|
//! | `ensure_parties`  | `GET /v2/parties` (reuse) or `POST /v2/parties` (allocate, hint = id)   |
//! | `create_chain`    | create `ChainProposal` → each member exercises `Sign` → `Finalize`      |
//! | `append`          | exercise `AuditChain.Append` (consuming: one successor only)            |
//! | `chain_view`      | `POST /v2/state/active-contracts` filtered by the party                 |
//! | `attempt_archive` | exercise `Archive` on an `AuditAnchor` acting as one party only         |
//!
//! Party ids on Canton look like `DREAMTALK::1220ab…`. The trait and everything above it use the
//! short id (`DREAMTALK`); this module maps between the two.
//!
//! In the sandbox every party is hosted on the one participant, so this client can act as each of
//! them. On a multi-node network each member would sign from its own participant.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};
use sthira_core::{Anchor, AnchorInput, ArchiveAttempt, AuditLedger, ChainSpec, ChainView, LedgerError, Party};

/// The Daml package name, as written in `prototype-sandbox/daml.yaml`.
pub const PACKAGE_NAME: &str = "sthiranet-audit";
const MODULE: &str = "Sthiranet.Audit";

type Result<T> = std::result::Result<T, LedgerError>;

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
    http: Client,
    /// short id → full Canton party id
    parties: BTreeMap<String, String>,
    /// chain id → contract id of the current `AuditChain` (a cache; the ledger is the truth)
    chains: Mutex<HashMap<String, String>>,
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

fn texts(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

/// A Ledger API error body (`{code, cause, …}`) as one line.
fn api_error(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(v) if v.get("code").is_some() => format!("{}: {}", text(&v, "code"), text(&v, "cause")),
        _ => body.chars().take(400).collect(),
    }
}

fn anchor_of(ev: &Value, update_id: &str, recorded_at: &str) -> Anchor {
    let a = &ev["createArgument"];
    Anchor {
        contract_id: text(ev, "contractId"),
        chain_id: text(a, "chainId"),
        seq: int(&a["seq"]),
        prev_head: text(a, "prevHead"),
        head: text(a, "head"),
        merkle_root: text(a, "merkleRoot"),
        leaves: texts(&a["leaves"]),
        event_count: int(&a["eventCount"]),
        tx_count: int(&a["txCount"]),
        from_tx: int(&a["fromTx"]),
        to_tx: int(&a["toTx"]),
        from_time: text(a, "fromTime"),
        to_time: text(a, "toTime"),
        signatories: texts(&ev["signatories"]).iter().map(|s| short(s)).collect(),
        update_id: update_id.to_string(),
        recorded_at: recorded_at.to_string(),
    }
}

/// The first created event of `template` in a transaction.
fn created(tx: &Value, template: &str) -> Option<Value> {
    tx.get("events")?.as_array()?.iter().find_map(|e| {
        let c = e.get("CreatedEvent")?;
        let t = c.get("templateId")?.as_str()?;
        t.ends_with(&format!(":{MODULE}:{template}")).then(|| c.clone())
    })
}

impl CantonLedger {
    /// Connects and waits (up to a minute) until the participant has joined its synchronizer.
    pub async fn connect(cfg: CantonConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| LedgerError(format!("HTTP client: {e}")))?;
        let me = Self { cfg, http, parties: BTreeMap::new(), chains: Mutex::new(HashMap::new()) };
        let v = me.get("/v2/version").await?;
        if v.get("version").is_none() {
            return Err(LedgerError(format!("{} does not look like a JSON Ledger API v2", me.cfg.url)));
        }
        // The JSON API answers before the participant has joined its synchronizer; parties can only
        // be allocated (and commands submitted) once it has.
        for _ in 0..60 {
            let s = me.get("/v2/state/connected-synchronizers").await?;
            if s.get("connectedSynchronizers").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
                return Ok(me);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(LedgerError(format!("the participant at {} is not connected to a synchronizer yet", me.cfg.url)))
    }

    /// The participant's Canton version.
    pub async fn version(&self) -> Result<String> {
        Ok(text(&self.get("/v2/version").await?, "version"))
    }

    pub fn config(&self) -> &CantonConfig {
        &self.cfg
    }

    /// The full Canton party id behind a short id.
    pub fn party_id(&self, short_id: &str) -> Option<&str> {
        self.parties.get(short_id).map(String::as_str)
    }

    /// Replaces full party ids (`NAME::1220…`) in a message with their short form.
    fn shorten(&self, msg: &str) -> String {
        self.parties.values().fold(msg.to_string(), |m, full| m.replace(full.as_str(), &short(full)))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.cfg.url.trim_end_matches('/'))
    }

    async fn read(&self, path: &str, res: reqwest::Result<reqwest::Response>) -> Result<Value> {
        let res = res.map_err(|e| LedgerError(format!("Canton unreachable at {}: {e}", self.cfg.url)))?;
        let status = res.status();
        let body = res.text().await.map_err(|e| LedgerError(format!("Canton {path}: {e}")))?;
        if !status.is_success() {
            return Err(LedgerError(api_error(&body)));
        }
        serde_json::from_str(&body).map_err(|e| LedgerError(format!("Canton {path}: {e}")))
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.read(path, self.http.get(self.url(path)).send().await).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.read(path, self.http.post(self.url(path)).json(body).send().await).await
    }

    fn full(&self, short_id: &str) -> Result<String> {
        self.parties
            .get(short_id)
            .cloned()
            .ok_or_else(|| LedgerError(format!("party {short_id} is not allocated on the participant")))
    }

    /// Submits one command acting as `act_as` and returns the resulting transaction.
    async fn submit(&self, act_as: &[String], command: Value) -> Result<Value> {
        let command_id = format!("sthira-{}-{}", std::process::id(), rand::random::<u64>());
        let body = json!({
            "commands": {
                "commands": [command],
                "commandId": command_id,
                "userId": self.cfg.user_id,
                "actAs": act_as,
            }
        });
        let v = self.post("/v2/commands/submit-and-wait-for-transaction", &body).await?;
        v.get("transaction").cloned().ok_or_else(|| LedgerError(format!("unexpected submit answer: {v}")))
    }

    async fn exercise(&self, act_as: &str, template: &str, cid: &str, choice: &str, arg: Value) -> Result<Value> {
        let cmd = json!({ "ExerciseCommand": {
            "templateId": tid(template), "contractId": cid, "choice": choice, "choiceArgument": arg
        } });
        self.submit(&[act_as.to_string()], cmd).await
    }

    /// The active contracts of `templates` that `party` (full id) is a stakeholder of.
    async fn active(&self, party: &str, templates: &[&str]) -> Result<Vec<Value>> {
        let end = self.get("/v2/state/ledger-end").await?;
        let offset = end.get("offset").and_then(Value::as_i64).unwrap_or(0);
        let filters: Vec<Value> = templates
            .iter()
            .map(|t| json!({ "identifierFilter": { "TemplateFilter": { "value": { "templateId": tid(t), "includeCreatedEventBlob": false } } } }))
            .collect();
        let body = json!({
            "activeAtOffset": offset,
            "eventFormat": { "filtersByParty": { party: { "cumulative": filters } }, "verbose": false }
        });
        let v = self.post("/v2/state/active-contracts", &body).await?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.pointer("/contractEntry/JsActiveContract/createdEvent").cloned())
            .collect())
    }

    /// Every party id known to the participant.
    async fn list_parties(&self) -> Result<Vec<String>> {
        let listed = self.get("/v2/parties").await?;
        Ok(listed
            .get("partyDetails")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| d.get("party").and_then(Value::as_str).map(str::to_string))
            .collect())
    }

    /// Allocates a party with this hint. If another client allocated it first (two nodes starting
    /// at once), the existing party is used instead.
    async fn allocate(&self, id: &str) -> Result<String> {
        match self.post("/v2/parties", &json!({ "partyIdHint": id, "identityProviderId": "" })).await {
            Ok(v) => v
                .pointer("/partyDetails/party")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| LedgerError(format!("allocating party {id}: unexpected answer {v}"))),
            // The other allocation may still be in flight; give it a few seconds to appear.
            Err(e) => {
                for _ in 0..20 {
                    if let Some(p) = self.list_parties().await?.into_iter().find(|p| short(p) == id) {
                        return Ok(p);
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(LedgerError(format!("allocating party {id}: {}", e.0)))
            }
        }
    }

    /// The contract id of the chain's current `AuditChain`, as `party` sees it.
    async fn chain_cid(&self, party: &str, chain_id: &str) -> Result<Option<String>> {
        if let Some(c) = self.chains.lock().expect("chain cache").get(chain_id).cloned() {
            return Ok(Some(c));
        }
        let found = self
            .active(party, &["AuditChain"])
            .await?
            .into_iter()
            .find(|ev| text(&ev["createArgument"], "chainId") == chain_id)
            .map(|ev| text(&ev, "contractId"));
        if let Some(c) = &found {
            self.chains.lock().expect("chain cache").insert(chain_id.to_string(), c.clone());
        }
        Ok(found)
    }
}

impl AuditLedger for CantonLedger {
    async fn ensure_parties(&mut self, ids: &[Party]) -> Result<()> {
        let missing: Vec<&Party> = ids.iter().filter(|id| !self.parties.contains_key(*id)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        let known = self.list_parties().await?;
        for id in missing {
            let full = match known.iter().find(|p| short(p) == *id) {
                Some(p) => p.clone(),
                None => self.allocate(id).await?,
            };
            self.parties.insert(id.clone(), full);
        }
        Ok(())
    }

    async fn create_chain(&self, spec: &ChainSpec) -> Result<()> {
        let sub = self.full(&spec.submitter)?;
        let all: Vec<String> = spec.members.iter().map(|m| self.full(m)).collect::<Result<_>>()?;
        let tx = self
            .submit(
                std::slice::from_ref(&sub),
                json!({ "CreateCommand": { "templateId": tid("ChainProposal"), "createArguments": {
                    "chainId": spec.chain_id, "submitter": sub, "members": all, "signed": [sub],
                    "genesisHead": spec.genesis_head
                } } }),
            )
            .await?;
        let mut proposal = created(&tx, "ChainProposal")
            .map(|c| text(&c, "contractId"))
            .ok_or_else(|| LedgerError("ChainProposal was not created".into()))?;
        for m in all.iter().filter(|m| **m != sub) {
            let tx = self.exercise(m, "ChainProposal", &proposal, "Sign", json!({ "signer": m })).await?;
            proposal = created(&tx, "ChainProposal")
                .map(|c| text(&c, "contractId"))
                .ok_or_else(|| LedgerError(format!("{} did not sign", short(m))))?;
        }
        let tx = self.exercise(&sub, "ChainProposal", &proposal, "Finalize", json!({ "actor": sub })).await?;
        let chain = created(&tx, "AuditChain").ok_or_else(|| LedgerError("AuditChain was not created".into()))?;
        self.chains.lock().expect("chain cache").insert(spec.chain_id.clone(), text(&chain, "contractId"));
        Ok(())
    }

    async fn append(&self, chain_id: &str, submitter: &str, a: &AnchorInput) -> Result<Anchor> {
        let sub = self.full(submitter)?;
        let cid = self
            .chain_cid(&sub, chain_id)
            .await?
            .ok_or_else(|| LedgerError(format!("no AuditChain {chain_id} on the ledger")))?;
        let arg = json!({
            "seq": a.seq.to_string(), "prevHead": a.prev_head, "head": a.head, "merkleRoot": a.merkle_root,
            "leaves": a.leaves, "eventCount": a.event_count.to_string(), "txCount": a.tx_count.to_string(),
            "fromTx": a.from_tx.to_string(), "toTx": a.to_tx.to_string(),
            "fromTime": a.from_time, "toTime": a.to_time,
        });
        let res = self.exercise(&sub, "AuditChain", &cid, "Append", arg).await;
        let tx = match res {
            Ok(tx) => tx,
            Err(e) => {
                // The cached chain contract may be stale (another writer); forget it.
                self.chains.lock().expect("chain cache").remove(chain_id);
                return Err(LedgerError(format!("Canton rejected the append: {}", self.shorten(&e.0))));
            }
        };
        let anchor_ev =
            created(&tx, "AuditAnchor").ok_or_else(|| LedgerError("no AuditAnchor in the transaction".into()))?;
        let next = created(&tx, "AuditChain").ok_or_else(|| LedgerError("no successor AuditChain".into()))?;
        self.chains.lock().expect("chain cache").insert(chain_id.to_string(), text(&next, "contractId"));
        Ok(anchor_of(&anchor_ev, &text(&tx, "updateId"), &text(&tx, "recordTime")))
    }

    async fn chain_view(&self, party: &str, chain_id: &str) -> Result<Option<ChainView>> {
        let full = self.full(party)?;
        let events = self.active(&full, &["AuditChain", "AuditAnchor"]).await?;
        let mut anchors = Vec::new();
        let mut chain = None;
        for ev in &events {
            if text(&ev["createArgument"], "chainId") != chain_id {
                continue;
            }
            let t = ev["templateId"].as_str().unwrap_or_default();
            if t.ends_with(":AuditAnchor") {
                let off = ev["offset"].as_i64().unwrap_or_default();
                anchors.push(anchor_of(ev, &format!("offset {off}"), &text(ev, "createdAt")));
            } else if t.ends_with(":AuditChain") {
                chain = Some(ev);
            }
        }
        let Some(ev) = chain else { return Ok(None) };
        anchors.sort_by_key(|a| a.seq);
        let a = &ev["createArgument"];
        Ok(Some(ChainView {
            chain_id: chain_id.to_string(),
            submitter: short(&text(a, "submitter")),
            members: texts(&a["members"]).iter().map(|m| short(m)).collect(),
            genesis_head: text(a, "genesisHead"),
            last_seq: int(&a["lastSeq"]),
            last_head: text(a, "lastHead"),
            anchors,
        }))
    }

    async fn attempt_archive(&self, chain_id: &str, actor: &str, seq: u64) -> Result<ArchiveAttempt> {
        let view = self.chain_view(actor, chain_id).await?;
        let Some(a) = view.and_then(|c| c.anchors.into_iter().find(|a| a.seq == seq)) else {
            return Ok(ArchiveAttempt {
                ok: false,
                actor: actor.into(),
                chain_id: chain_id.into(),
                seq,
                contract_id: None,
                required: vec![],
                missing: vec![],
                reason: format!(
                    "{actor} sees no anchor #{seq} of chain {chain_id}: not a stakeholder, so it cannot even reference it"
                ),
            });
        };
        let me = self.full(actor)?;
        let res = self.exercise(&me, "AuditAnchor", &a.contract_id, "Archive", json!({})).await;
        let missing: Vec<Party> = a.signatories.iter().filter(|s| *s != actor).cloned().collect();
        Ok(match res {
            Ok(_) => ArchiveAttempt {
                ok: true,
                actor: actor.into(),
                chain_id: chain_id.into(),
                seq,
                contract_id: Some(a.contract_id),
                required: a.signatories,
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
                missing,
                reason: format!("Canton refused: {}", self.shorten(&e.0)),
            },
        })
    }
}
