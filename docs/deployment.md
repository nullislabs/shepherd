# Deploying Shepherd

The operator side: the `engine.toml` reference, building module artefacts, and local runs. For the production deploy (systemd, backup, observability) see [`docs/production.md`](./production.md); for containers see [`docs/deployment/docker.md`](deployment/docker.md); for multiple chains see [`docs/deployment/multi-chain.md`](deployment/multi-chain.md). Module-author topics are in the [SDK overview](./sdk.md) and the [first-module tutorial](./tutorial-first-module.md).

## What an operator runs

A deployment is one or more engine processes, each pointed at:

1. an `engine.toml` describing the local environment (chain RPCs, resource caps, state directory);
2. `[[modules]]` entries listing `.wasm` artefacts and their `module.toml` manifests;
3. `[[adapters]]` entries listing venue-adapter components (the bundled `cow-venue` adapter);
4. a `state_dir` the engine creates and owns (the redb local-store).

CoW order submission needs the venue platform, so run the `shepherd` binary, not the bare `nexum`. Modules and adapters are declared statically; the engine does not pull them from a registry today.

## `engine.toml` reference

`engine.example.toml` at the repo root is the annotated template. The shape:

```toml
[engine]
# Local-store redb directory, created at boot.
state_dir = "./data"
# `tracing_subscriber::EnvFilter` directive; `RUST_LOG` overrides at start.
log_level = "info"

[engine.metrics]
# Prometheus exporter. Disabled unless enabled = true (bare `nexum` never binds it).
enabled = true
bind_addr = "127.0.0.1:9100"

# Per-module wasmtime resource caps. Every field is optional; omitted
# values resolve to built-in defaults. Applies uniformly to every module.
[limits]
fuel_per_event      = 1_000_000_000   # ~1s of pure compute; wasmtime traps on exhaustion
event_deadline_secs = 120             # wall-clock backstop for unmetered host-call time (min 1)
memory_bytes        = 67_108_864      # 64 MiB linear-memory ceiling
state_bytes         = 52_428_800      # 50 MiB local-store quota

# One [chains.<id>] per chain, keyed by EVM decimal id. `ws://`/`wss://`
# engage the pubsub transport (blocks push via eth_subscribe); `http://`/
# `https://` poll (blocks via eth_getBlockByNumber, logs via eth_getLogs).
# Both work; prefer wss:// where the provider offers it.
[chains.1]
rpc_url = "https://ethereum-rpc.publicnode.com"
# request_timeout_secs = 30   # per-request JSON-RPC timeout (default 30, 0 rejected at boot)

[chains.11155111]
rpc_url = "wss://ethereum-sepolia-rpc.publicnode.com"
```

The full `[limits.*]` subtables (`http`, `chain`, `logs`, `poison`, `dispatch`, `quota`, `watch`) are documented inline in `engine.example.toml`.

### `[[modules]]` and `[[adapters]]`

```toml
[[modules]]
path     = "modules/twap-monitor.wasm"
manifest = "shepherd/modules/twap-monitor/module.toml"   # defaults to module.toml beside `path`

[[adapters]]
path       = "target/wasm32-wasip2/release/cow_venue.wasm"
manifest   = "shepherd/crates/cow-venue/module.toml"
http_allow = ["api.cow.fi"]   # outbound wasi:http allowlist the operator grants
```

The orderbook base URL is not an engine setting: the `cow-venue` adapter reads it from its own `module.toml` `[config]` (`chain`, and optional `orderbook-url` to point at a barn or mock). See [`docs/deployment/multi-chain.md`](deployment/multi-chain.md).

## Building module `.wasm` artefacts

Modules compile to `wasm32-wasip2`. Add the target once:

```sh
rustup target add wasm32-wasip2
```

Build release artefacts from the workspace root:

```sh
cargo build --target wasm32-wasip2 --release \
  -p twap-monitor -p ethflow-watcher
cargo build --target wasm32-wasip2 --release -p cow-venue --features adapter
```

Artefacts land in `target/wasm32-wasip2/release/*.wasm`. Copy them to wherever `engine.toml` points. CI guards a size regression:

```sh
ls -lh target/wasm32-wasip2/release/*.wasm
```

## Local runs

Build and run the `shepherd` binary against an `engine.toml`:

```sh
cargo run -p shepherd -- --engine-config engine.toml
```

The single-module shortcut takes positional paths and synthesizes a one-module config:

```sh
cargo run -p shepherd -- \
  target/wasm32-wasip2/release/twap_monitor.wasm \
  shepherd/modules/twap-monitor/module.toml
```

At boot the engine creates `state_dir/local-store.redb`, opens RPC providers, loads components, calls `init`, and begins dispatching. Console output is `tracing` JSON, or pretty with `--pretty-logs`. For systemd runs see [`docs/production.md`](./production.md).

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `init failed: unsupported` | Module needs a chain RPC not configured. | Add the missing `[chains.<id>]` entry. |
| `unknown chain ... (no engine.toml RPC entry)` | `chain::request` for a chain not in `engine.toml`. | Add the chain. |
| `OutOfFuel` trap, restart loop | `on_event` exceeds `[limits] fuel_per_event`. | Bump `fuel_per_event`, or audit the module's loop bounds. |
| `MemoryOutOfBounds` trap | Linear-memory growth exceeds `[limits] memory_bytes`. | Bump `memory_bytes`; profile the module. |
| dispatch exceeded its wall-clock deadline, module marked dead | A host call blocked past `[limits] event_deadline_secs`. | Tighten the module's host-call timeouts, or bump `event_deadline_secs`. |

## Reference

- [SDK overview](./sdk.md)
- [First-module tutorial](./tutorial-first-module.md)
- ADR-0001 (`docs/adr/0001-engine-toml-separate-from-nexum-toml.md`): why `engine.toml` and `module.toml` are split.
- ADR-0003 (`docs/adr/0003-local-store-namespacing.md`): how `state_dir/local-store.redb` partitions across modules.
