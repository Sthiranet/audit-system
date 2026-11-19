"""Independent reference implementation of Sthiranet hashing spec v3, used to generate
hashing-v3.json. The Rust crate sthira-core must reproduce every value (see its vectors test).

Run: python3 -I test-vectors/generate_v3.py > test-vectors/hashing-v3.json

Spec v3 is the event shape for row-level database change logs (MySQL binlog first). Every field is
always present; absent values are JSON null. Decimal amounts are strings.

JCS here is json.dumps with sorted keys and no whitespace. That equals RFC 8785 for these inputs:
strings, integers, null and booleans (no floats on purpose).
"""
import hashlib
import json


def jcs(v):
    return json.dumps(v, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def h(*parts):
    d = hashlib.sha256()
    for p in parts:
        d.update(p)
    return d.hexdigest()


def leaf(salt, event):
    return h(b"\x00", bytes.fromhex(salt), jcs(event).encode())


def node(l, r):
    return h(b"\x01", bytes.fromhex(l), bytes.fromhex(r))


def split(n):
    k = 1
    while k * 2 < n:
        k *= 2
    return k


def mth(leaves):
    if not leaves:
        return h(b"")
    if len(leaves) == 1:
        return leaves[0]
    k = split(len(leaves))
    return node(mth(leaves[:k]), mth(leaves[k:]))


def path(leaves, m):
    if len(leaves) == 1:
        return []
    k = split(len(leaves))
    if m < k:
        return path(leaves[:k], m) + [mth(leaves[k:])]
    return path(leaves[k:], m - k) + [mth(leaves[:k])]


def head(prev, root, meta):
    return h(b"\x02", bytes.fromhex(prev), bytes.fromhex(root), jcs(meta).encode())


def genesis(chain):
    return h(b"\x03", b"sthiranet/v1/genesis/", chain.encode())


SERVER = "3e11fa47-71ca-11e1-9e33-c80aa9429562"
CAPTURED = "2026-11-20T10:00:05.000000Z"


def ev(tx, pos, row, table, key, op, before, after, committed):
    return {
        "v": 3, "chain": "DT",
        "source": {"connector": "mysql-binlog", "server": SERVER, "db": "erp", "table": table,
                   "gtid": f"{SERVER}:{tx}", "tx": str(tx), "file": "binlog.000042", "pos": str(pos), "row": row},
        "key": key, "op": op, "before": before, "after": after,
        "committed_at": committed, "captured_at": CAPTURED,
    }


events = [
    # tx 101: an order and its first line, inserted together.
    ev(101, 4410, 0, "orders", {"id": "5001"}, "insert", None,
       {"id": 5001, "customer_id": 17, "status": "NEW", "total": "1250.00", "currency": "INR",
        "created_at": "2026-11-20 10:00:01"}, "2026-11-20T10:00:01.000000Z"),
    ev(101, 4410, 1, "order_lines", {"line_no": "1", "order_id": "5001"}, "insert", None,
       {"order_id": 5001, "line_no": 1, "sku": "SKU-0042", "qty": 5, "unit_price": "250.00"},
       "2026-11-20T10:00:01.000000Z"),
    # tx 102: an update with both full images, non-ASCII text.
    ev(102, 4987, 0, "customers", {"id": "17"}, "update",
       {"id": 17, "name": "Asha Rao", "city": "Pune", "credit_limit": "50000.00", "blocked": False},
       {"id": 17, "name": "Asha Rao", "city": "Pune — Baner", "credit_limit": "75000.00", "blocked": False},
       "2026-11-20T10:00:02.000000Z"),
    # tx 103: a payment row and a delete.
    ev(103, 5532, 0, "payments", {"id": "9001"}, "insert", None,
       {"id": 9001, "invoice_id": 7001, "amount": "1250.00", "method": "NEFT", "reference": None},
       "2026-11-20T10:00:03.000000Z"),
    ev(103, 5532, 1, "invoices", {"id": "6999"}, "delete",
       {"id": 6999, "order_id": 4999, "amount": "80.00", "status": "DRAFT"}, None,
       "2026-11-20T10:00:03.000000Z"),
]

salts = [f"{i + 0xa1:02x}" * 32 for i in range(len(events))]
leaves = [leaf(s, e) for s, e in zip(salts, events)]
g = genesis("DT")
root = mth(leaves)
meta1 = {"chain": "DT", "seq": 1, "from_tx": 101, "to_tx": 103,
         "from_time": "2026-11-20T09:59:55.000000Z", "to_time": CAPTURED, "event_count": 5, "tx_count": 3}
h1 = head(g, root, meta1)
# An empty batch repeats the previous to_tx.
meta2 = {"chain": "DT", "seq": 2, "from_tx": 103, "to_tx": 103,
         "from_time": CAPTURED, "to_time": "2026-11-20T10:00:15.000000Z", "event_count": 0, "tx_count": 0}
empty = mth([])
h2 = head(h1, empty, meta2)

print(json.dumps({
    "spec": "sthiranet hashing v3",
    "events": [{"salt": s, "event": e, "canonical": jcs(e), "leaf": l} for s, e, l in zip(salts, events, leaves)],
    "batch": {"leaves": leaves, "root": root, "proofs": [{"index": i, "path": path(leaves, i)} for i in range(len(leaves))]},
    "genesis": {"chain": "DT", "head": g},
    "heads": [
        {"prev_head": g, "root": root, "meta": meta1, "canonical_meta": jcs(meta1), "head": h1},
        {"prev_head": h1, "root": empty, "meta": meta2, "canonical_meta": jcs(meta2), "head": h2},
    ],
}, indent=2, ensure_ascii=False))
