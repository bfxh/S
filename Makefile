.PHONY: fmt lint test gate gate-full deny

fmt:
	cargo fmt --all

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

gate:
	bash scripts/gate.sh --skip-dial9 --skip-hotpath

gate-full:
	bash scripts/gate.sh

deny:
	cargo deny check licenses bans sources advisories
