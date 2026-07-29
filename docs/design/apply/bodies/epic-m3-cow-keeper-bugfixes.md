Carry the still-live TWAP and composable keeper correctness fixes onto the ported keeper.

## Goal
Land the outstanding TWAP and composable keeper correctness fixes on the ported keeper: deduplication and retry classification, gate-marker leaks, revert-selector loops, the signature-race retry, and conditional-order removal, plus reconcile the grant deliverable divergence so the shipped keeper matches what was promised.

## Scope
These are known-live correctness bugs in the current TWAP and composable keepers that must not be lost when the keeper is ported onto the generic venue seam. The work covers the retry and deduplication paths, leaked gate markers, tight loops on revert selectors, the race between signing and submission, and removal of stale conditional orders. Alongside the code fixes, it reconciles where the delivered behaviour diverged from the grant deliverable so the ported keeper is both correct and accountable to what was committed.

Milestone: M4: CoW on the generic seam (the shepherd bundle).
