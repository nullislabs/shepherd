Consolidate the CoW on-chain event ABIs the keepers watch into the shepherd-cow WIT package so they are owned only at the shepherd L3 repo.

## Why
All CoW protocol knowledge, including the on-chain event ABIs the keepers decode, belongs to the shepherd L3 repo and must never leak into L1 or L2. Today the ABI surfaces are entangled with the legacy host-extension surface that is retiring. Consolidating them under wit/shepherd-cow gives a single L3-owned CoW WIT surface and keeps the generic layers free of CoW specifics. Part of milestone M4: CoW on the generic seam (the shepherd bundle). Blocked by: cow-onvidere-epic. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Consolidate the CoW event-ABI surfaces under wit/shepherd-cow: the ConditionalOrderCreated topic-0, EthFlow.OrderPlacement, and any other CoW on-chain event surfaces the keepers decode.
- Ensure the composable-cow and ethflow keepers resolve their event ABIs from this package.
- Mark the legacy host-extension interface in shepherd:cow as retiring; it is deleted at the fork-gated poll wire-swap.

## Done when
- The shepherd-cow event-ABI WITs (ConditionalOrderCreated, EthFlow.OrderPlacement, and any other CoW on-chain event surfaces the keepers decode) live under wit/shepherd-cow and are consumed only by L3 crates.
- No videre:* or nexum:host package uses shepherd:cow.
- The composable-cow and ethflow keepers resolve their event ABIs from this package.
- The legacy host-extension surface in shepherd:cow is clearly marked as retiring.
