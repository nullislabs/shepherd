---
status: proposed
supersedes: ADR-0005 (compiled-in cow-api backend)
---

# Venues are dynamically installed adapter components; CoW becomes the first adapter

## Context

The engine's only domain-specific code is the CoW integration: the `shepherd:cow/cow-api` WIT interface and the `cow_orderbook` host backend (ADR-0005). A CoW order is one instance of a general shape: an intent to give value for value, submitted to a venue. The next domains on the roadmap are a Swarm postage purchase and an off-chain marketplace, not further ERC-20 swap venues, so a "trading pair" abstraction is the wrong altitude; the abstraction axis is the venue itself.

Two constraints shaped the decision:

1. **Venues arrive after the app ships.** On mobile and super-app targets, a new venue must be user-installable without an app-store deployment. A compiled-in backend (or a Rust plugin trait resolved at build time) cannot satisfy this.
2. **Policy is core.** The host must understand what modules submit (consent, spend limits, audit). Policy inputs must not be forgeable by the components being policed.

Two facts about the existing modules bound the design from below. Strategy is venue-coupled through on-chain contracts (a TWAP part is a ComposableCoW artifact; no abstraction makes it submittable elsewhere), so venue-portable strategy modules are not a goal. And ethflow-watcher is observe-only, so observation is a first-class venue operation, not an afterthought.

## Decision

A venue integration is a **venue adapter**: a sandboxed wasm component, distributed and consented through the same pipeline as modules (ENS discovery, Swarm fetch, hash verification, manifest, supervision), implementing a new `venue-adapter` world:

- Imports: narrow, manifest-declared, per-adapter-scoped transport (`http` to the venue's endpoints, `messaging` on its topics, read-only `chain`). Never `identity`, never keys.
- Exports: `derive-header(body) -> intent-header`, `submit(body) -> receipt`, `status(receipt)`, `cancel(receipt)`.

Strategy modules import a minimal `nexum:intent/pool` interface (`submit`/`status`/`cancel` keyed by venue id, body as opaque bytes). Bodies carry their own chain routing (a multichain adapter resolves per-chain endpoints, as the current backend does), and the derived header's `settlement` field exposes the choice to policy. The host routes pool calls to the installed adapter, calls `derive-header`, runs guard policy (ADR-0011) on the derived header, and only then forwards to the adapter's `submit`. Headers are always host-obtained from the adapter, never module-supplied, so a module cannot understate what it gives away.

A venue is normatively defined by language-neutral artefacts, not by a crate: the borsh body schema per version, golden vectors (body bytes and the expected derived header), and the submission error-classification table as data. The venue author's Rust crate (body types and codec, typed client with retry classification, adapter implementation, behind feature slices) is the first-class implementation of that specification, not its definition, so non-Rust module authors target the venue from generated WIT bindings plus the published schema. Modules never link against adapters directly: every hop is module to host to adapter, which is what keeps policy interposed, fuel attributed per store, and an adapter trap from poisoning the calling module.

Intent status flows back through the existing event mechanism as a new event variant; adapters poll or subscribe per their transport, and modules are transport-blind.

The typed ontology lives in a shared, egress-neutral `nexum:value-flow` types package (assets, amounts, settlement domains) plus `nexum:intent` (header, status, receipt). Venue body schemas are per-venue, typed guest-side in venue SDK crates and at `derive-header`, so adding a venue is an adapter release plus an SDK crate, not a host release and not a WIT bump of a closed body variant.

Provenance is two-tier: a platform-signed curated registry by default, plus an install-by-ENS escape hatch behind a stronger warning. Adapters are always separate artifacts from strategy modules; a venue and a strategy by the same author are two visible installs.

The CoW integration becomes the first adapter, built as a component from day one and bundled with the shepherd distribution. Bundled is not compiled-in: the same artifact installs dynamically on other hosts. `shepherd:cow/cow-api` and the `cow_orderbook` backend are retired once the port completes. The Swarm postage-purchase adapter is the N=2 proving ground; the `nexum:value-flow` vocabulary does not freeze before it round-trips submit, status, and policy.

## Considered options

- **Compiled-in venue plugin registry (Rust trait, host release per venue).** Rejected: fails the app-store constraint outright; every venue addition redeploys every host.
- **Typed `intent-body` closed variant in WIT.** Rejected: WIT variants are closed, so the body type churns per venue and version-couples all venues to one package; with dynamic adapters the body must be opaque at the pool boundary anyway. Typing is recovered where it is real (guest SDK crates, `derive-header`).
- **Full typed intent ontology at the WIT boundary including venue payloads.** Rejected: WIT has no parametric polymorphism, so maximal generality degenerates into `(schema, bytes)` with extra steps, and a module-supplied typed envelope over an opaque payload is unverifiable (the metadata-lies problem). Deriving the header host-side from the body is the sound inversion.
- **Dissolve `cow-api` into allowlisted `http`.** Rejected: venues are not necessarily HTTP (Swarm PSS, Waku, libp2p, on-chain), and raw transport capability erases the consent surface and the policy checkpoint permanently.
- **Module-supplied headers with host spot-checking.** Rejected: spot-checking requires a venue codec host-side, which is the adapter again, minus the guarantees.
- **Bundled module+venue single artifacts.** Rejected: consent would conflate strategy and venue, venue logic duplicates per module, and collusion becomes one opaque install.

## Consequences

- The engine loses its last domain-specific code; shepherd is a distribution (engine + bundled CoW adapter), which is what the hygiene refactors around the RuntimeTypes lattice were already moving toward.
- A third-party adapter can misdescribe (`derive-header` lies) or grief (delay, drop, leak order details the venue would see anyway) but cannot move value: no keys, no unscoped transport. Theft prevention does not rest on adapter honesty; it rests on the guard at the identity boundary (ADR-0011) and on-chain approval hygiene. Spend-limit accuracy rests on adapter publisher trust, which is what the curated registry and consent copy manage.
- Strategy stays guest-side per ADR-0006; what leaves modules is encoding, transport, and observation. The SDK gains a venue-generic chassis (watch sets, gate keys, receipt-keyed idempotency, retry classification) extracted from twap-monitor and ethflow-watcher.
- Two sandbox hops (module, host, adapter) add marshalling per submission. Negligible at order-submission rates.
- Adapters need the module supervision surface (restart, poison, metering) and manifest capability enforcement; both exist and are reused.
- The venue catalogue is user-gated rather than host-gated: permissionless with consent, which is the platform's general posture.
