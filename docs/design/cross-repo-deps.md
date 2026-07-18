# Cross-repo dependencies

How the three repos source each other after the carve, and how the
transitional umbrella keeps the monorepo building as one workspace until
then. The repos: `nullislabs/nexum-runtime` (the `nexum/` grouping,
including `nexum-world`), `nullislabs/videre-nexum-module` (`videre/`),
and `nullislabs/shepherd` staying as the app-level bundle (`shepherd/`).
Dependencies flow strictly up: shepherd on videre and nexum-runtime,
videre on nexum-runtime, nexum-runtime on neither. Plan:
`docs/design/videre-split-plan.md` (design-notes branch), section 6.2
decisions D9 and D10.

## Transitional umbrella (pre-carve)

- One workspace, one hoisted dependency table, one `Cargo.lock`, atomic
  folds across the groupings.
- Member manifests write cross-group Rust dependencies in their final
  post-carve form: exact git-tag pins on the owning repo, one tag per
  repo.
- The umbrella root `[patch]` table redirects each pinned repo URL to
  the in-tree crate, so the monorepo builds the working tree, not the
  tag, and stays green through the whole train.
- Each grouping carries `Cargo.repo.toml`, the workspace root it ships
  with at carve time; its member list mirrors the umbrella's members
  for that grouping.
- Each grouping resolves WIT crate-locally; cross-group WIT is sourced
  through `wit-deps` git tags with checked-in locks
  (`<group>/wit/deps.toml` plus `deps.lock`).

## Convergence path (decision D9)

1. Git-tag pins, at carve time: cross-repo Rust deps are exact `tag`
   pins, cross-repo WIT rides `wit-deps` git tags, locks are committed.
   No semver resolution; a bump is an explicit repin downstream.
2. crates.io, once the surfaces stabilise: `nexum-runtime` and the
   videre crates publish; downstreams move to caret ranges; the
   `[patch]` table and the git pins disappear.
3. wkg/OCI, alongside crates.io: the WIT packages publish to
   `ghcr.io/nullislabs`; `wit-deps` git sources become registry
   fetches.
4. shepherd stays app-level: it pins upstream tags and never publishes.

## Enforcement

`scripts/check-dep-sync.sh`, blocking as the `dep-sync` CI job and
locally via `just check-dep-sync`:

- the crate DAG points strictly up across the groupings for every
  workspace member, including path deps and `wit/` symlinks;
- every umbrella `[patch]` entry targets the owning grouping's in-tree
  crate and neutralises a written pin, with no orphans;
- git-tag pins agree on one tag per upstream repo everywhere they are
  written;
- each `Cargo.repo.toml` member list mirrors the umbrella's;
- `wit-deps` manifests and locks exist together, stay key-synced, and
  their tags agree with the Rust pins.

A check whose artefact is not yet written skips visibly and starts
enforcing the moment the artefact lands.
