---
status: proposed
---

# Push CoW Protocol primitives to `cow-rs` first, adopt in `nexum-engine` second

## Context

Implementing ADR-0005 (cow-api backend) and supporting guest-side TWAP / EthFlow modules per ADR-0006 surfaces a recurring question: when the engine or its modules need a piece of CoW Protocol logic that the `cowprotocol` Rust SDK does not yet expose (rich orderbook error variants, custom orderbook URLs, wasm32 compatibility), do we write that logic locally and tidy it up upstream later, or do we add it to the open upstream PR first and only land the engine wiring afterwards?

The failure mode is well-known: duplicating work that an existing crate could do is the AI-coding anti-pattern most likely to land in a contribution. The same risk applies to any engine-side reimplementation of protocol logic.

The line between **protocol primitives** (which belong in `cowprotocol`) and **strategy implementations** (which belong in guest modules, per ADR-0006) is the operating principle. This ADR covers only the protocol-primitive additions; TWAP polling and EthFlow event decoding stay in guest modules and are explicitly **not** primitives we push to `cowprotocol`.

## Decision

Protocol-level CoW logic - anything that an indexer, a bot, or a non-`nexum` Rust consumer of CoW Protocol would also need to interact with the protocol - lands as additional commits on `cowdao-grants/cow-rs` PR #5 first (head branch `bleu/cow-rs:main`), and is consumed by `nexum-engine` and by guest modules via the `[patch.crates-io]` rev bump (ADR-0004). The engine and the modules never write throwaway local copies of the same logic with the intent to "port later".

The concrete set of primitives this ADR commits to upstream, in priority order:

1. **`cowprotocol::OrderPostError` rich variants + `retry_hint(&self) -> RetryHint`** - typed orderbook submission errors (`QuoteNotFound`, `InvalidQuote`, `InsufficientAllowance`, `InsufficientBalance`, `TooManyLimitOrders`, `InvalidAppData`, `AppDataFromMismatch`, `SellAmountOverflow`, `ZeroAmount`, `TransferSimulationFailed`, `ExcessiveValidTo`, …) with a `retry_hint()` helper classifying each into `TryNextBlock`, `BackoffSeconds(u64)`, or `Drop`. Mirrors watch-tower's `API_ERRORS_TRY_NEXT_BLOCK` / `API_ERRORS_BACKOFF` / `API_ERRORS_DROP` tables. Without this, every Rust consumer of CoW reinvents the same mapping, and modules spam the orderbook with permanently-broken orders. **Critical-path, not optional.**

2. **`cowprotocol::OrderBookApi::with_base_url(chain_id, base_url)`** - custom-URL constructor for barn / staging / forked deployments. Unblocks per-chain orderbook URL overrides in `engine.toml` (ADR-0005).

3. **`cowprotocol` `wasm32` compatibility** - feature-gate the `reqwest` dependency so guest modules can use the pure types (`Order`, `OrderCreation`, `OrderUid`, signing schemes, error variants) without dragging in an HTTP client. **Critical for ADR-0006**: modules implement TWAP and EthFlow logic in guest code and need `cowprotocol` types compiled to wasm32. Without this, guest modules fall back to duplicating type definitions.

Lower-priority follow-ons (`OrderUid::from_slice`, retry middleware on `OrderBookApi`, `OrderCreation::from_gpv2`) are good-to-have but are not blocking for the M2 host or module scope.

## Considered options

- **Implement locally, refactor upstream later.** Faster short term but predictably leaves an indeterminate amount of duplicated logic in the engine, contradicts the conventions established on cow-rs PR #5, and grows technical debt every time cow-rs evolves the underlying types. Rejected.
- **Push TWAP / ComposableCoW primitives** (`composable::poll_and_build_order`) into `cowprotocol`. Rejected: TWAP is a concrete strategy on top of the protocol, not part of the protocol. Putting it in the SDK forces every consumer to use one polling implementation and one error-mapping policy. Per ADR-0006, TWAP polling lives in guest module code, not in shared layers.
- **Push EthFlow log-decoding primitives** (`eth_flow::decode_placement`) into `cowprotocol`. **Rejected for the same reason**: EthFlow event decoding is an implementation detail of how a particular module relays orders into the orderbook. The protocol layer defines the order types and the orderbook submission endpoint; the act of decoding an on-chain event into an `OrderCreation` is module-side logic. Modules decode `OrderPlacement` directly with `alloy_sol_types` and construct the `OrderCreation` with the EIP-1271 signing scheme.
- **Wait for cow-rs upstream maintainers to add these on their own.** No evidence anyone else is doing this work; the grant timeline does not permit waiting.
- **Vendor a fork of cow-rs inside `nullislabs/shepherd`.** Worst of all worlds: blocks neither the engine nor cow-rs from drifting, and forces every other CoW consumer to re-derive the same primitives.
- **Host-side `AppDataResolver` (LRU cache + GET against `/api/v1/app_data/{hash}`).** Rejected after verifying watch-tower's behavior: it never fetches app-data. The trader uploads the JSON to the orderbook via `PUT /api/v1/app_data/{hash}` separately; the relayer module just submits and reacts to `INVALID_APP_DATA` (backoff 1 min) / `APPDATA_FROM_MISMATCH` (drop) via the error map in item 1 above.

## Consequences

- Every M2 engine or module issue that consumes one of the three primitives above is blocked on the corresponding commit landing in PR #5's head branch. Items 1, 2, 3 can be authored as independent commits and pushed in parallel rather than serially.
- `[patch.crates-io]` rev in the workspace `Cargo.toml` (ADR-0004) is bumped after each push to PR #5; the bump is the engine's signal that a new primitive is consumable.
- Commits added to PR #5 follow its established conventions: alloy reuse over local reimplementation, GPL-3.0, edition 2024, terse rustdoc.
- The engine repo stays small: `nexum-engine` contains WIT, host wiring, supervisor, redb store, alloy provider pool, and `engine.toml` schema, with nothing about CoW Protocol semantics.
- Guest modules consume `cowprotocol` types directly (gated on the wasm32 feature in item 3). The `shepherd-sdk` crate in M3 may add ergonomic wrappers on top, but those live on the guest side, not behind a WIT boundary.
- A follow-on Bleu module - the Rust-side equivalent of `cowprotocol/refunder` (permissionless `invalidateOrder` triggering for expired EthFlow orders) - becomes natural to ship once an ethflow-watcher module lands. Out of scope for M2 but explicitly enabled by the same primitives.
- TWAP polling logic (decode `ConditionalOrderCreated`, eth_call `getTradeableOrderWithSignature`, decode return, build `OrderCreation`) and EthFlow event decoding stay entirely in guest module code. The `cowprotocol` crate provides only the types and the orderbook client; the strategy is the module's.

_Errata: `crates/nexum-engine` was renamed to `crates/nexum-runtime` + `crates/nexum-cli` in the 0.2 refactor. The `cowprotocol` crate now publishes to crates.io (the workspace pins `0.2.0`), so the PR-#5-head consumption model above is historical; a `[patch.crates-io]` git override to `nullislabs/cow-rs` remains active pending a release with the hash-only `OrderCreationAppData` constructor - see the comment above the patch block in the root `Cargo.toml` for the current state._
