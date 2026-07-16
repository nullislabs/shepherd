Move all WIT resolution to crate-local wit-deps and source cross-package WIT from pinned git tags, so each repo resolves its own WIT plus its cross-repo dependencies after the carve.

## Why
Today every bindgen! and the venue macro resolve WIT via a workspace-ancestor walk into a shared ../../wit/* tree. After the carve each repo must resolve its own WIT plus cross-repo deps, following the dependency DAG nexum:host (leaf) <- videre:value-flow <- videre:intent / videre:venue <- videre:adapter <- shepherd:cow. Part of milestone M5: The gated three-repo split. Blocked by: Transitional path-dep cargo workspace in the three groupings (git-tag pin path plus dep-sync CI). See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Introduce wit-deps (deps.toml) per prospective repo; flip every bindgen! path list and the macro WIT-root; rewrite find_wit_root (lib.rs:512).
- Source cross-package WIT from git tags and check in the lockfiles.
- Document the convergence to wkg/OCI plus per-package semver.

## Done when
- All bindgen and macro WIT resolution is crate-local (wit/ + wit/deps/).
- Cross-repo WIT is sourced from pinned git tags with lockfiles checked in.
- The acyclic DAG builds both in-repo and cross-repo.
- The registry and semver policy is documented.
