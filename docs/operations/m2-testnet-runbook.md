# M2 testnet runbook (Sepolia)

How to actually run the M2 modules - twap-monitor and ethflow-watcher -
on Sepolia and exercise the full path the unit tests cannot: real
`eth_subscribe` streams, real `eth_call` reverts, real orderbook
submissions.

Two flavours:

1. **Smoke run**: boot the engine, watch the supervisor pick up every
   `ConditionalOrderCreated` / `OrderPlacement` log that lands on
   Sepolia. Passive; you do not produce traffic. 15-30 min wall clock.
2. **Round-trip run**: smoke run plus you author a TWAP order via a
   Sepolia Safe and an EthFlow swap via the public CoW Swap UI. The
   engine indexes / decodes / submits. 1-2 h.

Both share the same boot. The round-trip is the smoke run with a hand
on the wheel.

---

## 0. Prerequisites

- Rust toolchain matching `rust-toolchain.toml` (nightly with
  `wasm32-wasip2` target). `rustup target add wasm32-wasip2` once.
- `just` (`cargo install just` or `brew install just`).
- Sepolia RPC. Public endpoint in `engine.m2.toml` works for short
  runs; switch to Alchemy/Infura with a key for anything past ~20 min.
- For the round-trip:
  - A Sepolia EOA with some test ETH ([Alchemy faucet](https://sepoliafaucet.com)).
  - A [Sepolia Safe](https://app.safe.global/?chain=sep) (only for the
    TWAP half).

---

## 1. Smoke run

```bash
just run-m2
```

Equivalent long form:

```bash
cargo build -p twap-monitor    --target wasm32-wasip2 --release
cargo build -p ethflow-watcher --target wasm32-wasip2 --release
cargo run   -p nexum-cli -- --engine-config engine.m2.toml
```

### What you should see in the first ~5 seconds (observed)

```
INFO nexum_runtime  nexum starting
INFO nexum_runtime::host::provider_pool  opening chain RPC provider chain_id=11155111 url="wss://..."
INFO nexum_runtime::supervisor  loading module manifest manifest=modules/twap-monitor/module.toml
[manifest] required capabilities: logging, local-store, chain, cow-api
INFO nexum_runtime::supervisor  compiling component component=target/wasm32-wasip2/release/twap_monitor.wasm
INFO nexum_runtime::host::impls::logging  twap-monitor init module="twap-monitor"
INFO nexum_runtime::supervisor  init succeeded module=twap-monitor
INFO nexum_runtime::supervisor  loading module manifest manifest=modules/ethflow-watcher/module.toml
[manifest] required capabilities: logging, local-store, chain, cow-api
INFO nexum_runtime::supervisor  compiling component component=target/wasm32-wasip2/release/ethflow_watcher.wasm
INFO nexum_runtime::host::impls::logging  ethflow-watcher init module="ethflow-watcher"
INFO nexum_runtime::supervisor  init succeeded module=ethflow-watcher
INFO nexum_runtime::supervisor  supervisor up count=2
INFO nexum_runtime  supervisor ready modules=2 chains=1
INFO nexum_runtime::runtime::event_loop  block subscription open chain_id=11155111
INFO nexum_runtime::runtime::event_loop  log subscription open module=twap-monitor chain_id=11155111
INFO nexum_runtime::runtime::event_loop  log subscription open module=ethflow-watcher chain_id=11155111
```

Then every ~12s (Sepolia block time):

```
INFO nexum_runtime::runtime::event_loop  dispatch block chain_id=11155111 number=N
```

### What to verify

| Check | How |
|---|---|
| Both modules booted | `module_count: 2` + 2 `loaded module` lines |
| Subscriptions wired | 2 log subs + 1 block sub |
| No traps in the first 10 blocks | `alive: 2` stays at 2; no `module ... trapped` lines |
| State persistence works | `ls data/m2/` shows `ls.redb` growing |

### Stopping cleanly

Ctrl-C. Tear down `./data/m2/` between runs if you want a fresh slate.

### Common surprises

- **Public RPC throttles after a few minutes.** Symptom: `eth_subscribe`
  reconnects in a loop. Fix: switch to Alchemy/Infura. Edit the
  `[chains.11155111]` block in `engine.m2.toml` (env-substitution is
  not wired yet).
- **You see `eth_call failed (...); defaulting to TryNextBlock`.** This
  is twap-monitor polling watches that are still empty (no
  `ConditionalOrderCreated` indexed yet). Expected on a fresh `./data/m2`.
- **You see NO log dispatches for hours.** Sepolia has low ComposableCoW
  / EthFlow traffic. The smoke run is mostly a "stay alive" test until
  you produce events yourself (see round-trip below).

---

## 2. Round-trip run

Same boot as #1; you produce the events.

### 2a. TWAP half (via Safe + Compose)

The TWAP flow lives behind a Safe, not an EOA, because ComposableCoW
expects the conditional-order owner to be an EIP-1271 verifier.

1. **Create a Sepolia Safe** at <https://app.safe.global/?chain=sep>.
   Single signer with your EOA is fine. Fund it with ~0.05 Sepolia
   ETH (gas) and ~10 of a Sepolia ERC-20 you want to sell.
2. **Install the Compose app** in the Safe. CoW Protocol publishes the
   ComposableCoW Watch Tower as a Safe app on Sepolia.
   - In Safe -> Apps -> Add custom app: use the URL from
     <https://github.com/cowprotocol/composable-cow> README ("Add to
     Safe").
3. **Author a TWAP order**. Compose UI -> "TWAP". Recommended for the
   first run:
   - Sell: 1 of your test ERC-20.
   - Buy: any Sepolia stable.
   - Split into 2 parts, 5-minute interval, validity 30 min.
   - Confirm + sign the Safe tx.
4. **Watch the engine logs.** Within ~12s of the Safe tx confirming,
   you should see:
   ```
   INFO  twap-monitor  indexed watch:0x<safe>:0x<params_hash>
   ```
   Then on the next blocks where the tranche is ready:
   ```
   INFO  twap-monitor  poll watch:... -> Ready
   INFO  twap-monitor  submitted submitted:0x<orderUid>
   ```
   Sometimes you see `TryAtEpoch(t)` instead of `Ready` - that means
   the tranche is gated until time `t`. Wait the configured interval.
5. **Confirm on the orderbook.** Get the UID from the log, then:
   ```bash
   curl https://api.cow.fi/sepolia/api/v1/orders/0x<uid>
   ```
   You should see the order JSON back. Trade settlement on Sepolia is
   spotty (solvers do not always pick up); the goal of this test is
   that the order reached the orderbook, not that it filled.

### 2b. EthFlow half (via swap.cow.fi)

EthFlow does not need a Safe - any EOA works.

1. Go to <https://swap.cow.fi/#/11155111/swap/native> (Sepolia native
   ETH selector).
2. Connect your EOA, select a small swap (e.g. 0.001 SETH -> any
   token), confirm.
3. The CoWSwapEthFlow contract on Sepolia
   (`0xbA3cB4...EadeC`) emits `OrderPlacement`.
4. **Watch the engine logs:**
   ```
   INFO  ethflow-watcher  ethflow submitted 0x<orderUid>
   ```
   If you see `ethflow backoff 0x<uid> ...` instead: orderbook
   classified the submit as retriable. Wait one block, the watcher
   does not retry on its own today (planned for M4 supervisor
   restart wiring).

   If you see `ethflow dropped 0x<uid> ...`: orderbook rejected
   permanently (most likely `DuplicateOrder` - CoW Swap submits the
   order itself first, ethflow-watcher races and loses). Expected; the
   `dropped:{uid}` row is the regression guard, not the
   failure signal here.

### What "passing M2 round-trip" looks like

- At least one `submitted:{uid}` row in `data/m2/ls.redb` written by
  each module.
- Both modules still alive (`alive: 2`) at the end of the run.
- Zero `module ... trapped` lines in the engine log.
- `curl api.cow.fi/sepolia/api/v1/orders/<uid>` returns the order JSON
  for at least one submitted UID (`null` means the orderbook never
  accepted; non-null means we round-tripped).

---

## 3. Inspecting state after a run

The local-store is a redb file. Quick inspection without writing a
tool:

```bash
# Build the example mini-CLI the engine ships
cargo run -p nexum-cli --bin ls-dump -- data/m2/ls.redb 2>/dev/null \
  || echo "no ls-dump bin in 0.2 - read via the engine on next boot"
```

Today the canonical way to read the store is to boot the engine again
on the same `state_dir`: the supervisor logs every `watch:` /
`submitted:` / `dropped:` row it loads. A proper inspector is
production-hardening scope (M4).

---

## 4. What this run does NOT prove

- **Throughput / soak stability**. That is the 7-day soak.
- **Cross-module isolation under load**. That is the 4-6h
  multi-module E2E run. The local-store namespace test guarantees the
  invariant in unit; the runbook above is a single-Safe / single-EOA
  setup.
- **Resource-limit enforcement under adversarial guests**. Fuel + memory tests (M4 territory).
- **Security review**. Tracked separately (M4 territory).

The M2 runbook covers: "does the engine actually boot the two M2
modules end-to-end against Sepolia, route real subscription events
through the wit-bindgen + WitBindgenHost path, and round-trip orders
to the CoW orderbook". That is the deliverable M2 is responsible for.

---

## 5. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `connection refused` / WS retries | Public node throttled | Switch RPC to Alchemy / Infura |
| `module twap-monitor trapped: OutOfFuel` | Dispatch path exceeded fuel budget | Almost certainly an upstream issue, file as a separate issue; raise `[engine.limits]` fuel temporarily |
| `eth_call failed (rate limited)` repeatedly | Public node | Same as above |
| `ParseManifestError: missing capability cow-api` | Engine version mismatch with module.toml | `cargo build -p nexum-cli --release` and use the fresh binary |
| `data/m2/ls.redb` not created | `state_dir` not writable | Check permissions, or change `state_dir` in `engine.m2.toml` |

---

## 6. References

- Engine config schema: `crates/nexum-runtime/src/engine_config.rs`
- M2 modules: `modules/twap-monitor/`, `modules/ethflow-watcher/`
- ADR-0005 (cow-api routing): `docs/adr/0005-cow-api-via-cached-orderbookapi.md`
- ADR-0006 (twap + ethflow helpers): `docs/adr/0006-cow-twap-ethflow-host-helpers.md`
- ADR-0009 (host trait surface): `docs/adr/0009-host-trait-surface.md`
- M2 PRs in `bleu/nullis-shepherd`: #2-#11
