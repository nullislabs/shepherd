Execute the physical cut as three history-preserving git-filter-repo extractions, one per repo, run as a single coordinated operation once the cut gates are green.

## Why
This is the physical cut. Once the cut gates are green, three git-filter-repo --path extractions preserve history, one per repo, reusing the keeper-rename template: range-limited git-filter-repo plus a byte-identical tip oracle plus jj/mergiraf. Cross-repo Rust is wired via git-tag pins and WIT via wit-deps git tags, and videre:* is kept additively extensible so the post-cut second venue can drive a non-breaking cross-repo fix. Part of milestone M5: The gated three-repo split. Blocked by: Cut go/no-go gate: assert runtime venue-agnostic and cow on the generic seam before any carve; Transitional path-dep cargo workspace in the three groupings (git-tag pin path plus dep-sync CI); WIT cross-repo consumption: wit-deps flip, git-tag sourcing, wkg/OCI registry convergence. See docs/design/videre-split-plan.md and docs/design/issue-milestone-plan.md.

## Scope
- Carve nexum-runtime (L1): crates/nexum-runtime, nexum-sdk, nexum-sdk-test, nexum-world, nexum-module-macros, nexum-launch, the bare nexum bin, wit/nexum-host.
- Carve videre (L2): videre-sdk, videre-test, videre-macros, videre-host, wit/videre-*, echo-venue/echo-client.
- Carve shepherd (L3, the shepherd bundle): cow-venue, shepherd-sdk (absorbed into the bundle), shepherd-cow-host, shepherd-sdk-test, shepherd-backtest, the shepherd bin (a nexum-runtime host), wit/shepherd-cow.
- Wire cross-repo Rust via git-tag pins and WIT via wit-deps git tags.
- Keep videre:* additively extensible so the post-cut second venue can drive a non-breaking cross-repo fix.

## Done when
- Three history-preserving repos are carved (byte-identical tip oracle per repo, full history preserved): nexum-runtime, videre, and shepherd (the shepherd bundle, shepherd-sdk absorbed).
- Each repo builds standalone on pinned cross-repo deps.
- The acyclic DAG holds.
- The L1 zero-leak gate is green in the nexum-runtime repo.
