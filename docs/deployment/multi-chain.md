# Multi-chain deployment

Running Shepherd against multiple EVM chains. The engine dispatches each module only to the chains it subscribes to, so one process can serve modules watching Mainnet, Gnosis Chain, Arbitrum One, and Base at once. For the base `engine.toml` reference see [`docs/deployment.md`](../deployment.md).

Keeper modules subscribe per chain and may span chains freely. Venue adapters do not: the `cow-venue` adapter fixes its orderbook chain at `init` from its own `[config] chain`, and registers under the fixed venue id `cow` (`CowVenue::ID`), which carries no chain component. Two cow adapters in one process would register under the same id. Single-process multi-chain submission is a non-goal: run one engine process per submitting chain, each paired with the matching adapter manifest (`module.toml` for Mainnet, `module.sepolia.toml` for Sepolia).

## Chain support matrix

The chains this repo's modules target. The CoW orderbook supports more (the `cowprotocol` crate's `Chain` enum also carries BNB, Polygon, Avalanche, Linea, Plasma); the same wiring applies. A `[chains.<id>]` entry for a chain with no orderbook deployment still opens block and log subscriptions, but any submission on it fails at runtime.

| Chain | Chain ID | Orderbook slug | Barn (staging) |
|-------|----------|----------------|----------------|
| Ethereum Mainnet | 1 | `mainnet` | yes |
| Gnosis Chain | 100 | `xdai` | yes |
| Base | 8453 | `base` | no |
| Arbitrum One | 42161 | `arbitrum_one` | yes |
| Sepolia | 11155111 | `sepolia` | yes |

## Per-chain RPC wiring

One `[chains.<id>]` table per chain. `ws://`/`wss://` engage the pubsub transport (blocks push via `eth_subscribe(newHeads)`); `http://`/`https://` poll (blocks via `eth_getBlockByNumber`, logs via `eth_getLogs`). Both transports carry block and log subscriptions; there is no WebSocket requirement. Prefer `wss://` where the provider offers it, since push is lower-latency than polling.

```toml
[chains.1]                                   # Ethereum Mainnet
rpc_url = "${MAINNET_RPC_URL}"

[chains.100]                                 # Gnosis Chain
rpc_url = "${GNOSIS_RPC_URL}"

[chains.8453]                                # Base
rpc_url = "${BASE_RPC_URL}"

[chains.42161]                               # Arbitrum One
rpc_url = "${ARBITRUM_RPC_URL}"

[chains.11155111]                            # Sepolia
rpc_url = "${SEPOLIA_RPC_URL}"
```

`${VAR}` tokens are substituted at boot from the environment; a missing variable fails fast with the exact name. Under Docker Compose, forward the variables from the host `.env`:

```yaml
environment:
  MAINNET_RPC_URL:
  GNOSIS_RPC_URL:
  BASE_RPC_URL:
  ARBITRUM_RPC_URL:
  SEPOLIA_RPC_URL:
```

## Orderbook URLs

The `cow-venue` adapter resolves the orderbook base URL from its `[config] chain` using the canonical `https://api.cow.fi/<slug>/` pattern. Override it per adapter in the adapter's `module.toml` to point at a barn (staging) instance or a local mock:

```toml
# shepherd/crates/cow-venue/module.toml
[config]
chain = 11155111
orderbook-url = "https://barn.api.cow.fi/sepolia/"
```

| Chain | Production URL | Barn (staging) URL |
|-------|---------------|-------------------|
| Mainnet (1) | `https://api.cow.fi/mainnet/` | `https://barn.api.cow.fi/mainnet/` |
| Gnosis (100) | `https://api.cow.fi/xdai/` | `https://barn.api.cow.fi/xdai/` |
| Base (8453) | `https://api.cow.fi/base/` | none |
| Arbitrum One (42161) | `https://api.cow.fi/arbitrum_one/` | `https://barn.api.cow.fi/arbitrum_one/` |
| Sepolia (11155111) | `https://api.cow.fi/sepolia/` | `https://barn.api.cow.fi/sepolia/` |

## Contract addresses

CREATE2-stable, identical on every supported chain:

| Contract | Address |
|----------|---------|
| `GPv2Settlement` | `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` |
| `GPv2VaultRelayer` | `0xC92E8bdf79f0507f65a392b0ab4667716BFE0110` |
| `ComposableCoW` | `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74` |

EthFlow is not CREATE2-stable. The current production deployment is address-identical on every chain CoW supports (the `cowprotocol` crate pins it as `ETH_FLOW_PRODUCTION` from `cowprotocol/ethflowcontract` `networks.prod.json`):

```
0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC
```

Legacy per-version EthFlow deployments live at other addresses, and a future version may land per-chain. When porting `ethflow-watcher` to a new chain, verify the `[[subscription]] address` against `networks.prod.json` for that chain.

## Subscription duplication

A module subscribes per chain: to watch the same event on multiple chains, declare one `[[subscription]]` block per chain. The engine opens a separate stream for each and routes dispatches independently, tagging each event with its `chain_id`.

```toml
# twap-monitor on Mainnet + Gnosis: ComposableCoW.ConditionalOrderCreated
[[subscription]]
kind = "chain-log"
chain_id = 1
address = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"
event_signature = "0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361"

[[subscription]]
kind = "block"
chain_id = 1

[[subscription]]
kind = "chain-log"
chain_id = 100
address = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"
event_signature = "0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361"

[[subscription]]
kind = "block"
chain_id = 100
```

## Event topics

keccak256 of the event signatures, identical on every chain; only the EthFlow `address` changes. Package of record: `wit/shepherd-cow/cow-events.wit`.

| Event | Topic-0 |
|-------|---------|
| `ComposableCoW.ConditionalOrderCreated(address,(address,bytes32,bytes))` | `0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361` |
| `CoWSwapEthFlow.OrderPlacement(...)` | `0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9` |

## Resource sizing

Each new chain adds one always-on block subscription, N log subscriptions (one per module subscribing on that chain), and M `eth_call` per block for polling modules (M scales with active orders). Budget a paid RPC tier per chain; see [`docs/production.md`](../production.md) for capacity and the `shepherd_chain_request_total{outcome="err"}` degradation signal.

## See also

- [`docs/deployment.md`](../deployment.md): `engine.toml` reference and single-module quickstart.
- [`docs/production.md`](../production.md): systemd, RPC selection, alerting.
- [`docs/deployment/docker.md`](./docker.md): container image layout.
- `shepherd/modules/twap-monitor/module.toml`, `shepherd/modules/ethflow-watcher/module.toml`: canonical subscription examples.
