---
status: proposed
---

# ComposableCoW poll is a structured non-reverting verdict; the module is a generic handler-agnostic monitor

## Context

ADR-0006 decided that TWAP and EthFlow run as guest modules over low-level host primitives, with the host protocol-neutral and no specialised `shepherd:cow/twap` interface. That decision stands. What ADR-0006 baked in, and what this ADR supersedes, is the poll *mechanism*: a module calling `ComposableCoW.getTradeableOrderWithSignature(owner, params, "", [])`, decoding the return *or the revert reason* with `alloy_sol_types`, mapping the revert selectors to a rich off-chain `PollOutcome`, and computing its own poll schedule (the TWAP epoch gates). ADR-0006 itself anticipated this change: "A newer ComposableCoW iteration in development may simplify the polling surface significantly."

That iteration exists. The nullisLabs composable-cow fork (`/code/nullisLabs/composable-cow`, `docs/architecture.md`) splits settlement from polling and makes the polling path structured and non-reverting. `getTradeableOrderWithSignature()` and `checkOrder()` now return a `PollResult { GeneratorResult generator; FillStatus fill; uint filledAmount; Restriction restriction }`. The handler's `poll()` (via `BaseConditionalOrder.poll()`) wraps `generateOrder()` in a try/catch on-chain and returns a `GeneratorResult { code: POST | WAIT_TIMESTAMP | WAIT_BLOCK | TRY_NEXT_BLOCK | INVALID | NEEDS_INPUT; order; nextPollTimestamp; waitUntil; bytes4 reasonCode }`. The revert-decode moved on-chain; the schedule is supplied by the contract (`nextPollTimestamp` sentinels: `0` = poll at `validTo + 1`, `type(uint256).max` = stop); the fill and restriction overlays are composed by the registry, orthogonal to the verdict.

The consequence is that a monitor no longer needs any handler-specific off-chain logic. TWAP's epoch arithmetic is on-chain in `getNextPollTimestamp()`; the off-chain side reads a struct field. One generic poller drives every handler (TWAP, StopLoss, GoodAfterTime, PerpetualStableSwap, TradeAboveThreshold): none of shepherd's flagship roster emits `PollNeedsOffchainInput`, so none needs an order-module sandbox.

The blocker is deployment. The fork is `abiVersion 2.0.0-dev`, `deployments/networks.json` is `networks: {}`, and every chain shepherd targets has only the upstream reverting `ComposableCoW`. The poll wire cannot retarget to a contract that is not on-chain.

## Decision

The CoW module is a generic, handler-agnostic ComposableCoW monitor (a `ccow-monitor`), not a TWAP-specific strategy. It indexes `ConditionalOrderCreated`, polls each authorised order, and switches on `GeneratorResult.code` and nothing else: `POST` (with a signature emitted) submits the order through `nexum:intent/pool`; `WAIT_TIMESTAMP` / `WAIT_BLOCK` reschedule at `waitUntil`; `TRY_NEXT_BLOCK` re-polls; `INVALID` drops the watch; `NEEDS_INPUT` hands to the offchainInput layer or parks. It decodes no revert selectors, computes no schedule, and holds no per-handler branch. TWAP survives as a watch scope and golden vectors, not as code.

The chassis (ADR-0009) supplies the mechanism: the watch-set and journal stores are unchanged; the two-gate store (`next_block:` / `next_epoch:`) now holds the contract-supplied `waitUntil` / `nextPollTimestamp` slots, so its value source moves from off-chain revert-decode to the contract verdict while its shape is unchanged. The `ConditionalSource` seam returns a structured `Verdict` mirroring `GeneratorResultCode` plus the hints; it does not decode or schedule.

Two classification concerns stay separate: the poll verdict comes from the contract (`GeneratorResultCode` + `bytes4 reasonCode`, log only), while the CoW orderbook-API submit-error `errorType` table (the REST POST `/api/v1/orders` response) is a distinct data-driven concern that the module keeps.

Migration is HYBRID and gated on deployment:

- Migrate the Rust verdict seam now (zero contract dependency): the `ConditionalSource::Outcome` becomes the structured `Verdict`, and the sweep dispatches on it.
- Quarantine the deployed 1.x reverting poll behind a named `LegacyRevertAdapter` that maps the five upstream selectors to the `Verdict` (`PollTryAtEpoch` to `WAIT_TIMESTAMP`, `PollTryNextBlock` to `TRY_NEXT_BLOCK`, `PollNever` / `OrderNotValid` to `INVALID`). The module posts through the target seam against the deployed contract, so the grant demo stays green.
- When the fork's `deployments/networks.json` is non-empty on a shepherd target chain, replace the adapter with a direct `PollResult` struct-read, delete the adapter and `shepherd-sdk/src/cow/composable.rs`, and regenerate golden vectors against `bytes4 reasonCode` and `PollResult`.

Merge gate: the poll retarget (`shepherd-sdk/src/cow/composable.rs`, `modules/*/src/strategy.rs`, and the acceptance of this ADR over the reverting model) must not merge until `deployments/networks.json` is non-empty on a shepherd target chain.

## Considered options

- **Redirect now: retarget the poll to the fork's structured surface immediately.** Rejected. The fork is deployed on no chain shepherd targets; every `eth_call` would revert selector-not-found, and shepherd cannot deploy a third party's registry on its own clock. This regresses grant M2 from "posts orders on testnet" to "compiles against an undeployed ABI."
- **Land-then-migrate: merge the M1 train on the old model, migrate wholesale after the grant.** Rejected as the default because it ships a public verdict seam (`PollOutcome`) that is immediately broken by the target, forcing a re-port of the consumer. The seam type carries no contract dependency, so it is migrated now for free.
- **Keep the off-chain revert-decode as the permanent model.** Rejected. It duplicates on-chain the decode the fork does once, per-handler, and it is the source of the handler-specific off-chain logic this ADR removes.

## Consequences

- `shepherd-sdk/src/cow/composable.rs` (the five-selector `sol!` mirror, `decode_revert`, `classify_poll_error`, the `PollOutcome` enum) is deleted at the wire-swap; its replacement is a struct-read, not a re-port. Until then it survives only as the `LegacyRevertAdapter`'s guts.
- The `twap-monitor` module generalises into `ccow-monitor`; the TWAP-specific poll body (`poll_one`, `decode_return`, `classify_poll_error`, the module-side `TryAtEpoch` / `TryOnBlock` gates) is removed. Existing MockHost tests remain the behaviour-identity proof through the seam migration.
- The chassis two-gate store is re-documented: `next_block:` and `next_epoch:` are contract-supplied `waitUntil` and `nextPollTimestamp` slots, not off-chain epoch math.
- New capabilities land as follow-ons and are the only places a per-handler off-chain concern reappears: `IOrderModule` for `NEEDS_INPUT` offchainInput acquisition, `IOrderManifest` enumeration with `offchainInput = abi.encode(index)` fan-out, and merkle-payload discovery. None is needed for the flagship roster.
- The rename set threads through the module and its vectors: `getTradeableOrder` to `generateOrder`; `PollTryAtEpoch(uint256, string)` to `PollTryAtTimestamp(uint256, bytes4)`; `PollNever` removed in favour of `OrderNotValid`; `getTradeableOrderWithSignature` returns `PollResult`, not `(order, bytes)`; string reason payloads become `bytes4 reasonCode`; the poll no longer reverts for order conditions.
- `docs/diagrams/sequence-twap.mmd` (already flagged in ADR-0006's consequences) is updated to show the structured poll: one `checkOrder` call returning a verdict and hints, no revert path.
- ADR-0006's host-neutrality decision is unchanged. This ADR supersedes only its poll-mechanism description. ADR-0007's `OrderPostError::retry_hint()` remains the orderbook-submit contract; it was never the poll contract.
