# Build the cow composition-root engine binary.
build-engine:
    cargo build -p shepherd-engine

# Build the two production keeper modules (ccow-monitor +
# ethflow-watcher) for wasm32-wasip2.
build-modules:
    cargo build -p ccow-monitor    --target wasm32-wasip2 --release
    cargo build -p ethflow-watcher --target wasm32-wasip2 --release

# Build everything this repo ships. The cow venue is native Rust now, so
# it builds with the engine that registers it.
build: build-modules build-engine

# Run the workspace test suite.
test:
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features

# Format / lint.
fmt:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

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
        -p ccow-monitor -p ethflow-watcher
    # nextest for the suite (as CI does); doctests run separately since nextest
    # does not cover them.
    cargo nextest run --workspace --all-features --no-fail-fast
    cargo test --doc --workspace --all-features
