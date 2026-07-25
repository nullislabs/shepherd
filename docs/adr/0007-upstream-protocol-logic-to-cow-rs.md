---
status: accepted
---

# Push CoW Protocol primitives to `cow-rs` first, adopt in the runtime second

## Context

When the runtime or its modules need CoW Protocol logic the `cowprotocol` crate does not yet expose, the choice is to write it locally and tidy up upstream later, or add it upstream first and land the wiring afterwards. Duplicating logic an existing crate could own is the anti-pattern to avoid. Protocol primitives (order types, signing schemes, orderbook errors) belong in `cowprotocol`; strategy implementations (TWAP polling, EthFlow decoding) stay in guest modules per ADR-0006.

## Decision

Protocol-level CoW logic, anything a non-`nexum` Rust consumer of the protocol would also need, lands in `cowprotocol` first and is consumed via the `[patch.crates-io]` rev bump (ADR-0004). The runtime never writes throwaway local copies with intent to port later.

The primitives this covers: `OrderPostError` rich variants plus `retry_hint`, classifying each submission error into try-next-block / backoff / drop; and `wasm32` compatibility (feature-gating `reqwest`) so guest modules use the pure types compiled to wasm without an HTTP client.

## Consequences

- The runtime repo stays free of CoW Protocol semantics: it holds WIT, host wiring, supervisor, redb store, provider pool, and the `engine.toml` schema.
- Guest modules consume `cowprotocol` types directly, gated on the wasm32 feature.
- `cowprotocol` now publishes to crates.io (workspace pins `0.2.0`); a `[patch.crates-io]` override to `nullislabs/cow-rs` remains active pending a release with the hash-only `OrderCreationAppData` constructor (ADR-0004).
