# scripts/ — E2E automation

Three-step automation for the E2E run on Sepolia. Wraps
the runbook (`docs/operations/e2e-testnet-runbook.md`) + the prep
punch list (`docs/operations/e2e-prep.md`) into shell
scripts so the operator only has to (a) fill in `.env` and
(b) decide when to stop.

## One-time setup

```bash
cp scripts/env-template scripts/.env
$EDITOR scripts/.env       # fill in RPC URLs + EOA private key
```

`.env` is gitignored — secrets stay on disk, never enter chat,
never get committed.

Required external tools:

- `cargo` + the `wasm32-wasip2` target (already there if you've
  built the workspace before).
- `cast` from foundry (`curl -L https://foundry.paradigm.xyz | bash && foundryup`).
- `jq`, `curl`, `python3` with `pip3 install eth-utils eth-abi pycryptodome`.

## Running

```bash
scripts/e2e-run.sh        # boots engine, captures metrics baseline (~1 min)
scripts/e2e-onchain.sh    # submits TWAP + EthFlow on-chain (~1 min, ~0.005 ETH)
#  … engine runs for ~5 h to hit the 1500-block acceptance bar …
scripts/e2e-finish.sh     # SIGINTs engine, captures end metrics, generates report
```

Three artefacts land in `docs/operations/e2e-reports/`:

| File | Provenance |
|---|---|
| `engine-<ts>.log` | Full JSON-formatted supervisor log (~5 MB / 5 h). |
| `metrics-start-<ts>.txt` | `/metrics` snapshot at boot. |
| `metrics-end-<ts>.txt` | `/metrics` snapshot at SIGINT. |
| `e2e-report-<date>.md` | Auto-filled E2E report. Operator reviews + signs off + commits. |

The first three are gitignored; the report is committed manually
once you've reviewed it.

## Script details

### `e2e-run.sh`

- Renders `engine.e2e.toml` → `engine.e2e.local.toml`
  (gitignored via `*.local.toml`) with `RPC_URL_SEPOLIA`
  substituted in. Embedded URL key never reaches git.
- Cleans `data/e2e/` for a fresh local-store.
- Builds 5 modules + engine in `--release`.
- Launches via `nohup`; engine survives the parent shell exiting.
- Waits ≤ 60 s for `supervisor ready modules=5 chains=1`.
- Persists `ENGINE_PID`, `LOG_FILE`, `METRICS_START`, `START_TS`,
  `START_ISO` into `scripts/.state` (gitignored).

### `e2e-onchain.sh`

Pre-flight:
- Derives the EOA address from `OPERATOR_PRIVATE_KEY` and asserts
  it matches the pinned `0x7bF140727D27ea64b607E042f1225680B40ECa6A`.
- Asserts EOA balance ≥ 0.02 ETH.

Required actions:
1. **TWAP** — `cast send ComposableCoW.create((handler,salt,staticInput),true)`
   with calldata derived freshly per invocation by
   `scripts/_twap_calldata.py` (sets `t0 = now - 60` so part 0 is
   Ready immediately; hardcoding `t0 = 0` is the prior bug). Fires
   `ConditionalOrderCreated` → twap-monitor logs `watch:`.
2. **EthFlow** — calls `scripts/_ethflow_quote.py` to hit cow.fi
   `/api/v1/quote`, encodes the returned `EthFlowOrder.Data`,
   then `cast send EthFlow.createOrder` with the right msg.value.
   Fires `OrderPlacement` → ethflow-watcher logs `submitted:`.

Optional (gated on `RUN_OPTIONAL_PRESIGN=1` in `.env`):
3. `WETH9.deposit()` payable 0.01 ETH.
4. `GPv2Settlement.setPreSignature(uid, true)` with the pinned UID.
5. `WETH9.approve(GPv2VaultRelayer, 0.005 ETH)`.

Each tx hash appended to `scripts/.state` so the report generator
can link them.

> stop-loss already produces `submitted:{uid}` on the very first
> block (verified in run-prep smoke — the CoW orderbook accepts
> PreSign orders upfront). The optional path is only needed if you
> want the order to actually **settle** on-chain.

### `e2e-finish.sh`

- Captures `metrics-end-<ts>.txt`.
- Sends `SIGINT` to the engine PID.
- Waits ≤ 30 s for `graceful shutdown complete` in the log
  (graceful-shutdown path).
- Escalates to `SIGKILL` if the engine is still alive after 30 s.
- Invokes `e2e-report-gen.sh` to write the filled-in report.

### `e2e-report-gen.sh`

Reads `LOG_FILE`, `METRICS_START`, `METRICS_END`, `START_ISO`,
`END_ISO`, and the `TX_*` hashes from `scripts/.state`; computes:

- Chain coverage (first/last block from `block_number` log fields).
- Per-module first terminal marker timestamp + sample line.
- Delta of every `shepherd_*` Prometheus counter / histogram.
- ERROR + trapped + poisoned tallies.
- Per-row acceptance checklist (auto-checks block delta ≥ 1500,
  marker per module, zero traps, zero poisons, zero ERRORs,
  TWAP+EthFlow tx hashes present).

Writes `e2e-report-<date>.md` in `docs/operations/e2e-reports/`.
Operator: review + add anomalies (section 6) + sign off
(section 8) + commit with `git add -f`.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `scripts/.env not found` | First run | `cp scripts/env-template scripts/.env && $EDITOR .env` |
| `cast wallet address failed` | bad PK format | Must be `0x` + 64 hex chars. No spaces. |
| `engine did not reach supervisor-ready in 60s` | RPC unreachable / config error | `tail -30 docs/operations/e2e-reports/engine-*.log` to see why |
| `cow.fi /quote returned 4xx` | Orderbook didn't like the quote params | Read the body in the error; usually a token-pair issue. Wait + retry if Sepolia orderbook is flaky. |
| `engine already running` | Prior run not finished | `scripts/e2e-finish.sh` (or `kill -INT $(grep ENGINE_PID scripts/.state | cut -d= -f2)`) |
| `block delta` in report is low | Run was too short | The acceptance bar is ≥ 1500 (~5 h). Anything less is insufficient even with all 5 markers. |

## Re-running cleanly

```bash
scripts/e2e-finish.sh       # safe even if it's the only command — graceful exit
rm -rf data/e2e             # wipe local-store
rm scripts/.state           # wipe run state
scripts/e2e-run.sh          # fresh start
```
