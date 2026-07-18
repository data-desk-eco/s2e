# s2e — native cli + wasm. `make` builds the release cli.
BIN := target/release/s2e

.PHONY: all release debug wasm test check clean
all: release

release: ## optimised native cli → $(BIN)
	cargo build --release -p s2e-cli

debug:
	cargo build -p s2e-cli

wasm: ## browser core via wasm-pack → wasm/pkg
	wasm-pack build wasm --target web

test:
	cargo test

check:
	cargo check --workspace

clean:
	cargo clean
