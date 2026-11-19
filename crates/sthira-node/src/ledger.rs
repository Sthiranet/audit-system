//! Opening the chain's ledger: the one place that knows the ledger is Canton.

use sthira_batcher::persist::{ChainInfo, DataDir};
use sthira_core::{Anchor, AuditLedger, ChainSpec};
use sthira_ledger::{CantonConfig, CantonLedger};

use crate::Res;

/// Connects to the chain's participant and allocates (or finds) every member.
pub async fn connect(info: &ChainInfo) -> Res<CantonLedger> {
    let cfg = CantonConfig { url: info.ledger_url.clone(), user_id: info.ledger_user.clone() };
    let mut ledger =
        CantonLedger::connect(cfg).await.map_err(|e| format!("{}. Is the sandbox running? (make sandbox)", e.0))?;
    ledger.ensure_parties(&info.members).await.map_err(|e| e.0)?;
    Ok(ledger)
}

/// Connects and makes sure the ledger holds the history this data directory recorded. A chain
/// that does not exist yet is created when nothing was anchored before. Returns the ledger and the
/// last anchored batch number.
pub async fn open_for_node(dir: &DataDir, info: &ChainInfo) -> Res<(CantonLedger, u64)> {
    let ledger = connect(info).await?;
    let receipts = dir.load_receipts().map_err(|e| e.to_string())?;
    let view = ledger.chain_view(&info.submitter, &info.chain_id).await.map_err(|e| e.0)?;
    let last = match view {
        None if receipts.is_empty() => {
            let spec = ChainSpec {
                chain_id: info.chain_id.clone(),
                submitter: info.submitter.clone(),
                members: info.members.clone(),
                genesis_head: info.genesis_head.clone(),
            };
            ledger
                .create_chain(&spec)
                .await
                .map_err(|e| format!("creating chain {} on Canton: {}", info.chain_id, e.0))?;
            0
        }
        None => {
            return Err(format!(
                "Canton at {} has no chain {}. The sandbox keeps its state in memory, so a restarted sandbox has \
                 forgotten it; start a new chain with another --data directory",
                info.ledger_url, info.chain_id
            ));
        }
        Some(c) => {
            if receipts.is_empty() && !c.anchors.is_empty() {
                return Err(format!(
                    "chain {} already exists on this ledger with {} anchors from another data directory; pick \
                     another chain id with `run --chain …` in a new --data directory",
                    info.chain_id,
                    c.anchors.len()
                ));
            }
            if c.genesis_head != info.genesis_head {
                return Err(format!("Canton's chain {} has a different genesis head", info.chain_id));
            }
            for a in &receipts {
                if !c.anchors.iter().any(|x| x.seq == a.seq && x.head == a.head) {
                    return Err(format!(
                        "Canton's chain {} does not match this data directory at batch #{} (was the sandbox \
                         restarted and the chain created again?)",
                        info.chain_id, a.seq
                    ));
                }
            }
            c.last_seq
        }
    };
    Ok((ledger, last))
}

/// The anchors as `party`'s own ledger copy holds them. This is what a verifier compares against:
/// never the node's own files.
pub async fn anchors_as(info: &ChainInfo, party: &str) -> Res<Vec<Anchor>> {
    let ledger = connect(info).await?;
    let view = ledger.chain_view(party, &info.chain_id).await.map_err(|e| e.0)?;
    view.map(|c| c.anchors).ok_or_else(|| format!("{party} holds no copy of chain {} on Canton", info.chain_id))
}
