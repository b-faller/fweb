.PHONY: check

check:
	cargo fmt --all
	cargo clippy --all --workspace -- -D warnings
	cargo test --workspace
	cargo doc --workspace --document-private-items
	cargo +nightly udeps --workspace
	cargo update
	cargo audit
