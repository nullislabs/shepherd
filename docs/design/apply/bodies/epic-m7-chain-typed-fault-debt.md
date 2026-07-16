Finishes the typed-fault story across chain and the stub backends and clears the outstanding chain request-batch and backfill debt.

## Goal
Complete the typed-fault handling across the chain layer and its stub backends, and pay down the accumulated chain request-batching and backfill debt.

## Scope
The chain layer and its stub backends carry an incomplete typed-fault story that needs finishing so faults surface as structured types rather than opaque errors. Alongside that, the request-batch and backfill paths have accrued debt that should be cleared to keep chain access efficient and correct. The work fits together as a single robustness pass over the chain seam.

Milestone: M8: Post-v1 hardening and debt.
