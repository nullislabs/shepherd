Reorganize the monorepo crates into the three prospective repo groupings as a single cargo workspace with intra-grouping path-deps, and define how cross-repo dependencies will be sourced after the carve.

## Why
The split must not lose the single hoisted dependency table, the shared Cargo.lock, or atomic folds. The mitigation is a transitional umbrella superproject with path-deps that converges to git-tag pins and then crates.io, so the monorepo can keep building as one workspace right up to the physical cut. Part of milestone M5: The gated three-repo split. Blocked by: s1-gate-runtime-venue-agnostic; s1b-gate-cow-on-generic-seam. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Reorganize the monorepo crates into the three prospective groupings as workspace members with intra-grouping path-deps.
- Define the post-carve cross-repo Rust dependency medium: git-tag pins first, with a documented path to crates.io.
- Add a dep-sync CI check.

## Done when
- The three-grouping path-dep workspace builds with the acyclic crate DAG verified.
- The cross-repo dependency medium is documented (git-tag to crates.io).
- The dep-sync CI check is green and enforcing.
