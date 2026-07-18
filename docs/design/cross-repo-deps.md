# Cross-repo dependencies

How shepherd sources the two upstream repos after the carve. The repos:
`nullislabs/nexum-runtime` (L1, the generic runtime, including
`nexum-world`), `nullislabs/videre-nexum-module` (L2, the videre venue
platform), and this repo, `nullislabs/shepherd` (L3, the app-level CoW
bundle). Dependencies flow strictly up: shepherd on videre and
nexum-runtime, videre on nexum-runtime, nexum-runtime on neither.

## Pinning

- Cross-repo Rust deps are exact git `rev` (commit) pins on the owning
  repo: `nexum-*` on nexum-runtime, `videre-*` on videre-nexum-module.
  No semver resolution; a bump is an explicit repin.
- Cross-repo WIT rides `wit-deps` commit-tarball sources with a
  checked-in lock (`wit/deps.toml` plus `deps.lock`), one commit per
  upstream repo.
- The videre `client` capability the keepers declare is registered in the
  root `extensions.toml`; the `#[nexum_sdk::module]` macro resolves it
  from the nearest ancestor registry.

## History (pre-carve umbrella)

Before the carve the three groupings lived in one workspace under a
transitional umbrella `Cargo.toml`: member manifests wrote the final
cross-repo pins, and a root `[patch]` table redirected each pinned repo
URL to the in-tree grouping so the monorepo built the working tree and
stayed green through the split train. The carve removed the umbrella, the
`dep-sync` gate and the groupings; each upstream grouping shipped with its
`Cargo.repo.toml` renamed to `Cargo.toml`.

## Convergence path

1. Git-rev pins, today: exact commit pins, WIT via `wit-deps` tarballs,
   locks committed. A bump is an explicit downstream repin.
2. crates.io, once the surfaces stabilise: `nexum-runtime` and the videre
   crates publish; downstreams move to caret ranges and the git pins
   disappear.
3. wkg/OCI, alongside crates.io: the WIT packages publish to
   `ghcr.io/nullislabs`; `wit-deps` git sources become registry fetches.

shepherd stays app-level: it pins upstream commits and never publishes.
