Prove `nexum-runtime` is venue-agnostic by deleting the privileged router field and flipping the zero-leak CI check from advisory to blocking. This is a go/no-go gate before repos are cut.

## Why
Cutting repos before the host is proven venue-agnostic would freeze an intent-shaped host into a repo boundary, which is expensive to undo later. The forcing function is deleting the `HostState.pool_router` field (`state.rs:54`) and carrying the router in a composite `Ext` lattice instead; if the host still boots an echo-venue with no intent or cow crate in the graph, it is genuinely generic. Part of milestone M2: Generic venue-agnostic host. Blocked by: p0-acyclicity-scaffold; CI gate: nexum-runtime has zero venue/intent/cow symbols. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Delete the `HostState.pool_router` field and carry the router in a composite `Ext` lattice.
- Promote the acyclicity and zero-leak CI check from advisory to blocking.
- Add an echo-venue boot integration test as the oracle.

## Done when
- The `pool_router` field is deleted.
- The zero-leak CI check is blocking and green on `nexum-runtime`.
- The echo-venue boots through the generalized seam with no intent or cow crate in the graph.
