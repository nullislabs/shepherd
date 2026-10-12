# Deploying shepherd

This guide is for the operator of the `shepherd` engine.
It covers the parts that are specific to this repository: the binary, the shipped `engine.*.toml` files, the venue section, the two shipped modules, and the environment variables.

`shepherd` is the nexum engine with the videre venue platform and the CoW venue compiled in.
The engine-level topics are in the nexum-runtime operator handbook: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.
Read the handbook for the systemd unit, the state backup, the cursors, the logs, the metric table, the alert rules, the RPC selection, and the refusals that an upgrade meets.
The handbook names the bare binary `nexum`.
Use `shepherd` in its place: for example, `nexum digest` is `shepherd digest`.
This repository pins nexum-runtime to a git rev in each crate manifest, so read the handbook at that rev if `main` has moved.

The related guides are:

- [Docker deployment](deployment/docker.md): the image, the compose files, and the minimum configuration for a fresh server.
- [Multi-chain deployment](deployment/multi-chain.md): what one engine process can do across chains, and how to retarget a module.
- [Soak runbook](operations/soak-runbook.md): the 7-day stability run.

## What an operator runs

A deployment is one or more `shepherd` processes.
Each process reads one engine config file.
The file gives these items:

1. The local environment: the state directory, the log level, the metrics listener, and one RPC endpoint for each chain.
2. One `[[modules]]` entry for each module: the path of a `.wasm` component and the path of its `component.toml` manifest.
3. One `[extensions.videre.venues.cow]` table that selects the orderbook chain of the CoW venue.

The CoW venue is native Rust inside the `shepherd` binary.
It is not a `.wasm` file, and the engine config does not load it from a path.
The bare `nexum` binary does not contain the venue, so it cannot run the shipped modules.

The engine reads `[[modules]]` at boot only.
It pulls nothing from a registry.
To add, change, or remove a module, edit the engine config and restart the process.

## Build from source

The workspace uses a pinned Rust 1.96 toolchain and the `wasm32-wasip2` target.
The flake devshell supplies both.

```sh
nix develop
just build
```

`just build` produces these files:

| File | Content |
|---|---|
| `target/debug/shepherd` | The engine binary, as a debug build. |
| `target/wasm32-wasip2/release/ccow_monitor.wasm` | The `ccow-monitor` module. |
| `target/wasm32-wasip2/release/ethflow_watcher.wasm` | The `ethflow-watcher` module. |

For a release binary, use the command that the `Dockerfile` uses:

```sh
cargo build -p shepherd-engine --release --locked
```

The result is `target/release/shepherd`.

## Run

```sh
./target/debug/shepherd --engine-config engine.toml
```

`cargo run -p shepherd-engine -- --engine-config engine.toml` does the same from the source tree.
Without `--engine-config`, the engine reads `engine.toml` in the working directory.
If that file is absent, the engine has no module to run and stops with `NothingToRun`.

The engine writes JSON log lines to stdout.
Add `--pretty-logs` for the human-readable format.

A successful boot writes these lines in this order:

1. `shepherd starting`.
2. `opening chain RPC provider`, one for each `[chains.<id>]` table.
3. `observability listening at /metrics, /healthz and /readyz`, when the metrics listener is enabled.
4. `loading component manifest` and `init succeeded`, one pair for each module.
5. `supervisor ready`, with the fields `modules`, `alive`, `verified`, and `shutdown_drain_secs`.

Send SIGINT to stop the engine.
A clean stop writes `graceful shutdown complete`.

## Engine config

`engine.example.toml` is the annotated template.
Copy it to `engine.toml`, which git ignores, and edit the copy.
A minimal config for the two shipped modules has this shape:

```toml
[engine]
state_dir = "./data"
log_level = "info"

[engine.metrics]
enabled = true
bind_addr = "127.0.0.1:9100"

[chains.1]
rpc_url = "${MAINNET_RPC_URL}"

[chains.11155111]
rpc_url = "${SEPOLIA_RPC_URL}"

[[modules]]
id = "ccow-monitor"
path = "target/wasm32-wasip2/release/ccow_monitor.wasm"
manifest = "modules/ccow-monitor/component.toml"
digest = "sha256:<64 hex chars>"

[[modules]]
id = "ethflow-watcher"
path = "target/wasm32-wasip2/release/ethflow_watcher.wasm"
manifest = "modules/ethflow-watcher/component.toml"
digest = "sha256:<64 hex chars>"

[extensions.videre.venues.cow]
chain = 1
```

Read [Multi-chain deployment](deployment/multi-chain.md) before you use this shape in production.
The two shipped manifests target different chains, and one venue serves one chain.

### `[engine]`

| Key | Default | Meaning |
|---|---|---|
| `state_dir` | `./data` | The directory of the local store. The engine creates `local-store.redb` in it. |
| `log_level` | `info` | A `tracing` filter directive. The `RUST_LOG` environment variable overrides it. |
| `require_component_digest` | `true` | When `true`, the engine refuses a `[[modules]]` entry that has no `digest`. |
| `log_backfill_concurrency` | `16` | The number of concurrent `eth_getLogs` requests during an event backfill. |

### `[engine.metrics]`

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `false` | When `true`, one listener serves `/metrics`, `/healthz`, and `/readyz`. |
| `bind_addr` | `127.0.0.1:9100` | The socket address of that listener. |

Every metric name starts with `nexum_runtime_`.
The handbook, section 6, has the full table, and section 7 has the alert rules.

### `[chains.<id>]`

Write one table for each chain that a module trigger names.
The key is the decimal chain id.

| Key | Default | Meaning |
|---|---|---|
| `rpc_url` | none | The RPC endpoint. A `ws://` or `wss://` URL receives new blocks by subscription. An `http://` or `https://` URL polls. |
| `request_timeout_secs` | `30` | The timeout of one JSON-RPC request. |
| `max_log_range_blocks` | `1000` | The largest block range that the endpoint accepts for one `eth_getLogs` request. |

The engine refuses the boot if a trigger names a chain that has no table.
The handbook, section 8, explains how to select an endpoint and how to set `max_log_range_blocks`.

### `[[modules]]`

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | The operator name of the module. It is the key of a `[policy.component.<id>]` row. |
| `path` | yes | The path of the `.wasm` component. |
| `manifest` | no | The path of the manifest. The default is a `component.toml` file beside `path`. |
| `digest` | yes, unless `require_component_digest = false` | The `sha256:<hex>` pin of the component bytes. |

Get the pin from the binary:

```sh
./target/debug/shepherd digest target/wasm32-wasip2/release/ccow_monitor.wasm
```

The command prints one `sha256:<64 hex chars>` line.
Every shipped `engine.*.toml` file sets `require_component_digest = false`.
For a production run, set a `digest` on each entry and remove that line.

The `manifest` key also lets you run a shipped component with your own manifest.
The multi-chain guide uses it to retarget a module to a different chain.

### `[extensions.videre.venues.cow]`

This table configures the CoW venue.
`cow` is the only venue id that this binary links, and any other id refuses at load.

| Key | Required | Meaning |
|---|---|---|
| `chain` | yes | The chain of the orderbook. Write a decimal chain id or an orderbook slug such as `"sepolia"`. |
| `orderbook_url` | no | A different orderbook base URL, for a staging orderbook or a mock. |
| `owner` | no | The address that the pre-sign submit path posts as `from`. Without it, the venue refuses an unsigned order. |
| `timeout_ms` | no | The time limit of one orderbook request, in milliseconds. |

One table means one chain: the venue speaks to one orderbook for each engine process.
The venue owns its HTTP client.
The `[policy]` HTTP allowlists apply to guest modules only, so they do not limit the venue.

If the table is absent, the engine logs a warning and registers no venue.
A module that declares `[venue] body_version` in its manifest then refuses to boot.
`ccow-monitor` is such a module.

### Resource limits

`[policy]` and `[limits.*]` set the resource ceilings and the outbound HTTP limits.
`engine.example.toml` documents each key inline.
The handbook, sections 8 and 11, explains the defaults and the refusals.

### Retired keys

The engine parses the config with unknown keys denied.
These keys from earlier releases now refuse at load:

- `[[adapters]]` and `[[services]]`.
  The venue is native code, and `[extensions.videre.venues.cow]` replaces the adapter settings.
- `[limits] fuel_per_event`, `memory_bytes`, and `state_bytes`.
  The `[policy]` table replaces them.

## Environment variables

The engine replaces each `${NAME}` token in the config file with the value of the environment variable `NAME`.
A name can contain uppercase letters, digits, and underscores.
A variable that is not set stops the boot, and the error gives the name.

The engine does the replacement before it parses the TOML.
The replacement therefore applies to comments too.
Do not write a `${NAME}` token in a comment of your config file unless that variable is set.

| Variable | Used by | Purpose |
|---|---|---|
| `MAINNET_RPC_URL` | `engine.docker.toml`, `engine.example.toml`, `engine.soak.docker.toml` | The RPC endpoint of chain 1. |
| `GNOSIS_RPC_URL` | `engine.docker.toml`, `engine.example.toml` | The RPC endpoint of chain 100. |
| `SEPOLIA_RPC_URL` | `engine.docker.toml`, `engine.example.toml` | The RPC endpoint of chain 11155111. |
| `ARBITRUM_RPC_URL` | `engine.docker.toml`, `engine.example.toml` | The RPC endpoint of chain 42161. |
| `BASE_RPC_URL` | `engine.docker.toml`, `engine.example.toml` | The RPC endpoint of chain 8453. |
| `SHEPHERD_IMAGE` | `docker-compose.yml`, `docker-compose.soak.yml` | The image reference. The default is `ghcr.io/nullislabs/shepherd:latest`. |
| `SHEPHERD_ENGINE_CONFIG` | `docker-compose.yml` | The host path of the engine config that the container mounts. The default is `./engine.docker.toml`. |
| `RUST_LOG` | the engine | Overrides `[engine] log_level`. Leave it unset so that the config file is the one source. |

`.env.example` is the template for the five RPC variables.

## Shipped modules

Both modules are `wasm32-wasip2` components.
The host grants a module only the capabilities that its manifest declares under `[dependencies]`.
The manifest format is in the nexum-runtime manifest reference: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/02-modules-triggers-packaging.md>.

### `ccow-monitor`

`ccow-monitor` is a generic ComposableCoW keeper.
It indexes the conditional orders of one ComposableCoW registry, polls them when a new block arrives, and submits each order that is ready to the CoW venue.

| Item | Value in `modules/ccow-monitor/component.toml` |
|---|---|
| Capabilities | `logging`, `local-store`, `chain`, `client` |
| Chain | 1 (Ethereum Mainnet) |
| Registry | `0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232`, in `[config] registry` and in each event trigger `address` |
| Event triggers | `ConditionalOrderCreated`, `ConditionalOrderRemoved`, `MerkleRootSet` |
| Other triggers | `block` on chain 1, and `intent-status` for the venue `cow` |
| Backfill | `resume = true` and `start_block = 25674440` on each event trigger |
| Venue | `[venue] body_version = 1` |

The registry is a fork deployment.
It is not the upstream ComposableCoW contract at `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74`.
`[config] registry` must be equal to the `address` of each event trigger, because the engine filters logs by the trigger address and the module polls the configured address.

### `ethflow-watcher`

`ethflow-watcher` observes EthFlow orders.
It decodes each `OrderPlacement` log, computes the order UID, and asks the CoW venue to watch the status of that UID.
It never submits an order.

| Item | Value in `modules/ethflow-watcher/component.toml` |
|---|---|
| Capabilities | `client`, `logging`, `local-store` |
| Chain | 11155111 (Sepolia) |
| Contract | `0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC` |
| Event trigger | `OrderPlacement` |
| Other triggers | `intent-status` for the venue `cow` |
| Backfill | `resume = true` and `start_block = 7541028` |

### First boot

On the first boot, each event trigger reads the logs from its `start_block` to the chain head.
The engine fetches them in `eth_getLogs` chunks of `max_log_range_blocks` and logs the progress.
After that, `resume = true` keeps a cursor in the local store, and a restart reads only the blocks that it missed.

## Shipped configs

| File | Purpose | `[chains]` tables | Venue chain |
|---|---|---|---|
| `engine.example.toml` | The annotated template. It lists no module. | 1, 100, 11155111, 42161, 8453 | none (commented out) |
| `engine.docker.toml` | The config for the container image. | 1, 100, 11155111, 42161, 8453 | 1 |
| `engine.soak.docker.toml` | The config for the soak stack. | 1 | 1 |
| `engine.soak.toml` | The soak config for a run outside Docker. | 1 | 1 |
| `engine.m2.toml`, `engine.m3.toml` | The smoke scenarios. | 1 | 1 |
| `engine.e2e.toml`, `engine.load.toml` | The end-to-end and load scenarios. | 1 | 1 |

The engine refuses a boot when a module trigger names a chain that has no `[chains.<id>]` table.
`engine.m2.toml`, `engine.m3.toml`, `engine.e2e.toml`, `engine.load.toml`, `engine.soak.toml`, and `engine.soak.docker.toml` declare only `[chains.1]`.
Each of them loads at least one module whose manifest has a trigger on chain 11155111.
As committed, each of them therefore stops at boot with `UnconfiguredChain`.
Before you use one of them, add the missing chain table to your copy, or retarget the module with a manifest override.
The multi-chain guide shows both steps.

`engine.m3.toml`, `engine.e2e.toml`, and `engine.soak.toml` also name the `price-alert` and `balance-tracker` example modules.
Those modules are in the nexum-runtime repository, and this repository does not build them.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `Substitute(Missing { name: "MAINNET_RPC_URL" })` | The config has a `${MAINNET_RPC_URL}` token and the variable is not set. | Set the variable, or remove the table that uses it. Look in comments too. |
| `UnconfiguredChain { name: "ethflow-watcher", chain_id: 11155111, configured: {1} }` | A trigger of that module names a chain that has no `[chains.<id>]` table. | Add the table, or retarget the module with a manifest override. |
| `no registered venue declares [venue] body_versions` | The `[extensions.videre.venues.cow]` table is absent and a module needs the venue. | Add the table. |
| `venue registration refused: [extensions.videre]: unsupported chain id 999` | `chain` is not a chain that the CoW orderbook supports. | Use a supported chain. The multi-chain guide has the list. |
| `venue registration refused: [extensions.videre]: unknown field` | The table names a venue id other than `cow`, or a key that the venue does not have. | Correct the key. The keys use underscores, for example `orderbook_url`. |
| `DigestUnpinned` | A `[[modules]]` entry has no `digest` and `require_component_digest` is `true`. | Add the line that `shepherd digest <path>` prints. |
| `unknown field` with `adapters` or `services` in the message | The config has a retired table. | Remove it. See "Retired keys". |
| `NothingToRun` | The config lists no module, or the engine found no config file. | Give `--engine-config` and at least one `[[modules]]` entry. |
| `/readyz` answers `503` | No module is dispatchable. | Read the body: it gives the state of each module. Then read the handbook, section 9. |

## References

- [Docker deployment](deployment/docker.md).
- [Multi-chain deployment](deployment/multi-chain.md).
- [Soak runbook](operations/soak-runbook.md).
- The nexum-runtime operator handbook: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.
- The nexum-runtime manifest reference: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/02-modules-triggers-packaging.md>.
- `engine.example.toml`: the annotated engine config template.
