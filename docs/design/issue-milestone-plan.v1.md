# Videre three-layer split — issue & milestone reorganisation plan

*nullislabs/shepherd · generated 2026-07-15 · DATA ONLY — nothing was created, closed, edited, merged or re-milestoned on GitHub. Apply after review.*

Design source of truth: `docs/design/venue-platform-architecture.md` + `docs/design/videre-split-plan.md`.

## a. Executive summary

The reorg reshapes the eight legacy milestones (M0-M7) into a set that reads top-to-bottom as the videre execution order - P0 (contract reshape/master gate) -> S1 (generic host) -> S1b (CoW on the seam) -> S2 (the gated cut) -> S3 (second venue) - followed by three cross-cutting buckets (videre SDK/DX, the real egress guard, and the rolling debt pile). The biggest structural move is repurposing old M1 (#8): the intent-core it was chartered for is delivered (#137/#135/#131/#222 all close on the M1 train), so #8 becomes the P0 free-WIT-fold milestone anchored on R6 (the acyclicity master gate) and the videre:* rename/normalize/quote work, executed as one oracle-validated git-filter-repo fold. M0 (#7) is repurposed as the S1 long pole - making nexum-runtime venue-agnostic (grow Extension<T>, extract VenueRegistry, delete the pool_router field, zero-leak CI gate) - absorbing the dissolved old-M2 lifecycle hardening. The largest dedup/close wins: eight drafted issues collapse onto existing tracker issues rather than being created (host-identity-signing-backend=#52, egress-guard-hardening-epic=#139, cow-onvidere-epic=#138, cow-venue-cdylib=#324, cow-api-retire=#293, ethflow-keeper=#328, composable-cow-keeper-port=#327, s3-gate-second-protocol-venue=#140), and seven existing issues close as already-delivered-by-the-M1-train or obsoleted by the Phase-0 migration-cruft deletion. #325/#326 fold into #324 (one cow-adapter deliverable) and #329 folds into #293 (the cow-cone retirement). The concrete CoW cone (venue cleave, keeper ports, cow-api retirement) is re-milestoned M1->M2/S1b so it lands after the generic seam it depends on, while venue-agnostic L1 hardening (#294/#266/#53/#51/#107/#244) consolidates under S1. Net: 56 net-new issues across the reshaped set, with two internal-overlap seams flagged for implementation-time coordination - host-generic-component-kind vs adapter-supervision-sweeps (both fold R8) and host-wit-deps-flip-carve vs s2-wit-cross-repo-consumption/s2-three-carves (the L1 slice of the carve). Two existing epics (#274 split, #273 preset-trait) are rescoped rather than duplicated to sit under the drafted split/seam epics, and #136 is rescoped away from its now-wrong 'retire shepherd-sdk' premise since the split keeps shepherd-sdk as the L3 keeper crate.

**By the numbers.** Tracker today: **61 open issues** across **8 live milestones** (plus 5 open issues with no milestone; milestone #1 *Host backends real* is dissolved/closed). The plan:

- **8 milestones repurposed** (renamed + rechartered in place — no new milestones, no deletions) so they read top-to-bottom as the videre execution order **P0 → S1 → S1b → S2 → S3**, then three cross-cutting buckets (videre SDK/DX, real egress guard, rolling debt).
- **56 new issues to create** (out of 64 drafted — the other **8 dedup** onto existing tracker issues).
- **7 issues to close** (delivered by the M1 train or obsoleted by Phase-0 migration-cruft deletion).
- **6 issues to rescope/retitle/re-milestone**; **2 merge groups** (3 issues folded into 2 targets).
- The single hard ordering constraint: **R6 (host↔intent WIT decouple) is the master gate** — until `nexum:host` stops importing `nexum:intent`, an acyclic split is physically impossible. Nothing in S1+ may begin before R6 lands, and no repo carve may begin before all three cut gates (a/b/c) are green.

## b. Reorganised milestone plan

Each milestone below is an **existing** milestone repurposed in place. `New` = drafted issues to create under it; `Moved-in` = existing open issues to re-milestone into it.

### P0 — Videre contract reshape & the R6 master gate

*Repurposes milestone **#8**.*

Repurposes old M1 (intent core is delivered: #137/#135/#131/#222 closed). Now owns the free, in-monorepo pre-release WIT fold that every later phase depends on. Land R6 host<->intent decouple (the master gate: host event carries opaque status bytes); rename nexum:intent/value-flow/adapter -> videre:*; normalize all packages to @0.1.0; add quoting (client+adapter); reshape venue-error (rate-limited{retry-after-ms}+denied) and value-flow (named records); spec the opaque-status destructuring contract and the install-time body-versions handshake key; ship the advisory-only M1 guard posture; and finish the M1 train to a single green linear tip. Executed as ONE oracle-validated git-filter-repo/jj fold. Nothing is pinned so every change is a free recompile; this phase MUST complete before any carve.

**New issues (create here):**
- `split-epic-p0` — Epic (P0): free monorepo reshape - land the master gate that makes an acyclic split possible  _(phase P0, epic)_
- `host-r6-decouple` — R6: decouple nexum:host from nexum:intent - host event carries opaque status bytes (MASTER GATE)  _(phase P0, task)_
- `gap-opaque-status-contract-spec` — Spec the opaque-status destructuring contract (versioned discriminator) that host event commits to - blocks R6  _(phase P0, docs)_
- `videre-epic` — Epic: videre - the generic intent-venue abstraction (L2)  _(phase P0, epic)_
- `videre-wit-rename` — videre: rename nexum:intent/* WIT + readability renames -> videre:*  _(phase P0, task)_
- `videre-wit-surface` — videre: pin the videre:* WIT surface (types / venue / value-flow)  _(phase P0, task)_
- `videre-wit-normalize` — videre: normalize all WIT packages to a single @0.1.0  _(phase P0, chore)_
- `videre-quote` — videre: add quote to videre:venue (client + adapter) + IntentClient typestate  _(phase P0, task)_
- `videre-body-versions-handshake` — videre: install-time body-versions schema handshake  _(phase S1, task)_
- `gap-handshake-manifest-key-decision` — Decide the install-time handshake manifest key (body_version vs version-set) + supported-set match semantics  _(phase P0, docs)_
- `gap-p0-wit-fold-execution` — Execute Phase 0 as ONE oracle-validated git-filter-repo/jj fold across the M1 train (regenerate goldens, re-assert tip oracle)  _(phase P0, chore)_
- `gap-p0-fold-tail-hygiene` — P0 fold tail: codec version discriminator (#297), migration-cruft deletion, denied() MUST-NOT-retry doc  _(phase P0, task)_
- `p0-acyclicity-scaffold` — Land the acyclicity / zero-leak CI gate for nexum-runtime (advisory in P0, blocking in S1)  _(phase P0, chore)_
- `guard-advisory-m1` — guard: ship advisory-only posture for M1 (keep AllowAll, feature-gate the pool import, document the checkpoint as not-yet-enforcing)  _(phase P0, task)_
- `guard-deny-quota` — guard: charge quota on guard-deny to close the busy-loop DoS  _(phase P0, bug)_
- `gap-m1-green-tip-gate` — Gate: finish M1 to a single green linear dev/m1 tip before any carve  _(phase P0, chore)_

### S1 — Generic venue-agnostic host (nexum-runtime L1) + lifecycle

*Repurposes milestone **#7**.*

Repurposes M0 (library-first composable runtime) and absorbs the dissolved old-M2 execution/lifecycle hardening. Now the long-pole S1 phase: make nexum-runtime venue-agnostic by growing Extension<T> to worker/provider roles (service+provider, HostService, ProviderKind), extracting PoolRouter->VenueRegistry as an extension-owned service and DELETING the privileged HostState.pool_router field (the forcing-function acceptance test), extracting the generic supervised-component/host-actor primitive from AdapterActor (folds R8), de-hardcoding the KNOWN table into nexum-world, the bare Ext=() launcher, and the permanent zero-leak CI gate. Plus L1 execution/resource/lifecycle hardening that survives the split unchanged (per-module quotas, fuel accounting, graceful drain, pluggable log seam, WASI allowlist, handler-DoS, router watch-set bound).

**New issues (create here):**
- `host-generalize-epic` — Epic: make nexum-runtime a generic, venue-agnostic component host  _(phase S1, epic)_
- `split-epic-s1` — Epic (S1): make nexum-runtime venue-agnostic - generalize the Extension seam (the long pole)  _(phase S1, epic)_
- `host-extension-seam-roles` — Grow the Extension seam to carry worker/provider roles (service + provider, ProviderKind, HostService)  _(phase S1, task)_
- `host-venue-registry-extract` — Extract PoolRouter -> VenueRegistry as an extension-owned service; delete the privileged supervisor field  _(phase S1, task)_
- `host-generic-component-kind` — Extract a generic supervised-component/host-actor primitive from AdapterActor; collapse match kind (folds R8)  _(phase S1, task)_
- `host-nexum-world-registry` — De-hardcode the KNOWN capability table (drop baked pool + cow-api rows); extract world synthesis to nexum-world lib  _(phase S1, task)_
- `host-zero-leak-ci-gate` — CI gate: nexum-runtime has zero venue/intent/cow symbols  _(phase S1, chore)_
- `host-generic-launcher-bin` — Extract a generic launcher lib + bare Ext=() nexum engine bin (retire the backwards nexum-cli -> cow dep)  _(phase S1, task)_
- `s1-gate-runtime-venue-agnostic` — GATE (a): prove nexum-runtime is venue-agnostic - flip the zero-leak CI check to blocking  _(phase S1, task)_
- `gap-videre-host-platform-crate` — Build the videre-host crate + videre::platform() registration (VenueRegistry + provider-kind + EgressGuard seam + bindgens)  _(phase S1, task)_
- `adapter-supervision-sweeps` — supervisor: fold venue adapters into the restart/poison sweeps and expose adapters_alive (R8)  _(phase S1, task)_

**Moved-in existing issues (re-milestone here):** #294 (runtime: complete execution and lifecycle hardening), #266 (runtime: graceful-shutdown drain for durable state (flush the store / block Ctrl-C on in-flight commits)), #265 (runtime: make the log pipeline pluggable through a ComponentBuilder seam), #244 (security: handler dosing), #107 (runtime: verify fuel accounting during host function calls), #53 (runtime: enforce per-module resource limits and local-store quota), #51 (runtime: extend manifest capability allowlist to the WASI surface), #273 (runtime: Runtime preset trait capability gaps (extensions and pre-built instances)), #321 (runtime: bound the intent-router status-watch set (eviction + config))

### S1b — CoW on the generic seam (concrete venue + keepers)

*Repurposes milestone **#3**.*

Repurposes M2 'Concrete CoW modules'. Prove the generic seam carries a REAL venue, not just echo. Cleave cow-venue into an orderbook-only venue vs a composable-cow keeper (CI-gated clean); build the cow adapter cdylib (#[videre::venue] over wasi:http, #324); settle the idempotency seam before assembly moves into the adapter; port the composable-cow keeper (#327) and ethflow keeper (#328) onto videre:venue/client; retire CowApiHost/cow-api/cow-ext (#293, the biggest lever); own the shepherd-cow event-ABI WITs at L3; rewrite docs/05+08 source-of-truth. The fork-gated poll wire-swap (delete composable.rs) rides here but stays deferred/decoupled. Carries the still-live cow module bugs (#320/#121/#75/#48/#54) into the ported keeper.

**New issues (create here):**
- `split-epic-s1b` — Epic (S1b): CoW on the generic seam - real adapter cdylib + keeper on videre:venue/client  _(phase S1b, epic)_
- `s1b-gate-cow-on-generic-seam` — GATE (b): CoW rides the generic seam - keeper on videre:venue/client, CowApiHost retired  _(phase S1b, task)_
- `cleave-cow-venue` — Cleave cow-venue: orderbook-only venue vs composable-cow keeper  _(phase S1b, task)_
- `cow-idempotency-seam` — Settle the CoW idempotency seam before order assembly moves into the adapter  _(phase S1b, task)_
- `shepherd-cow-event-abi-wits` — Own the shepherd-cow event-ABI WITs at L3  _(phase S1b, task)_
- `composable-poll-wire-swap` — Composable-cow poll wire-swap: delete composable.rs / LegacyRevertAdapter (fork-gated)  _(phase deferred, task)_
- `gap-docs-source-of-truth-rewrite` — Rewrite docs/05 + docs/08 as source-of-truth: venue persona is shipped; adapters are THE extension mechanism; cow-api is legacy read-path  _(phase S1b, docs)_

**Moved-in existing issues (re-milestone here):** #138 (intent: CoW venue adapter and flagship module ports), #324 (cow: build the cow adapter component (adapter slice) with timeout transport middleware), #327 (modules: re-point twap-monitor onto pool submit/status via the cow adapter), #328 (modules: re-point ethflow-watcher onto the pool observe/status path via the cow adapter), #293 (cow: retire the legacy cow-api host shim and cow cone), #323 (cow: ratify or reconcile the retry classification table vs cowprotocol::ApiError::retry_hint), #320 (twap: one-block retry before dropping on InvalidEip1271Signature (same-block wiring+create race)), #121 (sdk+twap: treat DuplicatedOrder as already-submitted; add errorType→retry classification), #75 (twap-monitor retries unknown revert selectors every block forever), #48 (twap-monitor orphaned gate markers leak on decode-failure path), #54 (Support ComposableCoW v2 ConditionalOrderRemoved event), #64 (Grant M2 ComposableCoW contract-modification deliverable divergence)

### S2 — The gated three-repo cut & delivery infrastructure

*Repurposes milestone **#4**.*

Repurposes M3 'Infrastructure'. The physical split, gated on all three cut gates (a runtime venue-agnostic, b cow on the generic seam, c genuine second-protocol venue). Transitional path-dep workspace in the three groupings; wit-deps flip + git-tag sourcing + wkg/OCI convergence; the cut go/no-go checklist; three history-preserving git-filter-repo carves (nexum-runtime / videre / CoW-on-videre) with the byte-identical tip oracle. Plus operator delivery: CI/CD hardening (incl. the sccache fork-PR fail-open #337), the multi-chain provider map + docs, ghcr packaging, and the Swarm remote-store backend.

**New issues (create here):**
- `split-epic-s2` — Epic (S2): the gated repo cut - transitional workspace, WIT plumbing, three history-preserving carves  _(phase S2, epic)_
- `s2-transitional-workspace` — Transitional path-dep cargo workspace in the 3 groupings (+ git-tag pin path + dep-sync CI)  _(phase S2, task)_
- `s2-wit-cross-repo-consumption` — WIT cross-repo consumption: wit-deps flip + git-tag sourcing + wkg/OCI registry convergence  _(phase S2, task)_
- `s2-cut-gate-checklist` — Cut go/no-go gate: assert (a) runtime venue-agnostic + (b) cow on the seam + (c) second-protocol venue before any carve  _(phase S2, task)_
- `s2-three-carves` — Three history-preserving git-filter-repo carves: nexum-runtime / videre / CoW-on-videre  _(phase S2, task)_
- `host-wit-deps-flip-carve` — Flip nexum:host WIT to crate-local wit-deps and carve nexum-runtime as the L1 repo  _(phase S2, task)_

**Moved-in existing issues (re-milestone here):** #274 (epic: split the shepherd instantiation into its own repository), #337 (ci: sccache R2 config (#336) hard-fails every fork PR — secrets don't flow to fork pull_request runs), #151 (remote-store: real Swarm backend), #125 (packaging: ghcr image name mismatch breaks fresh-server docker compose pull), #124 (docs: multi-chain deployment patterns (Mainnet/Arbitrum/Base/Gnosis))

### S3 — Second-venue acceptance & vocabulary freeze

*Repurposes milestone **#10**.*

Repurposes M6 'Second venue and vocabulary freeze'. The acceptance phase that de-risks R1: build a genuine non-cow second-protocol venue (rfq or amm-router, #140) against videre-sdk alone, exercising quote and surfaces CoW does not, feeding contract fixes back pre-cut. Cut gate (c). Then the videre:value-flow 1.0 freeze decisions (#330, retitled) and the curated adapter registry + consent surface (#141).

**New issues (create here):**
- `split-epic-s3` — Epic (S3): second-venue acceptance - prove videre is genuinely venue-neutral  _(phase S3, epic)_

**Moved-in existing issues (re-milestone here):** #140 (intent: postage adapter as N=2 and the value-flow freeze), #330 (wit: freeze-gate decisions for nexum:value-flow (asset-amount canonicalization, native-token settlement)), #141 (intent: curated adapter registry and consent surface)

### videre SDK, macros & reth/alloy DX

*Repurposes milestone **#5**.*

Repurposes M4 'SDK and DX'. The venue/keeper author front door and DX build-out: videre-sdk (renamed from nexum-venue-sdk, + Keeper::sweep assembler + VenueClient), #[videre::venue] single blessed path, #[videre::keeper] + typed VenueClient<V>, the videre-test conformance kit, guest SDK seams+Mocks for identity/messaging/remote-store, the alloy Provider chain seam, and the reth/alloy DX polish cluster (VenueFault mirror, Order builder, uniform non_exhaustive, sealed traits, single-source fault/KNOWN const). Plus the grant-scoped DX deliverables and residual nexum-sdk/macro consolidation.

**New issues (create here):**
- `videre-sdk-crate` — videre-sdk: rename nexum-venue-sdk + add Keeper::sweep assembler + VenueClient  _(phase S1, task)_
- `videre-venue-macro` — #[videre::venue]: single blessed authoring path emitting impl VenueAdapter  _(phase S1, task)_
- `videre-keeper-macro` — #[videre::keeper] macro + typed VenueClient<V>  _(phase S1b, task)_
- `videre-conformance-kit` — videre-test: conformance kit + cargo-test-fails-if-wire-drifts gate  _(phase S1, task)_
- `host-backend-guest-seams` — Add guest SDK seams + Mocks for identity/messaging/remote-store, wired to the stub backends  _(phase deferred, task)_
- `gap-alloy-provider-seam` — Chain DX: alloy Provider seam over ChainHost::request (HostTransport: alloy Transport) + carry ChainMethod to the guest  _(phase deferred, task)_
- `gap-dx-polish-cluster` — reth/alloy DX polish cluster: VenueFault mirror, Order builder, uniform non_exhaustive, sealed traits, single-source fault/KNOWN const, kill *_to_golden bridges  _(phase deferred, task)_

**Moved-in existing issues (re-milestone here):** #291 (local-store: add contains, len and count metadata queries), #264 (sdk: convert the bind-macro error/level shims to From impls), #127 (epic: grant delivery plan - remaining PRs, evidence runs, and sequencing to July 31), #136 (sdk: one SDK plan - nexum-sdk, macros, clean break of shepherd-sdk), #322 (sdk: make the IntentBody derive no_std (emit ::core/::alloc paths))

### Egress guard (real, teeth)

*Repurposes milestone **#9**.*

Keeps M5 'Egress guard' as the home for the REAL (non-AllowAll) guard, deferred wholly per decision 3 (M1 is advisory-only). Single-decode / derive-before-guard TOCTOU fix, move the checkpoint to the signed unsigned-tx / identity boundary, GuardPolicy::check sync->async, bring http egress under the compile-time world guarantee, messaging.query scope enforcement, mock-grant fidelity. Anchored by the rescoped guard epic #139 and gated on the real keystore identity backend #52.

**New issues (create here):**
- `guard-derive-before-guard` — guard: close the derive-header-before-guard side-effect escape and the TOCTOU double-decode (single-decode the body through the checkpoint)  _(phase deferred, task)_
- `guard-signing-boundary` — guard: move the checkpoint to the signed unsigned-tx / identity boundary so the requires-signing path is covered  _(phase deferred, task)_
- `guard-policy-async` — guard: make GuardPolicy::check async (the real guard needs I/O: simulate, remote analyzers)  _(phase deferred, task)_
- `guard-egress-cap-world-guarantee` — guard: bring venue egress capabilities (http + import-narrowing) under the compile-time world guarantee  _(phase deferred, task)_
- `messaging-query-scope` — messaging: enforce messaging.query scope (goes live with the 0.3 Waku backend)  _(phase deferred, bug)_
- `mock-grant-fidelity` — capabilities: align mock capability-grant fidelity to the real host grant  _(phase deferred, task)_

**Moved-in existing issues (re-milestone here):** #139 (guard: simulate, analyzers, policy, identity checkpoint), #52 (identity: real signing backend (keystore))

### Post-v1 hardening & debt (rolling)

*Repurposes milestone **#6**.*

Keeps M7 as the rolling, non-gating debt bucket. Deferred videre concepts (maker-side offer #355, RFQ firm-quote additive, Materialiser<Source,Venue>), doc-consistency passes, typed-fault/backend debt, test-harness/clock-seam debt, perf (parking_lot, bulk getLogs), the deferred messaging/remote-store backends and payload-codec convention, and the grant soak/reporting items.

**New issues (create here):**
- `rfq-firm-quote-additive` — videre: RFQ firm-quote - additive firm: option<firm-quote> on the quote record (taker-side, when a real RFQ venue appears)  _(phase deferred, task)_
- `materialiser-source-venue` — videre: Materialiser<Source, Venue> - the venue-neutral keeper materialiser (M7)  _(phase deferred, task)_

**Moved-in existing issues (re-milestone here):** #355 (videre: add 'offer' / provide-liquidity (maker-side, two-sided venues) — post-0.1), #341 (docs: doc 02 says subscriptions wire up before init - actual boot order is the opposite), #302 (runtime: wide-range eth_getLogs bulk backfill for large log gaps), #289 (docs: typed-fault doc consistency pass (ADR-0011, rpc, migration, house-style)), #288 (chain: flatten request-batch's dead outer chain-error or record the escape hatch), #286 (sdk: From/TryFrom between the wit-bindgen Fault and the SDK Fault), #285 (runtime: richer typed faults for the remote-store, identity and messaging backends), #284 (test: give the supervisor's poison window and restart backoff a clock seam), #283 (test: grow the harness a multi-module variant and port the boot_single e2e tests), #280 (perf: migrate std::sync locks to parking_lot where not held across await), #269 (host: populate Fault::RateLimited.retry_after_ms and map 429/timeout by type), #212 (messaging: payload encoding convention for nexum-native topics), #152 (messaging: real Waku publish backend), #105 (state: batch and host-side filtered operations on the state seam), #65 (7-day unattended soak test not evidenced)

## c. New issues to create

| key | title | milestone | phase | depends_on |
|-----|-------|-----------|-------|------------|
| `split-epic-p0` | Epic (P0): free monorepo reshape - land the master gate that makes an acyclic split possible | M1: Intent core and CoW venue adapter | P0 | — |
| `host-r6-decouple` | R6: decouple nexum:host from nexum:intent - host event carries opaque status bytes (MASTER GATE) | M1: Intent core and CoW venue adapter | P0 | — |
| `gap-opaque-status-contract-spec` | Spec the opaque-status destructuring contract (versioned discriminator) that host event commits to - blocks R6 | videre-split: P0 (pre-fold) | P0 | — |
| `videre-epic` | Epic: videre - the generic intent-venue abstraction (L2) | M1: Intent core and CoW venue adapter | P0 | host-r6-decouple, host-seam-generalize |
| `videre-wit-rename` | videre: rename nexum:intent/* WIT + readability renames -> videre:* | M1: Intent core and CoW venue adapter | P0 | host-r6-decouple |
| `videre-wit-surface` | videre: pin the videre:* WIT surface (types / venue / value-flow) | M1: Intent core and CoW venue adapter | P0 | videre-wit-rename |
| `videre-wit-normalize` | videre: normalize all WIT packages to a single @0.1.0 | M1: Intent core and CoW venue adapter | P0 | videre-wit-rename |
| `videre-quote` | videre: add quote to videre:venue (client + adapter) + IntentClient typestate | M1: Intent core and CoW venue adapter | P0 | videre-wit-surface |
| `videre-body-versions-handshake` | videre: install-time body-versions schema handshake | M1: Intent core and CoW venue adapter | S1 | videre-wit-surface, host-seam-generalize |
| `gap-handshake-manifest-key-decision` | Decide the install-time handshake manifest key (body_version vs version-set) + supported-set match semantics | videre-split: P0 (decisions) | P0 | — |
| `gap-p0-wit-fold-execution` | Execute Phase 0 as ONE oracle-validated git-filter-repo/jj fold across the M1 train (regenerate goldens, re-assert tip oracle) | videre-split: P0 (fold) | P0 | host-r6-decouple, gap-opaque-status-contract-spec, videre-wit-rename, videre-wit-normalize, videre-quote, videre-wit-surface, gap-p0-fold-tail-hygiene |
| `gap-p0-fold-tail-hygiene` | P0 fold tail: codec version discriminator (#297), migration-cruft deletion, denied() MUST-NOT-retry doc | videre-split: P0 (fold) | P0 | videre-wit-surface |
| `p0-acyclicity-scaffold` | Land the acyclicity / zero-leak CI gate for nexum-runtime (advisory in P0, blocking in S1) | M3: Infrastructure | P0 | — |
| `guard-advisory-m1` | guard: ship advisory-only posture for M1 (keep AllowAll, feature-gate the pool import, document the checkpoint as not-yet-enforcing) | M1: Intent core and CoW venue adapter | P0 | egress-guard-hardening-epic |
| `guard-deny-quota` | guard: charge quota on guard-deny to close the busy-loop DoS | M5: Egress guard | P0 | egress-guard-hardening-epic |
| `gap-m1-green-tip-gate` | Gate: finish M1 to a single green linear dev/m1 tip before any carve | M1 (green tip) | P0 | gap-p0-wit-fold-execution, guard-deny-quota, videre-body-versions-handshake |
| `host-generalize-epic` | Epic: make nexum-runtime a generic, venue-agnostic component host | M0: Runtime architecture and lifecycle | S1 | — |
| `split-epic-s1` | Epic (S1): make nexum-runtime venue-agnostic - generalize the Extension seam (the long pole) | M0: Runtime architecture and lifecycle | S1 | split-epic-p0 |
| `host-extension-seam-roles` | Grow the Extension seam to carry worker/provider roles (service + provider, ProviderKind, HostService) | M0: Runtime architecture and lifecycle | S1 | host-r6-decouple |
| `host-venue-registry-extract` | Extract PoolRouter -> VenueRegistry as an extension-owned service; delete the privileged supervisor field | M0: Runtime architecture and lifecycle | S1 | host-extension-seam-roles |
| `host-generic-component-kind` | Extract a generic supervised-component/host-actor primitive from AdapterActor; collapse match kind (folds R8) | M0: Runtime architecture and lifecycle | S1 | host-extension-seam-roles |
| `host-nexum-world-registry` | De-hardcode the KNOWN capability table (drop baked pool + cow-api rows); extract world synthesis to nexum-world lib | M0: Runtime architecture and lifecycle | S1 | host-extension-seam-roles |
| `host-zero-leak-ci-gate` | CI gate: nexum-runtime has zero venue/intent/cow symbols | M0: Runtime architecture and lifecycle | S1 | host-venue-registry-extract, host-nexum-world-registry, host-generic-component-kind |
| `host-generic-launcher-bin` | Extract a generic launcher lib + bare Ext=() nexum engine bin (retire the backwards nexum-cli -> cow dep) | M0: Runtime architecture and lifecycle | S1 | host-extension-seam-roles |
| `s1-gate-runtime-venue-agnostic` | GATE (a): prove nexum-runtime is venue-agnostic - flip the zero-leak CI check to blocking | M0: Runtime architecture and lifecycle | S1 | p0-acyclicity-scaffold |
| `gap-videre-host-platform-crate` | Build the videre-host crate + videre::platform() registration (VenueRegistry + provider-kind + EgressGuard seam + bindgens) | videre-split: S1 (generalization) | S1 | host-extension-seam-roles, host-venue-registry-extract, host-generic-component-kind |
| `adapter-supervision-sweeps` | supervisor: fold venue adapters into the restart/poison sweeps and expose adapters_alive (R8) | M5: Egress guard | S1 | egress-guard-hardening-epic |
| `split-epic-s1b` | Epic (S1b): CoW on the generic seam - real adapter cdylib + keeper on videre:venue/client | M2: Concrete CoW modules | S1b | split-epic-s1 |
| `s1b-gate-cow-on-generic-seam` | GATE (b): CoW rides the generic seam - keeper on videre:venue/client, CowApiHost retired | M2: Concrete CoW modules | S1b | s1-gate-runtime-venue-agnostic |
| `cleave-cow-venue` | Cleave cow-venue: orderbook-only venue vs composable-cow keeper | M1: Intent core and CoW venue adapter | S1b | cow-onvidere-epic |
| `cow-idempotency-seam` | Settle the CoW idempotency seam before order assembly moves into the adapter | M1: Intent core and CoW venue adapter | S1b | cow-onvidere-epic, cleave-cow-venue |
| `shepherd-cow-event-abi-wits` | Own the shepherd-cow event-ABI WITs at L3 | M2: Concrete CoW modules | S1b | cow-onvidere-epic |
| `composable-poll-wire-swap` | Composable-cow poll wire-swap: delete composable.rs / LegacyRevertAdapter (fork-gated) | M2: Concrete CoW modules | deferred | cow-onvidere-epic, composable-cow-keeper-port |
| `gap-docs-source-of-truth-rewrite` | Rewrite docs/05 + docs/08 as source-of-truth: venue persona is shipped; adapters are THE extension mechanism; cow-api is legacy read-path | videre-split: S1b | S1b | cow-api-retire |
| `split-epic-s2` | Epic (S2): the gated repo cut - transitional workspace, WIT plumbing, three history-preserving carves | M3: Infrastructure | S2 | split-epic-s1b |
| `s2-transitional-workspace` | Transitional path-dep cargo workspace in the 3 groupings (+ git-tag pin path + dep-sync CI) | M3: Infrastructure | S2 | s1-gate-runtime-venue-agnostic, s1b-gate-cow-on-generic-seam |
| `s2-wit-cross-repo-consumption` | WIT cross-repo consumption: wit-deps flip + git-tag sourcing + wkg/OCI registry convergence | M3: Infrastructure | S2 | s2-transitional-workspace |
| `s2-cut-gate-checklist` | Cut go/no-go gate: assert (a) runtime venue-agnostic + (b) cow on the seam + (c) second-protocol venue before any carve | M3: Infrastructure | S2 | s1-gate-runtime-venue-agnostic, s1b-gate-cow-on-generic-seam, s3-gate-second-protocol-venue |
| `s2-three-carves` | Three history-preserving git-filter-repo carves: nexum-runtime / videre / CoW-on-videre | M3: Infrastructure | S2 | s2-cut-gate-checklist, s2-transitional-workspace, s2-wit-cross-repo-consumption |
| `host-wit-deps-flip-carve` | Flip nexum:host WIT to crate-local wit-deps and carve nexum-runtime as the L1 repo | M6: Second venue and vocabulary freeze | S2 | host-zero-leak-ci-gate, host-generic-launcher-bin |
| `split-epic-s3` | Epic (S3): second-venue acceptance - prove videre is genuinely venue-neutral | M6: Second venue and vocabulary freeze | S3 | split-epic-s1 |
| `videre-sdk-crate` | videre-sdk: rename nexum-venue-sdk + add Keeper::sweep assembler + VenueClient | M4: SDK and DX | S1 | videre-wit-surface, host-seam-generalize |
| `videre-venue-macro` | #[videre::venue]: single blessed authoring path emitting impl VenueAdapter | M4: SDK and DX | S1 | videre-sdk-crate |
| `videre-keeper-macro` | #[videre::keeper] macro + typed VenueClient<V> | M4: SDK and DX | S1b | videre-venue-macro, videre-quote |
| `videre-conformance-kit` | videre-test: conformance kit + cargo-test-fails-if-wire-drifts gate | M4: SDK and DX | S1 | videre-sdk-crate |
| `host-backend-guest-seams` | Add guest SDK seams + Mocks for identity/messaging/remote-store, wired to the stub backends | M4: SDK and DX | deferred | — |
| `gap-alloy-provider-seam` | Chain DX: alloy Provider seam over ChainHost::request (HostTransport: alloy Transport) + carry ChainMethod to the guest | post-M1: DX build-out (Phase 4) | deferred | — |
| `gap-dx-polish-cluster` | reth/alloy DX polish cluster: VenueFault mirror, Order builder, uniform non_exhaustive, sealed traits, single-source fault/KNOWN const, kill *_to_golden bridges | post-M1: DX build-out (Phase 4) | deferred | — |
| `guard-derive-before-guard` | guard: close the derive-header-before-guard side-effect escape and the TOCTOU double-decode (single-decode the body through the checkpoint) | M5: Egress guard | deferred | egress-guard-hardening-epic |
| `guard-signing-boundary` | guard: move the checkpoint to the signed unsigned-tx / identity boundary so the requires-signing path is covered | M5: Egress guard | deferred | egress-guard-hardening-epic, #52, #139 |
| `guard-policy-async` | guard: make GuardPolicy::check async (the real guard needs I/O: simulate, remote analyzers) | M5: Egress guard | deferred | egress-guard-hardening-epic |
| `guard-egress-cap-world-guarantee` | guard: bring venue egress capabilities (http + import-narrowing) under the compile-time world guarantee | M5: Egress guard | deferred | egress-guard-hardening-epic |
| `messaging-query-scope` | messaging: enforce messaging.query scope (goes live with the 0.3 Waku backend) | M5: Egress guard | deferred | egress-guard-hardening-epic |
| `mock-grant-fidelity` | capabilities: align mock capability-grant fidelity to the real host grant | M5: Egress guard | deferred | egress-guard-hardening-epic |
| `rfq-firm-quote-additive` | videre: RFQ firm-quote - additive firm: option<firm-quote> on the quote record (taker-side, when a real RFQ venue appears) | M7: Post-v1 hardening and debt | deferred | #355 |
| `materialiser-source-venue` | videre: Materialiser<Source, Venue> - the venue-neutral keeper materialiser (M7) | M7: Post-v1 hardening and debt | deferred | — |

*(56 new issues. `depends_on` entries prefixed `#` are existing tracker issues; bare keys are other new issues in this table. Entries like `host-seam-generalize` in a few drafts are alias references to the seam-generalization work now owned by `host-extension-seam-roles` + `gap-videre-host-platform-crate`.)*

## d. Existing issues to CLOSE

| # | title | reason |
|---|-------|--------|
| #339 | docs: migration guide still calls the manifest `nexum.toml` throughout (canonical is `module.toml`) | Obsolete: fixes nexum.toml->module.toml inside docs/migration/0.1-to-0.2.md, which is already deleted (HEAD 7c66b6c) as Phase-0 migration cruft. Fixing a deleted file is moot. |
| #287 | cow-ext: map reqwest timeout and 429 to Fault::Timeout / Fault::RateLimited | Targets shepherd-cow-host ext_cow.rs - the legacy cow-api extension that cow-api-retire (#293) deletes. The timeout/429->typed-fault requirement is carried forward by the cow-venue adapter's errorType->venue-error projection + the R1 venue-error reshape; the host-chain equivalent lives on as #269. |
| #222 | sdk: keeper ConditionalSource trait, retry dispatch (Retrier), and cow::run loop | Delivered by #239: ConditionalSource/Retrier/RetryAction/cow::run landed in nexum-sdk/keeper.rs with the M1 train. Forward Verdict/Keeper::sweep rework is carried by videre-sdk-crate. |
| #137 | intent: WIT packages, venue-adapter world, pool router | Delivered by the M1 train (#226-#234): nexum:value-flow+nexum:intent WIT, venue-adapter world, PoolRouter, nexum-venue-sdk, conformance kit, echo-venue. Successor is the drafted videre-epic (rename/quote/normalize/R6), not a reopen. |
| #135 | sdk: extract the venue-generic strategy chassis | Delivered by the M1 train (#146/#222/#148/#147/#149): the keeper primitives + single-venue loop landed in nexum-sdk/keeper.rs. Deferred generalization is videre-sdk-crate (Keeper::sweep) + materialiser-source-venue (M7). |
| #131 | docs: record the intent architecture (venue adapters, egress guard) | Docs-only, delivered by PR #132 (docs 08/09 + egress-guard ADR); the design docs now exist. Go-forward doc work is gap-docs-source-of-truth-rewrite. |
| #7 | Roadmap: v0.3 — make the runtime real | Stale pre-restructure roadmap epic, no milestone. Its goal is largely delivered (chain/local-store/logging live) and its workstreams are decomposed into M0-M7 + individual issues (#52/#152/#151/#285). Nothing tracks against it. |

## e. MODIFY / rescope

| # | title | change |
|---|-------|--------|
| #139 | guard: simulate, analyzers, policy, identity checkpoint | Rescope to fold the venue-platform R3/R5/R8 router/capability/lifecycle hardening the guard engine epic did not enumerate (single-decode/derive-before-guard, signed-tx boundary, GuardPolicy async, http-under-world-guarantee, messaging.query scope, adapter sweeps, mock fidelity) as children. Keep M5. This IS the drafted egress-guard-hardening-epic; nothing shipped. Depends on #52 (identity) and stays advisory-only for M1. |
| #330 | wit: freeze-gate decisions for nexum:value-flow (asset-amount canonicalization, native-token settlement) | Retitle the package under the videre rename: nexum:value-flow -> videre:value-flow. Keep the two freeze-gate ontology decisions (minimal-length canonical amount encoding; native-token representable-but-invalid) - NOT addressed by the Phase-0 named-records reshape. Stays M6/S3 as the freeze gate, distinct from videre-wit-surface (reshape, not freeze). |
| #289 | docs: typed-fault doc consistency pass (ADR-0011, rpc, migration, house-style) | Drop the docs/migration/0.1-to-0.2.md bullet (that file is deleted as Phase-0 cruft). Keep the still-valid items: ADR-0011 {errorType,description,data} restoration, docs/07 rpc data-payload callout, docs/production.md house-style pass, stale .mmd/.png regen. Stays M7. Distinct from gap-docs-source-of-truth-rewrite (docs/05+08 venue-persona). |
| #274 | epic: split the shepherd instantiation into its own repository | Rescope from a TWO-repo split (core + shepherd instantiation) to the THREE-repo umbrella (nexum-runtime <- videre <- CoW-on-videre) spanning the drafted split-epic family (split-epic-p0/s1/s1b/s2/s3; closest single correspondent split-epic-s2, the physical cut). Update the two-way partition to three-way; fold its post-split preset items into the generalization. Re-milestone M7->M3. |
| #273 | runtime: Runtime preset trait capability gaps (extensions and pre-built instances) | Rescope: the preset/Runtime-trait launch-surface ask is subsumed by the S1 seam generalization (host-extension-seam-roles + gap-videre-host-platform-crate grow Extension<T>; host-generic-launcher-bin retires the backwards nexum-cli->cow dep with a bare Ext=() bin). Residual = the MockRuntime preset path (rolls into host-backend-guest-seams/#80). Re-milestone M7->M0. |
| #136 | sdk: one SDK plan - nexum-sdk, macros, clean break of shepherd-sdk | Rescope: premise is now wrong - the videre split does NOT retire shepherd-sdk (recast as the L3 CoW keeper crate, kept). Residual: (a) nexum-venue-sdk->videre-sdk rename is videre-sdk-crate; (b) 'clean break / workspace-member removal' becomes the S2 carve (s2-three-carves). Retitle to the surviving nexum-sdk/macro consolidation; re-milestone M1->M4. |

## f. MERGE

| from → into | titles |
|-------------|--------|
| #325, #326 → #324 | #325 cow: publish golden vectors and wire the conformance kit ; #326 cow: bundle the cow adapter into the distribution (config-installed, not compiled in) → **#324 cow: build the cow adapter component (adapter slice) with timeout transport middleware** |
| #329 → #293 | #329 sdk: retire the shepherd-sdk workspace member and migrate remaining dependents → **#293 cow: retire the legacy cow-api host shim and cow cone** |

## g. DEDUP (drafted key ↔ existing #)

These drafted issues are **not created** — the existing tracker issue already covers them (rescope the existing one per section e where noted).

| drafted key | existing # | existing title |
|-------------|-----------|----------------|
| `host-identity-signing-backend` | #52 | identity: real signing backend (keystore) |
| `egress-guard-hardening-epic` | #139 | guard: simulate, analyzers, policy, identity checkpoint |
| `cow-onvidere-epic` | #138 | intent: CoW venue adapter and flagship module ports |
| `cow-venue-cdylib` | #324 | cow: build the cow adapter component (adapter slice) with timeout transport middleware |
| `cow-api-retire` | #293 | cow: retire the legacy cow-api host shim and cow cone |
| `ethflow-keeper` | #328 | modules: re-point ethflow-watcher onto the pool observe/status path via the cow adapter |
| `composable-cow-keeper-port` | #327 | modules: re-point twap-monitor onto pool submit/status via the cow adapter |
| `s3-gate-second-protocol-venue` | #140 | intent: postage adapter as N=2 and the value-flow freeze |

## h. Ordered apply sequence

Respects phase order (P0 → S1 → S1b → S2 → S3) and the R6 master gate. Steps 1–4 are pure bookkeeping and can be done immediately; issue **creation** (step 5+) follows the dependency chain.

**Step 1 — Repurpose the 8 milestones (retitle + recharter in place).**
- Rename each existing milestone to its new charter name (section b): #8→P0, #7→S1, #3→S1b, #4→S2, #5→videre SDK/DX, #9→Egress guard, #10→S3, #6→Post-v1 debt.
- No milestones are created or deleted; milestone #1 stays dissolved/closed.

**Step 2 — Close the 7 delivered/obsolete issues (clears the board first).**
- Close #339 — docs: migration guide still calls the manifest `nexum.toml` throughout (canonical is `module.toml`).
- Close #287 — cow-ext: map reqwest timeout and 429 to Fault::Timeout / Fault::RateLimited.
- Close #222 — sdk: keeper ConditionalSource trait, retry dispatch (Retrier), and cow::run loop.
- Close #137 — intent: WIT packages, venue-adapter world, pool router.
- Close #135 — sdk: extract the venue-generic strategy chassis.
- Close #131 — docs: record the intent architecture (venue adapters, egress guard).
- Close #7 — Roadmap: v0.3 — make the runtime real.

**Step 3 — Resolve the 2 merge groups.**
- Fold #325 + #326 into #324 (single cow-adapter-cdylib deliverable); close #325/#326 as merged.
- Fold #329 into #293 (cow-api-cone retirement); close #329 as merged.

**Step 4 — Rescope/retitle/re-milestone the 6 modify issues (section e).**
- #139 → the egress-guard hardening epic (M5, keep). #330 → videre:value-flow freeze (M6). #289 → drop the deleted-file bullet, keep the rest (M7). #274 → three-repo umbrella (M3). #273 → subsumed by S1 seam, keep MockRuntime residual (M0). #136 → nexum-sdk/macro consolidation (M4).

**Step 5 — P0: create the master-gate chain FIRST (milestone #8 / P0).**
- Create `gap-opaque-status-contract-spec` (design note the gate implements against) and `gap-handshake-manifest-key-decision` — no deps.
- Create `split-epic-p0` and `videre-epic` (epics).
- Create `host-r6-decouple` (THE MASTER GATE) — it and the videre WIT-reshape issues (`videre-wit-rename` → `videre-wit-surface`/`videre-wit-normalize`/`videre-quote`) all land together in ONE fold.
- Create the fold-execution + hygiene owners: `gap-p0-fold-tail-hygiene`, `gap-p0-wit-fold-execution`.
- Create `p0-acyclicity-scaffold` (advisory CI), `guard-advisory-m1`, `guard-deny-quota`, `videre-body-versions-handshake`, and the tip gate `gap-m1-green-tip-gate`.
- Execute the Phase-0 fold; land M1 to a single green linear `dev/m1` tip (gap-m1-green-tip-gate) before ANY carve.

**Step 6 — S1: generalize the host (milestone #7 / S1). Blocked on R6.**
- Create epics `host-generalize-epic`, `split-epic-s1`.
- Create `host-extension-seam-roles` (the long pole) → then `host-venue-registry-extract`, `host-generic-component-kind`, `host-nexum-world-registry`, `host-generic-launcher-bin`, `gap-videre-host-platform-crate`, `adapter-supervision-sweeps`.
- Create `host-zero-leak-ci-gate`; then `s1-gate-runtime-venue-agnostic` — flip the zero-leak check to BLOCKING; delete `HostState.pool_router`. This is cut gate (a).

**Step 7 — videre SDK/DX (milestone #5) — proceeds alongside S1 once the seam exists.**
- Create `videre-sdk-crate` → `videre-venue-macro`, `videre-conformance-kit`; then `videre-keeper-macro`. Plus `host-backend-guest-seams`, `gap-alloy-provider-seam`, `gap-dx-polish-cluster`.

**Step 8 — S1b: CoW on the generic seam (milestone #3). Blocked on gate (a).**
- Create epic `split-epic-s1b`; then `cleave-cow-venue` → `cow-idempotency-seam` → `cow-venue-cdylib` (=#324, rescope not create) → `composable-cow-keeper-port` (=#327) → `ethflow-keeper` (=#328); `shepherd-cow-event-abi-wits`; `cow-api-retire` (=#293); `gap-docs-source-of-truth-rewrite`.
- Create `s1b-gate-cow-on-generic-seam` — cut gate (b). Keep `composable-poll-wire-swap` deferred/fork-gated and DECOUPLED from the keeper port.

**Step 9 — S3 gate (milestone #10) — needed as cut gate (c), runs in parallel after gate (a).**
- Create epic `split-epic-s3`; `s3-gate-second-protocol-venue` (=#140, rescope) — a genuine non-cow venue against videre-sdk alone. Gate (c).

**Step 10 — S2: the gated cut (milestone #4). Blocked on gates a+b+c ALL green.**
- Create epic `split-epic-s2`; `s2-transitional-workspace` → `s2-wit-cross-repo-consumption`; `s2-cut-gate-checklist` (asserts a+b+c); `host-wit-deps-flip-carve`; finally `s2-three-carves` (the three history-preserving carves). NOTHING carves until the checklist is green.

**Step 11 — Egress guard, real teeth (milestone #9) — deferred, gated on #52 + #139.**
- Rescoped #139 is the epic. Create children `guard-derive-before-guard`, `guard-signing-boundary`, `guard-policy-async`, `guard-egress-cap-world-guarantee`, `messaging-query-scope`, `mock-grant-fidelity`. (`host-identity-signing-backend` = #52, rescope not create.)

**Step 12 — Rolling debt (milestone #6).**
- Create `rfq-firm-quote-additive` (dep #355) and `materialiser-source-venue` when their prerequisites (a second real venue) exist.

> Two internal-overlap seams to coordinate at implementation time (flagged in the reconcile summary): `host-generic-component-kind` vs `adapter-supervision-sweeps` (both fold R8), and `host-wit-deps-flip-carve` vs `s2-wit-cross-repo-consumption`/`s2-three-carves` (the L1 slice of the carve).
