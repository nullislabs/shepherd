# Load test runbook

Stresses the `twap-monitor` + `ethflow-watcher` modules under synthetic load using a local Anvil fork of Sepolia and a mock orderbook. Answers one question: how many TWAP+EthFlow events per block the engine dispatches before something breaks.

Acceptance bar:

| Scenario | Per-block load | Expected outcome |
|---|---|---|
| Baseline | 5 TWAP + 5 EthFlow | 100% terminal markers within 3 blocks; p99 latency < 2 s; zero fuel exhaust; zero traps |
| Medium | 20 TWAP + 20 EthFlow | Graceful degradation: `backoff:` markers OK, `shepherd_module_errors_total` stays 0 |
| Saturation | 50 TWAP + 50 EthFlow | Expected to saturate; report identifies the bottleneck |

## 0. Prerequisites

```
rustup target add wasm32-wasip2
brew install foundry              # anvil + cast
cargo --version  >= 1.87
```

`anvil --fork-url` needs an HTTP archive endpoint. Add to `scripts/.env`:

```
RPC_URL_SEPOLIA_HTTP=https://eth-sepolia.g.alchemy.com/v2/<YOUR_KEY>
```

(Public nodes throttle the fork warmup; use Alchemy / drpc / similar.)

## 1. Boot

`scripts/load-run.sh` is the single entry point:

```bash
# baseline (5 TWAP + 5 EthFlow per block, 1 minute)
./scripts/load-run.sh

# medium
./scripts/load-run.sh --twap-per-block 20 --ethflow-per-block 20 --duration-min 2 --scenario medium

# saturation
./scripts/load-run.sh --twap-per-block 50 --ethflow-per-block 50 --duration-min 2 --scenario saturation
```

The script:

1. Sources `scripts/load-bootstrap.sh`: starts Anvil (port 8545) and `shepherd/tools/orderbook-mock` (port 9999).
2. Builds the two module `.wasm`, the `shepherd` binary, and `nexum/tools/load-gen`.
3. Starts the engine on `engine.load.toml`.
4. Snapshots `/metrics`.
5. Runs `nexum/tools/load-gen` for the requested duration.
6. Snapshots `/metrics` again.
7. Tears everything down and drops a report at `docs/operations/load-reports/load-NxM-YYYY-MM-DD.md`.

Ctrl-C triggers `load_teardown`. If a child escapes, run `./scripts/load-teardown.sh`.

## 2. Components

- **Anvil (port 8545)**: `anvil --fork-url $RPC_URL_SEPOLIA_HTTP --port 8545 --block-time 1`. Forks Sepolia at the latest block, inheriting ComposableCoW, CoWSwapEthFlow, the TWAP handler, WETH9, and COW at their pinned addresses, so the test EOA calls real bytecode with no local deployment.
- **Mock orderbook (port 9999)**: `shepherd/tools/orderbook-mock` serves `POST /api/v1/orders`, returning a synthetic 56-byte OrderUid. Knobs (env in `scripts/load-bootstrap.sh`): `--latency-ms` injects response latency; `--error-rate` returns a fraction as an `ApiError` envelope, alternating `InsufficientFee` (`TryNextBlock`) and `InvalidSignature` (`Drop`). Leave both 0 for the saturation probe to isolate the engine-side bottleneck.
- **Engine (`engine.load.toml`)**: RPC `ws://localhost:8545`; cow orderbook URL `http://localhost:9999`; Prometheus on `127.0.0.1:9100`; `state_dir = ./data/load` (wiped each run); modules `twap-monitor` + `ethflow-watcher`.
- **Load generator (`nexum/tools/load-gen`)**: connects to the Anvil WS, calls `anvil_impersonateAccount` + `anvil_setBalance` on the pinned EOA, then each new block fires N `ComposableCoW.create(...)` + M `CoWSwapEthFlow.createOrder(...)` calls, each with a fresh counter-derived salt.

## 3. Acceptance reading

The report at `docs/operations/load-reports/load-NxM-YYYY-MM-DD.md` carries mock-orderbook stats, the load-gen submit breakdown, the engine log tail, and the metrics snapshot pair. Look at:

- `shepherd_event_latency_seconds{module="twap-monitor"}` quantiles: p99 < 2 s for baseline.
- `shepherd_cow_api_submit_total{outcome="ok"}`: tracks the load-gen success count.
- `shepherd_module_errors_total`: must stay 0 for baseline/medium; any non-zero count on saturation is the headline.
- `shepherd_chain_request_total{method="eth_call"}`: twap-monitor polls via `eth_call`; the count shows how hard the poll races the next block.

## 4. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| Anvil exits within 5 s | Forking endpoint rejected | Ensure `RPC_URL_SEPOLIA_HTTP` is an archive endpoint |
| `wasm32-wasip2` build fails on `wit-bindgen` | Toolchain stale | `rustup target add wasm32-wasip2` |
| Engine never reaches `supervisor ready` | Stale wasm artefacts | `rm -rf target/wasm32-wasip2` and rerun |
| `/metrics` never comes up | Port 9100 in use | Edit `engine.load.toml` `bind_addr` and the curl URL in `scripts/load-run.sh` |
| `load-gen` errors with "EOA not impersonated" | Anvil restarted mid-run | `scripts/load-teardown.sh && scripts/load-run.sh` |

## 5. References

- Sister doc (live Sepolia E2E): `docs/operations/e2e-testnet-runbook.md`
- Engine config: `engine.load.toml`
- Tools: `shepherd/tools/orderbook-mock/`, `nexum/tools/load-gen/`
- Scripts: `scripts/load-bootstrap.sh`, `scripts/load-run.sh`, `scripts/load-teardown.sh`
