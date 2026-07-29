<!-- Generated review-comment sweep across the M1 train (#240–#334), reconciled against origin/refactor/rename-chassis-to-keeper (#335). #239 handled separately (fix 04b2aff + threads resolved). -->

# M1 Train Review-Comment Sweep — Cross-Train Triage

**Headline.** 53 unresolved threads were swept across 15 cars (#240–#297, #334). **47 are STILL_LIVE** at the end-of-train tip (`origin/refactor/rename-chassis-to-keeper`), **5 are RESOLVED_DOWNSTREAM**, and **1 is SUPERSEDED** (chassis→keeper rename dissolved its anchor). Zero RESOLVED_IN_CAR, zero MOOT. Of the 47 live, **19 are med** (no high survives — both highs were resolved downstream) and **28 are doc/DX nits**. Nothing is merge-blocking; the ADR-0013 Verdict seam and the chassis→keeper rename reworded surrounding code but left almost every flagged line substantively intact.

## Still live — act on these

### Med severity (correctness / contract / coverage)

| PR | file:line | sev | issue | action |
|----|-----------|-----|-------|--------|
| 249 | host/pool_router.rs (via adapter.wit):14 | med | `derive-header` doc claims purity the WIT can't enforce; router now calls `derive_header` **before** `guard.check` — side effects escape policy | fix (doc caveat / sub-world) |
| 249 | host/impls/messaging.rs:50 | med | `messaging.query` not scope-checked while `publish` is; scope hole goes live with 0.3 Waku backend | leave-open |
| 249 | supervisor.rs:630 | med | Missing-manifest surfaces as misleading "declares module kind EventModule"; error should branch missing vs wrong-kind | fix |
| 248 | wit/nexum-intent/types.wit:48 | med | `valid-until` doc says ms but SDK `Tick.epoch_s` is seconds — silent /1000; rename `valid-until-ms` before 0.1.0 freeze | leave-open |
| 248 | wit/nexum-intent/types.wit:124 | med | `denied()` rides the same error channel as retryable variants but lacks MUST-NOT-retry guidance | leave-open |
| 247 | wit/nexum-value-flow/types.wit:72 | med | ERC arms use anonymous positional tuples; named records would be self-documenting + non-breaking-extensible pre-freeze | leave-open (design call) |
| 250 | host/pool_router.rs:341 | med | Guard-deny path doesn't charge quota → busy-loop DoS (LATENT: only AllowAllGuard ships in M1); reviewer's one-liner fix is clean | fix |
| 250 | host/pool_router.rs:77 | med | `GuardPolicy::check` sync; egress-guard epic needs async I/O — later change is breaking | leave-open (epic owner) |
| 251 | nexum-venue-sdk/src/faults.rs:130 | med | `RateLimited` arm untested — the one fold that drops structured data (`retry_after_ms`) | fix (add test) |
| 242 | shepherd-backtest/src/replay.rs:170 | med | `classify_ok` buckets any `app_data_resolved==None` as RejectedExpected, hiding unrelated failures (backtest only) | leave-open |
| 242 | modules/examples/stop-loss/src/strategy.rs:98 | med | Dedup-key asymmetry (`server_uid` write vs `uid_hex` read) → re-POST until validTo on UID drift | leave-open |
| 334 | shepherd-sdk/src/cow/composable.rs:76 | med | `Verdict::Post.next_poll_timestamp:u64` `0`-sentinel collides with fork wire semantics; model as `Option`/`NextPoll` now (no-op today) | fix |
| 334 | shepherd-sdk/src/cow/run.rs:67 | med | `NeedsInput` arm info-logs+busy-polls every tick, no dispatch test (LATENT until fork makes it reachable) | fix |
| 243 | crates/nexum-macros/src/lib.rs:68 | med | Handler match is name-only → async `on_block` compiles to `.await`-less call with opaque error | leave-open |
| 296 | crates/nexum-venue-sdk/src/lib.rs:75 | med | "undeclared capability = compile error" doesn't hold (blanket chain+messaging shims); two adapter contracts coexist, neither canonical | leave-open (spans #297) |
| 296 | modules/examples/echo-venue/Cargo.toml:16 | med | Pins wit-bindgen 0.58 vs workspace 0.59 → duplicate tree in Cargo.lock | fix (bump) |
| 297 | nexum-venue-test/src/transport.rs:9 | med | Mock grant fidelity diverges from host (exact-match vs path-prefix, gates query host doesn't, no fetch/chain scope); doc's "exactly as the host would" false | leave-open |
| 297 | nexum-venue-test/src/codec.rs:162 | med | Empty vector set passes `check` vacuously; re-encode-divergence branch untested | leave-open |
| 297 | nexum-venue-test/src/codec.rs:23 | med | Cross-language vector/golden files carry no version discriminator + `deny_unknown_fields` → additive field hard-fails old kits | leave-open |

### Nits (doc-wording / DX polish — mostly lgahdl)

| PR | file:line | issue | action |
|----|-----------|-------|--------|
| 251 | nexum-venue-sdk/src/adapter.rs:26 | `derive_header` purity claim WIT can't enforce | fix |
| 251 | nexum-venue-sdk/src/client.rs:90 | "carries no Display" false — wit-bindgen 0.58 generates Display for error-slot | fix |
| 248 | wit/nexum-intent/types.wit:14 | wrongly calls `none` a Python keyword (real risk: Rust bindgen title-casing) | fix |
| 248 | wit/nexum-intent/types.wit:76 | `settled(option<...>)` None case undocumented | leave-open |
| 247 | wit/nexum-value-flow/types.wit:66 | BZZ cited as native gas token (it's an ERC-20) | fix |
| 247 | wit/nexum-value-flow/types.wit:85 | `amount` field lacks its own doc (WIT renders field docs separately) | fix |
| 249 | engine_config.rs:132 | "may reach" → "may publish to" (only publish gated) | fix |
| 250 | host/pool_router.rs:286 | `quota_admits` doc says "Read-only" but it inserts/prunes | fix |
| 240 | crates/nexum-sdk/src/keeper.rs:337 | `ConditionalSource::label` doc "compositions"→"implementations", inverted subject-verb | fix |
| 240 | modules/twap-monitor/src/strategy.rs:111 | doc uses informal "row" vs SDK "watch"/WatchSet terminology | fix |
| 242 | shepherd-sdk/src/cow/run.rs:63 | permanent watch removal logged at info, not warn | leave-open |
| 242 | shepherd-sdk/src/cow/order.rs:85 | new-chain advisory buried mid-sentence; wants `# Note` | leave-open |
| 243 | nexum-macros/src/lib.rs:31 | "the Guest impl" ambiguous + missing `clippy::too_many_arguments` hint | leave-open |
| 243 | nexum-macros/src/lib.rs:133 | `__NexumModuleExport` non-hygienic name (collision precluded by 1-module-per-cdylib) | leave-open |
| 246 | nexum-macros/src/lib.rs:94 | unverified "must not shadow std prelude names" corollary (now duplicated at :303) | leave-open |
| 246 | nexum-macros/src/lib.rs:146 | `on_`-handler error only says "rename"; should suggest separate impl block | leave-open |
| 246 | docs/migration/0.1-to-0.2.md:432 | ambiguous "Earlier drafts of this section" | leave-open |
| 245 | docs/05-sdk-design.md:181 | move "Handlers are synchronous" callout before the code example | leave-open |
| 296 | supervisor/tests.rs:257 | pin-test comment "never depended on toolchain elision" contradicts sibling docs | leave-open |
| 296 | justfile:11 | aggregate `build` recipe omits `build-venue` → dev flow SKIPs pin test (CI half now covered) | leave-open |
| 297 | nexum-venue-test/src/reference.rs:240 | drift-assert instructs blind regeneration vs diagnosing as regression | leave-open |
| 297 | nexum-venue-test/src/codec.rs:34 | duplicate vector names accepted; `Expectation` lacks `deny_unknown_fields` | leave-open |
| 334 | shepherd-sdk/src/cow/composable.rs:60 | enum doc "Post is the only variant never produced" contradicts NeedsInput doc | fix |
| 258 | crates/cow-venue/src/composable.rs:26 | unbounded `static_input: Vec<u8>` OOM note (borsh incremental-alloc already mitigates; cap belongs at ingest) | leave-open |
| 241 | shepherd-sdk-test/src/lib.rs:316 | `enqueue_response` doc omits re-call-extends-sequence footgun | leave-open |
| 241 | shepherd-sdk-test/src/lib.rs:281 | `MockVenue` "observations" jargon vs idempotent-server-state framing | leave-open |
| 241 | nexum-sdk-test/src/lib.rs:205 | `MockLocalStore` doc omits root namespace key = `""` | leave-open |
| 241 | nexum-sdk-test/src/lib.rs:236 | `namespaced()` Panics doc gives wrong reason (should be `""` aliases root) | leave-open |

## Superseded / resolved — safe to close

| PR | count | why |
|----|-------|-----|
| 240 | 1 | **ADR-0013 Verdict seam** — `poll_one` now warns with revert selector + node message on the permanent-drop path (strictly more than asked) |
| 243 | 1 | **Per-module world synthesis** (commit `3f21565`) — hardcoded `shepherd:cow/shepherd` world gone; modules import only declared `[capabilities]`, so the spurious cow-api import is structurally eliminated |
| 245 | 3 | **Doc fixed downstream** ×2 (`CowApiHost` now names `cow_api_request`; `nexum::module` vs `nexum_sdk::module` reconciled by explicit gloss) + **1 superseded** by keeper rename (chassis anchor no longer exists) |
| 296 | 1 | **CI + e2e coverage added** — CI now builds `echo-venue`/`echo-client` (pin test executes, not SKIP) and a real `Supervisor::boot` round-trip test exercises the macro WIT end-to-end |

## Patterns

- **Doc/wording nits dominate** (28 of 47 live), almost all from **lgahdl**, most flagged "leave-open" as low-value end-of-train polish. A large share are byte-identical to the car head — later cars reworded neighbours but never touched the flagged line.
- **Same defect recurs across the venue-adapter surface**: the `derive_header` "pure derivation, no side effects" claim the WIT world cannot enforce appears on **both #249 (adapter.wit:14) and #251 (adapter.rs:26)** — and #249 is now *materially* live because the landed router calls `derive_header` before `guard.check`.
- **Watch-drop diagnostic visibility** (info-vs-warn, silent disappearance) recurs: #240 (resolved downstream), #242 run.rs:63, #334 run.rs:67.
- **Pre-freeze 0.1.0 WIT contract debt** clusters on #247/#248: unit-in-name ambiguity, anonymous vs named ADTs, missing field-level docs, retry semantics, factual token errors — the reviewer consistently pushed to fix these *before* the freeze.
- **"Latent until epic/fork lands"** is a repeated shape for the med items: guard-Deny unreachable (only `AllowAllGuard` ships) #250, `NeedsInput` unreachable #334, `messaging.query` scope hole until 0.3 #249, async guard trait #250 — cheap now, breaking later.
- **Conformance-kit hardening** (#297): recurring "vacuous pass / missing version discriminator / missing `deny_unknown_fields`" theme on the cross-language file formats — same class of guard-gap flagged four times.
- **wit-bindgen version/behaviour misstatements**: the "carries no Display" claim (#251) and the 0.58-vs-0.59 pin skew (#296) both trace to bindgen-version assumptions.
