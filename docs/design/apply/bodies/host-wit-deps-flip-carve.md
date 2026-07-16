Flip the nexum:host WIT resolution to crate-local wit-deps and physically extract nexum-runtime as its own L1 repository once the host is proven venue-agnostic.

## Why
Today bindgen! and the venue macro resolve WIT through a workspace-ancestor walk into a shared ../../wit/* tree, which cannot survive a physical repo split. Once the runtime is proven venue-agnostic (zero-leak gate green), the runtime slice can be pulled out of the monorepo on crate-local WIT with its own semver. This requires the runtime generalization to land first and the free-reshape window to be closed before the cut. Part of milestone M5: The gated three-repo split. Blocked by: host-zero-leak-ci-gate; host-generic-launcher-bin. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Introduce wit-deps (deps.toml) for the runtime crate and check in lockfiles.
- Flip every bindgen! path list and the macro WIT-root off ../../wit/* to crate-local wit/ + wit/deps/.
- Perform a history-preserving git-filter-repo --path extraction of nexum-runtime as the L1 repository, reusing the keeper-rename template (byte-identical tip oracle plus jj/mergiraf).
- Adopt independent per-package semver for nexum:host (@0.1.x).

## Done when
- nexum-runtime builds against crate-local wit/ + wit/deps with lockfiles checked in.
- The history-preserving git-filter-repo carve yields a standalone L1 repo passing the byte-identical tip oracle.
- The zero-leak gate is green in the carved repo.
- nexum:host is on independent semver.
