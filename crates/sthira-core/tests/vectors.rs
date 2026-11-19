//! Spec v3 test vectors in `test-vectors/hashing-v3.json`, produced by an independent Python
//! implementation (`test-vectors/generate_v3.py`). The Rust kernel must reproduce every value.

use serde::Deserialize;
use sthira_core::{
    BatchMeta, Event, canonical, genesis_head, head_hash, inclusion_proof, leaf_hash, merkle_root, verify_inclusion,
};

#[derive(Deserialize)]
struct Vectors {
    events: Vec<EventVector>,
    batch: BatchVector,
    genesis: GenesisVector,
    heads: Vec<HeadVector>,
}

#[derive(Deserialize)]
struct EventVector {
    salt: String,
    event: Event,
    canonical: String,
    leaf: String,
}

#[derive(Deserialize)]
struct BatchVector {
    leaves: Vec<String>,
    root: String,
    proofs: Vec<ProofVector>,
}

#[derive(Deserialize)]
struct ProofVector {
    index: usize,
    path: Vec<String>,
}

#[derive(Deserialize)]
struct GenesisVector {
    chain: String,
    head: String,
}

#[derive(Deserialize)]
struct HeadVector {
    prev_head: String,
    root: String,
    meta: BatchMeta,
    canonical_meta: String,
    head: String,
}

fn load() -> Vectors {
    let path = format!("{}/../../test-vectors/hashing-v3.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("vector file")).expect("vector json")
}

#[test]
fn events_canonicalise_and_hash_to_the_reference_leaves() {
    for e in &load().events {
        // Round-tripping through the typed event must not change the canonical form.
        assert_eq!(canonical(&e.event), e.canonical);
        assert_eq!(leaf_hash(&e.salt, &e.event), e.leaf);
    }
}

#[test]
fn batch_root_and_proofs_match() {
    let b = load().batch;
    assert_eq!(merkle_root(&b.leaves), b.root);
    for p in &b.proofs {
        assert_eq!(inclusion_proof(&b.leaves, p.index).unwrap(), p.path);
        assert!(verify_inclusion(&b.leaves[p.index], p.index, b.leaves.len(), &p.path, &b.root));
    }
}

#[test]
fn genesis_and_heads_match() {
    let v = load();
    assert_eq!(genesis_head(&v.genesis.chain), v.genesis.head);
    for h in &v.heads {
        assert_eq!(canonical(&h.meta), h.canonical_meta);
        assert_eq!(head_hash(&h.prev_head, &h.root, &h.meta), h.head);
    }
}
