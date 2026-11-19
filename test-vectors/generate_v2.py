"""Independent reference implementation of Sthiranet hashing spec v2, used to generate
hashing-v2.json. The Rust crate sthira-core must reproduce every value (see its vectors test).

Run: python3 -I test-vectors/generate_v2.py > test-vectors/hashing-v2.json

Spec v2 is the event shape for API-sourced change logs. Every field is always present; absent
values are JSON null.

JCS here is json.dumps with sorted keys and no whitespace. That equals RFC 8785 for these inputs:
strings, integers, null, booleans, and the floats used below (17.68, 0.5, 1250.0 is avoided on
purpose because Python writes "1250.0" where ECMAScript writes "1250").
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


SYSTEM = "audit-api.example.test"


def ev(pos, table, key, op, before, after, actor, channel, committed, captured):
    return {
        "v": 2, "chain": "DT",
        "source": {"connector": "dreamtalk-audit-api", "system": SYSTEM, "table": table,
                   "pos": str(pos), "channel": channel, "origin": None},
        "key": {"id": key}, "op": op, "before": before, "after": after, "actor": actor,
        "committed_at": committed, "captured_at": captured,
    }


SYSTEM_ACTOR = {"id": None, "name": None, "org": None, "kind": "system"}
USER_ACTOR = {"id": "42", "name": "Asha Rao", "org": "7", "kind": "user"}

events = [
    # A request log row with a float and an endpoint path.
    ev(1001, "api_request_log", "501", "insert", None,
       {"id": 501, "user": None, "status": "SUCCESS", "endpoint": "/reviews/summary/", "duration_ms": 17.68,
        "status_code": 200, "error_message": "No survey pending summary"},
       SYSTEM_ACTOR, "GET", "2026-11-18T05:39:01.767051Z", "2026-11-18T05:39:05.000000Z"),
    # An update with no before image (the source did not supply one).
    ev(1002, "app_user", "724", "update", None,
       {"id": 724, "role": "ORG_ADMIN", "is_staff": False, "is_superuser": False, "password": "***MASKED***",
        "full_name": "Test User 0001", "organization": 7},
       USER_ACTOR, "POST", "2026-11-18T05:39:02.100000Z", "2026-11-18T05:39:05.000000Z"),
    # An update with both images, by a named user, non-ASCII text.
    ev(1003, "dreamaudio_recordingpresence", "24", "update",
       {"id": 24, "user": 42, "last_seen": "2026-11-18T05:39:00.000000+00:00", "display_name": "Asha Rao"},
       {"id": 24, "user": 42, "last_seen": "2026-11-18T05:39:03.500000+00:00", "display_name": "Asha Rao"},
       USER_ACTOR, None, "2026-11-18T05:39:03.510000Z", "2026-11-18T05:39:05.000000Z"),
    # A UUID key and a nested object.
    ev(1004, "chat_message", "83ced43d-1460-4ab8-81b7-b134f3a0d251", "insert", None,
       {"id": "83ced43d-1460-4ab8-81b7-b134f3a0d251", "text": "Namaste — meeting at 5", "meta": {"pinned": False, "score": 0.5}},
       USER_ACTOR, None, "2026-11-18T05:39:04.000000Z", "2026-11-18T05:39:05.000000Z"),
    # A delete: before only.
    ev(1005, "dreamreviews_surveyquestionsubmissions", "1839", "delete",
       {"id": 1839, "parsed": False, "status": "PENDING", "question": 1104, "transcript": None},
       None, SYSTEM_ACTOR, None, "2026-11-18T05:39:04.900000Z", "2026-11-18T05:39:05.000000Z"),
]

salts = [f"{i + 0xa1:02x}" * 32 for i in range(len(events))]
leaves = [leaf(s, e) for s, e in zip(salts, events)]
g = genesis("DT")
root = mth(leaves)
meta = {"chain": "DT", "seq": 1, "from_pos": "1001", "to_pos": "1005",
        "from_time": "2026-11-18T05:38:55.000000Z", "to_time": "2026-11-18T05:39:05.000000Z", "event_count": 5}
h1 = head(g, root, meta)

print(json.dumps({
    "spec": "sthiranet hashing v2",
    "events": [{"salt": s, "event": e, "canonical": jcs(e), "leaf": l} for s, e, l in zip(salts, events, leaves)],
    "batch": {"leaves": leaves, "root": root, "proofs": [{"index": i, "path": path(leaves, i)} for i in range(len(leaves))]},
    "genesis": {"chain": "DT", "head": g},
    "heads": [{"prev_head": g, "root": root, "meta": meta, "canonical_meta": jcs(meta), "head": h1}],
}, indent=2, ensure_ascii=False))
