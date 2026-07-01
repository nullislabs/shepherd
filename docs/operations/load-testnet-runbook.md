# Load test runbook

How to stress shepherd's `twap-monitor` + `ethflow-watcher` modules
under synthetic load using a local Anvil fork of Sepolia and a mock
orderbook.

The acceptance bar is:

| Scenario | Per-block load | Expected outcome |
|---|---|---|
| Baseline | 5 TWAP + 5 EthFlow | 100% terminal markers within 3 blocks; p99 latency < 2s; zero fuel exhaust; zero traps |
| Medium | 20 TWAP + 20 EthFlow | Graceful degradation - `backoff:` markers OK, `shepherd_module_errors_total` stays 0 |
| Saturation | 50 TWAP + 50 EthFlow | Expected to saturate; report identifies the bottleneck |

This runbook is distinct from
`docs/operations/e2e-testnet-runbook.md` (correctness on live Sepolia)
and the 7-day soak (wall-clock stability).

---

## 0. Prerequisites

### Toolchain

```
rustup target add wasm32-wasip2
brew install foundry              # for `anvil` + `cast`
cargo --version  >= 1.87
```

### Sepolia archive endpoint

`anvil --fork-url` needs an HTTP archive endpoint to seed the fork.
Add to `scripts/.env`:

```
RPC_URL_SEPOLIA_HTTP=https://eth-sepolia.g.alchemy.com/v2/<YOUR_KEY>
```

(Public nodes throttle the initial fork warmup; use Alchemy / drpc /
similar.)

---

## 1. Boot

The three supporting processes (Anvil, orderbook-mock, engine) live in
the background; `scripts/load-run.sh` is the single entry point.

```bash
# baseline (default knobs: 5 TWAP + 5 EthFlow per block, 1 minute)
./scripts/load-run.sh

# medium load
./scripts/load-run.sh --twap-per-block 20 --ethflow-per-block 20 \
    --duration-min 2 --scenario medium

# saturation probe
./scripts/load-run.sh --twap-per-block 50 --ethflow-per-block 50 \
    --duration-min 2 --scenario saturation
```

The script:

1. Sources `scripts/load-bootstrap.sh` -> starts Anvil (`port 8545`)
   and `tools/orderbook-mock` (`port 9999`).
2. Builds `twap-monitor` + `ethflow-watcher` `.wasm`, the
   `nexum-engine` binary, and `tools/load-gen`.
3. Starts the engine pointed at `engine.load.toml`.
4. Snapshots `/metrics` from the engine.
5. Runs `tools/load-gen` for the requested duration.
6. Snapshots `/metrics` again.
7. Tears everything down.
8. Drops a report at `docs/operations/load-reports/load-NxM-YYYY-MM-DD.md`.

If you Ctrl-C, the trap calls `load_teardown` and kills the children
before exit. If something escapes (bash trap missed), run
`./scripts/load-teardown.sh` explicitly.

---

## 2. What each component does

### Anvil (port 8545)

```
anvil --fork-url $RPC_URL_SEPOLIA_HTTP --port 8545 --block-time 1
```

Forks Sepolia at the latest block. Inherits every contract the test
needs (ComposableCoW, CoWSwapEthFlow, TWAP handler, WETH9, COW token)
at their pinned Sepolia addresses, so the test EOA can call
`ComposableCoW.create(...)` and `CoWSwapEthFlow.createOrder(...)`
against real bytecode without any local deployment step.

`--block-time 1` mines a block per second, matching Sepolia's
~12s cadence... loosely. The point of the load test is to push N+M
transactions into each block, not to mimic mainnet block times.

### Mock orderbook (port 9999)

`tools/orderbook-mock` serves the two endpoints shepherd's `cow-api`
host backend hits per submission:

- `POST /api/v1/orders` - returns a synthetic 56-byte OrderUid.
- `GET /api/v1/app_data/{hash}` - returns the empty appData document
  so `resolve_app_data` is satisfied without a real
  registry.

Knobs (set via env in `scripts/load-bootstrap.sh` if needed):

- `--latency-ms` - inject artificial latency on every response.
- `--error-rate` - fraction of POST /orders responses that return a
  recognised `ApiError` envelope. Alternates between
  `InsufficientFee` (`TryNextBlock`) and `InvalidSignature` (`Drop`).

For the saturation probe, leaving `latency_ms=0` and `error_rate=0`
isolates the engine-side bottleneck from orderbook-side variability.

### Engine (engine.load.toml)

- `[chains.11155111] rpc_url = "ws://localhost:8545"`
- `[chains.11155111] orderbook_url = "http://localhost:9999"`
- Prometheus enabled on `127.0.0.1:9100`
- `state_dir = ./data/load` (wiped at the start of every run)
- Module list: `twap-monitor` + `ethflow-watcher` only

### Load generator (tools/load-gen)

Connects to the Anvil WebSocket, calls `anvil_impersonateAccount` +
`anvil_setBalance` on the pinned EOA
(`0x7bF140727D27ea64b607E042f1225680B40ECa6A`), then in a loop, every
new block, fires N `ComposableCoW.create(...)` calls plus M
`CoWSwapEthFlow.createOrder(...)` calls. Each create uses a fresh
salt (counter-derived) so the txs do not collide on the
ComposableCoW dedup check.

`anvil_impersonateAccount` skips signing entirely - one fewer
overhead under load.

---

## 3. Acceptance reading

After a run, the report at
`docs/operations/load-reports/load-NxM-YYYY-MM-DD.md` carries:

- mock-orderbook stats (success vs. error count) - matches load-gen's
  reported submit-attempt count, modulo `error_rate`.
- load-gen tail - submit success/failure breakdown per block.
- engine log tail - watch for `module trap`, `poisoned`,
  `init failed`, `WS reconnect`.
- metrics delta filename pair (auto-delta lands in a follow-up).

Look at:

- `shepherd_event_latency_seconds{module="twap-monitor"}` quantiles -
  p99 < 2s for the baseline scenario.
- `shepherd_cow_api_submit_total{outcome="ok"}` - should track the
  load-gen success count.
- `shepherd_module_errors_total` - must stay 0 for baseline/medium;
  any non-zero count on saturation is the headline.
- `shepherd_chain_request_total{method="eth_call"}` - twap-monitor
  polls via `eth_call`; the count tells you how aggressively the
  poll is racing the next block.

---

## 4. What this does NOT prove

- WS reconnect resilience (7-day soak).
- Diverse appData / order-shape correctness (the backtest).
- Multi-day memory drift (7-day soak).
- Real-orderbook 4xx variety (the backtest).
- Provider rate-limit handling on the live network.

This test answers exactly one question: "How many TWAP+EthFlow events
per block can shepherd dispatch before something breaks?" Use it
alongside the soak, not instead of it.

---

## 5. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Anvil exits within 5s | Forking endpoint rejected | Check `RPC_URL_SEPOLIA_HTTP` is an archive endpoint, not a pruned node. Alchemy free tier works. |
| `cargo build --target wasm32-wasip2` fails on `wit-bindgen` | Toolchain stale | `rustup target add wasm32-wasip2` (re-run; may have rolled). |
| Engine never reaches `supervisor ready` | wasm artefacts not built | The script builds them, but a stale `target/wasm32-wasip2/release/*` from another branch can collide. `rm -rf target/wasm32-wasip2` and rerun. |
| `/metrics` never comes up | Port 9100 in use | Edit `engine.load.toml` `bind_addr` (and the curl URL in `scripts/load-run.sh`). |
| `load-gen` errors with "EOA not impersonated" | Anvil restarted mid-run | `scripts/load-teardown.sh && scripts/load-run.sh` from scratch. |

---

## 6. References

- Sister doc (live Sepolia E2E): `docs/operations/e2e-testnet-runbook.md`
- Engine config: `engine.load.toml`
- Tools: `tools/orderbook-mock/`, `tools/load-gen/`
- Scripts: `scripts/load-bootstrap.sh`, `scripts/load-run.sh`, `scripts/load-teardown.sh`
