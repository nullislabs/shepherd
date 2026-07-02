# Deploying Shepherd

This guide covers the **operator** side - running a `nexum`
instance against a fleet of WASM modules. For module-author topics
(building a module from scratch, writing tests, packaging) see the
[SDK overview](./sdk.md) and the [first-module
tutorial](./tutorial-first-module.md).

## What an operator runs

A Shepherd deployment is one or more `nexum` processes, each
pointed at:

1. an `engine.toml` describing the local environment (chain RPCs,
   resource caps, where state lives);
2. one or more `[[modules]]` entries listing `.wasm` artefacts and
   their `module.toml` manifests;
3. a `state_dir` the engine creates / owns (the redb local-store
   database).

Modules are statically declared in `engine.toml`. The engine does
not pull them from a registry today; you ship the `.wasm` files
alongside the binary and reference them by path.

## `engine.toml` reference

```toml
[engine]
# Directory the local-store redb file (and future engine artefacts)
# will be created under. Created automatically at boot.
state_dir = "./data"

# `tracing_subscriber::EnvFilter`-compatible directive. `RUST_LOG`
# overrides at process start.
log_level = "info"

# Resource caps applied to every module store at instantiation.
# wasmtime traps a module that overruns either; the supervisor then
# logs and continues on the next event.
[engine.limits]
# Fuel budget granted before every `on_event` invocation.
# 1 unit ~ 1 wasm instruction. 1 billion ~ ~1 second of pure compute.
fuel_per_event = 1_000_000_000
# Linear-memory ceiling per module, in bytes. Default 64 MiB.
memory_bytes = 67_108_864

# One [chains.<id>] table per chain the engine should be able to
# reach. Chain ids are EVM decimal.
#
#   ws:// + wss:// — alloy pubsub transport (REQUIRED for the
#                    eth_subscribe-backed [[subscription]] kinds:
#                    `block`, `log`).
#   http:// + https:// — HTTP transport; request/response only,
#                        no subscriptions.
#
# Mix and match: a chain used only for eth_call (e.g. a Chainlink
# oracle module) can be HTTP; chains carrying log subscriptions
# need WebSocket.

[chains.1]
rpc_url = "https://ethereum-rpc.publicnode.com"

[chains.100]
rpc_url = "https://rpc.gnosischain.com"

[chains.11155111]
rpc_url = "wss://ethereum-sepolia-rpc.publicnode.com"

[chains.42161]
rpc_url = "https://arb1.arbitrum.io/rpc"
```

### `[[modules]]` entries

> 0.2 takes the module path + manifest as positional CLI args (a
> single module per engine process). The multi-module
> `[[modules]]` array is shipped by the supervisor work in nullislabs/shepherd PR #9.

Once the supervisor PR lands, the syntax is:

```toml
[[modules]]
name = "twap-monitor"
wasm = "modules/twap-monitor.wasm"
manifest = "modules/twap-monitor/module.toml"

[[modules]]
name = "ethflow-watcher"
wasm = "modules/ethflow-watcher.wasm"
manifest = "modules/ethflow-watcher/module.toml"
```

## Building module `.wasm` artefacts

Modules compile to the `wasm32-wasip2` target. Add the target once
per dev machine:

```sh
rustup target add wasm32-wasip2
```

Then build release artefacts from the workspace root:

```sh
cargo build --target wasm32-wasip2 --release \
  -p twap-monitor -p ethflow-watcher
```

The `.wasm` files land in
`target/wasm32-wasip2/release/{twap_monitor,ethflow_watcher}.wasm`.
Copy them to wherever your `engine.toml` points (typical:
`./modules/` next to the binary).

Size sanity check after a build (CI guards regression):

```sh
ls -lh target/wasm32-wasip2/release/*.wasm
```

The M2 modules sit at 270–310 KB optimised. Sudden +10× growth
usually means a fresh dependency landed in the wasm graph — review
`cargo tree -p <module> --target wasm32-wasip2` to confirm.

## Single-binary local runs

The 0.2 engine ships as the `nexum` binary. From the
workspace root, dispatch a module against a test event:

```sh
cargo run -p nexum-cli -- \
  target/wasm32-wasip2/release/twap_monitor.wasm \
  modules/twap-monitor/module.toml
```

On a fresh checkout, the engine creates `./data/local-store.redb`,
opens RPC providers for the chains in `engine.toml`, loads the
component, calls `init`, and dispatches a synthetic block event.
Console output is `tracing` JSON (or pretty if you set
`RUST_LOG=info,nexum_runtime=debug`).

For systemd-style production runs, see `docs/production.md`.

## Docker

A reference Dockerfile + Compose file is planned for M5.
Until that lands, build manually:

```dockerfile
# (sketch — full Dockerfile is planned for M5)
FROM rust:1.91 as build
COPY . /src
WORKDIR /src
RUN cargo build --release -p nexum-cli
RUN rustup target add wasm32-wasip2 \
 && cargo build --target wasm32-wasip2 --release \
      -p twap-monitor -p ethflow-watcher

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/nexum /usr/local/bin/
COPY --from=build /src/target/wasm32-wasip2/release/*.wasm /modules/
COPY engine.toml /etc/shepherd/engine.toml
ENTRYPOINT ["/usr/local/bin/nexum"]
```

Mount the `state_dir` as a volume so the redb file survives container
restarts.

## Observability

### Logs

Every host backend logs through `tracing`. Set `RUST_LOG` to filter:

```sh
RUST_LOG=info,nexum_runtime=debug,nexum_runtime::host::cow_orderbook=trace \
  cargo run -p nexum-cli -- ...
```

Recommended baseline for production:

```
RUST_LOG=info,nexum_runtime::host=debug
```

The structured-logging audit consolidates the field set
across every dispatch / state change / submission path so a single
JSON grep reconstructs each order's timeline.

### Prometheus metrics

A planned metrics exporter wires a `metrics-exporter-prometheus` endpoint at
`engine.toml::[engine.metrics].bind_addr` (default
`127.0.0.1:9100`). Once it lands, scrape with:

```yaml
scrape_configs:
  - job_name: shepherd
    static_configs:
      - targets: ['shepherd-host:9100']
```

Suggested Grafana panels (dashboard JSON planned):

- Module uptime — `shepherd_module_uptime_seconds{module}`
- Event latency p50 / p95 / p99 —
  `shepherd_event_latency_seconds{module}`
- Submit success rate —
  `rate(shepherd_cow_api_submit_total{outcome="success"}[5m])`
  /
  `rate(shepherd_cow_api_submit_total[5m])`
- Fuel headroom —
  `1 - (shepherd_fuel_consumed / 1_000_000_000)`
- Memory pressure —
  `shepherd_memory_peak_bytes / 67_108_864`

## Backups

`state_dir/local-store.redb` is the only durable state the engine
holds. redb's WAL means a file-level snapshot taken while the
engine is running is consistent; for safety, either:

- Pause the engine (`systemctl stop shepherd`), copy the file, then
  restart. Sub-second downtime on a small store.
- Use `redb::Database::backup` from a sidecar.

The store is per-module-namespaced (32-byte keccak prefix per
`module.name`), so a fresh deployment can re-import partial backups
without cross-module bleed.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `init failed: unsupported` | Module imports a capability that needs a chain RPC not configured. | Add the missing `[chains.<id>]` entry to `engine.toml`. |
| `unknown chain ... (no engine.toml RPC entry)` | Module dispatched `chain::request` for a chain not in `engine.toml`. | Same — add the chain. |
| `OutOfFuel` trap, immediate restart loop | Module's `on_event` exceeds `[engine.limits].fuel_per_event`. | Bump `fuel_per_event`, or audit the module's loop bounds. |
| `MemoryOutOfBounds` trap | Module's linear-memory growth exceeds `[engine.limits].memory_bytes`. | Bump `memory_bytes`; profile the module for runaway allocations. |
| `submit failed (... InvalidAppData)` | Module sent an `OrderCreation` with a non-empty app-data hash but `app_data = "{}"`. | Out of M2 scope — modules currently only support `EMPTY_APP_DATA_JSON`. Patch is on the M3 follow-up board. |

## Reference

- [SDK overview](./sdk.md)
- [First-module tutorial](./tutorial-first-module.md)
- ADR-0001 (`docs/adr/0001-engine-toml-separate-from-nexum-toml.md`)
  — why `engine.toml` and `module.toml` are split.
- ADR-0003 (`docs/adr/0003-local-store-namespacing.md`) — how the
  `state_dir/local-store.redb` file partitions across modules.
- ADR-0005 (`docs/adr/0005-cow-api-via-cached-orderbookapi.md`) —
  how the `cow-api` host backend caches per-chain `OrderBookApi`
  clients.
