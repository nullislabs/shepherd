# Deploying shepherd

This guide is for the operator of the `shepherd` engine.
`shepherd` is the nexum engine with the videre venue platform and the CoW venue compiled in.

This guide covers only what is specific to this repository: the binary, the engine config, the CoW venue, and the two shipped modules.
The engine-level topics are in the nexum-runtime operator handbook: <https://github.com/nxm-rs/nexum-runtime/blob/main/docs/production.md>.
Read the handbook for the systemd unit, the state backup, the logs, the metric table, the alert rules, the RPC selection, and the upgrade refusals.
The handbook names the binary `nexum`.
Use `shepherd` in its place: for example, `nexum digest` is `shepherd digest`.

The related guides are:

- [Docker deployment](deployment/docker.md): the image and the Compose file.
- [Soak runbook](operations/soak-runbook.md): how to record a multi-day run.

## What an operator runs

A deployment is one `shepherd` process with one engine config file.
The file gives these items:

1. The local environment: the state directory, the log level, the metrics listener, and one RPC endpoint for each chain.
2. One `[[modules]]` entry for each module: the path of a `.wasm` component and the path of its manifest.
3. One `[extensions.videre.venues.cow]` table that selects the orderbook chain of the CoW venue.

The CoW venue is native code inside the `shepherd` binary.
It is not a `.wasm` file, and the engine config does not load it from a path.

The engine reads `[[modules]]` at boot only.
To add, change, or remove a module, edit the engine config and restart the process.

## Build and run from source

The workspace uses a pinned Rust 1.96 toolchain and the `wasm32-wasip2` target.
The flake devshell supplies both.

```sh
nix develop
just build
cargo build -p shepherd-engine --release --locked
```

These commands produce these files:

| File | Content |
|---|---|
| `target/release/shepherd` | The engine binary. |
| `target/wasm32-wasip2/release/ccow_monitor.wasm` | The `ccow-monitor` module. |
| `target/wasm32-wasip2/release/ethflow_watcher.wasm` | The `ethflow-watcher` module. |

Copy `engine.example.toml` to `engine.toml`, which git ignores, and edit the copy.
Then start the engine:

```sh
export MAINNET_RPC_URL=wss://<your mainnet endpoint>
./target/release/shepherd --engine-config engine.toml
```

Without `--engine-config`, the engine reads `engine.toml` in the working directory.
The engine writes JSON log lines to stdout.
Add `--pretty-logs` for the human-readable format.

The boot is complete when the log has the line `supervisor ready`.
That line has the fields `modules`, `alive`, `verified`, and `shutdown_drain_secs`.

Send SIGINT to stop the engine.
A clean stop writes the line `graceful shutdown complete`.

## Engine config

A config for the two shipped modules has this shape:

```toml
[engine]
state_dir = "./data"
log_level = "info"

[engine.metrics]
enabled = true
bind_addr = "127.0.0.1:9100"

[chains.1]
rpc_url = "${MAINNET_RPC_URL}"

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

`engine.example.toml` is the annotated template, and it also documents the `[policy]` and `[limits.*]` resource ceilings.
The engine refuses a config that has a key that it does not know.

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
| `bind_addr` | `127.0.0.1:9100` | The socket address of that listener. The listener has no authentication. |

Every metric name starts with `nexum_runtime_`.
The handbook, section 6, has the metric table, and section 7 has the alert rules.

### `[chains.<id>]`

Write one table for each chain that a module trigger names.
The key is the decimal chain id.
The engine refuses the boot if a trigger names a chain that has no table.

| Key | Default | Meaning |
|---|---|---|
| `rpc_url` | none | The RPC endpoint. A `ws://` or `wss://` URL receives new blocks by subscription. An `http://` or `https://` URL polls. |
| `request_timeout_secs` | `30` | The timeout of one JSON-RPC request. |
| `max_log_range_blocks` | `1000` | The largest block range that the endpoint accepts for one `eth_getLogs` request. |

The handbook, section 8, explains how to select an endpoint.

The engine replaces a `${NAME}` token in a value with the environment variable `NAME` when it loads the file.
Use a token for each RPC URL that holds a key, so that the key is not in the file.
A variable that is not set stops the boot, and the error gives the name.

The engine prints the full RPC URL in its log at each start.
Remove it before you give a log to another person: the soak runbook shows how.

### `[[modules]]`

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | The operator name of the module. It is the key of a `[policy.component.<id>]` table. |
| `path` | yes | The path of the `.wasm` component. |
| `manifest` | no | The path of the manifest. The default is a `component.toml` file beside `path`. |
| `digest` | yes, unless `require_component_digest = false` | The `sha256:<hex>` pin of the component bytes. |

Get the pin from the binary:

```sh
./target/release/shepherd digest target/wasm32-wasip2/release/ccow_monitor.wasm
```

The command prints one `sha256:<64 hex chars>` line.

### `[extensions.videre.venues.cow]`

This table configures the CoW venue.
`cow` is the only venue id that this binary links.

| Key | Required | Meaning |
|---|---|---|
| `chain` | yes | The chain of the orderbook: a decimal chain id, or an orderbook slug such as `"mainnet"`. |
| `orderbook_url` | no | A different orderbook base URL, for a staging orderbook or a mock. |
| `owner` | no | The address that the pre-sign submit path posts as `from`. |
| `timeout_ms` | no | The time limit of one orderbook request, in milliseconds. |

If the table is absent, the engine registers no venue.
A module that declares `[venue] body_version` in its manifest then refuses to boot.
`ccow-monitor` is such a module.

## Shipped modules

Both modules are `wasm32-wasip2` components, and both manifests target Ethereum Mainnet (chain 1).
The host grants a module only the capabilities that its manifest declares under `[dependencies]`.
The manifest format is in the nexum-runtime manifest reference: <https://github.com/nxm-rs/nexum-runtime/blob/main/docs/02-modules-triggers-packaging.md>.

### `ccow-monitor`

`ccow-monitor` is a ComposableCoW keeper.
It indexes the conditional orders of one ComposableCoW registry, polls them when a new block arrives, and submits each order that is ready to the CoW venue.
With the venue on chain 1, it submits to the production mainnet orderbook.

| Item | Value in `modules/ccow-monitor/component.toml` |
|---|---|
| Capabilities | `logging`, `local-store`, `chain`, `client` |
| Registry | `0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232`, in `[config] registry` and in each event trigger `address` |
| Event triggers | `ConditionalOrderCreated`, `ConditionalOrderRemoved`, `MerkleRootSet` |
| Other triggers | `block` on chain 1, and `intent-status` for the venue `cow` |
| First boot | Each event trigger reads the logs from block 25674440, the deployment block of the registry. |

The registry is a fork deployment.
It is not the upstream ComposableCoW contract at `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74`, and the module does not watch orders that are registered there.

### `ethflow-watcher`

`ethflow-watcher` observes EthFlow orders.
It decodes each `OrderPlacement` log, computes the order UID, and asks the CoW venue to watch the status of that UID.
It never submits an order.

| Item | Value in `modules/ethflow-watcher/component.toml` |
|---|---|
| Capabilities | `client`, `logging`, `local-store` |
| Contract | `0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC` |
| Event trigger | `OrderPlacement` |
| Other triggers | `intent-status` for the venue `cow` |
| First boot | The trigger starts at the chain head and reads no history. |

Both modules keep a cursor in the local store for each event trigger.
After a restart, a trigger reads the blocks that it missed.
The endpoint must still have the logs of those blocks: a node that prunes old logs returns an empty answer for them, not an error.

## Multi-chain deployment

A trigger names its chain in the manifest of its module.
The engine config has one `[chains.<id>]` table for each chain that a trigger or a chain read names.
There are two patterns:

| Pattern | Use it when |
|---|---|
| One engine process for each chain | A module uses the CoW venue. This is the pattern for the shipped modules. |
| One engine process for many chains | No module uses the CoW venue, or each venue call is for the venue chain. |

The CoW venue is the reason for the first pattern: it serves one chain in one process.
The engine does not compare the venue chain with the chains that the modules read, so you must keep them equal.
A test in this repository holds the shipped files to that rule.

### Supported chains

The venue accepts these chains, from the `cowprotocol` crate that this repository pins:

| Chain | Chain id | Slug |
|---|---|---|
| Ethereum Mainnet | 1 | `mainnet` |
| BNB Smart Chain | 56 | `bnb` |
| Gnosis Chain | 100 | `xdai` |
| Polygon | 137 | `polygon` |
| Base | 8453 | `base` |
| Plasma | 9745 | `plasma` |
| Arbitrum One | 42161 | `arbitrum_one` |
| Avalanche | 43114 | `avalanche` |
| Linea | 59144 | `linea` |
| Sepolia | 11155111 | `sepolia` |

### One process for each chain

Give each process its own config, its own venue `chain`, its own `state_dir`, and its own metrics `bind_addr`.
To run a shipped module on a different chain, copy its manifest, change the chain in the copy, and give the `[[modules]]` entry the path of the copy.

This example runs `ethflow-watcher` on Gnosis Chain from a source build.
The `cowprotocol` crate records the same EthFlow contract address for each chain, so only `chain_id` changes:

```bash
sed 's/^chain_id = 1$/chain_id = 100/' modules/ethflow-watcher/component.toml > ethflow-gnosis.toml
```

```toml
[engine]
state_dir = "./data-gnosis"
require_component_digest = false

[engine.metrics]
enabled = true
bind_addr = "127.0.0.1:9101"

[chains.100]
rpc_url = "${GNOSIS_RPC_URL}"

[[modules]]
id = "ethflow-watcher"
path = "target/wasm32-wasip2/release/ethflow_watcher.wasm"
manifest = "ethflow-gnosis.toml"

[extensions.videre.venues.cow]
chain = 100
```

The example loads the module without a digest.
For production, give the `[[modules]]` entry a `digest` and remove the `require_component_digest` line.

`ccow-monitor` needs the ComposableCoW registry of the chain.
In its manifest copy, change the `registry` key of `[config]`, and the `chain_id`, the `address`, and the `start_block` of each trigger.
This repository records a registry for Ethereum Mainnet only.

Under Docker Compose, a second chain needs a second Compose project.
Its override file must mount the manifest copy and forward the RPC variable of that chain.

### One process for many chains

Declare one table for each chain:

```toml
[chains.1]
rpc_url = "${MAINNET_RPC_URL}"

[chains.100]
rpc_url = "${GNOSIS_RPC_URL}"

[chains.8453]
rpc_url = "${BASE_RPC_URL}"

[chains.42161]
rpc_url = "${ARBITRUM_RPC_URL}"
```

The engine sends each trigger to the modules that name its chain.
The boot stops if a trigger names a chain that has no table.
Do not use this pattern for a module that sends venue calls for more than one chain.

### What is tested

- The automated tests of this repository run the Ethereum Mainnet layout.
  They also make sure that one config can declare Ethereum Mainnet, Gnosis Chain, Base, and Arbitrum One, and that the venue accepts each of them.
- The Gnosis Chain example above was started one time by hand.
  The module became `alive` and its event trigger opened at the chain head.
- The nexum-runtime tests cover one process that sends triggers to modules on different chains.
- No run of many days was made on a chain other than Ethereum Mainnet.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `Substitute(Missing { name: "MAINNET_RPC_URL" })` | The config has a `${MAINNET_RPC_URL}` token and the variable is not set. | Set the variable. |
| `UnconfiguredChain` | A trigger of the named module reads a chain that has no `[chains.<id>]` table. | Add the table. |
| `no registered venue declares [venue] body_versions` | The `[extensions.videre.venues.cow]` table is absent and a module needs the venue. | Add the table. |
| `venue registration refused` with `unsupported chain` | `chain` is not in the table of "Supported chains". | Use a supported chain. |
| `venue registration refused` with `unknown field` | The table names a venue id other than `cow`, or a key that the venue does not have. | Correct the key. |
| `DigestUnpinned` | A `[[modules]]` entry has no `digest` and `require_component_digest` is `true`. | Add the line that `shepherd digest <path>` prints. |
| `NothingToRun` | The config lists no module, or the engine found no config file. | Give `--engine-config` and at least one `[[modules]]` entry. |
| `/readyz` answers `503` | No module is dispatchable. | Read the body: it gives the state of each module. Then read the handbook, section 9. |
