# Mainnet run, October 2026

Shepherd `v0.1.0` ran for 7 days on Ethereum Mainnet without manual intervention.
In that time it posted all 28 parts of a live TWAP order, and a solver filled each part.
This file gives the setup, the results, and the problems that the run found.

## Setup

- Image: `ghcr.io/nullislabs/shepherd:v0.1.0`, built from shepherd `v0.1.0` with nexum-runtime `v0.1.0` and videre `v0.1.0`.
- Engine configuration: the shipped [`engine.docker.toml`](../../../engine.docker.toml), without changes.
- Modules: the two shipped modules, `ccow-monitor` and `ethflow-watcher`.
- Deployment: the Kustomize base in [`deploy/kubernetes`](../../../deploy/kubernetes) at `v0.1.0`, on a Kubernetes cluster.
  The overlay set a storage class for a network block volume and added one network rule for the metrics collector.
- Chain: Ethereum Mainnet, through one node over HTTP.
  The node had no WebSocket listener, so the engine read new blocks by HTTP polling.
- Window: 2026-10-13 07:18:07 UTC to 2026-10-20 07:18:07 UTC, 168 hours.
  Nobody changed or restarted the deployment in the window.
- Metrics: Prometheus scraped `/metrics` every 60 seconds.
  The queries are those of the [soak runbook](../../operations/soak-runbook.md).
- Logs: the container output, as JSON lines.

## Results

| Check | Result |
|---|---|
| Engine restarts | 0. The engine start time did not change, and the platform saw one container for the whole window. |
| Uptime at the end of the window | 604,799 seconds |
| Hours with a full set of metrics | 168 of 168, each with 60 samples |
| Modules alive | Both, in every hour |
| Module errors, restarts, quarantine | 0 |
| Stream reconnects | 0 |
| Out-of-memory events | 0 |
| ERROR log lines | 0 |
| WARN log lines | 6 (see below) |
| Blocks delivered | 50,223 (blocks 26,182,022 to 26,232,244), one dispatch each |
| Highest distance between chain head and delivered block | 1 block |
| Dispatches, `ccow-monitor` | 50,223 block, 3,310 intent status, 1 event |
| Dispatches, `ethflow-watcher` | 1,937 event, 3,310 intent status |
| Handler time p50 / p95 / p99, `ccow-monitor` | 2.5 / 4.8 / 5.0 ms |
| Handler time p50 / p95 / p99, `ethflow-watcher` | 3.0 / 8.5 / 9.7 ms |
| Delivery latency p95 (engine receipt to dispatch) | 0.04 ms (`ccow-monitor`), 0.24 ms (`ethflow-watcher`) |
| Block age at dispatch p50 / p95 / p99 | 9.3 / 15.4 / 16.4 s, which is mostly the HTTP poll interval |
| Memory per module | 1,114,112 bytes for each module, constant |
| Process memory (working set) | 25.3 to 29.4 MiB by hour; highest sample 30.8 MiB |
| CPU | 0.00024 cores on average; highest 5-minute rate 0.0013 cores |
| State volume used | 0.08 MiB at the start, 0.57 MiB at the end |

The 6 WARN lines:

- 2 lines `loading unverified` at boot, because the shipped manifests do not pin a module digest.
- 4 lines `status poll failed - retrying on the next cadence` on 2026-10-15, for EthFlow orders.
  Three were `order not found` from the orderbook, and one was a transport failure.
  The next poll succeeded each time.

The block source was silent for 60 seconds twice, at 2026-10-13 16:23 and 2026-10-18 04:43 UTC.
Each time the engine continued from its position, and no block was missed.

## Live TWAP order

The order sold 18,013.23 COW for BZZ in 28 parts, one part every 6 hours.
Each part had a fixed lowest buy amount of 1,869.42 BZZ.
The owner is an account with an EIP-7702 delegation, so the orderbook checks each order with ERC-1271.

- Registration: transaction `0x180439111b226e204395cf7a704e385ccfa105ab3c47a7dfec41d0d0d96f9cab`, block 26,182,397, 2026-10-13 08:33:11 UTC.
- Contracts: the registry `0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232` and the TWAP handler `0x4E17A65d14E7f37d2A9f0389f17eFd41AAA64c91` from [nullislabs/composable-cow](https://github.com/nullislabs/composable-cow/tree/deployed/mainnet-owned-twap), and the account delegate `0x15236F06922A288B68e57Ab42e397920a3F3Bb99`.
  These contracts are for testing only and are not audited.
- App data: `{"appCode":"shepherd","metadata":{},"version":"1.15.0"}`, hash `0x44cdde416f10e429486b241e5ca4750899eb51c999bbbc289edf7092fa644b3a`.

Results:

- The engine posted 28 orders, one for each part, and logged 28 `submitted` lines.
  The orderbook holds exactly one order for each part.
- The engine posted each part 4 to 17 seconds after the part opened.
  Part 0 opened before the registration was mined; the engine posted it 10 seconds after the registration.
- A solver filled all 28 parts: 18,013.23 COW for 54,289.90 BZZ in total.
- The median time from post to settlement was 18 seconds.
  Parts 22 and 25 took 1 h 50 min and 2 h 28 min, because the market price was at the limit.
  They filled 0.00% and 0.04% above the lowest buy amount.
- After the last part, `ccow-monitor` logged that the order was complete.

[`twap-parts.csv`](twap-parts.csv) has one row for each part, with the order UID and the settlement transaction.

## EthFlow

`ethflow-watcher` watches EthFlow orders and does not post them.
In the window it saw 1,937 EthFlow placements and 3,310 status changes: 1,850 Fulfilled, 1,345 Open, 106 Expired and 9 Cancelled.
These counts include the 28 TWAP parts, because of the second problem below.

## Problems found

- `ethflow-watcher` keeps one store key for each order that it sees, and never deletes it.
  This is most of the growth of the state volume: about 245 bytes for each order.
  See [#701](https://github.com/nullislabs/shepherd/issues/701).
- The host sends each intent status to every module that listens to the venue, not only to the module that owns the intent.
  `ethflow-watcher` therefore logged and recorded the TWAP parts, and `ccow-monitor` received the status of every EthFlow order.
  See [#702](https://github.com/nullislabs/shepherd/issues/702).
- `ccow-monitor` has three event triggers on the same contract, and the engine opens one log source for each.
  Its backfill and source lines therefore show three times in the log.
  This is three sources, not duplicate work, but each block range is read three times.
- The metric `nexum_runtime_chain_request_total` counts only the chain calls of modules, not the polling of the engine itself.
  It does not show the health of the node.

## Limits of this run

- One chain only. The other chains are tested at the configuration level.
- No RPC outage and no module crash happened in the window, so the run does not show recovery from them.
  The tests of nexum-runtime cover these cases: a trapping module beside a healthy one ([lifecycle tests](https://github.com/nxm-rs/nexum-runtime/blob/v0.1.0/crates/nexum-runtime-supervisor/src/supervisor/tests/lifecycle.rs)), and a hung node and a failed source open ([event loop](https://github.com/nxm-rs/nexum-runtime/blob/v0.1.0/crates/nexum-runtime-supervisor/src/runtime/event_loop.rs)).
- The node was the only RPC endpoint. A fallback endpoint is not in scope.
- The platform restart counter of the cluster did not cover this namespace.
  The restart evidence is the engine start time, the uptime and the single container.

## Files

- [`hourly-metrics.csv`](hourly-metrics.csv): one row for each hour of the window, from the Prometheus metrics.
  The columns are uptime, engine restarts, scrape samples, modules alive, module errors and restarts, dispatches for each module, handler time p95, block age p95, head distance, working set, CPU and state volume.
- [`twap-parts.csv`](twap-parts.csv): one row for each TWAP part.
- [`engine-log.jsonl.gz`](engine-log.jsonl.gz): every engine log line in the window, 8,673 lines.
  The RPC endpoint is replaced with `<rpc-endpoint>`.
