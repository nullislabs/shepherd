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

Each group owns its WIT: `nexum/wit/nexum-host`, `videre/wit/videre-{types,
value-flow,venue}`, `shepherd/wit/shepherd-cow`. There is no shared root `wit/`.

Cross-group WIT follows the same tier order as the crates and is vendored into
the consuming group's `wit/deps/` by [wit-deps]: `videre/wit/deps.toml` pulls
`nexum-host`; `shepherd/wit/deps.toml` pulls `nexum-host` plus the three
`videre-*` packages. The manifests use path sources into the owning group's
tree, and the checked-in `deps.lock` digests pin the vendored copies. After
editing an owned WIT package, re-run `wit-deps` from each consuming group root
(`videre/`, `shepherd/`) and commit the refreshed `wit/deps` + `deps.lock`.

`resolve_wit_packages` (`nexum/crates/nexum-world`) walks manifest-dir ancestors
to the nearest `wit/` tree and resolves every package there, vendored
`wit/deps/<package>` before owned `wit/<package>`; it never falls through to an
outer tree, so a group cannot use WIT it has not vendored. The hardcoded
`wit_bindgen::generate!`/`bindgen!`/`include_str!` path lists point at the same
group-local trees.

Convergence at the physical carve: the path sources in each `deps.toml` flip to
pinned git-tag tarball URLs of the owning repo (wit-deps `url` + `sha256`
sources), then post-carve to wkg/OCI per-package semver releases. Only the
manifests change; the resolver and the vendored layout stay as they are.

[wit-deps]: https://github.com/bytecodealliance/wit-deps
