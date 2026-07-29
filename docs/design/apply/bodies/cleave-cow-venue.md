Split crates/cow-venue into two: an orderbook-only venue and a separate composable-cow keeper. Today lib.rs re-exports both OrderBody (order.rs) and ComposableBody (composable.rs) from the same crate.

## Why
The load-bearing rule is that the CoW venue is only the CoW orderbook: submit, quote, status, and cancel of an OrderBody on api.cow.fi, mapping orderbook errors to venue errors, with no knowledge of ComposableCoW, getTradeableOrderWithSignature, revert selectors, TWAP, or EthFlow. Mixing the composable keeper into the venue crate breaks that boundary and means a new CoW keeper cannot be written without dragging in composable machinery. This is a pure-Rust re-split with zero contract dependency, so it can land now. Part of milestone M4: CoW on the generic seam (the shepherd bundle). Blocked by: cow-onvidere-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Keep in crates/cow-venue: the orderbook only, OrderBody, the borsh codec, classification.toml/rs, and the orderbook client.
- Move to a separate composable-cow keeper crate/module: ComposableBody, the COMPOSABLE_COW address and topic-0, getTradeableOrderWithSignature, the revert-selector handling, LegacyRevertAdapter, and Verdict.
- Drop the Composable variant from the venue body.
- Add a CI gate that asserts the venue crate carries none of the composable symbols.
- Regenerate goldens.

## Done when
- crates/cow-venue contains only orderbook concerns: OrderBody, the borsh codec, classification.toml/rs, and the orderbook client.
- ComposableBody, composable.rs, getTradeableOrderWithSignature, COMPOSABLE_COW, ConditionalOrderCreated, the revert-selector handling, LegacyRevertAdapter, and Verdict live in a separate composable-cow keeper crate/module.
- A CI gate asserts the venue crate has zero Composable*, getTradeableOrder, or revert-selector symbols.
- A new CoW keeper producing OrderBodys can be written without importing composable machinery.
- The workspace is green and goldens are regenerated.
