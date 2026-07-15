---
status: accepted
supersedes: ADR-0005 (compiled-in cow-api backend)
reconciled: 2026-07-15 (against the shipped M1 tree + the 7 platform decisions of 2026-07-14)
---

# Venues are dynamically installed adapter components; CoW becomes the first adapter

> **M1 status — shipped and vindicated.** The core of this ADR is real in the M1 train:
> the `venue-adapter` component kind, `nexum:intent@0.1.0` + `nexum:value-flow@0.1.0`,
> the host `PoolRouter`, `nexum-venue-sdk` + the `#[nexum::venue]` macro, the
> `nexum-venue-test` conformance kit, and the `echo-venue` reference adapter that proves the
> seam end to end. What is **not** yet built: a concrete CoW **adapter component** (the
> `cow-venue` crate carries the body/codec, composable-order, `classification`, and typed
> `client` slices but no `cdylib`/adapter slice — grep for `export_venue_adapter!`/`#[venue]`
> in it is empty), and the identity-boundary guard this ADR leans on for theft prevention.
> That guard is ADR-0012, deferred wholesale to the egress-guard epic; M1 ships an advisory
> `AllowAllGuard` no-op (decision R3). The live CoW submit path still runs through the legacy
> `shepherd:cow/cow-api` event-module extension, not through an adapter. See
> `docs/design/venue-platform-architecture.md` for the full reconciliation and the seven
> decisions folded in below.

## Context

The engine's only domain-specific code is the CoW integration: the `shepherd:cow/cow-api` WIT interface and the `cow_orderbook` host backend (ADR-0005). A CoW order is one instance of a general shape: an intent to give value for value, submitted to a venue. The next domains on the roadmap are a Swarm postage purchase and an off-chain marketplace, not further ERC-20 swap venues, so a "trading pair" abstraction is the wrong altitude; the abstraction axis is the venue itself.

Two constraints shaped the decision:

1. **Venues arrive after the app ships.** On mobile and super-app targets, a new venue must be user-installable without an app-store deployment. A compiled-in backend (or a Rust plugin trait resolved at build time) cannot satisfy this.
2. **Policy is core.** The host must understand what modules submit (consent, spend limits, audit). Policy inputs must not be forgeable by the components being policed.

Two facts about the existing modules bound the design from below. Strategy is venue-coupled through on-chain contracts (a TWAP part is a ComposableCoW artifact; no abstraction makes it submittable elsewhere), so venue-portable strategy modules are not a goal. And ethflow-watcher is observe-only, so observation is a first-class venue operation, not an afterthought.

## Decision

A venue integration is a **venue adapter**: a sandboxed wasm component, distributed and consented through the same pipeline as modules (ENS discovery, Swarm fetch, hash verification, manifest, supervision), targeting exactly one world — the shipped `nexum:adapter/venue-adapter`:

- Imports (shipped): the scoped Nexum host transport it needs — `nexum:host/chain` and `nexum:host/messaging` — plus `wasi:http`, which is **linked separately** and gated per-adapter by the `[[adapters]].http_allow` allowlist in `engine.toml` (the same way `event-module` treats outbound HTTP). Time and randomness are ambient `wasi:clocks`/`wasi:random`. Never `identity`, never keys, no `local-store`/`remote-store`/`logging`. The adapter linker withholds everything else, so an adapter that reaches for it fails to instantiate. `ADAPTER_CAPABILITIES = ["chain", "messaging"]` in `manifest/capabilities.rs` is the source of truth.
- Exports (shipped, `nexum:intent/adapter@0.1.0` plus the world's own `init`): `init(config)`, `derive-header(body) -> intent-header`, `submit(body) -> submit-outcome`, `status(receipt)`, `cancel(receipt)`. `submit` returns a `submit-outcome` **variant** — `accepted(receipt)` for a venue that holds the intent, or `requires-signing(unsigned-tx)` for an on-chain-settlement (ethflow-style) venue that has no receipt until the host signs and sends a transaction. That variant is present from day one precisely so bolting on the signing path later would not break every deployed module.

Per platform decision **Q1 (2026-07-14): no venue-specific host interfaces.** An adapter is transport-only over the *generic* Nexum host set + `wasi:http`; the host interface set is kept ample enough that venues need nothing venue-specific. There is no extension-namespace machinery in `synthesize_venue` or the adapter linker. `shepherd:cow/cow-api` survives **only** as the legacy `event-module` read path (the live CoW submit still runs through it), **not** as an adapter capability — an adapter cannot import it.

Strategy modules import a minimal `nexum:intent/pool` interface (`submit`/`status`/`cancel` keyed by venue id, body as opaque bytes; `pool` is a declared module capability, `INTENT_CAPABILITIES = ["pool"]`). Bodies carry their own chain routing (a multichain adapter resolves per-chain endpoints, as the current backend does), and the derived header's `settlement` field exposes the choice to policy. The host `PoolRouter` resolves the venue id to the installed adapter, gates the caller's quota, calls `derive-header`, runs the guard seam on the derived header, and only then forwards to the adapter's `submit`. Headers are always host-obtained from the adapter, never module-supplied, so a module cannot understate what it gives away. **M1 caveat:** the guard is `AllowAllGuard`, an advisory no-op (ADR-0012 / decision R3); the router's `derive → guard → submit` shape is real, but the checkpoint has no teeth yet, and it inspects the adapter's *own* derived header rather than the settled bytes.

A venue is normatively defined by language-neutral artefacts, not by a crate: the borsh body schema per version, golden vectors (body bytes and the expected derived header), and the submission error-classification table as data. The venue author's Rust crate (body types and codec, typed client with retry classification, adapter implementation, behind feature slices) is the first-class implementation of that specification, not its definition, so non-Rust module authors target the venue from generated WIT bindings plus the published schema. Modules never link against adapters directly: every hop is module to host to adapter, which is what keeps policy interposed, fuel attributed per store, and an adapter trap from poisoning the calling module.

Intent status flows back through the existing event mechanism; adapters poll or subscribe per their transport, and modules are transport-blind. Per decision **Q2/R6 (2026-07-14)**, the host `event` stream carries **opaque status bytes** with a documented, versioned destructuring contract — *not* a typed `intent-status` case borrowed from `nexum:intent`. That decoupling drops the `nexum-host/types.wit` `use nexum:intent/types.{intent-status}` coupling, so a new lifecycle case in the intent ontology is no longer a breaking change to the host that recompiles every event-module.

The typed ontology lives in a shared, egress-neutral `nexum:value-flow@0.1.0` types package (assets, amounts, settlement domains) plus `nexum:intent@0.1.0` (header, status, receipt, `submit-outcome`, `venue-error`). `nexum:intent` deliberately does **not** depend on `nexum:host`, so the adapter contract's freeze cadence stays independent of host versioning. Venue body schemas are per-venue, typed guest-side in venue SDK crates and at `derive-header`, so adding a venue is an adapter release plus an SDK crate, not a host release and not a WIT bump of a closed body variant. Per decision **Q5**, the shipped 0.1 ontology is **EVM-only** as a scoping choice (`auth-scheme` and `unsigned-tx` are EVM-shaped); non-EVM settlement versions into a later revision. Nothing is pinned (see the versioning note below), so that reshape — and quoting, which 0.1 does not have — can land whenever a design partner arrives, at the cost of an internal recompile, not a wire break.

Provenance is two-tier: a platform-signed curated registry by default, plus an install-by-ENS escape hatch behind a stronger warning. Adapters are always separate artifacts from strategy modules; a venue and a strategy by the same author are two visible installs.

The CoW integration is designed to become the first adapter, built as a component and bundled with the shepherd distribution (bundled is not compiled-in: the same artifact installs dynamically on other hosts). **This has not shipped in M1.** `cow-venue` today is a `[lib]` crate of feature slices — `body` (the venue-neutral order/composable body types + borsh `IntentBody` codec), `composable`, `order`, data-table retry `classification`, and a typed `client` — carried by both a future adapter and by strategy modules; it has no `cdylib`/adapter slice. So `shepherd:cow/cow-api` and the `cow_orderbook` backend are **not** retired: they remain the live CoW submit path (`shepherd-sdk/src/cow` assembles `OrderCreation` JSON and bypasses `nexum:intent/pool` entirely). The clean-break port off the legacy surface is deferred. The Swarm postage-purchase adapter is the intended N=2 proving ground; because nothing is pinned, the `nexum:value-flow` vocabulary carries no freeze it must round-trip a second venue before breaking.

## Considered options

- **Compiled-in venue plugin registry (Rust trait, host release per venue).** Rejected: fails the app-store constraint outright; every venue addition redeploys every host.
- **Typed `intent-body` closed variant in WIT.** Rejected: WIT variants are closed, so the body type churns per venue and version-couples all venues to one package; with dynamic adapters the body must be opaque at the pool boundary anyway. Typing is recovered where it is real (guest SDK crates, `derive-header`).
- **Full typed intent ontology at the WIT boundary including venue payloads.** Rejected: WIT has no parametric polymorphism, so maximal generality degenerates into `(schema, bytes)` with extra steps, and a module-supplied typed envelope over an opaque payload is unverifiable (the metadata-lies problem). Deriving the header host-side from the body is the sound inversion.
- **Dissolve `cow-api` into allowlisted `http`.** Rejected: venues are not necessarily HTTP (Swarm PSS, Waku, libp2p, on-chain), and raw transport capability erases the consent surface and the policy checkpoint permanently.
- **Module-supplied headers with host spot-checking.** Rejected: spot-checking requires a venue codec host-side, which is the adapter again, minus the guarantees.
- **Bundled module+venue single artifacts.** Rejected: consent would conflate strategy and venue, venue logic duplicates per module, and collusion becomes one opaque install.

## Consequences

- **Shipped.** The `venue-adapter` kind, the two-face (`pool`/`adapter`) intent contract with opaque `list<u8>` bodies, the host `PoolRouter`, the venue-author SDK persona (`nexum-venue-sdk` + `#[nexum::venue]` + `nexum-venue-test`), and the `echo-venue` reference adapter are all in the M1 train. The abstraction axis (venue, not trading pair) held: `echo-venue` proves a non-CoW venue is implementable without any CoW code.
- The engine has **not** yet lost its last domain-specific code: `shepherd:cow/cow-api` + the `cow_orderbook` backend remain as the legacy event-module read/submit path (decision Q1 keeps them only in that role, not as an adapter capability). The clean-break to a bundled CoW adapter component is the deferred N=1 port.
- A third-party adapter can misdescribe (`derive-header` lies) or grief (delay, drop, leak order details the venue would see anyway) but cannot move value: no keys, no unscoped transport. Theft prevention does not rest on adapter honesty; it is meant to rest on the guard at the identity boundary (**ADR-0012**) and on-chain approval hygiene. **That guard is deferred to the egress-guard epic**; in M1 the only checkpoint is the advisory `AllowAllGuard`, so the theft-prevention anchor this ADR names is design intent, not a live control. Spend-limit accuracy rests on adapter publisher trust, which is what the curated registry and consent copy manage.
- **Identity signing lands with the guard, later** (decision 7): the `requires-signing(unsigned-tx)` submit outcome is representable in the shipped WIT, but the host `identity` path that would sign it is a 0.3 stub (`accounts() -> Ok(vec![])`), so no adapter can drive a signed settlement in M1.
- **Module↔venue body-schema agreement is an install-time handshake** (decision R7): bodies are opaque `list<u8>` with a guest-side version tag, so schema agreement is checked by a `body_version` (or version-set) manifest field that `Supervisor::install` asserts against the adapter's supported set, refusing to boot a mismatched pair. This is a manifest + supervisor change, not WIT-freeze-gated.
- Strategy stays guest-side per ADR-0006; what leaves modules is encoding, transport, and observation. The SDK gains a venue-generic chassis — the `keeper` (watch sets, gate keys, receipt-keyed idempotency, retry classification) — extracted from twap-monitor and ethflow-watcher; its parts ship, though the assembled `Keeper::sweep` orchestrator is still a DX follow-on.
- **`#[nexum::venue]` is the single blessed authoring path** (decision Q6). Today the macro emits the adapter's `Guest` export glue over an inherent `impl` block (as `echo-venue` uses it); the decided target is to have it emit an `impl VenueAdapter` and demote `export_venue_adapter!` to the internal codegen the macro expands to — one path, no public second door. That unification is a Phase-4 DX follow-on.
- Two sandbox hops (module, host, adapter) add marshalling per submission. Negligible at order-submission rates.
- Adapters need the module supervision surface (restart, poison, metering) and manifest capability enforcement; boot/install is reused, though adapters are **not** yet folded into the restart/poison sweeps (a trapped adapter stays dead until process restart — a known post-M1 hardening item).
- The venue catalogue is user-gated rather than host-gated: permissionless with consent, which is the platform's general posture.
- **Versioning is pre-release.** Every WIT package version string (`nexum:host@0.2.0`, `nexum:intent@0.1.0`, …) is accumulated cruft, not a compatibility boundary — nothing external pins them, and they normalize to a single `@0.1.0` at the true initial release. Until then a "breaking" WIT change costs only an internal recompile + a train fold, never a wire break.
