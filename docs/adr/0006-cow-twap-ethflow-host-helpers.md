---
status: proposed
---

# TWAP and EthFlow run as guest modules using low-level host primitives (no specialised `shepherd:cow` interfaces)

## Context

TWAP (over ComposableCoW) and EthFlow are the two CoW workflows the M2 grant ships modules for. The natural-seeming approach is to add `shepherd:cow/twap` and `shepherd:cow/ethflow` WIT interfaces that the host implements on top of `cowprotocol` crate primitives, so modules would call `twap.poll-and-submit(...)` and `ethflow.submit-from-log(...)` as host functions. This ADR rejects that direction.

The dividing line is protocol vs implementation. CoW Protocol primitives - order types, signing schemes, the orderbook REST surface - are protocol concerns and belong in shared layers (`cowprotocol` crate, `shepherd:cow/cow-api` interface). TWAP is one of many strategies built _on top of_ those primitives; ComposableCoW is the contract surface a TWAP module observes, but the act of polling, deciding when to submit, and reacting to orderbook errors is application logic. Putting that application logic in the host or in `cowprotocol` couples every consumer to one implementation and one error-handling policy.

Embedding a concrete TWAP implementation in an SDK is an architectural smell the grant explicitly seeks to alleviate. The grant seeks to enable Shepherd as the runtime where many independent strategy implementations coexist, each compiled to its own WASM module. A specialised `twap` interface in the host would defeat that goal: every Shepherd deployment would have to use the same polling implementation, the same error-mapping, the same retry hints, with no room for different strategies to differ on those choices.

## Decision

The `shepherd:cow` WIT package contains only the existing `cow-api` interface (REST passthrough + `submit-order`), which is protocol-level. No `twap` interface, no `ethflow` interface, no host-side helpers specific to either workflow.

TWAP and EthFlow modules implement their logic in Rust guest code using:

- **`nexum:host/chain`** - `request` (for `eth_call`, `eth_getLogs`, etc.), `subscribe-blocks`, `subscribe-logs`.
- **`nexum:host/local-store`** - for watch lists, cursors, and backoff state.
- **`nexum:host/logging`** - for structured logs.
- **`shepherd:cow/cow-api`** - `submit-order` for orderbook submission.
- **`cowprotocol` crate** (consumed directly by the module, gated on the wasm32 feature work in ADR-0007) - for protocol types: `Order`, `OrderCreation`, `OrderUid`, signing schemes, `OrderPostError`, etc.
- **`alloy_sol_types`** (or equivalent) - for ABI-aware decoding of `ConditionalOrderCreated`, `OrderPlacement`, `getTradeableOrderWithSignature` return values, and similar Solidity-typed payloads.

Concretely, a TWAP module's `on_event(block)` handler iterates the local-store watch set, makes an `eth_call` to `ComposableCoW.getTradeableOrderWithSignature(owner, params, "", [])` via `chain.request`, decodes the return (or revert reason) with `alloy_sol_types`, constructs an `OrderCreation` with `cowprotocol` types, and submits via `cow-api/submit-order`. Orderbook errors are interpreted via `OrderPostError::retry_hint()` (ADR-0007). Backoff state is persisted to `local-store`. All of this lives in module Rust source, not in the engine.

An EthFlow module's `on_event(log)` handler decodes the `OrderPlacement` event with `alloy_sol_types`, constructs the `OrderCreation` (with the EIP-1271 signing scheme pointing at the `CoWSwapEthFlow` contract), and submits the same way. Module-side, no host helper required.

## Considered options

- **Specialised `shepherd:cow/twap` and `shepherd:cow/ethflow` interfaces** with rich `PollOutcome` variants and per-event host helpers, backed by `composable::poll_and_build_order` and `eth_flow::decode_placement` primitives in the `cowprotocol` crate. Rejected: this puts a single concrete TWAP / EthFlow implementation behind a WIT boundary, forcing every Shepherd deployment to use the same polling policy, the same error-mapping, the same retry hints. It also blurs the protocol-vs-implementation boundary the grant is meant to clarify. Multiple TWAP implementations (different polling cadences, different error tolerances, different cancel-on-loss thresholds) must be able to coexist as separate modules without changing the host or the SDK.
- **Move TWAP / EthFlow primitives into `cowprotocol` crate but skip the WIT interfaces**, leaving modules to call `composable::poll_and_build_order` from guest code. Rejected for the same reason: `cowprotocol` is the protocol SDK, not the strategy SDK. Putting TWAP logic there embeds an implementation in the shared layer, which is the smell the grant seeks to fix.
- **Ship a thin `shepherd-sdk` helper crate** that wraps the low-level primitive calls (eth_call, decode, submit) into a convenient `Twap::poll(...)` interface for guest modules. **Acceptable for M3** because the helper would live in guest-callable code, not behind a WIT boundary - a module that wants different polling policy just doesn't use the SDK helper. The host stays neutral.
- **EthFlow as pure passive observer (no submission)**. Rejected on closer read of `cowprotocol/services/crates/autopilot/src/database/onchain_order_events/ethflow_events.rs`: the canonical CoW flow expects the event to be relayed into the orderbook, which is what autopilot currently does internally. Shepherd's `ethflow-watcher` externalises that role, so the module does submit; just from guest code, not via a specialised host interface.
- **TWAP merkle-proof / `setRoot` support in v1.** Deferred. The 0.2 module only handles `ComposableCoW.create()` (empty proof, single conditional order). `setRoot` polling requires off-chain proof derivation; when a real module needs it, it will be implemented in guest code using the same low-level primitives, possibly with an SDK helper to encapsulate the proof bookkeeping.

## Consequences

- `shepherd:cow@0.2.0` keeps `cow-api` as its only interface. No new WIT files in this ADR.
- `KNOWN_CAPABILITIES` in `crates/nexum-engine/src/manifest.rs` does **not** gain `"twap"` or `"ethflow"` entries. Modules declare the universal capabilities they actually use: `chain`, `local-store`, `logging`, `cow-api`.
- Modules ship larger (~150 LOC each estimated, up from the ~30 LOC the host-helper design implied), because event decoding, eth_call orchestration, OrderCreation construction, and error-hint interpretation now live in guest code. This is the explicit trade-off: more code per module, less coupling, more freedom for different strategies to coexist.
- Different TWAP polling strategies can coexist as different modules. Operators choose which to load via `engine.toml`'s `[[modules]]` array.
- The watch-tower TypeScript implementation remains the closest reference for what a TWAP module's logic looks like, but it is reference material, not a template the Rust module mirrors verbatim. A newer ComposableCoW iteration in development may simplify the polling surface significantly; the relevant decisions live in the module, not the host.
- `OrderPostError` rich variants + `retry_hint()` (ADR-0007 item 1, formerly item 3) become the primary protocol-level contract between the orderbook and any module submitting orders. Modules `match` on the typed error and apply the `RetryHint` (try-next-block / backoff-seconds / drop). This logic is generic across TWAP, EthFlow, stop-loss, and any future strategy.
- The M3 SDK (`shepherd-sdk` crate) is the natural home for ergonomic guest-side helpers: `WatchSet`, `PollLoop`, `BackoffLedger`, decode-and-submit utilities. The SDK is opt-in for module authors and lives entirely on the guest side; the host remains protocol-neutral.
- The architecture and sequence diagrams in `docs/diagrams/` that depict `twap.poll-and-submit` and `ethflow.submit-from-log` host calls reflect the rejected design and must be updated to show modules calling low-level primitives directly.

_Errata: `crates/nexum-engine` was renamed to `crates/nexum-runtime` + `crates/nexum-cli` in the 0.2 refactor._
