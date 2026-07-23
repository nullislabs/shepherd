# Multi-chain deployment patterns

This guide covers running Shepherd against multiple EVM chains simultaneously.
The engine dispatches each module only to the chains it subscribes to, so a
single `nexum` process can serve modules watching Mainnet, Gnosis Chain,
Arbitrum One, and Base at the same time.

---

## Chain support matrix

The table lists the chains this repo's modules target. The CoW Protocol
orderbook supports more (the `cowprotocol` crate's `Chain` enum also carries
BNB, Polygon, Avalanche, Linea, and Plasma); the same wiring pattern applies
to any of them. Shepherd can subscribe to block and log events on any EVM
chain; add only the chains your modules actually use.

| Chain | Chain ID | Orderbook slug | Barn (staging) | Notes |
|-------|----------|----------------|----------------|-------|
| Ethereum Mainnet | 1 | `mainnet` | ✓ | Primary production chain |
| Gnosis Chain | 100 | `xdai` | ✓ | Second-most-active CoW deployment |
| Base | 8453 | `base` | ✗ | |
| Arbitrum One | 42161 | `arbitrum_one` | ✓ | |
| Sepolia | 11155111 | `sepolia` | ✓ | Recommended testnet for soak runs |

The CoW Protocol orderbook is not deployed on every EVM chain. A `[chains.<id>]`
entry in `engine.toml` for a chain with no orderbook deployment will open block
and log subscriptions, but any module that calls `cow-api` will fail at runtime.

---

## `engine.toml` — per-chain RPC wiring

Add one `[chains.<id>]` table per chain. Log-subscription modules require a
WebSocket (`wss://`) transport; request-only modules can use HTTP.

```toml
[chains.1]                                   # Ethereum Mainnet
rpc_url = "${MAINNET_RPC_URL}"

[chains.100]                                 # Gnosis Chain
rpc_url = "${GNOSIS_RPC_URL}"

[chains.8453]                                # Base
rpc_url = "${BASE_RPC_URL}"

[chains.42161]                               # Arbitrum One
rpc_url = "${ARBITRUM_RPC_URL}"

[chains.11155111]                            # Sepolia (soak / staging)
rpc_url = "${SEPOLIA_RPC_URL}"
```

`${VAR}` tokens are substituted at engine boot from environment variables. A
missing variable fails fast with the exact variable name. In Docker Compose,
forward the variables from the host `.env` file:

```yaml
# docker-compose.yml (engine service environment section)
environment:
  MAINNET_RPC_URL:
  GNOSIS_RPC_URL:
  BASE_RPC_URL:
  ARBITRUM_RPC_URL:
  SEPOLIA_RPC_URL:
```

### Opt out of the WebSocket requirement

By default the engine logs an ERROR at boot when a chain is configured with an
HTTP URL because `block` and `log` subscriptions need WebSocket. If a chain is
used only for `chain::request` (poll-only modules with no `[[subscription]]`),
suppress it:

```toml
[chains.1]
rpc_url = "https://eth.llamarpc.com"
require_ws = false
```

---

## CoW Protocol orderbook URLs

The engine's `cow-api` extension resolves the orderbook URL automatically from
the chain ID using the canonical `https://api.cow.fi/<slug>/` pattern. No
extra config is needed for production.

Override individual chains to point at a staging ("barn") instance or a local
mock:

```toml
[extensions.cow.orderbook_urls]
# Point chain 11155111 at the barn (staging) orderbook:
11155111 = "https://barn.api.cow.fi/sepolia/"

# Point chain 1 at a local Wiremock during integration testing:
1 = "http://localhost:9999/"
```

**Canonical URLs by chain:**

| Chain | Production URL | Barn (staging) URL |
|-------|---------------|-------------------|
| Mainnet (1) | `https://api.cow.fi/mainnet/` | `https://barn.api.cow.fi/mainnet/` |
| Gnosis (100) | `https://api.cow.fi/xdai/` | `https://barn.api.cow.fi/xdai/` |
| Base (8453) | `https://api.cow.fi/base/` | — |
| Arbitrum One (42161) | `https://api.cow.fi/arbitrum_one/` | `https://barn.api.cow.fi/arbitrum_one/` |
| Sepolia (11155111) | `https://api.cow.fi/sepolia/` | `https://barn.api.cow.fi/sepolia/` |

---

## Contract addresses

### CREATE2-stable (identical on every chain)

These addresses are the same across all supported chains:

| Contract | Address |
|----------|---------|
| `GPv2Settlement` | `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` |
| `GPv2VaultRelayer` | `0xC92E8bdf79f0507f65a392b0ab4667716BFE0110` |
| `ComposableCoW` | `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74` |

### EthFlow

The **current** production EthFlow deployment is address-identical on every
chain CoW Protocol supports (the `cowprotocol` crate pins it as
`ETH_FLOW_PRODUCTION`, sourced from
[`cowprotocol/ethflowcontract`](https://github.com/cowprotocol/ethflowcontract)
`networks.prod.json`):

```
0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC  (all supported chains)
```

This is what `modules/ethflow-watcher/module.toml` wires for Sepolia, and the
same value carries to Mainnet, Gnosis Chain, Arbitrum One, and Base today.

Unlike ComposableCoW, however, the sameness is **not a CREATE2 guarantee** -
EthFlow has legacy per-version deployments at other addresses (e.g. the
v1.0.0 contracts), and a future version may land at a new address on a
per-chain schedule. When porting `ethflow-watcher` to a new chain or bumping
the contract version, verify the `[[subscription]] address` against
`networks.prod.json` for that chain rather than assuming the constant holds.

---

## Module manifests — the `[[subscription]]` duplication pattern

A module subscribes per chain. To watch the same event on multiple chains,
declare one `[[subscription]]` block per chain. The engine opens a separate
stream for each and routes dispatches independently.

### twap-monitor on Mainnet + Gnosis

```toml
# module.toml for twap-monitor (multi-chain)

# ComposableCoW.ConditionalOrderCreated on Mainnet
[[subscription]]
kind = "chain-log"
chain_id = 1
address = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"
event_signature = "0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361"

# New-block ticks on Mainnet (drives the poll loop)
[[subscription]]
kind = "block"
chain_id = 1

# ComposableCoW.ConditionalOrderCreated on Gnosis Chain
[[subscription]]
kind = "chain-log"
chain_id = 100
address = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"
event_signature = "0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361"

# New-block ticks on Gnosis Chain
[[subscription]]
kind = "block"
chain_id = 100
```

The module's `on_event` receives every event tagged with its `chain_id`; use
the chain ID to dispatch to the correct `chain::request` target and the correct
`cow-api` submission path.

### ethflow-watcher on Mainnet + Sepolia

```toml
# module.toml for ethflow-watcher (multi-chain)

# CoWSwapEthFlow.OrderPlacement on Mainnet
# IMPORTANT: verify this address against cowprotocol/ethflowcontract
# networks.prod.json before deploying — EthFlow is NOT CREATE2-stable.
[[subscription]]
kind = "chain-log"
chain_id = 1
address = "<MAINNET_ETHFLOW_ADDRESS>"
event_signature = "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"

# CoWSwapEthFlow.OrderPlacement on Sepolia
[[subscription]]
kind = "chain-log"
chain_id = 11155111
address = "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC"
event_signature = "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"
```

---

## Event topic reference

These are keccak256 hashes of the event signatures. They are the same on every
chain; only the contract `address` changes for EthFlow. Package of record:
`wit/shepherd-cow/cow-events.wit`.

| Event | Topic-0 |
|-------|---------|
| `ComposableCoW.ConditionalOrderCreated(address,(address,bytes32,bytes))` | `0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361` |
| `CoWSwapEthFlow.OrderPlacement(address,(address,address,address,uint256,uint256,uint32,bytes32,uint256,bytes32,bool,bytes32,bytes32),(uint8,bytes),bytes)` | `0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9` |

---

## Resource sizing (per additional chain)

Each new chain adds:

- **1 block subscription** (always-on WS stream, ~zero CPU when idle).
- **N log subscriptions**, where N = number of modules with a `chain-log`
  subscription on that chain.
- **M `eth_call`s per block** for polling modules (e.g. TWAP), where M scales
  linearly with the number of active registered orders on that chain.

RPC provider sizing: budget **≥ 25 sustained req/s per chain** (a paid
Alchemy / Infura / QuickNode tier - free tiers throttle `eth_subscribe`
under exactly this load). Dedicated endpoints per chain are preferable to
shared-rate plans when running `block` subscriptions simultaneously.

Monitor `shepherd_chain_request_total{outcome="err"}` per `chain_id` — a
sustained rate above 5% on any chain indicates RPC degradation.

---

## Full multi-chain `engine.docker.toml` example

```toml
[engine]
state_dir = "/var/lib/shepherd"
log_level  = "info"

[engine.metrics]
enabled   = true
bind_addr = "0.0.0.0:9100"

[chains.1]
rpc_url = "${MAINNET_RPC_URL}"

[chains.100]
rpc_url = "${GNOSIS_RPC_URL}"

[chains.8453]
rpc_url = "${BASE_RPC_URL}"

[chains.42161]
rpc_url = "${ARBITRUM_RPC_URL}"

[chains.11155111]
rpc_url = "${SEPOLIA_RPC_URL}"

[extensions.cow.orderbook_urls]
# Uncomment to override individual chains with barn or a local mock:
# 11155111 = "https://barn.api.cow.fi/sepolia/"

[[modules]]
path     = "/opt/shepherd/modules/twap_monitor.wasm"
manifest = "/opt/shepherd/manifests/twap-monitor.toml"

[[modules]]
path     = "/opt/shepherd/modules/ethflow_watcher.wasm"
manifest = "/opt/shepherd/manifests/ethflow-watcher.toml"
```

---

## See also

- [`docs/deployment.md`](../deployment.md) — `engine.toml` reference and
  single-module quickstart
- [`docs/production.md`](../production.md) — systemd, Docker, RPC provider
  recommendations, and alerting rules
- [`docs/deployment/docker.md`](./docker.md) — container image layout
- [`modules/twap-monitor/module.toml`](../../modules/twap-monitor/module.toml)
  — canonical subscription example
- [`modules/ethflow-watcher/module.toml`](../../modules/ethflow-watcher/module.toml)
  — EthFlow subscription with per-chain address caveat
