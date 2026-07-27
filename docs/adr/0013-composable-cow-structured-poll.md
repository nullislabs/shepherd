---
status: accepted
---

# ComposableCoW poll is a structured non-reverting verdict; the module is a generic handler-agnostic monitor

## Context

ADR-0006 kept TWAP and EthFlow as guest modules over low-level host primitives, with the host protocol-neutral. What it baked in, and what this ADR supersedes, is the poll *mechanism*: a module calling `getTradeableOrderWithSignature`, decoding the return or the revert reason, mapping revert selectors to an off-chain `PollOutcome`, and computing its own schedule.

The nullisLabs composable-cow fork splits settlement from polling and makes the poll path structured and non-reverting: `getTradeableOrderWithSignature` and `checkOrder` return a `PollResult` whose `GeneratorResult` carries a `code` (`POST` | `WAIT_TIMESTAMP` | `WAIT_BLOCK` | `TRY_NEXT_BLOCK` | `INVALID` | `NEEDS_INPUT`), the order, schedule sentinels, and a `bytes4 reasonCode`. The revert-decode and schedule move on-chain, so a monitor needs no handler-specific off-chain logic and one generic poller drives every handler.

## Decision

The CoW module is a generic ComposableCoW monitor: it indexes `ConditionalOrderCreated`, polls each authorised order, and switches on the poll verdict alone (`POST` submits through `videre:venue/client`; `WAIT_TIMESTAMP` / `WAIT_BLOCK` reschedule; `TRY_NEXT_BLOCK` re-polls; `INVALID` drops the watch; `NEEDS_INPUT` parks). It decodes no revert selectors, computes no schedule, and holds no per-handler branch.

Migration is hybrid and gated on deployment:

- The Rust verdict seam migrates now (zero contract dependency): the poll resolves to a structured `Verdict` and the sweep dispatches on it.
- The deployed 1.x reverting poll is quarantined behind a `LegacyRevertAdapter` that maps each upstream selector onto a `Verdict`, so the module posts through the target seam against the deployed contract.
- When the fork's `deployments/networks.json` is non-empty on a target chain, the adapter is replaced by a direct `PollResult` struct-read and golden vectors regenerate against `reasonCode` / `PollResult`.

The orderbook-API submit-error `errorType` table (the REST POST response) is a separate data-driven concern the module keeps; it is not the poll contract.

## Current state

The seam migration has shipped: `shepherd/crates/composable-cow` exports the structured `Verdict`, the `run` sweep dispatches on it (`shepherd/crates/composable-cow/src/run.rs`), and the deployed reverting wire is served by `LegacyRevertAdapter`. The fork is deployed on no target chain, so `LegacyRevertAdapter` is the live poll path; the structured non-reverting `PollResult` wire is not yet exercised in production. The wire-swap remains gated on the fork deploying.

## Consequences

- The five-selector revert mirror survives only as the `LegacyRevertAdapter` internals; at the wire-swap it is replaced by a struct-read, not re-ported.
- The two-gate store holds contract-supplied `waitUntil` / `nextPollTimestamp` slots rather than off-chain epoch math.
- `NEEDS_INPUT` offchainInput acquisition, manifest enumeration, and merkle-payload discovery land as follow-ons; none is needed by the current module roster.
- ADR-0006's host-neutrality decision is unchanged; this ADR supersedes only its poll-mechanism description. ADR-0007's `OrderPostError::retry_hint` remains the orderbook-submit contract, never the poll contract.
