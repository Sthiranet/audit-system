# Sthiranet prototype: sthira-node (Rust CLI) + the Daml anchoring model on a Canton sandbox.
# The web demo lives on the `demo` branch.
SHELL := /bin/bash
.PHONY: node sandbox daml-test test check clean

node:               ## build the prototype CLI (target/release/sthira-node); see README.md
	cargo build --release -p sthira-verify --bin sthira-node

sandbox:            ## start the Canton sandbox with the sthiranet-audit DAR (JSON Ledger API on :7575)
	cd prototype-sandbox && ./sandbox.sh

daml-test:          ## run the Daml Script tests of the sthiranet-audit package
	cd prototype-sandbox && dpm test

test:               ## all Rust tests (the live Canton test is skipped unless STHIRA_CANTON_URL is set)
	cargo test --workspace

check:              ## formatting and lints, as CI would run them
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

clean:              ## delete everything compiled (keeps ./sthira-data; remove that by hand for a fresh chain)
	cargo clean
