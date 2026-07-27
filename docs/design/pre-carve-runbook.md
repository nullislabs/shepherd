# Pre-carve go/no-go runbook (#406)

Precondition for beginning the carve (#407). All four gates below must be GREEN before any git-filter-repo extraction starts.

## 1. Go/no-go gate checklist

### Gate A: runtime venue-agnostic -- GREEN

- `nix develop -c bash scripts/check-venue-agnostic.sh`: all checks PASS (crate graphs clean, symbol scan empty, no privileged router field, no foreign WIT namespace, `nexum:host` resolves standalone). Exit 0.
- `rg pool_router -g '*.rs' nexum/`: no matches.
- `rg CowApiHost -g '*.rs'`: no matches anywhere in the tree; the only hit for `CowApiHost` is `docs/adr/0009-host-trait-surface.md` (documentation, not code).
- Tracking issues: #374, #383, #385 all CLOSED.
- `nix develop -c bash scripts/check-carve-groups.sh`: `carve-groups: OK -- every workspace crate is grouped and depends only within or below its tier`. Exit 0.

### Gate B: cow on the generic seam -- GREEN

- `shepherd/crates/cow-venue` exists, `[lib] crate-type = ["lib", "cdylib"]`.
- `cargo build -p cow-venue --target wasm32-wasip2 --features adapter` builds clean and emits `target/wasm32-wasip2/debug/cow_venue.wasm`.
- `shepherd/modules/ethflow-watcher/src/keeper.rs` imports `cow_venue::client::{CowClient, CowVenue}` and `videre_sdk::client::{Venue, VenueTransport}`; no `CowApiHost` reference in the module (`rg CowApiHost shepherd/modules/ethflow-watcher/` empty). The keeper runs on `videre:venue`/client, not `CowApiHost`.
- Issue #400 CLOSED.

### Gate C: WIT reshapes complete -- GREEN

- Root `wit/` deleted: `ls wit` fails, no such path.
- Crate-local WIT confirmed per group: `nexum/wit/nexum-host/`, `videre/wit/{videre-types,videre-value-flow,videre-venue}/`, `shepherd/wit/shepherd-cow/`.
- Cross-group vendoring in place: `videre/wit/deps.toml` pulls `nexum-host`; `shepherd/wit/deps.toml` pulls `nexum-host` plus the three `videre-*` packages; both have a checked-in `deps.lock`.
- `docs/design/carve-workspace.md` documents the transitional path-dep medium and the flip point to git-tag tarballs at the carve.
- Version homogenization: `workspace.package.version = "0.1.0"` in the root `Cargo.toml`; all `videre:*` WIT packages declare `@0.1.0` (`videre:types@0.1.0`, `videre:value-flow@0.1.0`, `videre:venue@0.1.0`).
- Issues #403 (grouping), #515 (0.1.0 homogenization), #640 (WIT reshape/wit-deps flip) all CLOSED/MERGED, on main.

### Gate D: videre:* additively extensible -- GREEN

- `rg -ni "freeze|frozen|@since|@unstable|stability" videre/wit/videre-types videre/wit/videre-value-flow videre/wit/videre-venue`: no matches. No freeze markers in any `videre:*` WIT package.
- All three packages sit at `@0.1.0`, not `@1.0.0`; the `videre:value-flow` 1.0 freeze (#330, OPEN) has not been applied.
- The second-venue gate (#410, milestone M6) is explicitly NOT required pre-carve; it is post-cut acceptance under M5/M6.

**Overall: all four gates GREEN. Cleared to carve.**

## 2. Carve procedure (#407)

Three history-preserving `git-filter-repo --path` extractions, one per repo, run as a single coordinated operation:

1. `nexum-runtime` (L1): `--path nexum/` (crates, sdk, sdk-test, world, module-macros, launch, the bare `nexum` bin, `wit/nexum-host`).
2. `videre` (L2): `--path videre/` (sdk, test, macros, host, `wit/videre-*`, echo-venue/echo-client).
3. `shepherd` (L3 bundle): `--path shepherd/` (composable-cow, cow-venue, shepherd-sdk absorbed into the bundle, the `shepherd` bin, the `ethflow-watcher` and `twap-monitor` modules, the `orderbook-mock` tool, `wit/shepherd-cow`).

Each extraction:

- Reuses the keeper-rename fold template (#366): range-limited `git-filter-repo` pass, jj-driven rebase, mergiraf conflict resolution.
- Is validated by a byte-identical tip oracle against the pre-carve monorepo state for that path.
- Conflicts during the fold are resolved via `jj` plus `mergiraf` (syntax-aware merge driver), not raw `git rebase`.

Done when: three repos exist, each passes its tip oracle, each builds standalone on pinned cross-repo deps, the acyclic DAG holds, and the L1 zero-leak gate is green in the carved `nexum-runtime` repo.

## 3. Cross-repo wiring at the cut

- Rust: intra-workspace path-deps flip to pinned git-tag references (`nexum-runtime`, `videre` tags consumed by the downstream repos).
- WIT: `wit-deps` `deps.toml` path sources (already in place from #640) flip to pinned git-tag tarball URLs (`url` + `sha256`) of the owning repo. `videre/wit/deps.toml` pins `nexum-host`; `shepherd/wit/deps.toml` pins `nexum-host` plus the three `videre-*` packages.
- Lockfiles (`Cargo.lock`, `wit/deps.lock`) are checked in at every repo.
- Post-carve convergence (not required at cut): crates.io for Rust, wkg/OCI per-package semver for WIT.

## 4. Force-replace decision (#549)

- `nullislabs/nexum-runtime` and `nullislabs/videre-nexum-module` are already-populated carve targets that have diverged (tracing refactor, 14 open dependabot PRs across both repos (8 on nexum-runtime, 6 on videre-nexum-module), stale `types.wit` variant removed by #520 upstream).
- The carve OVERWRITES both mains from the shepherd extraction. This is not a three-way reconciliation.
- Do not merge or rebase the existing carved-repo history.
- Close the 14 open dependabot PRs on both repos as superseded, at carve time, not before.
- Nothing from the tracing refactor is currently identified as wanted; if it is, it must be re-authored in shepherd before the carve base is cut.
- Fixes already landed in shepherd (e.g. #520) reach those repos only via the carve, never by propagation.

## 5. Additive-extensibility guard (#410)

- `videre:*` (types, value-flow, venue) stays additively extensible through the cut: no field frozen or removed.
- A post-cut abstraction fix (from the second venue) is therefore a non-breaking, additive cross-repo change, not a breaking one.
- The `videre:value-flow` 1.0 freeze (#330) is held until the post-cut second venue (#140) compiles and passes videre-test against the split videre-sdk and feeds back any shape corrections.
- Re-tag/re-pin runbook on any post-cut additive correction: re-tag `videre`, re-pin in `nexum-runtime`, `shepherd`, and the second-venue repo, regenerate goldens cross-repo, re-run the byte-identical tip oracle per repo.
- The dependency-sync and semver CI check stays enforcing through the correction window so an additive re-pin cannot silently drift.

## 6. Second-venue de-gated

Explicitly NOT a pre-carve requirement. The genuine non-cow second-protocol venue (#140) is post-cut acceptance under milestone M5/M6, built against the already-split videre-sdk. It cannot freeze the carve behind an unbuilt venue.

## 7. Sign-off

This runbook and all four gates (section 1) being GREEN is the precondition for beginning the carve (#407). No git-filter-repo extraction starts before this document is merged with all gates asserted GREEN.
