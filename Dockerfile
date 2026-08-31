# syntax=docker/dockerfile:1.6
#
# Multi-stage build for `shepherd` - the cow composition-root engine
# binary plus the production WASM modules and the bundled cow
# venue adapter baked into a single image.
#
# Stage 1 (`build`): full Rust toolchain + wasm32-wasip2 target, builds
# the engine in release mode + each module to a Component Model wasm
# artefact.
#
# Stage 2 (`runtime`): minimal Debian slim. Just `ca-certificates`
# (for HTTPS to cow.fi / paid RPCs), `tini` as PID 1 (forwards SIGINT
# for graceful shutdown per docs/production.md §2), and a non-root
# `shepherd` user owning `/var/lib/shepherd`.
#
# The runtime entrypoint expects `/etc/shepherd/engine.toml` to be
# mounted (read-only) — see `docker-compose.yml` and
# `docs/deployment/docker.md`.

# ------------------------------------------------------------------- chef

# Pin the Rust toolchain to 1.94 to match CI (.github/workflows/ci.yml)
# and the flake devshell exactly. The whole workspace — including the
# transitive wasmtime 46.x crates — builds and tests on 1.94; keeping the
# image, CI, and local dev on one rustc means CI validates the compiler
# the image ships and (once sccache moves to a shared backend) they can
# share one compiler cache. Bump in lockstep with CI/flake.
FROM rust:1.94-slim-bookworm AS chef

# Build deps for ring/openssl/cmake-using crates pulled in via alloy
# and cowprotocol. `clang` is for any inline-C bindings (e.g.
# pycryptodome-equivalent in the wasm side); cheap enough to bundle.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev cmake clang ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-wasip2

# cargo-chef splits the build into a deps-only layer and a thin
# workspace-crate layer (see the `build` stage).
RUN cargo install cargo-chef --locked --version 0.1.77

WORKDIR /src

# ---------------------------------------------------------------- planner

# Distil the dependency graph into a recipe. This reads only the
# Cargo.toml/Cargo.lock set, so the recipe (and therefore the cooked
# deps layer below) changes only when dependencies change — not on a
# strategy-code edit.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ------------------------------------------------------------------ build

FROM chef AS build

# Cook the dependency graph once, scoped to exactly what the real builds
# below produce: the engine's native deps and the modules' wasm32-wasip2
# deps. The `-p` scoping matters — an unscoped `cook --target wasm32-wasip2`
# would try to compile the engine's native-only deps (tokio, aws-lc-sys's C
# code) for wasm and fail. This layer is cached until Cargo.toml/Cargo.lock
# changes, so a strategy-code edit skips straight to recompiling only the
# changed workspace crate. `--locked` stays on the real builds below, which
# validate the committed Cargo.lock verbatim.
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release -p shepherd-engine --recipe-path recipe.json \
 && cargo chef cook --release --target wasm32-wasip2 \
      -p twap-monitor -p ethflow-watcher --recipe-path recipe.json

# Now the workspace sources. `.dockerignore` keeps the context lean
# (no `target/`, no `data/`, no large baseline fixtures).
# Only the workspace crates recompile here; deps come from the cooked layer.
COPY . .

# Engine binary in release. --locked ensures the committed Cargo.lock
# is used verbatim so builds are reproducible.
RUN cargo build -p shepherd-engine --release --locked

# The two production keeper modules. The wasm artefacts land under
# `target/wasm32-wasip2/release/<name_with_underscores>.wasm`. The cow
# venue is native Rust now, so it is linked into the engine binary above.
RUN cargo build -p twap-monitor     --target wasm32-wasip2 --release --locked \
 && cargo build -p ethflow-watcher  --target wasm32-wasip2 --release --locked

# ----------------------------------------------------------------- runtime

FROM debian:bookworm-slim AS runtime

# `tini` reaps zombies + forwards SIGINT/SIGTERM to the engine so the
# graceful-shutdown path actually runs (drain in-flight
# dispatch, persist `last_dispatched_block:{chain_id}` to local-store).
# `ca-certificates` is mandatory for HTTPS calls to cow.fi + paid RPC
# endpoints; the engine has no embedded TLS roots.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -s /usr/sbin/nologin -d /var/lib/shepherd shepherd \
    && install -d -o shepherd -g shepherd -m 0755 /var/lib/shepherd \
    && install -d -o root     -g root     -m 0755 /opt/shepherd \
    && install -d -o root     -g root     -m 0755 /opt/shepherd/modules \
    && install -d -o root     -g root     -m 0755 /opt/shepherd/manifests \
    && install -d -o root     -g root     -m 0755 /etc/shepherd

# Engine binary.
COPY --from=build /src/target/release/shepherd /usr/local/bin/shepherd

# Module .wasm artefacts. The Component Model wasm files are loaded
# by the engine at boot via the `[[modules]]` entries in engine.toml.
COPY --from=build /src/target/wasm32-wasip2/release/*.wasm /opt/shepherd/modules/

# Module manifests (the `component.toml` next to each guest crate). The
# engine resolves capability declarations + chain subscriptions from
# these at supervisor boot.
COPY --from=build /src/modules/twap-monitor/component.toml    /opt/shepherd/manifests/twap-monitor.toml
COPY --from=build /src/modules/ethflow-watcher/component.toml /opt/shepherd/manifests/ethflow-watcher.toml

# Drop privileges. The engine never needs root at runtime: it only
# reads /etc/shepherd/engine.toml, writes to /var/lib/shepherd, and
# binds 127.0.0.1:9100 inside the container.
USER shepherd
WORKDIR /var/lib/shepherd

# Metrics endpoint. The engine binds 127.0.0.1:9100 inside the
# container by default; docker-compose maps it to the host's
# loopback so Prometheus scrapes it via the docker network without
# exposing /metrics to the public internet.
EXPOSE 9100

# `--engine-config /etc/shepherd/engine.toml` matches the production
# guide's expected mount point. Operators override via
# `docker run ... -v /path/to/engine.toml:/etc/shepherd/engine.toml:ro`.
ENTRYPOINT ["/usr/bin/tini", "--", "shepherd"]
CMD ["--engine-config", "/etc/shepherd/engine.toml"]
