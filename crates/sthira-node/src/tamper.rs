//! Presenter actions on the node's STORED trail: what an insider with write access to the audit
//! store might do after a batch was anchored. Detecting it is the verifier's job, not this module's.
//! Repeated edits walk away from the original and never restore it; `untamper` puts it back.

use std::collections::HashSet;
use std::process::ExitCode;

use serde_json::{Map, Value};
use sthira_batcher::persist::{DataDir, TamperEntry, append_jsonl, rewrite_jsonl};
use sthira_connector::describe::describe;
use sthira_core::{Event, leaf_hash};

use crate::cli::TamperAction;
use crate::{Paint, Res, need_chain};

/// Fields an insider would most plausibly rewrite, tried in this order.
const PREFERRED: &[&str] = &["amount", "total", "credit_limit", "unit_price", "role", "status", "qty", "method"];

/// A different value of the same type. A decimal amount gains a digit (`1250.00` → `12500.00`).
fn edit_value(v: &Value) -> Value {
    match v {
        Value::String(s) => match s.split_once('.') {
            Some((int, frac)) if !int.is_empty() && int.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) => {
                Value::String(format!("{int}0.{frac}"))
            }
            _ => Value::String(format!("{s} (edited)")),
        },
        Value::Bool(b) => Value::Bool(!b),
        Value::Number(n) => n.as_i64().map(|i| Value::from(i + 1)).unwrap_or_else(|| Value::from(0)),
        _ => Value::String("edited".into()),
    }
}

/// The field to alter: never a primary-key column, only strings and numbers.
fn pick(row: &Map<String, Value>, key: &[String]) -> Option<String> {
    let usable =
        |k: &str| !key.iter().any(|c| c == k) && matches!(row.get(k), Some(Value::String(_) | Value::Number(_)));
    PREFERRED.iter().find(|k| usable(k)).map(|k| k.to_string()).or_else(|| {
        let mut keys: Vec<&String> = row.keys().filter(|k| usable(k)).collect();
        keys.sort();
        keys.first().map(|k| (*k).clone())
    })
}

/// Alters one field of the stored row image (`after`, or `before` for a delete). Returns what changed.
pub fn tamper_event(ev: &mut Event) -> String {
    let label = format!("{} {}", ev.source.table, ev.key_label());
    let key: Vec<String> = ev.key.keys().cloned().collect();
    let Some(Value::Object(row)) = ev.after.as_mut().or(ev.before.as_mut()) else {
        ev.after = Some(serde_json::json!({ "edited": true }));
        return format!("{label}: row image added");
    };
    let Some(field) = pick(row, &key) else {
        let n = row.get("_edited").and_then(Value::as_i64).unwrap_or(0) + 1;
        row.insert("_edited".into(), Value::from(n));
        return format!("{label}: field _edited set to {n}");
    };
    let old = row[&field].clone();
    let new = edit_value(&old);
    row.insert(field.clone(), new.clone());
    format!("{label}: {field} {old} → {new}")
}

pub fn tamper(dir: &DataDir, action: TamperAction, record: &str, cover: bool, p: Paint) -> Res<ExitCode> {
    need_chain(dir)?;
    let _lock = dir.lock().map_err(|e| e.to_string())?;
    let (mut rows, _) = dir.load_trail_from(None).map_err(|e| e.to_string())?;
    let i = rows
        .iter()
        .position(|r| r.id == record)
        .ok_or_else(|| format!("no record {record} in the node's trail (see `trail`)"))?;
    let original = rows[i].clone();
    let (name, description) = match action {
        TamperAction::Delete => {
            let r = rows.remove(i);
            ("delete", format!("deleted {} ({})", r.id, describe(&r.event).text))
        }
        TamperAction::Edit => {
            let r = &mut rows[i];
            let d = tamper_event(&mut r.event);
            if cover {
                r.leaf_hash = leaf_hash(&r.salt, &r.event);
            }
            ("edit", d)
        }
    };
    rewrite_jsonl(&dir.trail(), &rows).map_err(|e| e.to_string())?;
    let entry =
        TamperEntry { row_id: record.to_string(), action: name.into(), description: description.clone(), original };
    append_jsonl(&dir.tamper_log(), &[entry]).map_err(|e| e.to_string())?;
    println!("{} {description}", p.bad("Insider altered the audit store:"));
    if cover && action == TamperAction::Edit {
        println!("  {}", p.dim("The stored leaf hash was recomputed too, so the row looks self-consistent."));
    }
    println!("  {}", p.dim("Nothing on the ledger changed. Run `verify` to see whether the auditor notices."));
    Ok(ExitCode::SUCCESS)
}

pub fn untamper(dir: &DataDir, p: Paint) -> Res<ExitCode> {
    need_chain(dir)?;
    let _lock = dir.lock().map_err(|e| e.to_string())?;
    let log = dir.load_tamper_log().map_err(|e| e.to_string())?;
    if log.is_empty() {
        println!("Nothing to restore.");
        return Ok(ExitCode::SUCCESS);
    }
    let (mut rows, _) = dir.load_trail_from(None).map_err(|e| e.to_string())?;
    let mut done = HashSet::new();
    for e in &log {
        if !done.insert(e.row_id.clone()) {
            continue; // the first entry holds the true original
        }
        match rows.iter().position(|r| r.id == e.row_id) {
            Some(i) => rows[i] = e.original.clone(),
            None => rows.push(e.original.clone()),
        }
    }
    rows.sort_by_key(|r| (r.batch_seq, r.index));
    rewrite_jsonl(&dir.trail(), &rows).map_err(|e| e.to_string())?;
    std::fs::remove_file(dir.tamper_log()).map_err(|e| e.to_string())?;
    println!("{} {} record(s) restored to what was captured.", p.ok("✓"), done.len());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sthira_connector::generator::{GenConfig, generate};
    use sthira_connector::mysql::normalize_tx;

    #[test]
    fn repeated_edits_never_restore_the_original_or_touch_the_key() {
        for t in generate(&GenConfig { txs: 60, ..GenConfig::default() }) {
            for original in normalize_tx(&t, "DT", "t").events {
                let mut ev = original.clone();
                for round in 0..5 {
                    tamper_event(&mut ev);
                    assert_ne!(ev, original, "round {round}");
                    assert_eq!(ev.key, original.key);
                }
            }
        }
        assert_eq!(edit_value(&Value::String("1250.00".into())), Value::String("12500.00".into()));
    }
}
