Flip the flagship CoW keeper onto the generic venue seam: change run.rs from CowApiHost::submit_order(...) to pool.submit(CowVenue::ID, cow_body_bytes), retiring the CowApiHost trait and the cow-api host extension.

## Why
The split must not codify the generic L2 contract into a repo boundary while the flagship CoW venue still bypasses it. This is the go/no-go gate that proves the generic seam carries a real venue: the keeper submits through videre:venue/client rather than the CoW-specific host, and the cow-api host extension is removed. Note the fork-gate boundary: the Rust seam re-split lands now, and only the poll wire-swap is fork-blocked, so this gate must not be coupled to the fork-gated poll swap. Part of milestone M4: CoW on the generic seam (the shepherd bundle). Blocked by: s1-gate-runtime-venue-agnostic; cow-venue-cdylib; composable-cow-keeper-port; cow-api-retire. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Flip run.rs from CowApiHost::submit_order(...) to pool.submit(CowVenue::ID, cow_body_bytes).
- Retire the CowApiHost trait and the cow-api host extension.
- Keep this gate to the seam port only; do not couple it to the fork-gated poll swap.

## Done when
- The CoW adapter cdylib is on videre:adapter.
- The keeper submits through videre:venue/client, not CowApiHost.
- The venue-crate symbol gate is green.
- The idempotency seam is in place.
- The seam port is not coupled to the fork-gated poll swap.
