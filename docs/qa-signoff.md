# Internal QA sign-off - pre-upstream review pass

**Branch**: `qa/cleanup` (tip of M2 + M3 stack)
**Generated**: 2026-06-17

## Mechanical checks (workspace-wide)

| Check | Status | Notes |
|---|---|---|
| `cargo fmt --all --check` | ✅ | One pre-existing drift in `supervisor/tests.rs` (M1) plus M2/M3 leaf modules; bulk applied as single cleanup commit. |
| `cargo clippy --all-targets --workspace -- -D warnings` | ✅ | Clean. |
| `cargo test --workspace` | ✅ | 145 host tests + 1 doctest passing. |
| Em-dashes in `crates/`, `modules/`, `docs/` | ✅ | 0. One was in `price-alert/strategy.rs:4` (mine), fixed. |
| Em-dashes in `wit/**.wit` | ⚠ | 3 in mfw78's M1 prose. Intentionally left alone; flag for him in upstream review. |
| `warn(unused_crate_dependencies)` on every crate root | ✅ | sdk, sdk-test, nexum-engine, twap, ethflow, price-alert, balance-tracker, stop-loss. |
| WASM build (`wasm32-wasip2 --release`) | ✅ | All 5 modules build. Sizes: twap 314 KB, ethflow 282 KB, stop-loss 311 KB, price-alert 215 KB, balance-tracker 102 KB. |
| String-wrapped errors outside WIT boundary | ✅ | All hits in `crates/nexum-engine/src/host/impls/*` (FFI boundary - exception per rust-idiomatic skill). No leaks in SDK or modules. |

## Per-PR shape

| PR | Module | Tests | Strategy/lib split | Notes |
|---|---|---|---|---|
| #2-#7 | twap-monitor M2 | 13 | ❌ no split until #24 | Stacked TWAP. Strategy ↔ lib.rs split landed at #24. |
| #8-#10 | ethflow-watcher M2 | 7 | ❌ no split until #25 | Split landed at #25. |
| #11 | module.toml manifests | - | - | Both M2 modules have manifests with capability + subscription comments. ✅ |
| #12 | shepherd-sdk skeleton | - | - | Public surface present. |
| #13 | sdk helpers extraction | - | - | OK. |
| #14 | M2 on SDK | - | - | M2 modules now consume `shepherd_sdk::cow` / `chain` helpers. |
| #15 | shepherd-sdk-test (MockHost) | 8 | - | Full mock surface; matches Host trait. |
| #16 | SDK docs | - | - | README + rustdoc on public items. **See architectural finding below.** |
| #17 | deployment guide | - | - | `docs/06-production-hardening.md` exists. |
| #18 | price-alert | 11 | ❌ no split until #22 | Refactor at #22. |
| #19 | balance-tracker | 13 | ❌ never refactored | Acceptable: balance-tracker has no submit path, dispatch matrix simpler. **Optional follow-up: bring to same shape for consistency.** |
| #20 | tutorial | - | - | Rewritten as guided tour at #23; reads top-to-bottom against real stop-loss source. |
| #21 | rust-idiomatic compliance | - | - | em-dash purge, thiserror, warn(unused_crate_dependencies). ✅ |
| #22 | price-alert host-trait | 16 | ✅ | Reference shape. |
| #23 | stop-loss | 7 | ✅ | First module with the full M3 surface (chain + local-store + cow-api + logging). |
| #24 | twap-monitor host-trait | 20 | ✅ | Strategy split; 7 new MockHost dispatch tests. |
| #25 | ethflow-watcher host-trait | 12 | ✅ | Strategy split; 5 new MockHost tests including PR #10 c5e4d7d regression guard. |

## Architectural finding - DOC ↔ CODE divergence in M3 SDK

**`docs/05-sdk-design.md` describes a 2-layer SDK that does not exist**:

- `nexum-sdk` (universal) + `shepherd-sdk` (CoW extension) - we shipped only `shepherd-sdk`. No `nexum-sdk` crate.
- `#[nexum::module]` / `#[shepherd::module]` proc macros - not implemented. We use raw `wit_bindgen::generate!` + `WitBindgenHost` adapter pattern.
- Full alloy `Provider` backed by `HostTransport` - not implemented. We pass JSON-RPC method + params strings via `ChainHost::request`.
- Typed local-store helpers (serde over raw bytes) - not implemented. Modules call `host.set(&key, &value)` with raw bytes.
- Typed `Signer` for key management - not implemented. Modules use `Signature::PreSign` / `Signature::Eip1271`; no key custody on the module side.

**Two paths**, mfw78's call:

1. Update `docs/05-sdk-design.md` to describe what M3 actually shipped (Host traits + helpers + MockHost; defer proc macros, Provider, Signer, `nexum-sdk` split to M5+).
2. Or treat the doc as M5 north-star and implement the missing layers as part of M4 / M5 scope.

Doc is currently aspirational; code is M3-scoped. They need to agree before upstream review.

## Outstanding / deferred

| Item | Issue | Status |
|---|---|---|
| `#[non_exhaustive]` batch on SDK public enums (`HostErrorKind`, `LogLevel`, `PollOutcome`, `RetryAction`) | - | Held until just before upstream cut. |
| WIT-file em-dashes in upstream prose (3 occurrences) | - | Ask mfw78. |
| balance-tracker host-trait refactor (consistency with other 4 modules) | - | Optional follow-up. |
| PR description template (mfw78's "What does this PR do? / Why / Changes / Breaking changes / Testing / AI disclosure") | - | Cosmetic; could template-bump existing PR bodies before upstream push. |
| ADR for the M3 Host trait surface | - | None today. Worth one short ADR (0009 candidate) capturing the strategy/lib split decision before upstream review. |

## Sign-off

| Area | Ready for upstream? |
|---|---|
| M2 modules (twap + ethflow + manifests) | ✅ once PRs #24 + #25 land |
| M3 SDK + examples | ✅ pending doc 05 reconciliation |
| Tutorial | ✅ |
| Rust-idiomatic compliance | ✅ |
| Tests + builds | ✅ |
| Docs | ⚠ doc 05 vs code mismatch must resolve |
| ADRs | ⚠ M3 host trait surface lacks an ADR |

**Recommendation**: address the two ⚠ items (doc 05 + ADR-0009) before opening the consolidated upstream PR. Everything else is green.
