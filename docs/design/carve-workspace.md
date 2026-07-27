# Transitional three-grouping workspace (M5)

The monorepo is reorganized into the three prospective repo roots as one cargo
workspace, so it keeps building as a single unit with a shared `Cargo.lock` up to
the physical carve (#407). This is the refactor-now-cut-later step (#403).

## Groupings

Each top-level dir is a future repo root. Tier order is `nexum <- videre <- shepherd`.

| Dir         | Tier | Repo             | Contents |
|-------------|------|------------------|----------|
| `nexum/`    | L1   | `nexum-runtime`  | universal runtime, SDK, macros, launcher, `nexum-cli` bare bin; universal-package example modules and runtime fixtures |
| `videre/`   | L2   | `videre`         | intent/venue SDK, host, macros, status-body; the generic `echo` reference venue |
| `shepherd/` | L3   | `shepherd`       | `cow-venue`, `composable-cow`, the `shepherd` composition-root bin, the cow reference modules |

Layout within a group: `<group>/crates/*`, `<group>/modules/*`, `<group>/tools/*`.

## Dependency invariant

A crate depends only within its own tier or a lower one. An upward edge (nexum on
videre, videre on shepherd) would become a circular repo dependency at the carve,
so it is rejected in CI by `scripts/check-carve-groups.sh` (job `carve-groups`).
The physical layout is the source of truth: a crate's group is its top-level dir,
and the check derives every edge's tier from cargo metadata. There is no separate
mapping to drift.

## Cross-repo dependency medium

Cross-group edges stay as intra-workspace path-deps in this transitional umbrella,
preserving the single hoisted dependency table and shared lockfile. The medium
converges in three steps:

1. Path-deps (now) — one workspace, one `Cargo.lock`, atomic folds.
2. Git-tag pins (at carve, #407) — each repo pins its cross-repo deps to a tagged
   release; the `carve-groups` gate is joined by a dep-sync check that the pins
   resolve to the tagged versions.
3. crates.io (post-carve) — published semver releases replace the git-tag pins.

## WIT resolution

WIT stays in a single root `wit/` for this step. `resolve_wit_package`
(`nexum/crates/nexum-world`) walks manifest-dir ancestors to the nearest `wit/`,
so every crate resolves the shared tree regardless of depth. Splitting `wit/` into
per-group `wit/` + `wit/deps/` requires the crate-local wit-deps flip and is done
in #404/#405, not here. The hardcoded `wit_bindgen::generate!` path lists that
bypass the resolver were re-based one level deeper by the move.
