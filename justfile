# Build the shepherd engine binary.
build-engine:
    cargo build -p shepherd

# Build the bundled cow venue adapter component. Install via the
# engine.toml [[adapters]] stanza; the venue id is its manifest name.
build-cow-venue:
    cargo build --target wasm32-wasip2 --release -p cow-venue --features adapter

# Build the CoW keeper modules (twap-monitor, ethflow-watcher, stop-loss)
# for wasm32-wasip2.
build-modules:
    cargo build --target wasm32-wasip2 --release \
        -p twap-monitor -p ethflow-watcher -p stop-loss

# Build everything.
build: build-engine build-cow-venue build-modules

# Run host engine unit tests.
test:
    cargo test -p shepherd

# Run shepherd wired for the M2 smoke / round-trip scenario (Sepolia,
# twap-monitor + ethflow-watcher). --pretty-logs keeps the human-readable
# formatter; production deploys omit the flag and emit JSON.
run-m2: build-modules build-cow-venue build-engine
    cargo run -p shepherd -- --engine-config engine.m2.toml --pretty-logs

# Run the E2E integration scenario on Sepolia. JSON logs (no --pretty-logs)
# so a downstream `jq` filter can mine submitted/dropped/backoff markers.
run-e2e: build-modules build-cow-venue build-engine
    cargo run -p shepherd -- --engine-config engine.e2e.toml

# Orderbook-only gate: the CoW venue crate carries no composable symbol.
# Blocking in CI.
check-cow-orderbook-only:
    ./scripts/check-cow-orderbook-only.sh

# Check the workspace.
check:
    cargo check --workspace

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml one-to-one: rustfmt, clippy, rustdoc, the
# module wasms the integration tests need, and the workspace test suite,
# all under the `-D warnings` the CI workflow sets globally.
ci:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTFLAGS="-D warnings"
    export RUSTDOCFLAGS="-D warnings"
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo doc --workspace --no-deps
    cargo build --release --target wasm32-wasip2 \
        -p twap-monitor -p ethflow-watcher -p stop-loss
    cargo build --release --target wasm32-wasip2 -p cow-venue --features cow-venue/adapter
    cargo test --workspace --all-features --no-fail-fast
    ./scripts/check-cow-orderbook-only.sh
