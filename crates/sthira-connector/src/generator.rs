//! Offline dummy data: a small ERP's day, written as MySQL binlog transactions.
//!
//! The workload is deterministic for a given [`GenConfig`] (seeded RNG, no wall clock), so the same
//! command always produces the same file and a presentation can be rehearsed. GTID numbers are
//! consecutive from `first_tx`; timestamps advance by a few seconds per transaction from `start`.
//!
//! Schema `erp`:
//!
//! | table         | primary key           | written by                                   |
//! |---------------|-----------------------|----------------------------------------------|
//! | `app_users`   | `id`                  | role changes (privilege escalation is flagged) |
//! | `customers`   | `id`                  | onboarding, credit-limit changes             |
//! | `orders`      | `id`                  | new orders, status changes                   |
//! | `order_lines` | `order_id`, `line_no` | new orders (composite key)                   |
//! | `invoices`    | `id`                  | invoicing, payment, deleting drafts          |
//! | `payments`    | `id`                  | payments                                     |

use std::path::Path;

use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::mysql::{BinlogTx, RowChange, RowsEvent};

const DB: &str = "erp";
const CITIES: &[&str] = &["Pune", "Mumbai", "Bengaluru", "Chennai", "Hyderabad", "Delhi"];
const NAMES: &[&str] =
    &["Asha Rao", "Vikram Shah", "Meera Iyer", "Rohan Gupta", "Kavya Nair", "Arjun Mehta", "Priya Das", "Nikhil Joshi"];
const SKUS: &[&str] = &["SKU-0042", "SKU-0107", "SKU-0213", "SKU-0388", "SKU-0451"];
const ROLES: &[&str] = &["clerk", "accountant", "manager", "admin"];
const METHODS: &[&str] = &["NEFT", "RTGS", "UPI", "CHEQUE"];

/// What to generate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenConfig {
    pub seed: u64,
    /// Number of transactions.
    pub txs: u64,
    /// Commit time of the first transaction, Unix seconds.
    pub start: i64,
    /// GTID number of the first transaction.
    pub first_tx: u64,
}

impl Default for GenConfig {
    fn default() -> Self {
        // 2026-11-20T09:00:00Z
        Self { seed: 7, txs: 200, start: 1_795_165_200, first_tx: 1 }
    }
}

#[derive(Debug, Clone)]
struct Order {
    id: u64,
    customer: u64,
    total: u64, // paise
    status: &'static str,
}

#[derive(Debug, Clone)]
struct Invoice {
    id: u64,
    order: u64,
    amount: u64,
    status: &'static str,
}

/// The simulated database: the current row of everything the workload may touch again.
struct Erp {
    rng: StdRng,
    server_uuid: String,
    users: Vec<(u64, String, &'static str)>,
    customers: Vec<(u64, String, &'static str, u64)>, // id, name, city, credit limit (rupees)
    orders: Vec<Order>,
    invoices: Vec<Invoice>,
    next_id: u64,
    pos: u64,
}

fn money(paise: u64) -> String {
    format!("{}.{:02}", paise / 100, paise % 100)
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => unreachable!("rows are objects"),
    }
}

impl Erp {
    fn new(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let b: [u8; 16] = rng.random();
        let h = hex(&b);
        let server_uuid = format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]);
        Self {
            rng,
            server_uuid,
            users: Vec::new(),
            customers: Vec::new(),
            orders: Vec::new(),
            invoices: Vec::new(),
            next_id: 1000,
            pos: 4,
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn row(
        &mut self,
        table: &str,
        event: RowsEvent,
        pk: &[&str],
        before: Option<Value>,
        after: Option<Value>,
    ) -> RowChange {
        self.pos += self.rng.random_range(180..420);
        RowChange {
            db: DB.into(),
            table: table.into(),
            event,
            end_log_pos: self.pos,
            pk: pk.iter().map(|s| s.to_string()).collect(),
            before: before.map(obj),
            after: after.map(obj),
        }
    }

    fn user_json(u: &(u64, String, &'static str)) -> Value {
        json!({ "id": u.0, "name": u.1, "role": u.2, "active": true })
    }

    fn customer_json(c: &(u64, String, &'static str, u64)) -> Value {
        json!({ "id": c.0, "name": c.1, "city": c.2, "credit_limit": money(c.3 * 100), "blocked": false })
    }

    fn order_json(o: &Order) -> Value {
        json!({ "id": o.id, "customer_id": o.customer, "status": o.status, "total": money(o.total), "currency": "INR" })
    }

    fn invoice_json(i: &Invoice) -> Value {
        json!({ "id": i.id, "order_id": i.order, "amount": money(i.amount), "status": i.status })
    }

    fn new_user(&mut self) -> Vec<RowChange> {
        let id = self.id();
        let u = (id, NAMES.choose(&mut self.rng).copied().unwrap_or("User").to_string(), "clerk");
        let after = Self::user_json(&u);
        self.users.push(u);
        vec![self.row("app_users", RowsEvent::WriteRows, &["id"], None, Some(after))]
    }

    fn new_customer(&mut self) -> Vec<RowChange> {
        let id = self.id();
        let name = format!("{} Traders", NAMES.choose(&mut self.rng).copied().unwrap_or("New"));
        let city = CITIES.choose(&mut self.rng).copied().unwrap_or("Pune");
        let limit = self.rng.random_range(2..20) * 25_000;
        let c = (id, name, city, limit);
        let after = Self::customer_json(&c);
        self.customers.push(c);
        vec![self.row("customers", RowsEvent::WriteRows, &["id"], None, Some(after))]
    }

    fn new_order(&mut self) -> Vec<RowChange> {
        let Some(customer) = self.customers.choose(&mut self.rng).map(|c| c.0) else {
            return self.new_customer();
        };
        let id = self.id();
        let lines = self.rng.random_range(1..=3u64);
        let mut rows = Vec::new();
        let mut total = 0;
        let mut line_rows = Vec::new();
        for n in 1..=lines {
            let qty = self.rng.random_range(1..=20u64);
            let unit = self.rng.random_range(50..2_000u64) * 100;
            total += qty * unit;
            let sku = SKUS.choose(&mut self.rng).copied().unwrap_or("SKU-0001");
            line_rows.push(json!({ "order_id": id, "line_no": n, "sku": sku, "qty": qty, "unit_price": money(unit) }));
        }
        let o = Order { id, customer, total, status: "NEW" };
        rows.push(self.row("orders", RowsEvent::WriteRows, &["id"], None, Some(Self::order_json(&o))));
        for l in line_rows {
            rows.push(self.row("order_lines", RowsEvent::WriteRows, &["order_id", "line_no"], None, Some(l)));
        }
        self.orders.push(o);
        rows
    }

    fn confirm_order(&mut self) -> Vec<RowChange> {
        let candidates: Vec<usize> = (0..self.orders.len()).filter(|&i| self.orders[i].status == "NEW").collect();
        let Some(&i) = candidates.choose(&mut self.rng) else { return self.new_order() };
        let before = Self::order_json(&self.orders[i]);
        self.orders[i].status = "CONFIRMED";
        let after = Self::order_json(&self.orders[i]);
        vec![self.row("orders", RowsEvent::UpdateRows, &["id"], Some(before), Some(after))]
    }

    fn invoice_order(&mut self) -> Vec<RowChange> {
        let candidates: Vec<usize> = (0..self.orders.len()).filter(|&i| self.orders[i].status == "CONFIRMED").collect();
        let Some(&i) = candidates.choose(&mut self.rng) else { return self.confirm_order() };
        let id = self.id();
        // Some invoices stay drafts for a while; one of them may be deleted later.
        let status = if self.rng.random_bool(0.2) { "DRAFT" } else { "ISSUED" };
        let inv = Invoice { id, order: self.orders[i].id, amount: self.orders[i].total, status };
        let before = Self::order_json(&self.orders[i]);
        self.orders[i].status = "INVOICED";
        let after = Self::order_json(&self.orders[i]);
        let rows = vec![
            self.row("invoices", RowsEvent::WriteRows, &["id"], None, Some(Self::invoice_json(&inv))),
            self.row("orders", RowsEvent::UpdateRows, &["id"], Some(before), Some(after)),
        ];
        self.invoices.push(inv);
        rows
    }

    fn pay_invoice(&mut self) -> Vec<RowChange> {
        let candidates: Vec<usize> =
            (0..self.invoices.len()).filter(|&i| self.invoices[i].status == "ISSUED").collect();
        let Some(&i) = candidates.choose(&mut self.rng) else { return self.invoice_order() };
        let id = self.id();
        let method = METHODS.choose(&mut self.rng).copied().unwrap_or("NEFT");
        let reference = format!("UTR{:010}", self.rng.random_range(0..10_000_000_000u64));
        let before = Self::invoice_json(&self.invoices[i]);
        self.invoices[i].status = "PAID";
        let after = Self::invoice_json(&self.invoices[i]);
        let pay = json!({ "id": id, "invoice_id": self.invoices[i].id, "amount": money(self.invoices[i].amount),
                          "method": method, "reference": reference });
        vec![
            self.row("payments", RowsEvent::WriteRows, &["id"], None, Some(pay)),
            self.row("invoices", RowsEvent::UpdateRows, &["id"], Some(before), Some(after)),
        ]
    }

    fn delete_draft(&mut self) -> Vec<RowChange> {
        let candidates: Vec<usize> = (0..self.invoices.len()).filter(|&i| self.invoices[i].status == "DRAFT").collect();
        let Some(&i) = candidates.choose(&mut self.rng) else { return self.pay_invoice() };
        let inv = self.invoices.remove(i);
        vec![self.row("invoices", RowsEvent::DeleteRows, &["id"], Some(Self::invoice_json(&inv)), None)]
    }

    fn change_credit_limit(&mut self) -> Vec<RowChange> {
        if self.customers.is_empty() {
            return self.new_customer();
        }
        let i = self.rng.random_range(0..self.customers.len());
        let before = Self::customer_json(&self.customers[i]);
        let factor = if self.rng.random_bool(0.7) { 2 } else { 1 };
        self.customers[i].3 = self.customers[i].3 * factor + 25_000;
        let after = Self::customer_json(&self.customers[i]);
        vec![self.row("customers", RowsEvent::UpdateRows, &["id"], Some(before), Some(after))]
    }

    fn change_role(&mut self) -> Vec<RowChange> {
        if self.users.is_empty() {
            return self.new_user();
        }
        let i = self.rng.random_range(0..self.users.len());
        let before = Self::user_json(&self.users[i]);
        let current = self.users[i].2;
        let role = ROLES.iter().copied().filter(|r| *r != current).collect::<Vec<_>>().choose(&mut self.rng).copied();
        self.users[i].2 = role.unwrap_or("clerk");
        let after = Self::user_json(&self.users[i]);
        vec![self.row("app_users", RowsEvent::UpdateRows, &["id"], Some(before), Some(after))]
    }

    /// One transaction's row changes, picked by weight.
    fn next_rows(&mut self, n: u64) -> Vec<RowChange> {
        // Seed the database first: a few users and customers.
        if n < 3 {
            return self.new_user();
        }
        if n < 6 {
            return self.new_customer();
        }
        match self.rng.random_range(0..100) {
            0..=24 => self.new_order(),
            25..=42 => self.confirm_order(),
            43..=60 => self.invoice_order(),
            61..=78 => self.pay_invoice(),
            79..=85 => self.new_customer(),
            86..=91 => self.change_credit_limit(),
            92..=95 => self.delete_draft(),
            96..=97 => self.change_role(),
            _ => self.new_user(),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The workload as an iterator of binlog transactions.
pub fn generate(cfg: &GenConfig) -> impl Iterator<Item = BinlogTx> + use<> {
    let mut erp = Erp::new(cfg.seed);
    let mut time = cfg.start;
    let first = cfg.first_tx;
    (0..cfg.txs).map(move |n| {
        time += erp.rng.random_range(1..=6);
        let rows = erp.next_rows(n);
        BinlogTx {
            server_uuid: erp.server_uuid.clone(),
            tx: first + n,
            file: format!("binlog.{:06}", 1 + n / 500),
            timestamp: time,
            rows,
        }
    })
}

/// Writes the workload to `path` as JSONL, one [`BinlogTx`] per line. Returns the number of
/// transactions and row changes written.
pub async fn write_jsonl(cfg: &GenConfig, path: &Path) -> std::io::Result<(u64, u64)> {
    let mut out = BufWriter::new(tokio::fs::File::create(path).await?);
    let (mut txs, mut rows) = (0, 0);
    for t in generate(cfg) {
        let mut line = serde_json::to_vec(&t)?;
        line.push(b'\n');
        out.write_all(&line).await?;
        txs += 1;
        rows += t.rows.len() as u64;
    }
    out.flush().await?;
    Ok((txs, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_dense_and_well_formed() {
        let cfg = GenConfig { txs: 300, ..GenConfig::default() };
        let a: Vec<BinlogTx> = generate(&cfg).collect();
        assert_eq!(a, generate(&cfg).collect::<Vec<_>>(), "same seed, same data");
        assert_ne!(a, generate(&GenConfig { seed: 8, ..cfg.clone() }).collect::<Vec<_>>());
        assert_eq!(a.iter().map(|t| t.tx).collect::<Vec<_>>(), (1..=300).collect::<Vec<_>>());
        let mut last_pos = 0;
        for t in &a {
            assert!(!t.rows.is_empty());
            for r in &t.rows {
                assert!(r.end_log_pos > last_pos, "positions increase");
                last_pos = r.end_log_pos;
                match r.event {
                    RowsEvent::WriteRows => assert!(r.before.is_none() && r.after.is_some()),
                    RowsEvent::UpdateRows => assert!(r.before.is_some() && r.after.is_some()),
                    RowsEvent::DeleteRows => assert!(r.before.is_some() && r.after.is_none()),
                }
                let image = r.after.as_ref().or(r.before.as_ref()).unwrap();
                assert!(r.pk.iter().all(|c| image.contains_key(c)), "{} has its key columns", r.table);
            }
        }
        let tables: std::collections::BTreeSet<&str> =
            a.iter().flat_map(|t| t.rows.iter().map(|r| r.table.as_str())).collect();
        for t in ["app_users", "customers", "orders", "order_lines", "invoices", "payments"] {
            assert!(tables.contains(t), "workload touches {t}");
        }
        assert!(a.windows(2).all(|w| w[0].timestamp < w[1].timestamp));
    }
}
