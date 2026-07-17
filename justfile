# Build the engine binaries: the bare `nexum` engine and the cow
# composition root `shepherd`.
build-engine:
    cargo build -p nexum-cli -p shepherd

# Build the example WASM module
build-module:
    cargo build --target wasm32-wasip2 --release -p example

# Build the reference venue adapter (echo-venue) for wasm32-wasip2. Its
# per-component world pins the #[videre_sdk::venue] acceptance test.
build-venue:
    cargo build --target wasm32-wasip2 --release -p echo-venue

# Build everything
build: build-engine build-module

# Build the module then run the engine with it. The second argument is the
# module's module.toml — without it the engine prints the 0.1-compat
# deprecation warning and proceeds with empty capabilities/config.
run: build-module build-engine
    cargo run -p nexum-cli -- target/wasm32-wasip2/release/example.wasm modules/example/module.toml

# Run host engine unit tests
test:
    cargo test -p nexum-runtime

# Build module + engine, then run E2E integration tests
test-e2e: build-module build-engine
    cargo test -p nexum-runtime supervisor::tests::e2e

# Build the M2 modules (twap-monitor + ethflow-watcher) for wasm32-wasip2.
build-m2:
    cargo build -p twap-monitor    --target wasm32-wasip2 --release
    cargo build -p ethflow-watcher --target wasm32-wasip2 --release

# Run nexum wired for the M2 smoke / round-trip scenario
# (Sepolia, both M2 modules). See `docs/operations/m2-testnet-runbook.md`.
# --pretty-logs keeps the runbook-friendly human-readable formatter;
# production deploys omit the flag and emit JSON.
run-m2: build-m2 build-engine
    cargo run -p shepherd -- --engine-config engine.m2.toml --pretty-logs

# Build the M3 example modules (price-alert + balance-tracker + stop-loss)
# for wasm32-wasip2.
build-m3:
    cargo build -p price-alert     --target wasm32-wasip2 --release
    cargo build -p balance-tracker --target wasm32-wasip2 --release
    cargo build -p stop-loss       --target wasm32-wasip2 --release

# Run nexum wired for the M3 smoke / validation scenario
# (Sepolia, 3 example modules). See `docs/operations/m3-testnet-runbook.md`.
# --pretty-logs keeps the runbook-friendly human-readable formatter;
# production deploys omit the flag and emit JSON.
run-m3: build-m3 build-engine
    cargo run -p shepherd -- --engine-config engine.m3.toml --pretty-logs

# Build the http-probe example module (wasi:http fetch + allowlist
# denial demo) for wasm32-wasip2.
build-http-probe:
    cargo build -p http-probe --target wasm32-wasip2 --release

# Build all 5 modules required by the E2E run (twap-monitor +
# ethflow-watcher + price-alert + balance-tracker + stop-loss).
build-e2e: build-m2 build-m3

# Run the 4-6 h E2E integration scenario on Sepolia. All 5 modules
# dispatched simultaneously against a live RPC; metrics scraped at
# 127.0.0.1:9100/metrics. JSON logs (no --pretty-logs) so a
# downstream `jq` filter can mine submitted/dropped/backoff markers
# for the e2e report. See `docs/operations/e2e-testnet-runbook.md`.
run-e2e: build-e2e build-engine
    cargo run -p shepherd -- --engine-config engine.e2e.toml

# Zero-leak gate: host-layer crate graphs, runtime charter-symbol and
# router-field scans, and the nexum:host WIT leaf and foreign-namespace
# scans. Blocking in CI.
check-venue-agnostic:
    ./scripts/check-venue-agnostic.sh

# Check the entire workspace
check:
    cargo check --target wasm32-wasip2 -p example
    cargo check -p nexum-runtime
    cargo check -p nexum-cli -p shepherd

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml one-to-one: rustfmt, clippy, rustdoc, the
# module wasms the integration tests need, and the workspace test
# suite, all under the `-D warnings` the CI workflow sets globally.
ci:
    #!/usr/bin/env bash
    set -euo pipefail
    export RUSTFLAGS="-D warnings"
    export RUSTDOCFLAGS="-D warnings"
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo doc --workspace --no-deps
    cargo build --release --target wasm32-wasip2 \
        -p example -p twap-monitor -p ethflow-watcher -p price-alert \
        -p balance-tracker -p stop-loss -p http-probe -p echo-venue \
        -p echo-client -p clock-reader -p flaky-bomb -p flaky-venue -p fuel-bomb \
        -p memory-bomb -p panic-bomb -p slow-host
    cargo test --workspace --all-features --no-fail-fast
