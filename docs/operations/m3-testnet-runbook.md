# M3 testnet runbook (Sepolia)

How to exercise the M3 example modules - price-alert, balance-tracker,
stop-loss - on Sepolia. Same shape as the M2 runbook but the modules
are different:

- **price-alert** validates SDK `chain` helpers + Chainlink ABI decode.
  Read-only; no on-chain or orderbook action.
- **balance-tracker** validates SDK `chain::request` (raw RPC) +
  `local-store` per-key diff persistence. Read-only.
- **stop-loss** validates the full M3 surface: `chain::request` +
  `local-store` dedup + `cow-api::submit-order` with
  `Signature::PreSign`. Will attempt to submit a real CoW order to the
  Sepolia orderbook when the oracle price crosses the trigger.

In other words: M3 exercises the *strategy*-side SDK surface that M2
modules eventually consume. The runbook below validates everything in
~8 seconds of wall clock against the real Sepolia ETH/USD Chainlink
feed.

---

## 0. Prerequisites

- Same as the M2 runbook (Rust nightly + `wasm32-wasip2`, `just`
  optional, Sepolia RPC).
- For stop-loss to actually settle an order (not just submit and get
  rejected) you also need:
  - An EOA matching `[config] owner = ...` in
    `modules/examples/stop-loss/module.toml` that has called
    `setPreSignature(orderUid, true)` on the GPv2Settlement Sepolia
    contract for the computed UID.
  - That EOA holds + has approved enough of `sell_token` to settle.

  Without those, stop-loss will hit `TransferSimulationFailed` (or
  `InvalidSignature` / `InsufficientAllowance`) and log it as a
  retriable error or drop. **That outcome alone validates the
  orderbook round-trip** - same shape as the M2 EthFlow validation.

---

## 1. Smoke + active run

The M3 modules all subscribe to blocks only and start working
immediately - there is no `[[subscription]] kind = "log"` to wait for.
A single Sepolia block (~12 s) drives all three through their full
strategy.

```bash
just run-m3
```

Equivalent long form:

```bash
cargo build -p price-alert     --target wasm32-wasip2 --release
cargo build -p balance-tracker --target wasm32-wasip2 --release
cargo build -p stop-loss       --target wasm32-wasip2 --release
cargo run   -p nexum-engine -- --engine-config engine.m3.toml
```

### What you should see in the first ~10 seconds (observed)

```
INFO  nexum-engine starting
INFO  opening chain RPC provider chain_id=11155111 url="wss://..."
INFO  loading module manifest manifest=modules/examples/price-alert/module.toml
[manifest] required capabilities: logging, chain
INFO  compiling component component=...price_alert.wasm
INFO  price-alert init: oracle=0x694aa1769357215de4fac081bf1f309adc325306
      threshold=250000000000 direction=Below every_n_blocks=1
INFO  init succeeded module=price-alert
INFO  loading module manifest manifest=modules/examples/balance-tracker/module.toml
[manifest] required capabilities: logging, chain, local-store
INFO  compiling component component=...balance_tracker.wasm
INFO  balance-tracker init: 2 addresses, threshold=100000000000000000 wei
INFO  init succeeded module=balance-tracker
INFO  loading module manifest manifest=modules/examples/stop-loss/module.toml
[manifest] required capabilities: logging, chain, local-store, cow-api
INFO  compiling component component=...stop_loss.wasm
INFO  stop-loss init: owner=0x70997970c51812dc3a010c7d01b50e0d17dc79c8
      trigger=250000000000 sell=0x6810e776880c02933d47db1b9fc05908e5386b96
      buy=0xfff9976782d46cc05630d1f6ebab18b2324d6b14
INFO  init succeeded module=stop-loss
INFO  supervisor up count=3
INFO  supervisor ready modules=3 chains=1
INFO  block subscription open chain_id=11155111
```

Then on the FIRST Sepolia block dispatch (~5-15s after boot):

```
DEBUG chain::request chain_id=11155111 method=eth_call    # price-alert reads oracle
WARN  price-alert: TRIGGERED answer=174553978080 threshold=250000000000 (Below)
DEBUG chain::request chain_id=11155111 method=eth_getBalance  # balance-tracker addr 1
DEBUG chain::request chain_id=11155111 method=eth_getBalance  # balance-tracker addr 2
DEBUG chain::request chain_id=11155111 method=eth_call    # stop-loss reads oracle
DEBUG cow-api::submit-order chain_id=11155111 bytes=561
WARN  stop-loss retry on next block (0): orderbook error (TransferSimulationFailed):
      sell token cannot be transferred
```

That single block proves the entire M3 strategy surface end-to-end:
oracle read + ABI decode + multi-key local-store + cow-api submit +
typed retry classification, all routed through real wit-bindgen +
WitBindgenHost + supervisor dispatch on a live testnet.

### Why TRIGGERED fires immediately

The default `threshold = "2500.00"` in `module.toml::[config]` is
above the Sepolia Chainlink ETH/USD feed (which tracks a stale or
mocked value, often around $1745). Direction is `below`, so the very
first poll trips the alert. Tune `threshold` if you want to test the
"silent" path.

### Why stop-loss logs TransferSimulationFailed

The default `owner = 0x70997970...` in stop-loss's config is the
canonical hardhat test EOA (`anvil` account index 1). It does not own
or approve the `sell_token` on Sepolia, so the orderbook simulates
the would-be settle and rejects with
`TransferSimulationFailed`. **This is the orderbook returning a typed
error - the full submit path worked.** The module's
`classify_api_error` SDK helper correctly tagged it as retriable
(`TryNextBlock`), so the watch is left in place for the next block.

For the silent ("idle until trigger") run path, set `owner` to a real
EOA with the right allowances + pre-signature - see section 2 below.

---

## 2. Active validation (optional)

To see stop-loss actually submit + persist `submitted:{uid}` you need
to set up a real signed order:

1. Pick a Sepolia EOA you control.
2. In `modules/examples/stop-loss/module.toml`, set `owner = "0x..."`
   to that EOA.
3. Choose a `sell_token` / `buy_token` pair the EOA holds.
4. Compute the OrderUid the module will submit (the `build_creation`
   helper in `strategy.rs` shows the construction; you can also boot
   the engine once with a high trigger so it stays idle, then
   simulate-decode the would-be submit by reading the supervisor's
   debug log).
5. Call `GPv2Settlement.setPreSignature(uid, true)` from that EOA on
   Sepolia.
6. Approve `sell_token` to the GPv2VaultRelayer for the sell amount.
7. Lower the `trigger_price` in `module.toml` so the next poll fires.

On the next block:

```
INFO stop-loss TRIGGERED price=... trigger=...
DEBUG cow-api::submit-order ...
INFO stop-loss submitted submitted:0x<orderUid>
```

This is the M3 equivalent of the M2 EthFlow validation: same
end-to-end surface, different module.

---

## 3. State inspection

`./data/m3/ls.redb` accumulates the `last:{addr}` keys
(balance-tracker), `submitted:{uid}` / `dropped:{uid}` (stop-loss).
Same caveat as M2 - no `ls-dump` CLI today; reboot the engine on the
same `state_dir` and the supervisor logs every key it loads.

`rm -rf ./data/m3` between runs for a fresh slate.

---

## 4. What this does NOT prove

Same boundary as M2's section 4:

- Throughput / 7-day soak.
- Cross-module isolation under load (the 4-6 h E2E run).
- Adversarial resource exhaustion (M4 territory).
- Security review (M4 territory).
- `app_data` resolution for stop-loss orders with non-empty metadata
  -> M5 (typed `Cow` client with `raw_request`).

---

## 5. Troubleshooting

Most of the M2 runbook's section 5 applies verbatim. M3-specific:

| Symptom | Likely cause | Fix |
|---|---|---|
| `module stop-loss trapped: TransferSimulationFailed` | Trap vs warn confusion | The "sell token cannot be transferred" line is a Warn, not a trap. Module stays alive. Read again carefully. |
| Engine bails immediately with `log stream ended (WebSocket dropped?)` | Pre-fix M1 bug | Should not happen on this commit. The fix lands in `runtime/event_loop.rs`: `select_all` over empty `Vec` is replaced with `stream::pending()`. Regression test at `supervisor::tests::run_does_not_bail_when_both_stream_kinds_are_empty`. |
| `price-alert: TRIGGERED` does not fire | Oracle returned shape we cannot decode, or Sepolia public node throttled the `eth_call` | Check for `eth_call failed` warnings; switch to Alchemy. |
| `balance-tracker` only logs 1 of 2 addresses | RPC dropped a request mid-block | Same RPC throttle path; switch RPC. |

---

## 6. References

- M3 modules: `modules/examples/{price-alert,balance-tracker,stop-loss}/`
- SDK helpers exercised: `crates/shepherd-sdk/src/{chain,cow}/`
- ADR-0009 (host trait surface): `docs/adr/0009-host-trait-surface.md`
- M3 PRs in `bleu/nullis-shepherd`: #12-#26 (SDK + examples + tutorial + QA cleanup)
- M3 fix tail PRs: #27-#31 (CI matrix, rustdoc gate, doctests, supervisor integration, M2 runbook)
- M2 runbook (sister doc, same shape): `docs/operations/m2-testnet-runbook.md`
