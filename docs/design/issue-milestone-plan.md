# Videre three-layer split: issue and milestone plan (v3)

Reorganisation plan for `nullislabs/shepherd`, generated against the develop tracker state (61 open issues). This document is DATA and PLANNING only: nothing here has been created, closed, edited, or re-milestoned on GitHub. The machine-readable source of truth is `docs/design/issue-milestone-plan.json`; the architecture rationale lives in `docs/design/venue-platform-architecture.md` and `docs/design/videre-split-plan.md`.

The target platform is three layers with one acyclic dependency edge each: `nexum-runtime` (the universal host) is consumed by `videre` (the generic intent and venue abstraction), which is consumed by `shepherd` (the concrete CoW bundle). The milestones below read top to bottom as the execution order that gets us there.

Owner decisions folded in:

- D1: cut the three repos on two proofs only (host venue-agnostic, cow on the generic seam); the genuine second venue is post-cut acceptance, not a pre-cut gate.
- D2: chronological milestone spine, SDK early, guard late.
- D3: `shepherd` is the CoW bundle; the old `shepherd-sdk` is absorbed into it, not kept standalone.
- D4: epic decomposition via native GitHub sub-issues, every issue placed in Project #1 with exactly one Component, the Version field left unset.

Nine milestones are used (mfw78, 2026-07-16): milestone #7 stays as M0, the mostly-complete historical runtime; everything else stacks on top, and a brand-new milestone M2 holds the host-generalization work.

## 1. Chronological milestone walk

| Milestone | GitHub milestone | Focus | Epics | Child issues |
|-----------|------------------|-------|-------|--------------|
| M0: Runtime architecture and lifecycle | #7 (kept as-is) | The historical, mostly-complete runtime. Its only tracked open work is the execution and lifecycle hardening under epic #294. | 1 | 6 |
| M1: Videre contract reshape and host-intent decoupling | #8 (renamed) | The free in-monorepo contract reshape: decouple the host from intent (opaque status bytes), rename the intent packages to videre, normalize to 0.1.0, add quoting, land as one oracle-validated fold to a green tip. | 2 | 13 |
| M2: Generic venue-agnostic host | new | Make `nexum-runtime` a generic component host: grow the extension seam to worker and provider roles, extract the venue registry, delete the privileged router field, prove it with a blocking zero-leak CI check. | 2 | 12 |
| M3: Videre SDK, macros and DX | #5 (renamed) | The venue and keeper author front door lands before the CoW bundle and the split, because the keepers and the second venue are all authored against it: videre-sdk, the blessed macros, the conformance kit, guest seams, an alloy provider seam, the DX polish cluster. | 2 | 11 |
| M4: CoW on the generic seam (the shepherd bundle) | #3 (renamed) | Prove the generic seam carries a real venue: cleave the cow venue, build the adapter cdylib, port the keepers onto the videre venue client, retire the legacy cow-api cone, carry the live keeper bugfixes. | 3 | 17 |
| M5: The gated three-repo split | #4 (renamed) | The physical split, gated only on the two proofs: transitional workspace, crate-local WIT deps with cross-repo sourcing, the go/no-go checklist, three history-preserving carves, the first consumable videre release, plus operator delivery. | 2 | 17 |
| M6: Second-venue acceptance and vocabulary freeze | #10 (renamed) | Post-cut acceptance: build a genuine non-cow second venue against the published videre-sdk alone; only after it proves the abstraction do the value-flow freeze and the curated adapter registry land. | 1 | 3 |
| M7: Egress guard | #9 (renamed) | The real egress guard with teeth, deferred until after the SDK, the bundle and the split: async policy over live state, single-decode through the checkpoint, the signing boundary, capability and egress enforcement. | 2 | 7 |
| M8: Post-v1 hardening and debt | #6 (renamed) | The rolling, non-gating debt bucket: deferred videre concepts, typed-fault and chain debt, test-harness and perf debt, the deferred messaging backend, docs passes and soak evidence, and the slim grant tracker. | 5 | 17 |

Milestone #1 ("Host backends real") stays dissolved.

## 1b. Epic tree (native sub-issues)

Every milestone owns two to four epics (thin milestones fewer), each a native GitHub sub-issue parent. Children are listed in execution order; the tag in parentheses is the Project Component. Reused existing epics keep their number; new epics are marked NEW. No epic is nested under another epic: the umbrella framing lives in the milestone charter, not a super-epic.

### M0: Runtime architecture and lifecycle

- Epic #294 (reuse) `runtime: complete execution and lifecycle hardening` (Runtime/Lifecycle)
  - #51 extend the manifest capability allowlist to the WASI surface (Runtime/Lifecycle)
  - #53 enforce per-module resource limits and local-store quota (Runtime/Lifecycle)
  - #107 verify fuel accounting during host function calls (Runtime/Lifecycle)
  - #244 handler denial-of-service hardening (Runtime/Lifecycle)
  - #265 make the log pipeline pluggable through a builder seam (Observability)
  - #266 graceful-shutdown drain for durable state (Runtime/Lifecycle)

### M1: Videre contract reshape and host-intent decoupling

- Epic NEW `wit: land the host-intent decouple master gate for an acyclic split` (WIT/ABI)
  - wit: spec the opaque-status destructuring contract the host event commits to (WIT/ABI)
  - wit: carry opaque status bytes so the host stops importing intent (WIT/ABI)
  - engine: land the acyclicity and zero-leak CI check, advisory first (Engine)
  - guard: ship the advisory-only checkpoint posture (Runtime/Lifecycle)
  - guard: charge quota on a denied submit to close the busy-loop denial of service (Runtime/Lifecycle)
  - packaging: fold-tail hygiene, codec discriminator, migration-cruft deletion, retry doc caveat (Tooling/Packaging)
  - packaging: execute the contract reshape as one oracle-validated fold (Tooling/Packaging)
  - packaging: finish the runtime train to a single green linear tip (Tooling/Packaging)
- Epic NEW `wit: reshape the intent contract into the videre venue abstraction` (WIT/ABI)
  - wit: rename the intent packages to videre (WIT/ABI)
  - wit: pin the videre surface (types, venue, value-flow) (WIT/ABI)
  - wit: normalize every package to a single 0.1.0 (WIT/ABI)
  - wit: add quote to the videre venue client and adapter (WIT/ABI)
  - wit: decide the install-time handshake manifest key and match semantics (WIT/ABI)

### M2: Generic venue-agnostic host

- Epic NEW `runtime: make the host generic and venue-agnostic` (Runtime/Lifecycle)
  - runtime: grow the extension seam to worker and provider roles (Runtime/Lifecycle)
  - runtime: extract the venue registry and delete the privileged router field (Runtime/Lifecycle)
  - #321 bound the intent-router status-watch set with eviction and config (Runtime/Lifecycle)
  - runtime: extract a generic supervised-component primitive from the adapter actor (Runtime/Lifecycle)
  - runtime: fold venue adapters into the restart and poison sweeps (Runtime/Lifecycle)
  - engine: de-hardcode the known table and extract world synthesis into nexum-world (Engine)
  - runtime: extract a generic launcher and a bare engine binary (Runtime/Lifecycle)
  - #273 finish the runtime preset trait capability gaps for extensions and pre-built instances (Runtime/Lifecycle)
  - runtime: build the videre-host crate and platform registration (Runtime/Lifecycle)
  - wit: install-time body-versions schema handshake (WIT/ABI)
- Epic NEW `runtime: prove the host is venue-agnostic (zero-leak gate)` (Engine)
  - engine: fail CI when the host regains venue, intent or cow knowledge (Engine)
  - runtime: prove the host is venue-agnostic by flipping the zero-leak check to blocking (Engine)

### M3: Videre SDK, macros and DX

- Epic NEW `sdk: videre-sdk and the blessed venue and keeper authoring path` (SDK/DX)
  - #136 consolidate nexum-sdk and macros; rename nexum-venue-sdk to videre-sdk (SDK/DX)
  - sdk: rename to videre-sdk and add the keeper sweep assembler and venue client (SDK/DX)
  - #322 make the IntentBody derive no_std, emitting core and alloc paths (SDK/DX)
  - #264 convert the bind-macro error and level shims to From impls (SDK/DX)
  - sdk: the single blessed venue authoring macro (SDK/DX)
  - sdk: the videre conformance kit and wire-drift gate (SDK/DX)
  - sdk: the keeper macro and typed venue client (SDK/DX)
- Epic NEW `sdk: guest seams, alloy provider and the dx polish cluster` (SDK/DX)
  - sdk: guest seams and mocks for identity, messaging and remote-store (SDK/DX)
  - #291 add contains, len and count metadata queries to the local store (Storage)
  - sdk: an alloy provider seam over the chain host (SDK/DX)
  - sdk: the alloy-grade DX polish cluster (SDK/DX)

### M4: CoW on the generic seam (the shepherd bundle)

- Epic #138 (reuse) `intent: CoW venue adapter and flagship module ports` (CoW)
  - cow: cleave the venue into orderbook-only and a composable keeper (CoW)
  - cow: settle the idempotency seam before order assembly moves into the adapter (CoW)
  - #324 build the cow adapter cdylib with timeout transport middleware (CoW)
  - wit: own the shepherd-cow event-ABI packages at the bundle layer (WIT/ABI)
  - #323 ratify or reconcile the retry classification table against the api retry hint (CoW)
  - #327 re-point the twap monitor onto the cow adapter submit and status (CoW)
  - #328 re-point the ethflow watcher onto the cow adapter observe and status (CoW)
  - #293 retire the legacy cow-api host shim and cow cone (CoW)
  - cow: swap the composable poll wire and delete the legacy revert adapter, fork-gated (CoW)
- Epic NEW `cow: run the cow keeper on the generic seam and rewrite the docs` (CoW)
  - cow: run the cow keeper on the generic venue client (CoW)
  - docs: rewrite the platform docs as the shipped-venue source of truth (Docs)
- Epic NEW `cow: carry the live twap and composable keeper fixes into the port` (CoW)
  - #121 treat DuplicatedOrder as already-submitted and add errorType retry classification (CoW)
  - #48 stop twap-monitor orphaned gate markers leaking on the decode-failure path (CoW)
  - #75 stop twap-monitor retrying unknown revert selectors every block forever (CoW)
  - #320 one-block retry before dropping on an invalid signature, same-block create race (CoW)
  - #54 support the ConditionalOrderRemoved event (CoW)
  - #64 reconcile the grant contract-modification deliverable divergence (CoW)

### M5: The gated three-repo split

- Epic #274 (reuse) `packaging: carve nexum-runtime, videre and shepherd into three repos` (Tooling/Packaging)
  - packaging: transitional path-dep workspace in the three groupings (Tooling/Packaging)
  - packaging: flip the host WIT to crate-local deps and carve the runtime repo (Tooling/Packaging)
  - packaging: cross-repo WIT consumption with git-tag sourcing (Tooling/Packaging)
  - packaging: the cut go/no-go checklist, host venue-agnostic and cow on the seam (Tooling/Packaging)
  - packaging: carve nexum-runtime, videre and shepherd into three repos (Tooling/Packaging)
  - packaging: cut the first consumable videre release and graduate off the umbrella (Tooling/Packaging)
- Epic NEW `packaging: operator delivery, multi-chain and the swarm remote-store` (Tooling/Packaging)
  - #337 fix the sccache config that hard-fails every fork PR because secrets do not flow to fork runs (Tooling/Packaging)
  - #151 real Swarm remote-store backend (Storage)
  - #125 fix the ghcr image name mismatch that breaks fresh-server docker compose pull (Tooling/Packaging)
  - #124 multi-chain deployment patterns for Mainnet, Arbitrum, Base and Gnosis (Docs)

### M6: Second-venue acceptance and vocabulary freeze

- Epic #140 (reuse) `sdk: prove venue-neutrality with a second venue and freeze the vocabulary` (SDK/DX)
  - wit: hold the value-flow freeze until the second venue proves the abstraction (WIT/ABI)
  - #141 curated adapter registry and consent surface (SDK/DX)
  - #330 freeze-gate decisions for videre value-flow, amount canonicalization and native-token settlement (WIT/ABI)

The second-venue build is epic #140's own charter (the deduped second-venue draft), so it has no separate deliverable child; the freeze (#330) lands last, after acceptance.

### M7: Egress guard

- Epic #139 (reuse) `guard: simulate, analyzers, policy, identity checkpoint` (Runtime/Lifecycle)
  - guard: make the policy check async for simulate and remote analyzers (Runtime/Lifecycle)
  - guard: close the derive-before-check escape and single-decode the body (Runtime/Lifecycle)
  - #52 real keystore-backed signing identity backend (Host Backends)
  - guard: move the checkpoint to the signed-transaction identity boundary (Runtime/Lifecycle)
- Epic NEW `guard: capability and egress enforcement teeth` (Runtime/Lifecycle)
  - guard: bring http egress under the compile-time world guarantee (Runtime/Lifecycle)
  - host: enforce messaging query scope with the Waku backend (Host Backends)
  - host: align mock capability-grant fidelity to the real host grant (Host Backends)

### M8: Post-v1 hardening and debt

- Epic NEW `sdk: deferred videre abstraction concepts` (SDK/DX)
  - #355 add offer and provide-liquidity for maker-side two-sided venues, post 0.1 (WIT/ABI)
  - wit: additive firm-quote field for a taker-side RFQ venue (WIT/ABI)
  - sdk: the venue-neutral source-to-venue materialiser (SDK/DX)
- Epic NEW `chain: robustness and typed-fault debt` (Chain)
  - #269 populate the rate-limited retry hint and map 429 and timeout by type (Chain)
  - #288 flatten the request-batch dead outer chain-error or record the escape hatch (Chain)
  - #286 add From and TryFrom between the wit-bindgen fault and the SDK fault (SDK/DX)
  - #285 richer typed faults for the remote-store, identity and messaging backends (Observability)
  - #289 typed-fault doc consistency pass (Docs)
  - #302 wide-range bulk log backfill for large log gaps (Chain)
- Epic NEW `runtime: test-harness and performance debt` (Runtime/Lifecycle)
  - #283 grow a multi-module harness variant and port the boot end-to-end tests (Runtime/Lifecycle)
  - #284 give the supervisor poison window and restart backoff a clock seam (Runtime/Lifecycle)
  - #280 migrate std sync locks to parking_lot where not held across await (Runtime/Lifecycle)
  - #105 batch and host-side filtered operations on the state seam (Storage)
- Epic NEW `host: messaging backend, docs and soak evidence` (Host Backends)
  - #152 real Waku publish backend (Host Backends)
  - #212 payload encoding convention for nexum-native topics (Host Backends)
  - #341 fix doc 02 boot-order description that has subscriptions before init (Docs)
  - #65 evidence the seven-day unattended soak test (Tooling/Packaging)
- Epic #127 (reuse) `docs: grant delivery plan and evidence tracker` (Docs)
  - Slim grant-delivery tracker: remaining PRs, evidence runs and sequencing. Kept as an epic; its former children #121 and #125 are re-parented out to M4 and M5 and referenced from the body only. No functional children.

## 2. New issues index

Forty-one new leaf issues are created (the eleven remaining drafts collapse onto existing tracker issues; see the dedup table). Every new issue gets exactly one kind label, one Component, its milestone, and Project #1 membership.

| Milestone | Title | Kind | Component |
|-----------|-------|------|-----------|
| M1 | wit: spec the opaque-status destructuring contract the host event commits to | docs | WIT/ABI |
| M1 | wit: carry opaque status bytes so the host stops importing intent | breaking | WIT/ABI |
| M1 | wit: decide the install-time handshake manifest key and match semantics | docs | WIT/ABI |
| M1 | wit: rename the intent packages to videre | breaking | WIT/ABI |
| M1 | wit: pin the videre surface (types, venue, value-flow) | breaking | WIT/ABI |
| M1 | wit: normalize every package to a single 0.1.0 | debt | WIT/ABI |
| M1 | wit: add quote to the videre venue client and adapter | breaking | WIT/ABI |
| M1 | guard: ship the advisory-only checkpoint posture | security | Runtime/Lifecycle |
| M1 | guard: charge quota on a denied submit to close the busy-loop denial of service | security | Runtime/Lifecycle |
| M1 | engine: land the acyclicity and zero-leak CI check, advisory first | debt | Engine |
| M1 | packaging: fold-tail hygiene, codec discriminator, migration-cruft deletion, retry doc caveat | debt | Tooling/Packaging |
| M1 | packaging: execute the contract reshape as one oracle-validated fold | debt | Tooling/Packaging |
| M1 | packaging: finish the runtime train to a single green linear tip | debt | Tooling/Packaging |
| M2 | runtime: grow the extension seam to worker and provider roles | feature | Runtime/Lifecycle |
| M2 | runtime: extract the venue registry and delete the privileged router field | debt | Runtime/Lifecycle |
| M2 | runtime: extract a generic supervised-component primitive from the adapter actor | feature | Runtime/Lifecycle |
| M2 | runtime: fold venue adapters into the restart and poison sweeps | debt | Runtime/Lifecycle |
| M2 | engine: de-hardcode the known table and extract world synthesis into nexum-world | debt | Engine |
| M2 | runtime: extract a generic launcher and a bare engine binary | debt | Runtime/Lifecycle |
| M2 | runtime: build the videre-host crate and platform registration | feature | Runtime/Lifecycle |
| M2 | wit: install-time body-versions schema handshake | feature | WIT/ABI |
| M2 | engine: fail CI when the host regains venue, intent or cow knowledge | debt | Engine |
| M2 | runtime: prove the host is venue-agnostic by flipping the zero-leak check to blocking | debt | Engine |
| M3 | sdk: rename to videre-sdk and add the keeper sweep assembler and venue client | feature | SDK/DX |
| M3 | sdk: the single blessed venue authoring macro | dx | SDK/DX |
| M3 | sdk: the videre conformance kit and wire-drift gate | dx | SDK/DX |
| M3 | sdk: the keeper macro and typed venue client | feature | SDK/DX |
| M3 | sdk: guest seams and mocks for identity, messaging and remote-store | feature | SDK/DX |
| M3 | sdk: an alloy provider seam over the chain host | dx | SDK/DX |
| M3 | sdk: the alloy-grade DX polish cluster | dx | SDK/DX |
| M4 | cow: cleave the venue into orderbook-only and a composable keeper | feature | CoW |
| M4 | cow: settle the idempotency seam before order assembly moves into the adapter | feature | CoW |
| M4 | wit: own the shepherd-cow event-ABI packages at the bundle layer | feature | WIT/ABI |
| M4 | cow: run the cow keeper on the generic venue client | debt | CoW |
| M4 | cow: swap the composable poll wire and delete the legacy revert adapter, fork-gated | debt | CoW |
| M4 | docs: rewrite the platform docs as the shipped-venue source of truth | docs | Docs |
| M5 | packaging: transitional path-dep workspace in the three groupings | debt | Tooling/Packaging |
| M5 | packaging: flip the host WIT to crate-local deps and carve the runtime repo | debt | Tooling/Packaging |
| M5 | packaging: cross-repo WIT consumption with git-tag sourcing | debt | Tooling/Packaging |
| M5 | packaging: the cut go/no-go checklist, host venue-agnostic and cow on the seam | debt | Tooling/Packaging |
| M5 | packaging: carve nexum-runtime, videre and shepherd into three repos | breaking | Tooling/Packaging |
| M5 | packaging: cut the first consumable videre release and graduate off the umbrella | dx | Tooling/Packaging |
| M6 | wit: hold the value-flow freeze until the second venue proves the abstraction | debt | WIT/ABI |
| M7 | guard: make the policy check async for simulate and remote analyzers | debt | Runtime/Lifecycle |
| M7 | guard: close the derive-before-check escape and single-decode the body | security | Runtime/Lifecycle |
| M7 | guard: move the checkpoint to the signed-transaction identity boundary | security | Runtime/Lifecycle |
| M7 | guard: bring http egress under the compile-time world guarantee | security | Runtime/Lifecycle |
| M7 | host: enforce messaging query scope with the Waku backend | bug | Host Backends |
| M7 | host: align mock capability-grant fidelity to the real host grant | debt | Host Backends |
| M8 | wit: additive firm-quote field for a taker-side RFQ venue | feature | WIT/ABI |
| M8 | sdk: the venue-neutral source-to-venue materialiser | dx | SDK/DX |

Ten more epics are created fresh (the fourteen new epics minus the four that reuse an existing number), each carrying the `epic` label, a Component, a milestone and Project #1 membership. The reused epics (#294, #138, #274, #140, #139, #127) are not recreated.

## 3. Close

Seven open issues close as already delivered or obsoleted.

| Issue | Reason |
|-------|--------|
| #339 | Obsolete: it fixed a manifest reference inside a migration file that is already deleted as reshape cruft. |
| #287 | Targets the legacy cow-api extension that #293 deletes; the timeout and typed-fault requirement is carried by the adapter error projection and the venue-error reshape, and the host-chain equivalent lives on as #269. |
| #222 | Delivered by the runtime train: the source, retrier and retry-action primitives landed; forward keeper-sweep rework is carried by the videre-sdk work. |
| #137 | Delivered by the runtime train: the value-flow and intent packages, the venue-adapter world, the router, the venue SDK, the conformance kit and echo-venue all landed; the successor is the videre contract reshape epic. |
| #135 | Delivered by the runtime train: the keeper primitives and single-venue loop landed; deferred generalization is the videre-sdk sweep assembler and the M8 materialiser. |
| #131 | Docs-only, delivered; the design docs now exist. Go-forward doc work is the source-of-truth rewrite. |
| #7 | Stale pre-restructure roadmap epic with no milestone; its goal is largely delivered and its workstreams are decomposed across the milestones and individual issues. |

## 4. Modify

Six open issues are rescoped or retitled rather than recreated.

| Issue | Change |
|-------|--------|
| #139 | Rescope to the real egress guard: fold the router, capability and lifecycle hardening (single-decode, signed-transaction boundary, async policy, http under the world guarantee, messaging query scope, adapter sweeps, mock fidelity) in as children. Stays in M7, depends on #52, advisory-only until then. |
| #330 | Retitle under the videre rename (value-flow package). Keep the two freeze-gate ontology decisions. The freeze is held until the post-cut second venue proves the abstraction: it must not be applied at the cut or earlier; it lands in M6 only after acceptance. |
| #289 | Drop the deleted migration-file bullet; keep the still-valid typed-fault and docs-hygiene items. Stays in M8, distinct from the source-of-truth rewrite. |
| #274 | Rescope from a two-repo split to the three-repo split spanning the whole reshape; the physical carve is its closest correspondent. Re-milestone to M5. |
| #273 | Rescope: the preset launch-surface ask is subsumed by the extension-seam generalization and the bare-binary launcher; the residual mock-runtime preset path rolls into the guest seams. Re-milestone to M2. |
| #136 | Rescope (D3): shepherd-sdk is not kept standalone; it is absorbed into the shepherd bundle at the carve. The residual is the SDK and macro consolidation plus the nexum-venue-sdk to videre-sdk rename. Retitle and re-milestone to M3. Strip the `epic` label and demote to a normal issue. |

## 5. Merge

| From | Into | Note |
|------|------|------|
| #325, #326 | #324 | Golden-vector and distribution-bundling work folds into the single cow-adapter cdylib deliverable. |
| #329 | #293 | Absorbing shepherd-sdk into the bundle is part of retiring the legacy cow cone; #329 folds into #293 and closes as merged. #136 loses it as a child. |

## 6. Dedup

Eleven drafted items collapse onto existing tracker issues instead of being created. Eight leaf and epic bodies dedup by number; the epic dedups become reused epics.

| Drafted item | Existing issue |
|--------------|----------------|
| host-identity-signing-backend | #52 |
| egress-guard-hardening-epic | #139 (reused epic) |
| cow-onvidere-epic | #138 (reused epic) |
| cow-venue-cdylib | #324 |
| cow-api-retire | #293 |
| ethflow-keeper | #328 |
| composable-cow-keeper-port | #327 |
| second-protocol-venue draft | #140 (reused epic; the second-venue build is #140's own charter) |
| split (physical cut) draft | #274 (reused epic) |

The two remaining drafted epics that name the contract reshape and the host generalization become new epics; the umbrella framing lives in the milestone charters, not a super-epic.

## 7. Epic and Component summary

- 20 epics total: 6 reuse an existing tracker issue (#294, #138, #274, #140, #139, #127), 14 are new.
- Every issue has exactly one epic parent (single-parent native sub-issue), exactly one of the twelve Project Components, a milestone equal to its M-label, and Project #1 membership.
- Reused epics keep their live already-parented children (#294 keeps #51/#53/#107/#244/#265/#266; #138 keeps #293/#323/#324/#327/#328). The re-parent map is honoured: #321 and #273 to M2, #322 to M3, #121 to M4, #125 to M5, #330 to M6. #329 merges into #293.
- #136 and #141 currently carry the `epic` label and are demoted to leaves (their former children are re-parented or merged away, so nothing is stranded). #127 stays an epic, slim, with no functional children.
- The Version field is intentionally left unset everywhere.

## 8. Ordered apply sequence

The apply script (run by the owner, not here) proceeds in this order:

1. Rename the eight existing milestones in place by number to their new titles, and create the new milestone M2. Leave milestone #1 dissolved. Do not touch milestone #7's title.
2. Close the seven delivered or obsolete issues (#339, #287, #222, #137, #135, #131, #7).
3. Apply the two merges: retarget or note #325 and #326 into #324; fold #329 into #293 and close it as merged.
4. Apply the six modifies: rescope and retitle #139, #330, #289, #274, #273, #136; re-milestone as noted; strip the `epic` label from #136.
5. Create the 41 new leaf issues with their kind label, one Component, their milestone and Project #1 membership; leave Version unset.
6. Create the ten fresh epics with the `epic` label, a Component, a milestone and Project #1 membership; for the reused epics (#294, #138, #274, #140, #139, #127) set the Component and milestone and confirm the `epic` label.
7. Wire native sub-issue parent and child links in the child order listed in section 1b, one parent per child. Honour the re-parent map; strip the `epic` label from #141 and demote it under #140.
8. Set the Project #1 Component field for every issue and epic; add every issue to Project #1. Keep #127 slim with #121 and #125 referenced from its body only.
9. Verify: 61 open tracker issues accounted for (51 living, 6 as reused epics and 45 as children; 10 closed or merged), no double-parenting, Version unset everywhere.

Coverage is complete: the 10 open issues not placed under an epic are exactly the 7 close targets and the 3 merge sources.
