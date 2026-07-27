# M2 testnet runbook (Sepolia)

Runs twap-monitor and ethflow-watcher on Sepolia against real `eth_subscribe` streams, `eth_call` reverts, and orderbook submissions.

Two flavours share the same boot:

1. Smoke run: boot the engine and watch it pick up every `ConditionalOrderCreated` / `OrderPlacement` log that lands on Sepolia. Passive, 15-30 min.
2. Round-trip run: the smoke run plus you author a TWAP order via a Sepolia Safe and an EthFlow swap via the CoW Swap UI, 1-2 h.

## 0. Prerequisites

- Rust toolchain matching `rust-toolchain.toml` (nightly with `wasm32-wasip2`). `rustup target add wasm32-wasip2` once.
- `just`.
- Sepolia RPC. The public endpoint in `engine.m2.toml` works for short runs; switch to Alchemy/Infura for anything past ~20 min.
- Round-trip only: a Sepolia EOA with test ETH, and a Sepolia Safe for the TWAP half.

## 1. Smoke run

```bash
just run-m2
```

Long form:

```bash
cargo build -p twap-monitor    --target wasm32-wasip2 --release
cargo build -p ethflow-watcher --target wasm32-wasip2 --release
cargo run   -p shepherd-engine -- --engine-config engine.m2.toml --pretty-logs
```

Expected boot (~5 s):

```
INFO nexum_runtime  nexum starting
INFO nexum_runtime::host::provider_pool  opening chain RPC provider chain_id=11155111 url="wss://..."
INFO nexum_runtime::supervisor  init succeeded module=twap-monitor
INFO nexum_runtime::supervisor  init succeeded module=ethflow-watcher
INFO nexum_runtime::supervisor  supervisor up count=2
INFO nexum_runtime  supervisor ready modules=2 chains=1
INFO nexum_runtime::runtime::event_loop  block subscription open chain_id=11155111
INFO nexum_runtime::runtime::event_loop  log subscription open module=twap-monitor chain_id=11155111
INFO nexum_runtime::runtime::event_loop  log subscription open module=ethflow-watcher chain_id=11155111
```

Then a `dispatch block` line every ~12 s (Sepolia block time).

Verify:

| Check | How |
|---|---|
| Both modules booted | `count=2` + 2 `init succeeded` lines |
| Subscriptions wired | 2 log subs + 1 block sub |
| No traps in the first 10 blocks | no `module ... trapped` lines |
| State persistence works | `ls data/m2/` shows `ls.redb` growing |

Ctrl-C to stop. Remove `./data/m2/` between runs for a fresh slate.

## 2. Round-trip run

Same boot; you produce the events.

### 2a. TWAP half (Safe + Compose)

ComposableCoW expects the conditional-order owner to be an EIP-1271 verifier, so the TWAP flow runs behind a Safe, not an EOA.

1. Create a Sepolia Safe at <https://app.safe.global/?chain=sep> (single signer with your EOA). Fund it with ~0.05 Sepolia ETH and ~10 of a Sepolia ERC-20 to sell.
2. Add the ComposableCoW Compose app (Safe -> Apps -> Add custom app, URL from the composable-cow README).
3. Author a TWAP order in the Compose UI: sell 1 test ERC-20, buy any Sepolia stable, 2 parts, 5-minute interval, 30-minute validity. Sign the Safe tx.
4. Within ~12 s of the tx confirming:
   ```
   INFO  twap-monitor  indexed watch:0x<safe>:0x<params_hash>
   INFO  twap-monitor  poll watch:... -> Ready
   INFO  twap-monitor  submitted submitted:0x<orderUid>
   ```
`TryAtEpoch(t)` instead of `Ready` means the tranche is gated until time `t`; wait the configured interval.
5. Confirm on the orderbook (settlement on Sepolia is spotty; reaching the orderbook is the goal):
   ```bash
   curl https://api.cow.fi/sepolia/api/v1/orders/0x<uid>
   ```

### 2b. EthFlow half (swap.cow.fi)

Any EOA works.

1. Go to <https://swap.cow.fi/#/11155111/swap/native>.
2. Connect the EOA, select a small swap (e.g. 0.001 SETH -> any token), confirm.
3. CoWSwapEthFlow (`0xbA3cB4...EadeC`) emits `OrderPlacement`. Expected log:
   ```
   INFO  ethflow-watcher  ethflow submitted 0x<orderUid>
   ```
`ethflow backoff 0x<uid>` means the orderbook classified the submit as retriable; wait one block. `ethflow dropped 0x<uid>` means a permanent rejection (commonly `DuplicateOrder`, since CoW Swap submits the order first and the watcher races it); the `dropped:{uid}` row is the expected marker.

Passing round-trip: at least one `submitted:{uid}` row per module in `data/m2/ls.redb`, both modules alive at the end, zero `trapped` lines, and `curl api.cow.fi/sepolia/api/v1/orders/<uid>` returns the order JSON for at least one UID.

## 3. Inspecting state after a run

The local store is a redb file with no standalone dump tool. Reboot the engine on the same `state_dir`: the supervisor logs every `watch:` / `submitted:` / `dropped:` row it loads.

## 4. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `connection refused` / WS retries | Public node throttled | Switch RPC to Alchemy / Infura in `engine.m2.toml` |
| `module twap-monitor trapped: OutOfFuel` | Dispatch exceeded fuel budget | File an issue; raise `[engine.limits]` fuel temporarily |
| `eth_call failed (rate limited)` repeatedly | Public node | Same as above |
| `ParseManifestError: missing capability cow-api` | Engine/module.toml version mismatch | `cargo build -p shepherd-engine --release` and use the fresh binary |
| `data/m2/ls.redb` not created | `state_dir` not writable | Check permissions or change `state_dir` in `engine.m2.toml` |

## 5. References

- Engine config schema: `nexum/crates/nexum-runtime/src/engine_config.rs`
- M2 modules: `shepherd/modules/twap-monitor/`, `shepherd/modules/ethflow-watcher/`
- ADR-0005 (cow-api routing), ADR-0006 (twap + ethflow helpers), ADR-0009 (host trait surface)
