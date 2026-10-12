# Multi-chain deployment

This guide tells you what one `shepherd` process can do across chains, what it cannot do, and how to set up each case.
The engine config reference is in [Deploying shepherd](../deployment.md).
The container setup is in [Docker deployment](docker.md).

## Summary

- The engine is multi-chain in one process.
  Each module trigger names its own `chain_id`, and one process can serve triggers on many chains.
- The CoW venue is single-chain in one process.
  One engine process has one `[extensions.videre.venues.cow]` table, and that table has one `chain`.
- Order submission on more than one chain therefore needs one engine process for each chain that submits.
- The shipped manifests target two different chains: `ccow-monitor` targets Ethereum Mainnet, and `ethflow-watcher` targets Sepolia.
- The engine does not compare the venue chain with the trigger chains of a module.
  A mismatch boots without an error, so you must make the config consistent.

## What the engine does across chains

Each `[[trigger]]` table in a module manifest names a `chain_id`.
The engine config has one `[chains.<id>]` table for each chain, with the RPC endpoint of that chain.

```toml
[chains.1]                                   # Ethereum Mainnet
rpc_url = "${MAINNET_RPC_URL}"

[chains.100]                                 # Gnosis Chain
rpc_url = "${GNOSIS_RPC_URL}"

[chains.11155111]                            # Sepolia
rpc_url = "${SEPOLIA_RPC_URL}"

[chains.42161]                               # Arbitrum One
rpc_url = "${ARBITRUM_RPC_URL}"

[chains.8453]                                # Base
rpc_url = "${BASE_RPC_URL}"
```

`engine.docker.toml` and `engine.example.toml` list these five chains.
The engine replaces each `${NAME}` token with the value of the environment variable `NAME` when it loads the file.

These rules apply:

- If a trigger names a chain that has no `[chains.<id>]` table, the engine refuses the boot with `UnconfiguredChain`.
  The error gives the module, the chain id, and the chains that are configured.
- If a `[chains.<id>]` table uses a variable that is not set, the engine refuses the boot and names the variable.
  Remove each table that no trigger uses, so that you supply only the endpoints that you need.
- The engine opens one block stream for each chain that has a `block` trigger, and it shares that stream between the modules.
- The engine opens one log stream for each `event` trigger.
- A `ws://` or `wss://` endpoint receives new blocks by subscription.
  An `http://` or `https://` endpoint polls.
  Both transports serve block triggers and event triggers.

## What the CoW venue does across chains

The venue is the part of `shepherd` that speaks to a CoW orderbook.
`crates/shepherd-engine/src/venues.rs` reads its settings from one table:

```toml
[extensions.videre.venues.cow]
chain = 1
```

The table key is the venue id, `cow`, and the id has no chain part.
The table has one `chain` value, and the venue instance uses the orderbook of that chain for each request.
One process cannot hold a second `cow` venue for a second chain.

A module does not select the chain of the venue.
`ccow-monitor` and `ethflow-watcher` each send requests to the venue `cow`, and the venue uses its one orderbook.

The venue derives the orderbook URL from `chain`:

| Chain | Chain id | Orderbook slug | Production URL | Staging URL |
|---|---|---|---|---|
| Ethereum Mainnet | 1 | `mainnet` | `https://api.cow.fi/mainnet/` | `https://barn.api.cow.fi/mainnet/` |
| BNB Smart Chain | 56 | `bnb` | `https://api.cow.fi/bnb/` | none |
| Gnosis Chain | 100 | `xdai` | `https://api.cow.fi/xdai/` | `https://barn.api.cow.fi/xdai/` |
| Polygon | 137 | `polygon` | `https://api.cow.fi/polygon/` | none |
| Base | 8453 | `base` | `https://api.cow.fi/base/` | none |
| Plasma | 9745 | `plasma` | `https://api.cow.fi/plasma/` | none |
| Arbitrum One | 42161 | `arbitrum_one` | `https://api.cow.fi/arbitrum_one/` | `https://barn.api.cow.fi/arbitrum_one/` |
| Avalanche | 43114 | `avalanche` | `https://api.cow.fi/avalanche/` | none |
| Linea | 59144 | `linea` | `https://api.cow.fi/linea/` | none |
| Sepolia | 11155111 | `sepolia` | `https://api.cow.fi/sepolia/` | `https://barn.api.cow.fi/sepolia/` |

`chain` accepts the chain id or the slug.
A value that is not in this table refuses at load with `unsupported chain`.
The list is the `Chain` type of the `cowprotocol` crate that this repository pins.

The venue uses the production URL unless you set `orderbook_url`.
For the staging orderbook, give its URL:

```toml
[extensions.videre.venues.cow]
chain = 11155111
orderbook_url = "https://barn.api.cow.fi/sepolia/"
```

## What the shipped files do today

| Item | Chain | Source |
|---|---|---|
| `ccow-monitor` triggers | 1 | `modules/ccow-monitor/component.toml` |
| `ethflow-watcher` trigger | 11155111 | `modules/ethflow-watcher/component.toml` |
| Venue in `engine.docker.toml` | 1 | `[extensions.videre.venues.cow] chain = 1` |

With `engine.docker.toml` as committed, one process runs both modules against a mainnet venue:

- `ccow-monitor` is consistent.
  It reads its registry on Ethereum Mainnet and submits to the mainnet orderbook.
- `ethflow-watcher` is not consistent.
  It reads `OrderPlacement` logs on Sepolia and computes each order UID for Sepolia.
  It then asks the venue to watch the UID, and the venue asks the mainnet orderbook.
  The mainnet orderbook does not hold Sepolia orders, so the watch cannot follow the order.

`ethflow-watcher` never submits an order, so this mismatch places no order on a wrong chain.

Six other shipped `engine.*.toml` files declare only `[chains.1]`, and they do not boot with the shipped manifests.
"Shipped configs" in [Deploying shepherd](../deployment.md) lists them.

## Consistent layouts

Use one of these layouts.

### One engine on Ethereum Mainnet

- Set the venue `chain = 1`.
- Keep `ccow-monitor` as shipped.
- Retarget `ethflow-watcher` to chain 1 with a manifest override, or remove its `[[modules]]` entry.
- Declare `[chains.1]` only.

### One engine on Sepolia

- Set the venue `chain = 11155111`.
- Keep `ethflow-watcher` as shipped.
- Remove the `ccow-monitor` entry, or retarget it to a ComposableCoW registry on Sepolia with a manifest override.
  This repository records the registry address and the deployment block for Ethereum Mainnet only.
- Declare `[chains.11155111]` only.

### Order submission on more than one chain

Run one engine process for each chain that submits orders.
Give each process its own engine config, its own venue `chain`, its own state directory, and its own metrics address.
Load in each process only the modules that target the chain of that process.
"One process for each chain with Docker Compose" below shows the Compose setup.

## Retarget a module with a manifest override

A `[[modules]]` entry has a `manifest` key.
Use it to run a shipped component with a manifest that you own, in which each trigger names the chain that you want.

1. Copy the shipped manifest.

   From a clone, copy `modules/ethflow-watcher/component.toml`.
   From the image, use this command:

   ```bash
   mkdir -p manifests
   docker run --rm --entrypoint cat ghcr.io/nullislabs/shepherd:latest \
     /opt/shepherd/manifests/ethflow-watcher.toml > manifests/ethflow-watcher.mainnet.toml
   ```

2. Edit the copy.
   Change the values that belong to the chain.
   For `ethflow-watcher` on Ethereum Mainnet, the event trigger becomes:

   ```toml
   [[trigger]]
   on = "event"
   chain_id = 1
   address = "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC"
   event_signature = "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"
   resume = true
   start_block = <the block that created this address on chain 1>
   ```

   The `start_block` line above is a placeholder, and the file does not parse until you replace it with a number.
   Get the number from the creation transaction of the contract on the target chain.

3. Point the `[[modules]]` entry at the copy, and declare the chain.

   ```toml
   [chains.1]
   rpc_url = "${MAINNET_RPC_URL}"

   [[modules]]
   id = "ethflow-watcher"
   path = "/opt/shepherd/modules/ethflow_watcher.wasm"
   manifest = "/etc/shepherd/manifests/ethflow-watcher.mainnet.toml"

   [extensions.videre.venues.cow]
   chain = 1
   ```

4. With Docker Compose, mount the directory of the copy.
   `docker-compose.yml` mounts only the engine config, so add a `docker-compose.override.yml` file beside it:

   ```yaml
   services:
     shepherd:
       volumes:
         - ./manifests:/etc/shepherd/manifests:ro
   ```

5. Start the engine on an empty state directory.
   The module keeps its state in a namespace that the manifest selects, so the state of the earlier chain is still there after a retarget.
   With Docker Compose, `docker compose down -v` deletes the state volume.

These rules apply to each manifest that you edit:

- Change `chain_id` on each trigger of the module, not only on one.
  A trigger without a `chain_id`, such as `intent-status`, has nothing to change.
- `address` and `start_block` belong together.
  `start_block` is the deployment block of that `address` on that chain.
  A value that is too high makes the first boot miss the earlier orders.
- `start_block` needs `resume = true` on the same trigger.
- For `ccow-monitor`, set `[config] registry` to the same address as the `address` of each event trigger.
  The engine filters logs by the trigger address, and the module polls the configured registry.
- Set the venue `chain` to the chain of each module that uses the venue.
- The EthFlow contract has the same production address on the chains that the `cowprotocol` crate supports today.
  That can change for a new deployment, so confirm the address for your chain.

## One process for each chain with Docker Compose

`docker-compose.yml` sets a fixed container name and a fixed host port.
To run a second engine on the same host, use a second directory and change both.

Make one directory for each chain.
Put a copy of `docker-compose.yml`, an engine config, and a `.env` file in each directory.
Compose names the project after the directory, so each directory gets its own state volume and its own network.

In the second directory, add this `docker-compose.override.yml` file:

```yaml
services:
  shepherd:
    container_name: shepherd-sepolia
    ports: !override
      - "127.0.0.1:9101:9100"
```

`!override` replaces the port list of `docker-compose.yml`.
Without it, Compose adds the new port to the list and the second engine still asks for port 9100.

The engine config of the second directory, for a Sepolia engine that runs `ethflow-watcher`:

```toml
[engine]
state_dir = "/var/lib/shepherd"
require_component_digest = false

[engine.metrics]
enabled = true
bind_addr = "0.0.0.0:9100"

[chains.11155111]
rpc_url = "${SEPOLIA_RPC_URL}"

[[modules]]
id = "ethflow-watcher"
path = "/opt/shepherd/modules/ethflow_watcher.wasm"
manifest = "/opt/shepherd/manifests/ethflow-watcher.toml"

[extensions.videre.venues.cow]
chain = 11155111
```

Set `SHEPHERD_ENGINE_CONFIG` in the `.env` file of each directory to the name of that config.
Start each directory with `docker compose up -d --no-build`.
The second engine then answers on `http://127.0.0.1:9101/readyz`.

The `observability` profile also has a fixed container name and a fixed host port.
Run it in one directory only, or change both in the same override file.

## Contract addresses

| Contract | Address | Chains |
|---|---|---|
| `GPv2Settlement` | `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` | The same on each chain in the table above. |
| `GPv2VaultRelayer` | `0xC92E8bdf79f0507f65a392b0ab4667716BFE0110` | The same on each chain in the table above. |
| ComposableCoW registry that `ccow-monitor` uses | `0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232` | Ethereum Mainnet, deployed at block 25674440. |
| Upstream `ComposableCoW` | `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74` | Not used by the shipped manifest. |
| `CoWSwapEthFlow`, production | `0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC` | Sepolia deployment at block 7541028 in the shipped manifest. |
| `CoWSwapEthFlow`, staging | `0x04501b9b1D52e67f6862d157E00D13419D2D6E95` | For the staging orderbook. |

The registry of `ccow-monitor` is a fork deployment with its own address and its own deployment block.
It is not the upstream contract.

## Event topics

A topic is the keccak256 hash of the event signature, so it is the same on each chain.
Only `chain_id`, `address`, and `start_block` change when you retarget a trigger.

| Event | Topic 0 | Module |
|---|---|---|
| `ConditionalOrderCreated(address,(address,bytes32,bytes),bytes)` | `0x681609694274e6398a06f0051ec46e3ef05ed684d33a23297b5ebda248fcd9a1` | `ccow-monitor` |
| `ConditionalOrderRemoved(address,bytes32)` | `0x67e0f2b23e842ce65d7edff49765689c9c0931f911fc5971d09bb598cc1af4a9` | `ccow-monitor` |
| `MerkleRootSet(address,bytes32,(string[],bytes32[]),bytes)` | `0xaa8df5268339649ef2752ea240168c52869c3ab59c8c64078df854916f1b86d5` | `ccow-monitor` |
| `OrderPlacement(address,(address,address,address,uint256,uint256,uint32,bytes32,uint256,bytes32,bool,bytes32,bytes32),(uint8,bytes),bytes)` | `0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9` | `ethflow-watcher` |

## RPC load for each chain

Each chain that you add brings this load:

- One block stream, if a module has a `block` trigger on the chain.
- One log stream for each `event` trigger on the chain.
- The first-boot backfill of each `event` trigger, from its `start_block` to the chain head.
- The requests that the modules make.
  `ccow-monitor` polls its registered orders with `eth_call` when a new block arrives.

Use one paid endpoint for each chain.
The handbook, section 8, explains how to select an endpoint: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.

These metrics have a `chain_id` label:

- `nexum_runtime_chain_request_total`: the rate of `outcome="err"` shows a degraded endpoint.
- `nexum_runtime_source_reconnects_total`: the reconnects of each block stream and each log stream.
- `nexum_runtime_chain_response_capped_total`: the responses that the engine rejected for their size.

## See also

- [Deploying shepherd](../deployment.md): the engine config reference.
- [Docker deployment](docker.md): the image and the Compose files.
- `modules/ccow-monitor/component.toml` and `modules/ethflow-watcher/component.toml`: the shipped manifests.
- The nexum-runtime manifest reference: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/02-modules-triggers-packaging.md>.
