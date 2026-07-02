# syntax=docker/dockerfile:1.6
#
# Multi-stage build for `nexum` (Shepherd) - the engine binary
# plus the five production WASM modules baked into a single image.
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

# ----------------------------------------------------------------- build

# Pin the Rust toolchain to a version recent enough for the
# transitive wasmtime 45.x crates (which require rustc >= 1.93).
# Bump in lockstep with workspace Cargo.lock minimum-supported
# rustc — `cargo msrv` if uncertain.
FROM rust:1.96-slim-bookworm AS build

# Build deps for ring/openssl/cmake-using crates pulled in via alloy
# and cowprotocol. `clang` is for any inline-C bindings (e.g.
# pycryptodome-equivalent in the wasm side); cheap enough to bundle.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev cmake clang ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-wasip2

WORKDIR /src

# Copy the whole workspace. `.dockerignore` should keep the build
# context lean (no `target/`, no `data/`, no large baseline / backtest
# fixtures).
COPY . .

# Engine binary in release.
RUN cargo build -p nexum-cli --release

# Five production modules. The wasm artefacts land under
# `target/wasm32-wasip2/release/<name_with_underscores>.wasm`.
RUN cargo build -p twap-monitor     --target wasm32-wasip2 --release \
 && cargo build -p ethflow-watcher  --target wasm32-wasip2 --release \
 && cargo build -p price-alert      --target wasm32-wasip2 --release \
 && cargo build -p balance-tracker  --target wasm32-wasip2 --release \
 && cargo build -p stop-loss        --target wasm32-wasip2 --release

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
COPY --from=build /src/target/release/nexum /usr/local/bin/nexum

# Module .wasm artefacts. The Component Model wasm files are loaded
# by the engine at boot via the `[[modules]]` entries in engine.toml.
COPY --from=build /src/target/wasm32-wasip2/release/*.wasm /opt/shepherd/modules/

# Module manifests (the `module.toml` next to each cdylib crate). The
# engine resolves capability declarations + chain subscriptions from
# these at supervisor boot.
COPY --from=build /src/modules/twap-monitor/module.toml    /opt/shepherd/manifests/twap-monitor.toml
COPY --from=build /src/modules/ethflow-watcher/module.toml /opt/shepherd/manifests/ethflow-watcher.toml
COPY --from=build /src/modules/examples/price-alert/module.toml     /opt/shepherd/manifests/price-alert.toml
COPY --from=build /src/modules/examples/balance-tracker/module.toml /opt/shepherd/manifests/balance-tracker.toml
COPY --from=build /src/modules/examples/stop-loss/module.toml       /opt/shepherd/manifests/stop-loss.toml

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
ENTRYPOINT ["/usr/bin/tini", "--", "nexum"]
CMD ["--engine-config", "/etc/shepherd/engine.toml"]
