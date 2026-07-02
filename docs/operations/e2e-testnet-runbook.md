# E2E testnet runbook

How to exercise **all 5 modules** — twap-monitor, ethflow-watcher,
price-alert, balance-tracker, stop-loss — on a real Sepolia host
**simultaneously for 4-6 hours**. Same shape as the M2 + M3
runbooks, but this one runs the full production module suite and
captures a structured report (`docs/operations/e2e-reports/`).

The E2E run is the integration step between unit-test coverage
(MockHost, per-module strategy tests) and the 7-day soak.
The soak validates *stability*; this validates *correctness in a
live dispatch context* and surfaces cross-module bugs the soak
should not be discovering.

The acceptance bar is:

- ≥ 1500 Sepolia blocks (≈ 5 h at 12 s block time).
- Each of the 5 modules writes at least one terminal-state marker
  (`submitted:` / `dropped:` / `backoff:` / `TRIGGERED` / `last:`).
- 0 unexpected errors in the supervisor log.
- 0 well-behaved modules trapped or poisoned at end of run.
- A committed report + filed defects.

---

## 0. Prerequisites

### Toolchain

Same as the M2 + M3 runbooks (`rustup target add wasm32-wasip2`,
optionally `just`, a Sepolia WS RPC).

### RPC

The public Sepolia node (`wss://ethereum-sepolia-rpc.publicnode.com`)
throttles `eth_subscribe` and `eth_call` under sustained load. The
E2E run does at minimum:

- 1 block subscription (shared across 4 modules — price-alert,
  balance-tracker, stop-loss, twap-monitor block-tick).
- 2 log subscriptions (twap-monitor's
  `ComposableCoW.ConditionalOrderCreated` + ethflow-watcher's
  `CoWSwapEthFlow.OrderPlacement`).
- ≥ 4 `eth_call` per block from price-alert + balance-tracker
  (×2 addresses) + stop-loss, + 1 per registered TWAP order
  per block.

Override the `[chains.11155111] rpc_url` in `engine.e2e.toml`
with an Alchemy / Infura WS for the run:

```toml
[chains.11155111]
rpc_url = "wss://eth-sepolia.g.alchemy.com/v2/<KEY>"
```

### On-chain prep (operator)

The acceptance bar requires real on-chain submissions. Before
launching the run, prepare:

1. **A funded test EOA on Sepolia** (≥ 0.05 ETH for gas; the same
   EOA can satisfy the EthFlow swap + stop-loss `setPreSignature`
   sub-tasks).
2. **A Safe (or direct caller) that can call ComposableCoW** on
   Sepolia — for the TWAP conditional-order submission.
3. **stop-loss config aligned with that EOA**: update
   `modules/examples/stop-loss/module.toml::[config].owner` to the
   EOA address you control, and pick a `sell_token` / `buy_token`
   pair the EOA holds + has approved to the GPv2VaultRelayer.
   See `docs/operations/m3-testnet-runbook.md` section 2 for the
   full pre-sign + allowance recipe.

The E2E run will start cleanly without (1)/(2)/(3), but the
acceptance bar requires at least one `submitted:` marker on each
of twap-monitor / ethflow-watcher / stop-loss, and you only get
those by triggering each path on-chain.

---

## 1. Boot

The engine + all 5 modules + Prometheus `/metrics` endpoint:

```bash
just run-e2e
```

Equivalent long form:

```bash
just build-e2e         # builds the 5 module .wasm artefacts
cargo build -p nexum-cli
cargo run -p nexum-cli -- --engine-config engine.e2e.toml
```

### Expected boot sequence (~5 s)

```
INFO  nexum starting
INFO  opening chain RPC provider chain_id=11155111 url="wss://..."
INFO  metrics exporter listening at /metrics addr=127.0.0.1:9100
INFO  loading module manifest manifest=modules/twap-monitor/module.toml
INFO  compiling component component=...twap_monitor.wasm
INFO  init succeeded module=twap-monitor
INFO  loading module manifest manifest=modules/ethflow-watcher/module.toml
INFO  init succeeded module=ethflow-watcher
INFO  loading module manifest manifest=modules/examples/price-alert/module.toml
INFO  init succeeded module=price-alert
INFO  loading module manifest manifest=modules/examples/balance-tracker/module.toml
INFO  init succeeded module=balance-tracker
INFO  loading module manifest manifest=modules/examples/stop-loss/module.toml
INFO  init succeeded module=stop-loss
INFO  supervisor up count=5
INFO  supervisor ready modules=5 chains=1
INFO  block subscription open chain_id=11155111
INFO  log subscription open chain_id=11155111 module=twap-monitor
INFO  log subscription open chain_id=11155111 module=ethflow-watcher
```

If any of `count=5`, `modules=5`, or both log subscriptions are
missing, **stop the run and triage** — running 4-6 h on a
degraded engine wastes time the operator does not get back.

### Smoke at first block (~12 s after boot)

Within the first Sepolia block dispatched:

```
DEBUG dispatch block chain_id=11155111 number=N
DEBUG chain::request method=eth_call           # price-alert oracle read
DEBUG chain::request method=eth_getBalance     # balance-tracker addr 1
DEBUG chain::request method=eth_getBalance     # balance-tracker addr 2
DEBUG chain::request method=eth_call           # stop-loss oracle read
WARN  price-alert: TRIGGERED answer=... threshold=...
```

(See `docs/operations/m3-testnet-runbook.md` for the per-module
single-block expectations — the E2E run reproduces those plus
twap-monitor's empty poll loop until a `watch:` is registered.)

---

## 2. The 4-6 h run

### 2.1 Start the clock

Pipe the engine output to a JSON log file the operator can mine
with `jq` after the run:

```bash
just run-e2e 2>&1 | tee -a docs/operations/e2e-reports/engine-$(date -u +%Y%m%dT%H%M%SZ).log
```

Record `date -u --iso-8601=seconds` and `git rev-parse HEAD` in
section 1 of the report template.

### 2.2 Capture the metrics baseline

```bash
curl -s http://127.0.0.1:9100/metrics > docs/operations/e2e-reports/metrics-start.txt
```

### 2.3 Trigger each on-chain action

Run these as soon as the supervisor is `ready`:

1. **TWAP order** — call ComposableCoW from your Safe (or directly
   if you control the user). Within 1-2 blocks, twap-monitor logs:
   ```
   INFO twap-monitor watch:{orderHash} chain_id=11155111
   ```
2. **EthFlow swap** — execute a small ETH-flow swap from your EOA
   via the cow-swap front-end pointed at Sepolia. Within 1-2 blocks
   ethflow-watcher logs:
   ```
   INFO ethflow-watcher submitted:{uid}
   ```
   (or a typed `dropped:{uid}` if the orderbook rejected — both
   count as a terminal-state marker for section 4.)
3. **stop-loss trigger** — once your owner EOA has called
   `setPreSignature` and approved the sell token, lower
   `trigger_price` in `modules/examples/stop-loss/module.toml` to
   ≤ the current Sepolia Chainlink ETH/USD answer and reload the
   engine (or set it pre-boot if you already know the feed value).
   Within 1 block stop-loss logs:
   ```
   INFO stop-loss TRIGGERED price=... trigger=...
   INFO stop-loss submitted:{uid}
   ```

### 2.4 Idle until end of run

Once all three terminal markers are observed and the report's
section 4 has at least one entry per module, leave the engine
running undisturbed for the remainder of the 4-6 h window.

The operator should watch for these red flags (if any appears,
the run is a defect and section 6 must capture it):

| Red flag | Why it matters |
|---|---|
| `ERROR` from `nexum_runtime::*` | Acceptance #5: zero ERROR lines. |
| `module ... trapped:` for a non-fixture module | Trapping production-side modules is a defect. |
| `module ... poisoned` | Quarantine of a real module is a defect. |
| `stream reconnect attempt=N` with N rising | The WS is flapping (RPC issue or bug). One reconnect per chain is fine. |
| `chain::request` `err` rate > 5% | The RPC is degraded. Switch keys / providers. |

### 2.5 Capture metrics deltas + shutdown

At the end of the run window:

```bash
curl -s http://127.0.0.1:9100/metrics > docs/operations/e2e-reports/metrics-end.txt
# Ctrl-C the engine — graceful shutdown writes last_dispatched_block:
# > INFO graceful shutdown complete dispatched_blocks=N dispatched_logs=M uptime_secs=K
```

Diff the two snapshots to fill in the report's section 5:

```bash
diff <(grep '^shepherd_' docs/operations/e2e-reports/metrics-start.txt) \
     <(grep '^shepherd_' docs/operations/e2e-reports/metrics-end.txt)
```

---

## 3. Filling in the report

Copy the template at the start of the run:

```bash
DATE=$(date -u +%Y-%m-%d)
cp docs/operations/e2e-reports/e2e-report.template.md \
   docs/operations/e2e-reports/e2e-report-${DATE}.md
$EDITOR docs/operations/e2e-reports/e2e-report-${DATE}.md
```

Fill sections in this order:

1. **Section 1 (run metadata)** at boot.
2. **Section 3 (on-chain actions)** as you submit each one.
3. **Section 4 (terminal markers)** as each first marker fires.
4. **Section 5 (metrics)** once `metrics-end.txt` is captured.
5. **Section 6 (anomalies)** continuously — anything unexpected
   gets a row + an issue.
6. **Section 7 (acceptance checklist)** at the end — every box
   must be `[x]` for the run to pass.
7. **Section 8 (sign-off)** is the gating decision for the
   7-day soak.

Commit the filled-in report on the same branch as this runbook:

```bash
git add docs/operations/e2e-reports/e2e-report-${DATE}.md
git commit -m "ops(e2e): report from ${DATE} run"
git push
```

---

## 4. What this does NOT prove

- **Stability beyond ~5 h** → the 7-day soak (Sepolia + Arb Sepolia).
- **Adversarial resource exhaustion** → a fuel/memory adversarial fixtures run (M4 territory).
- **Security review** → tracked separately.
- **Production deployment story** → `docs/production.md`.
- **Multi-chain isolation under live WS drops** → partially
  proven by integration tests; full validation
  requires Arb Sepolia + Sepolia simultaneously, which the soak
  exercises.

---

## 5. Troubleshooting

Inherits the M2 + M3 runbook tables. E2E-specific:

| Symptom | Likely cause | Fix |
|---|---|---|
| `supervisor ready modules=4 chains=1` (or less) at boot | One of the 5 module manifests failed to load — likely a missing wasm artefact under `target/wasm32-wasip2/release/` | Re-run `just build-e2e` and verify all 5 `.wasm` files are present. |
| `INFO log subscription open chain_id=11155111` appears only once | One of the two log-subscribing modules failed init | Check the immediately preceding `init failed module=...` line; the failing module's `[capabilities]` or subscription `address` is the usual culprit. |
| RPC drops every ~30 min on `publicnode.com` | Public node rate limits | Switch to Alchemy / Infura per section 0. |
| `stop-loss TRIGGERED` fires immediately on default config | Default `trigger_price = 2500.00` is above Sepolia Chainlink ETH/USD (~$1745) and `direction = "below"`. See M3 runbook §1. | Tune `trigger_price` lower to test the "silent until trigger" path. |
| `twap-monitor` never logs `watch:` | No `ConditionalOrderCreated` event observed on Sepolia during the window | Submit the TWAP order from section 2.3 step 1. |
| `ethflow-watcher` never logs `submitted:` | No `OrderPlacement` event observed on Sepolia during the window | Execute the EthFlow swap from section 2.3 step 2. |

---

## 5.5. Known upstream constraints on Sepolia

These are not bugs in shepherd; they are documented gaps between
the on-chain protocol and the Sepolia orderbook's validation
config. The strategy code recognises each and degrades gracefully
(Drop, not retry storm). The soak report should call them out so
the reader does not file them as anomalies.

### EthFlow `validTo = u32::MAX` → `ExcessiveValidTo`

EthFlow on-chain orders carry `validTo = type(uint32).max` by
design: cancellation is operator-controlled via the EthFlow
contract, not orderbook-time-bounded. `cowprotocol::eth_flow`
documents this as the canonical CoW-side shape on every chain.

The Sepolia orderbook's max-validTo cap rejects this shape with
`errorType = "ExcessiveValidTo"`. Every `POST /api/v1/orders`
ethflow-watcher forwards on Sepolia therefore terminates as
`Drop` (since the host fix; before that fix the same case
manifested as an infinite `backoff:` loop).

Operator-visible behaviour after the strategy refinement:

- `ethflow dropped <uid> (400): orderbook error (ExcessiveValidTo)...`
- Log level: **Info** (not Warn).
- `dropped:{uid}` marker written exactly once per placement.
- The soak's Prometheus
  `shepherd_cow_api_submit_total{outcome="err"}` curve grows by
  exactly the EthFlow placement count, then stops.

Upstream confirmation with the cowprotocol/services team is
pending; if mainnet also rejects this shape the design needs
revisiting at the contract level (which is out of scope for
shepherd).

---

## 6. References

- M2 runbook (sister doc): `docs/operations/m2-testnet-runbook.md`
- M3 runbook (sister doc): `docs/operations/m3-testnet-runbook.md`
- Engine config: `engine.e2e.toml`
- Report template: `docs/operations/e2e-reports/e2e-report.template.md`
