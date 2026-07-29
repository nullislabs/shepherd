Delete composable.rs and LegacyRevertAdapter and switch the composable-cow poll onto the fork's structured non-reverting getTradeableOrderWithSignature. This is gated on the ComposableCoW fork being deployed and must not be coupled to the keeper port.

## Why
The structured non-reverting poll cannot be instantiated until the ComposableCoW fork is deployed: Verdict::Post is the one variant LegacyRevertAdapter never produces, and Verdict::NeedsInput is dead surface until IOrderModule and the fork land. This wire-swap is hard-blocked on the fork's deployments/networks.json being non-empty on a shepherd target chain. It must stay decoupled from the keeper port, otherwise the whole shepherd split freezes behind a third-party deployment clock. Part of milestone M4: CoW on the generic seam (the shepherd bundle). Blocked by: cow-onvidere-epic; composable-cow-keeper-port. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Delete composable.rs and LegacyRevertAdapter.
- Switch the poll onto the fork's structured non-reverting getTradeableOrderWithSignature.
- Fully populate Verdict::Post; wire and test the Verdict::NeedsInput arm.
- Model next_poll_timestamp as an Option or NextPoll rather than the 0 sentinel that collides with the fork wire.
- Delete the legacy host-extension surface in wit/shepherd-cow.

## Done when
- The work proceeds only once the fork's deployments/networks.json is non-empty on a shepherd target chain.
- composable.rs and LegacyRevertAdapter are deleted.
- Verdict::Post is fully populated by the structured non-reverting poll.
- The Verdict::NeedsInput arm is wired and dispatch-tested.
- Verdict::Post.next_poll_timestamp is modelled as Option or NextPoll, not the 0 sentinel that collides with the fork wire.
- The legacy host-extension surface in wit/shepherd-cow is deleted.
