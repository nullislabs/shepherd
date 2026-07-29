Delete the baked venue rows from the KNOWN capability table so per-namespace rows come from registered extensions, and factor world synthesis into a new plain `nexum-world` library. The capability model and per-component world synthesis already shipped, but the KNOWN table still bakes a pool row (`world.rs:74`) and a `cow-api` to `shepherd:cow` row (`world.rs:80`), which leaks a downstream name into the host.

## Why
As long as the KNOWN table hardcodes a pool row and a cow-api mapping, the host layer keeps knowledge of specific venues and cow. Sourcing per-namespace rows from registered extensions removes that knowledge, and extracting the synthesis and table into their own library gives the host a clean, venue-free capability core. Part of milestone M2: Generic venue-agnostic host. Blocked by: Grow the Extension seam to carry worker/provider roles. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Delete the baked pool and `cow-api` to `shepherd:cow` rows from the KNOWN table.
- Source per-namespace capability rows from registered extensions.
- Extract the `world.rs` synthesis and the KNOWN table into a new plain library, `nexum-world`.
- Rewrite `find_wit_root` from a workspace-ancestor walk to crate-local `wit/` plus `wit/deps` resolution.
- Keep `#[module]` in `nexum-module-macros`; leave venue and intent macros to `videre-macros`.

## Done when
- The baked pool and `cow-api`/`shepherd:cow` rows are removed from the KNOWN table.
- Capability rows are registry-driven from registered extensions.
- World synthesis and the table are extracted into a plain `nexum-world` library.
- `find_wit_root` resolves crate-local `wit/`.
- No venue or cow string remains in `nexum-world`.
