//! What a change means, in words a partner or auditor reads without opening JSON.
//!
//! Descriptions are display only: they are computed from the event and never hashed.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sthira_core::{Event, Op};

/// The business meaning of a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    /// Short tag, e.g. `payment`, `privilege`.
    pub kind: String,
    pub text: String,
    /// The change deserves an auditor's attention (privilege escalation, deleted invoice, …).
    pub flagged: bool,
}

fn s<'a>(row: Option<&'a Value>, k: &str) -> Option<&'a str> {
    row?.get(k)?.as_str()
}

fn field(row: Option<&Value>, k: &str) -> String {
    match row.and_then(|r| r.get(k)) {
        Some(Value::String(v)) => v.clone(),
        Some(v) => v.to_string(),
        None => "?".into(),
    }
}

fn changed(e: &Event) -> Vec<String> {
    let (Some(Value::Object(b)), Some(Value::Object(a))) = (&e.before, &e.after) else { return Vec::new() };
    let mut keys: Vec<&String> = a.keys().filter(|k| b.get(*k) != a.get(*k)).collect();
    keys.sort();
    keys.into_iter().map(|k| format!("{k} {} → {}", b.get(k).unwrap_or(&Value::Null), a[k])).collect()
}

const ROLE_RANK: &[&str] = &["clerk", "accountant", "manager", "admin"];

fn rank(r: Option<&str>) -> usize {
    r.and_then(|r| ROLE_RANK.iter().position(|x| *x == r)).unwrap_or(0)
}

pub fn describe(e: &Event) -> Description {
    let (b, a) = (e.before.as_ref(), e.after.as_ref());
    let row = e.row();
    let d = |kind: &str, text: String, flagged: bool| Description { kind: kind.into(), text, flagged };
    match (e.source.table.as_str(), e.op) {
        ("orders", Op::Insert) => d(
            "order",
            format!(
                "Order #{} placed by customer {} for ₹{}",
                field(row, "id"),
                field(row, "customer_id"),
                field(row, "total")
            ),
            false,
        ),
        ("orders", Op::Update) => {
            d("order", format!("Order #{} {} → {}", field(row, "id"), field(b, "status"), field(a, "status")), false)
        }
        ("order_lines", Op::Insert) => d(
            "order",
            format!(
                "Order #{} line {}: {} × {} at ₹{}",
                field(row, "order_id"),
                field(row, "line_no"),
                field(row, "qty"),
                field(row, "sku"),
                field(row, "unit_price")
            ),
            false,
        ),
        ("invoices", Op::Insert) => d(
            "invoice",
            format!(
                "Invoice #{} ({}) for order #{}: ₹{}",
                field(row, "id"),
                field(row, "status"),
                field(row, "order_id"),
                field(row, "amount")
            ),
            false,
        ),
        ("invoices", Op::Update) => d(
            "invoice",
            format!("Invoice #{} {} → {}", field(row, "id"), field(b, "status"), field(a, "status")),
            false,
        ),
        ("invoices", Op::Delete) => d(
            "invoice",
            format!("Invoice #{} ({}, ₹{}) deleted", field(row, "id"), field(row, "status"), field(row, "amount")),
            true,
        ),
        ("payments", Op::Insert) => d(
            "payment",
            format!(
                "Payment #{} of ₹{} for invoice #{} via {}",
                field(row, "id"),
                field(row, "amount"),
                field(row, "invoice_id"),
                field(row, "method")
            ),
            false,
        ),
        ("customers", Op::Insert) => d(
            "customer",
            format!(
                "Customer {} onboarded ({}), credit limit ₹{}",
                field(row, "name"),
                field(row, "city"),
                field(row, "credit_limit")
            ),
            false,
        ),
        ("customers", Op::Update) if s(b, "credit_limit") != s(a, "credit_limit") => d(
            "credit",
            format!(
                "Customer {} credit limit ₹{} → ₹{}",
                field(row, "name"),
                field(b, "credit_limit"),
                field(a, "credit_limit")
            ),
            true,
        ),
        ("app_users", Op::Insert) => {
            d("user", format!("User {} created as {}", field(row, "name"), field(row, "role")), false)
        }
        ("app_users", Op::Update) if s(b, "role") != s(a, "role") => d(
            "privilege",
            format!("User {} role {} → {}", field(row, "name"), field(b, "role"), field(a, "role")),
            rank(s(a, "role")) > rank(s(b, "role")),
        ),
        (table, op) => {
            let what = changed(e);
            let text = if what.is_empty() {
                format!("{table} {} {}", e.key_label(), op.as_str())
            } else {
                format!("{table} {}: {}", e.key_label(), what.join(", "))
            };
            d("change", text, op == Op::Delete)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::{GenConfig, generate};
    use crate::mysql::normalize_tx;

    #[test]
    fn every_generated_change_has_a_meaning_and_some_are_flagged() {
        let mut flagged = 0;
        for t in generate(&GenConfig { txs: 400, ..GenConfig::default() }) {
            for e in normalize_tx(&t, "DT", "t").events {
                let d = describe(&e);
                assert!(!d.text.contains('?'), "{}: {}", e.source.table, d.text);
                flagged += d.flagged as usize;
            }
        }
        assert!(flagged > 0);
    }
}
