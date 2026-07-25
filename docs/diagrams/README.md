# Diagrams

Mermaid sources and rendered PNGs covering the engine architecture, the CoW workflows (TWAP and EthFlow), and the engine internals that new contributors most often need to reason about.

> The rendered CoW-flow diagrams predate the venue-adapter rework: they show a `shepherd:cow/cow-api` host interface and an in-engine `OrderBookPool`. Order submission is now the `videre:venue` venue-adapter contract (the CoW venue is the `cow-venue` crate) and `shepherd:cow` carries only the `cow-events` enum. Treat the CoW-submission path in these diagrams as historical until the sources are regenerated.

## Architecture and CoW flows

| File | Type | Shows |
|---|---|---|
| `architecture.png` / `.mmd` | Component | Static view: external infra, nexum internals, WASM modules (twap-monitor, ethflow-watcher) consuming host primitives, and the `cowprotocol` crate (consumed via `[patch.crates-io]` and the wasm32 feature). |
| `sequence-ethflow.png` / `.mmd` | Sequence | `OrderPlacement` on-chain event handled in the `ethflow-watcher` guest module: `alloy_sol_types` decodes the event, the module builds an order with the EIP-1271 signing scheme using `cowprotocol` types, and submits it. |
| `sequence-twap.png` / `.mmd` | Sequence | `ConditionalOrderCreated` registration plus the per-block polling loop in the `twap-monitor` guest module: `alloy_sol_types` decodes registrations and `eth_call` returns, the module makes the `getTradeableOrderWithSignature` call via `chain.request`, builds the order via `cowprotocol` types, and submits it. |

## Engine internals (for contributors)

| File | Type | Shows |
|---|---|---|
| `module-lifecycle.png` / `.mmd` | State machine | Resolve → Load → Init → Run → Restart → Dead transitions and what triggers each. Documents the exponential-backoff restart policy and the implicit write transaction around `init`. |
| `engine-boot.png` / `.mmd` | Sequence | Boot order: engine.toml → tracing → ProviderPool → LocalStore → OrderBookPool → Supervisor (load each module) → open subscriptions → run event loop. |
| `wit-call-path.png` / `.mmd` | Sequence | One host call traced end-to-end: module Rust source → wit-bindgen stubs → WASM Component → wasmtime Linker → HostState trait impl → ProviderPool → alloy → Chain RPC, and back. Demystifies the WASM/Rust boundary. |
| `subscription-dispatch.png` / `.mmd` | Flow chart | How the supervisor aggregates `[[subscription]]` declarations across modules, opens shared block subscriptions (broadcast) and per-filter log subscriptions (routed), and dispatches events to the right `on_event` handlers. |

## Regenerate

```sh
cd docs/diagrams
for f in *.mmd; do
  npx -y @mermaid-js/mermaid-cli@latest -i "$f" -o "${f%.mmd}.png" -b white --width 1800
done
```

Mermaid sources are the source of truth; PNGs are committed for offline viewing and PR previews.
