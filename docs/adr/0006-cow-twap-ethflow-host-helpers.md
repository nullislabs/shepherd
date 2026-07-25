---
status: superseded
---

# TWAP and EthFlow run as guest modules using low-level host primitives

> **Superseded by the videre venue-adapter architecture.** The strategies-as-guest-modules line holds, but the protocol seam it assigned to `shepherd:cow/cow-api` is retired: modules submit typed intent bodies through the `videre:venue/client` pool seam, and the `cow-venue` adapter owns the orderbook edge.

## Context

TWAP (over ComposableCoW) and EthFlow are strategies built on top of CoW Protocol primitives, not protocol concerns themselves. The dividing line is protocol vs implementation: order types, signing schemes, and the orderbook surface belong in shared layers; polling, submit timing, and error reactions are application logic. Putting that logic in the host or in `cowprotocol` would force every deployment onto one implementation and one error-handling policy.

## Decision

The host stays protocol-neutral: no `twap` or `ethflow` host interface. TWAP and EthFlow modules implement their logic in guest Rust over the universal host primitives (`chain`, `local-store`, `logging`) plus the venue submit seam, using `cowprotocol` crate types for `Order` / `OrderCreation` / `OrderUid` / signing schemes and `alloy_sol_types` for ABI decoding. Different polling strategies coexist as separate modules chosen via `engine.toml`'s `[[modules]]`.

## Consequences

- `KNOWN_CAPABILITIES` gains no `twap` or `ethflow` entry; modules declare only the universal capabilities they use.
- Modules ship larger because event decoding, `eth_call` orchestration, order construction, and error-hint handling live in guest code. This is the explicit trade-off: more code per module, less coupling.
- `OrderPostError::retry_hint` (ADR-0007) is the orderbook-submit contract shared across strategies. The poll mechanism itself is superseded by [ADR-0013](0013-composable-cow-structured-poll.md).
