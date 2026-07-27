# Build the cow composition-root engine binary.
build-engine:
    cargo build -p shepherd-engine

# Build the two production keeper modules (twap-monitor +
# ethflow-watcher) for wasm32-wasip2.
build-modules:
    cargo build -p twap-monitor    --target wasm32-wasip2 --release
    cargo build -p ethflow-watcher --target wasm32-wasip2 --release

# Build the bundled cow venue adapter component. Install via the
# engine.toml [[adapters]] stanza; the venue id is its manifest name.
build-cow-venue:
    cargo build --target wasm32-wasip2 --release -p cow-venue --features adapter

# Build everything this repo ships.
build: build-modules build-cow-venue build-engine

# Run the workspace test suite.
test:
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features

# Format / lint.
fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run shepherd wired for the M2 smoke / round-trip scenario
# (Sepolia, both keeper modules). --pretty-logs keeps the
# runbook-friendly human-readable formatter; production deploys omit
# the flag and emit JSON.
run-m2: build-modules build-cow-venue build-engine
    cargo run -p shepherd-engine -- --engine-config engine.m2.toml --pretty-logs

# Run the E2E integration scenario on Sepolia. The scenario also loads
# the nexum example modules (price-alert + balance-tracker); build
# their wasms from a sibling nexum-runtime checkout into this repo's
# target dir first (see scripts/e2e-run.sh).
run-e2e: build-modules build-cow-venue build-engine
    cargo run -p shepherd-engine -- --engine-config engine.e2e.toml

# Managed e2e / load / soak drivers.
e2e *ARGS:
    ./scripts/e2e-run.sh {{ARGS}}

load *ARGS:
    ./scripts/load-run.sh {{ARGS}}

soak-snapshot *ARGS:
    ./scripts/soak-snapshot.sh {{ARGS}}

# Orderbook-only gate: the CoW venue crate carries no composable
# symbol. Blocking in CI.
check-cow-orderbook-only:
    ./scripts/check-cow-orderbook-only.sh

# Docker image (the shepherd-engine composition root + module wasms).
docker-build:
    docker build -t ghcr.io/nullislabs/shepherd:dev .

# Run the full CI series locally before pushing. Mirrors
# .github/workflows/ci.yml one-to-one: rustfmt, clippy, rustdoc, the
# module wasms the integration tests need, and the workspace test
# suite via nextest plus the doctests, all under the `-D warnings` the
# CI workflow sets globally.
ci:
    #!/usr/bin/env bash
    set -euo pipefail
    # Append -D warnings without clobbering the devshell's flags (mold linker,
    # set in flake.nix), so the local run keeps fast native linking. RUSTC_WRAPPER
    # is already sccache from the devshell shellHook.
    export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
    export RUSTDOCFLAGS="${RUSTDOCFLAGS:-} -D warnings"
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    ./scripts/check-cow-orderbook-only.sh
    cargo doc --workspace --no-deps
    cargo build --release --target wasm32-wasip2 \
        -p twap-monitor -p ethflow-watcher
    # Separate invocation on purpose: unifying `cow-venue/adapter` into the
    # module build would link the adapter's component export glue into every
    # keeper module wasm.
    cargo build --release --target wasm32-wasip2 -p cow-venue --features adapter
    # nextest for the suite (as CI does); doctests run separately since nextest
    # does not cover them.
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features
