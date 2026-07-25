# E2E testnet integration report: YYYY-MM-DD

> Copy to `e2e-report-YYYY-MM-DD.md` in this directory at the start of the run and fill in as it progresses. Sections marked **(operator)** are manual; the rest derive from logs and `/metrics` snapshots.

## 1. Run metadata

| Field | Value |
|---|---|
| Operator | (operator) |
| Start (UTC) | YYYY-MM-DDTHH:MM:SSZ |
| End (UTC)   | YYYY-MM-DDTHH:MM:SSZ |
| Wall clock  | Hh Mm |
| Engine commit | (`git rev-parse HEAD`) |
| Engine config | `engine.e2e.toml` |
| Run host | |
| RPC provider | |

## 2. Chain coverage

| Chain | First block | Last block | Block delta | Notes |
|---|---|---|---|---|
| Sepolia (11155111) | | | | |

Target: `block delta >= 1500` (>= 5 h at 12 s block time).

## 3. On-chain actions submitted by operator

### 3.1 TWAP conditional order (operator)

| Field | Value |
|---|---|
| Tx hash | 0x... |
| Block | |
| Safe / EOA | 0x... |
| ComposableCoW order hash | 0x... |
| Expected detection | twap-monitor logs `watch:{orderHash}` |

### 3.2 EthFlow swap (operator)

| Field | Value |
|---|---|
| Tx hash | 0x... |
| Block | |
| Sender EOA | 0x... |
| Sell amount (ETH wei) | |
| Expected detection | ethflow-watcher logs `submitted:{uid}` |

### 3.3 stop-loss pre-signature (operator)

| Field | Value |
|---|---|
| `setPreSignature` tx hash | 0x... |
| `sell_token` allowance tx hash | 0x... |
| Owner EOA | 0x... |
| Expected UID | 0x... |
| Expected detection | stop-loss logs `submitted:{uid}` once the oracle trips |

## 4. Per-module terminal-state markers

> Pull from the log with `jq 'select(.fields.message | test("submitted:|dropped:|backoff:|TRIGGERED|trapped"))'`. Each module must show at least one marker.

| Module | First marker timestamp | Marker | Sample line |
|---|---|---|---|
| twap-monitor     | | `watch:` / `submitted:` / `dropped:` | |
| ethflow-watcher  | | `submitted:` / `dropped:` | |
| price-alert      | | `TRIGGERED` (Warn) | |
| balance-tracker  | | `last:` write on first dispatch | |
| stop-loss        | | `TRIGGERED` / `submitted:` / `dropped:` | |

## 5. Error counts (from `/metrics` delta)

> Snapshot at boot and immediately before shutdown; fill the delta column.

| Metric | Start | End | Delta |
|---|---|---|---|
| `shepherd_module_errors_total{module="...",error_kind="trap"}` (per module) | | | |
| `shepherd_module_restarts_total{module="..."}` (per module) | | | |
| `shepherd_module_poisoned{module="..."}` (gauge, end-state) | n/a | | n/a |
| `shepherd_cow_api_submit_total{outcome="ok"}` | | | |
| `shepherd_cow_api_submit_total{outcome="err"}` | | | |
| `shepherd_chain_request_total{outcome="ok"}` | | | |
| `shepherd_chain_request_total{outcome="err"}` | | | |
| `shepherd_stream_reconnects_total{kind="block"}` | | | |
| `shepherd_stream_reconnects_total{kind="chain-log"}` | | | |
| `shepherd_event_latency_seconds` (p50 / p95 / p99 per module) | | | |

## 6. Anomalies + defects

> Each reproducible or unexplained anomaly is filed as a separate issue and linked here.

| # | Time (UTC) | Module | Summary |
|---|---|---|---|
| 1 | | | |

## 7. Acceptance checklist

- [ ] `block delta >= 1500`
- [ ] All 5 modules have >= 1 terminal-state marker in section 4
- [ ] `shepherd_module_errors_total{error_kind="trap"}` for well-behaved modules == 0
- [ ] No `[[modules]]`-listed module is `shepherd_module_poisoned == 1` at end
- [ ] No `ERROR` lines from `nexum_runtime` in the supervisor log
- [ ] At least one orderbook submit attempt landed on twap-monitor, ethflow-watcher, and stop-loss
- [ ] Report committed in this directory
- [ ] Defects filed and linked in section 6

## 8. Sign-off (operator)

> Ran clean / found N defects / blocking issues for the soak Y/N. The soak must not start until this says "no blocking issues".

## 9. Attachments

- `engine.log` (full supervisor JSON log)
- `metrics-start.txt`
- `metrics-end.txt`
