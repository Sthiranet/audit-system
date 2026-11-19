//! Spec test vectors in `test-vectors/hashing-v1.json` and `hashing-v2.json`, produced by independent
//! Python implementations (`test-vectors/generate.py`, `generate_v2.py`). The Rust kernel must
//! reproduce every value.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use sthira_core::{
    BatchMeta, Event, EventV2, canonical, genesis_head, head_hash, inclusion_proof, leaf_hash, merkle_root,
    verify_inclusion,
};

#[derive(Deserialize)]
#[serde(bound = "E: DeserializeOwned")]
struct Vectors<E> {
    events: Vec<EventVector<E>>,
    batch: BatchVector,
    genesis: GenesisVector,
    heads: Vec<HeadVector>,
}

#[derive(Deserialize)]
#[serde(bound = "E: DeserializeOwned")]
struct EventVector<E> {
    salt: String,
    event: E,
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

fn load<E: DeserializeOwned>(file: &str) -> Vectors<E> {
    let path = format!("{}/../../test-vectors/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("vector file")).expect("vector json")
}

fn check_events<E: DeserializeOwned + serde::Serialize>(v: &Vectors<E>) {
    for e in &v.events {
        // Round-tripping through the typed event must not change the canonical form.
        assert_eq!(canonical(&e.event), e.canonical);
        assert_eq!(leaf_hash(&e.salt, &e.event), e.leaf);
    }
}

fn check_batch<E>(v: &Vectors<E>) {
    let b = &v.batch;
    assert_eq!(merkle_root(&b.leaves), b.root);
    for p in &b.proofs {
        assert_eq!(inclusion_proof(&b.leaves, p.index).unwrap(), p.path);
        assert!(verify_inclusion(&b.leaves[p.index], p.index, b.leaves.len(), &p.path, &b.root));
    }
}

fn check_heads<E>(v: &Vectors<E>) {
    assert_eq!(genesis_head(&v.genesis.chain), v.genesis.head);
    for h in &v.heads {
        assert_eq!(canonical(&h.meta), h.canonical_meta);
        assert_eq!(head_hash(&h.prev_head, &h.root, &h.meta), h.head);
    }
}

#[test]
fn v1_events_canonicalise_and_hash_to_the_reference_leaves() {
    check_events(&load::<Event>("hashing-v1.json"));
}

#[test]
fn v1_batch_root_and_proofs_match() {
    check_batch(&load::<Event>("hashing-v1.json"));
}

#[test]
fn v1_genesis_and_heads_match() {
    check_heads(&load::<Event>("hashing-v1.json"));
}

#[test]
fn v2_events_canonicalise_and_hash_to_the_reference_leaves() {
    check_events(&load::<EventV2>("hashing-v2.json"));
}

#[test]
fn v2_batch_root_and_proofs_match() {
    check_batch(&load::<EventV2>("hashing-v2.json"));
}

#[test]
fn v2_genesis_and_heads_match() {
    check_heads(&load::<EventV2>("hashing-v2.json"));
}
