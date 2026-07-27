# M3 testnet runbook (Sepolia)

Exercises the example modules price-alert, balance-tracker, and stop-loss on Sepolia:

- price-alert: SDK `chain` helpers + Chainlink ABI decode. Read-only.
- balance-tracker: SDK `chain::request` (raw RPC) + `local-store` per-key diff persistence. Read-only.
- stop-loss: `chain::request` + `local-store` dedup + `cow-api::submit-order` with `Signature::PreSign`. Submits a real CoW order to the Sepolia orderbook when the oracle price crosses the trigger.

All three subscribe to blocks only and start working immediately; a single Sepolia block (~12 s) drives each through its full logic.

## 0. Prerequisites

- Same as the M2 runbook (Rust nightly + `wasm32-wasip2`, `just`, Sepolia RPC).
- For stop-loss to settle (not just submit and get rejected):
  - An EOA matching `[config] owner` in `modules/examples/stop-loss/module.toml` that has called `setPreSignature(orderUid, true)` on the GPv2Settlement Sepolia contract for the computed UID.
  - That EOA holds and has approved enough `sell_token` to settle.

Without those, stop-loss hits `TransferSimulationFailed` (or `InvalidSignature` / `InsufficientAllowance`) and logs it as a retriable error or drop. That outcome still validates the orderbook round-trip.

## 1. Smoke + active run

```bash
just run-m3
```

Long form:

```bash
cargo build -p price-alert     --target wasm32-wasip2 --release
cargo build -p balance-tracker --target wasm32-wasip2 --release
cargo build -p stop-loss       --target wasm32-wasip2 --release
cargo run   -p shepherd -- --engine-config engine.m3.toml --pretty-logs
```

Expected boot (~10 s):

```
INFO  nexum starting
INFO  init succeeded module=price-alert
INFO  init succeeded module=balance-tracker
INFO  init succeeded module=stop-loss
INFO  supervisor ready modules=3 chains=1
INFO  block subscription open chain_id=11155111
```

On the first Sepolia block dispatch (~5-15 s after boot):

```
DEBUG chain::request method=eth_call        # price-alert reads oracle
WARN  price-alert: TRIGGERED answer=174553978080 threshold=250000000000 (Below)
DEBUG chain::request method=eth_getBalance   # balance-tracker addr 1
DEBUG chain::request method=eth_getBalance   # balance-tracker addr 2
DEBUG chain::request method=eth_call        # stop-loss reads oracle
DEBUG cow-api::submit-order bytes=561
WARN  stop-loss retry on next block (0): orderbook error (TransferSimulationFailed): sell token cannot be transferred
```

That block proves the M3 module surface end-to-end: oracle read + ABI decode + multi-key local-store + cow-api submit + typed retry classification.

Why TRIGGERED fires immediately: the default `trigger_price` in `module.toml` is above the Sepolia Chainlink ETH/USD feed (a stale or mocked value), and `direction = below`, so the first poll trips. Raise `trigger_price` to test the silent path.

Why stop-loss logs TransferSimulationFailed: the default `owner` does not own or approve `sell_token` on Sepolia, so the orderbook simulates the settle and rejects with a typed error. The `classify_api_error` SDK helper tags it retriable (`TryNextBlock`) and leaves the watch for the next block.

## 2. Active validation (optional)

To see stop-loss submit and persist `submitted:{uid}`:

1. Set `owner` in `modules/examples/stop-loss/module.toml` to a Sepolia EOA you control.
2. Choose a `sell_token` / `buy_token` pair the EOA holds.
3. Compute the OrderUid (see `build_creation` in `keeper.rs`).
4. Call `GPv2Settlement.setPreSignature(uid, true)` from that EOA.
5. Approve `sell_token` to the GPv2VaultRelayer for the sell amount.
6. Lower `trigger_price` so the next poll fires.

On the next block:

```
INFO stop-loss TRIGGERED price=... trigger=...
INFO stop-loss submitted submitted:0x<orderUid>
```

## 3. State inspection

`./data/m3/ls.redb` accumulates `last:{addr}` (balance-tracker) and `submitted:{uid}` / `dropped:{uid}` (stop-loss). No standalone dump tool: reboot the engine on the same `state_dir` and the supervisor logs every key it loads. `rm -rf ./data/m3` for a fresh slate.

## 4. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `module stop-loss trapped: TransferSimulationFailed` | Trap vs warn confusion | `sell token cannot be transferred` is a Warn, not a trap; the module stays alive. |
| `price-alert: TRIGGERED` does not fire | Undecodable oracle shape, or throttled `eth_call` | Check for `eth_call failed`; switch to Alchemy. |
| `balance-tracker` logs only 1 of 2 addresses | RPC dropped a request mid-block | Switch RPC. |

## 5. References

- M3 modules: `modules/examples/{price-alert,balance-tracker,stop-loss}/`
- SDK chain helpers: `nexum/crates/nexum-sdk/src/chain/`
- ADR-0009 (host trait surface)
- M2 runbook (sister doc): `docs/operations/m2-testnet-runbook.md`
