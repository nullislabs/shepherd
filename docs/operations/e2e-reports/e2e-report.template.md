# E2E testnet integration report — YYYY-MM-DD

> Copy this file to `e2e-report-YYYY-MM-DD.md` in the same directory
> at the start of the run and fill it in as the run progresses.
> Sections marked **(operator)** must be filled in manually; the rest
> are derived from logs and `/metrics` snapshots.

## 1. Run metadata

| Field | Value |
|---|---|
| Operator | (operator) |
| Start (UTC) | YYYY-MM-DDTHH:MM:SSZ |
| End (UTC)   | YYYY-MM-DDTHH:MM:SSZ |
| Wall clock  | Hh Mm |
| Engine commit | (`git rev-parse HEAD`) |
| Engine config | `engine.e2e.toml` |
| Run host | (e.g. `bruno@bleu-mbp-m1`, `ec2-...`) |
| RPC provider | (alchemy / infura / publicnode / ...) |

## 2. Chain coverage

| Chain | First block | Last block | Block delta | Notes |
|---|---|---|---|---|
| Sepolia (11155111) | | | | |

Target: `block delta >= 1500` to clear the acceptance bar
(>= 1500 Sepolia blocks ≈ 5 h at 12 s block time).

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
| Expected detection | stop-loss logs `submitted:{uid}` once oracle trips |

## 4. Per-module terminal-state markers

> Pull from the engine log with the JSON filter
> `jq 'select(.fields.message | test("submitted:|dropped:|backoff:|TRIGGERED|trapped"))'`.
> Each module must show at least ONE marker for the acceptance bar.

| Module | First marker timestamp | Marker | Sample line |
|---|---|---|---|
| twap-monitor     | | `watch:` / `submitted:` / `dropped:` | |
| ethflow-watcher  | | `submitted:` / `dropped:` | |
| price-alert      | | `TRIGGERED` (Warn) | |
| balance-tracker  | | `last:` write on first dispatch | |
| stop-loss        | | `TRIGGERED` / `submitted:` / `dropped:` | |

## 5. Error counts (from `/metrics` delta)

> Capture two snapshots: at boot (`/metrics > metrics-start.txt`) and
> immediately before shutdown (`/metrics > metrics-end.txt`). Fill in
> the delta column.

| Metric | Start | End | Delta |
|---|---|---|---|
| `shepherd_module_errors_total{module="...",error_kind="trap"}` (per module) | | | |
| `shepherd_module_restarts_total{module="..."}` (per module) | | | |
| `shepherd_module_poisoned{module="..."}` (gauge, end-state per module) | n/a | | n/a |
| `shepherd_cow_api_submit_total{outcome="ok"}` | | | |
| `shepherd_cow_api_submit_total{outcome="err"}` | | | |
| `shepherd_chain_request_total{outcome="ok"}` | | | |
| `shepherd_chain_request_total{outcome="err"}` | | | |
| `shepherd_stream_reconnects_total{kind="block"}` | | | |
| `shepherd_stream_reconnects_total{kind="log"}` | | | |
| `shepherd_event_latency_seconds` (p50 / p95 / p99 per module) | | | |

## 6. Anomalies + defects

> Anything outside the expected log shape. Each anomaly that is
> reproducible OR has an unclear root cause must be filed as a
> separate issue and linked here.

| # | Time (UTC) | Module | Summary |
|---|---|---|---|
| 1 | | | |

## 7. Acceptance checklist

- [ ] `block delta >= 1500` (≥ 5 h coverage)
- [ ] All 5 modules have ≥ 1 terminal-state marker in section 4
- [ ] `shepherd_module_errors_total{error_kind="trap"}` for well-behaved modules == 0
- [ ] No `[[modules]]`-listed module is `shepherd_module_poisoned == 1` at end
- [ ] No `ERROR` lines from `nexum_engine` in the supervisor log
- [ ] At least one orderbook submit attempt landed (`ok` or typed
      `err` with retry/drop classification) on twap-monitor,
      ethflow-watcher, AND stop-loss
- [ ] Report committed in this directory
- [ ] Defects filed and linked in section 6

## 8. Sign-off (operator)

> Brief paragraph: ran clean / found N defects / blocking issues for
> soak Y/N. The soak MUST NOT start until this
> section says "no blocking issues".

…

## 9. Attachments

- `engine.log` (full supervisor JSON log; ≥ 4 h)
- `metrics-start.txt`
- `metrics-end.txt`
- (optional) `metrics-snapshots/` — every 60 s scrape if a soak-style
  Prometheus pull was not running
