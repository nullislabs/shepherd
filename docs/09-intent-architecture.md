# Intent Architecture: Venue Adapters and the Egress Guard

> **Status (design, 0.3+ direction):** This document records the target architecture agreed in the July 2026 architecture review. Nothing in it ships in 0.2. It supersedes the CoW-specific framing of the Layer 3 example in [08-platform-generalisation.md](08-platform-generalisation.md) and the compiled-in `cow-api` backend of ADR-0005. Decision records: [ADR-0010](adr/0010-venues-as-adapter-components.md) (venue adapters) and [ADR-0011](adr/0011-egress-guard-pipeline.md) (the guard pipeline).

## Motivation

Three pressures converge on the same design:

1. **CoW is a concrete implementation living inside a generic runtime.** `shepherd:cow/cow-api` and the `cow_orderbook` host backend are the only domain-specific code in the engine. A CoW order is one instance of a more general thing: an intent to give some value in exchange for some other value, submitted to a venue that settles it. The next domains on the roadmap are not further ERC-20 swap venues; they are a non-trading chain domain (a Swarm postage purchase: give BZZ, want storage capacity for a duration) and an off-chain marketplace (real-world assets, where settlement is a legal process rather than a chain state transition).
2. **Venues arrive after the app ships.** On the super-app and mobile targets (doc 08), a new venue must be installable by the user without an app-store deployment. That rules out compiled-in venue backends: the venue integration must itself be a distributable, sandboxed artifact, discovered and installed exactly like a module.
3. **The same engine embeds in a wallet.** The wallet embedding places the runtime at the signing boundary, where EIP-712 typed data and transaction payloads must be decoded, simulated, and analysed for threats before the user signs. The threat analysis itself should be performed by installable wasm modules, so that security vendors ship analysers the way strategy authors ship modules.

The unifying observation: intent submission, typed-data signing, and transaction signing are all **value egress events**. They deserve one vocabulary, one analysis pipeline, and one policy surface.

## Component kinds

The platform grows from one guest component kind to three. All three share the distribution pipeline (ENS discovery, Swarm fetch, content-hash verification, manifest, install-time consent) and the supervision machinery (restart policy, poison handling, fuel and epoch metering, capability enforcement).

| Kind | World | Role | Trust position |
|---|---|---|---|
| Strategy module | `event-module` (existing) | Owns strategy: when to emit intents, polling cadence, retry policy | Arbitrary author; capability-scoped |
| Venue adapter | `venue-adapter` (new) | One venue each: encodes intent bodies, derives headers, submits, observes status, over whatever transport the venue speaks | Curated registry plus ENS escape hatch; structurally cannot move value |
| Threat analyser | `analyzer` (new; descendant of the experimental `query-module` world) | Evaluates egress fact bundles, returns verdicts under a deadline | Tiered: pure fact-fed by default; network extras behind explicit consent |

```mermaid
flowchart LR
    subgraph Guests
        SM["strategy module<br/>(event-module)"]
        VA["venue adapter<br/>(venue-adapter)"]
        AN["threat analyser<br/>(analyzer)"]
    end
    subgraph Host
        POOL["intent pool router"]
        GUARD["egress guard<br/>(facts + policy)"]
        SIM["simulate backend"]
        ID["identity"]
    end
    SM -->|"submit(venue, body)"| POOL
    POOL -->|"derive-header / submit / status"| VA
    POOL --> GUARD
    ID -->|"sign requests"| GUARD
    GUARD -->|"fact bundle"| AN
    GUARD --> SIM
    VA -->|"scoped http / messaging / chain"| Transport[("venue transport:<br/>HTTPS, Waku, PSS,<br/>libp2p, chain")]
```

## The value-flow vocabulary

One WIT types package, egress-neutral, describes value in motion. It is shared by intent headers, simulation balance diffs, and analyser verdict subjects, so that "100 USDC leaves the user's control" is written the same way in all three places. This vocabulary is the real platform contract; it must outlive any individual interface and is the part that freezes hardest.

Sketch (illustrative, not frozen):

```wit
package nexum:value@0.1.0;

interface types {
    variant settlement {
        evm-chain(u64),
        offchain(string),      // jurisdiction / venue-defined domain
    }

    variant asset {
        native(settlement),
        erc20(tuple<u64, list<u8>>),             // (chain, address)
        erc721(tuple<u64, list<u8>, list<u8>>),  // (chain, address, id)
        erc1155(tuple<u64, list<u8>, list<u8>>),
        service(service-desc),                    // e.g. storage capacity for a duration
        external(external-desc),                  // RWA: deed, chattel, ...
    }

    record asset-amount { asset: asset, amount: list<u8> }  // big-endian unsigned
}
```

Design notes:

- `settlement` is a variant from day one. The current `chain-id: u64` plumbing assumes every venue settles on an EVM chain; the off-chain marketplace target breaks that assumption.
- `service` and `external` exist so a postage purchase and a physical-asset listing fit without forcing them into token shapes. For `external` assets the host can verify nothing; policy on them is plugin-attested, not host-verified, and the consent surface must say so.
- Policy has teeth on `gives` (what leaves the user's control). `wants` is display-grade: the host can rarely verify the counterparty's obligation and must not pretend to.

## Intents and venue adapters

### The intent core

```wit
package nexum:intent@0.1.0;

interface types {
    use nexum:value/types.{asset-amount, settlement};

    record intent-header {
        gives: list<asset-amount>,
        wants: list<asset-amount>,
        valid-until: option<u64>,
        settlement: settlement,
        authorisation: auth-scheme,   // eip712 | eip1271 | presign | offchain-sig | none
    }

    variant intent-status { pending, open, settled(option<list<u8>>), failed(fail-reason), expired, cancelled }
    type receipt = list<u8>;          // venue-scoped stable id (CoW: the 56-byte order UID)
}

interface pool {
    submit: func(venue: string, body: list<u8>) -> result<receipt, venue-error>;
    status: func(venue: string, receipt: receipt) -> result<intent-status, venue-error>;
    cancel: func(venue: string, receipt: receipt) -> result<_, venue-error>;
}
```

The body is opaque bytes at the pool boundary. Typing is recovered in two places where it is real: guest-side, where venue authors publish typed SDK crates for strategy modules, and at the adapter's `derive-header` export, whose return type is the stable ontology. There is no closed `intent-body` variant to churn per venue, and no way for a module to claim a header the host has not derived.

A fifth event variant, `intent-status(receipt, status)`, is delivered through the existing manifest-subscription mechanism. For HTTP venues the adapter polls; for Waku or PSS venues it subscribes; strategy modules receive identical events either way. Observation is first-class because one of the two flagship modules (ethflow-watcher) is observe-only: it verifies that intents created by others were indexed by the venue, and never submits.

### The adapter world

```wit
world venue-adapter {
    // narrow, manifest-declared, per-adapter-scoped imports:
    import http;        // e.g. the CoW adapter: api.cow.fi only
    import messaging;   // e.g. a Waku venue: its content topics only
    import chain;       // read-only lookups where needed

    export derive-header: func(body: list<u8>) -> result<intent-header, venue-error>;
    export submit: func(body: list<u8>) -> result<receipt, venue-error>;
    export status: func(receipt: receipt) -> result<intent-status, venue-error>;
    export cancel: func(receipt: receipt) -> result<_, venue-error>;
}
```

The host is a router plus a policy checkpoint: a strategy module calls `pool::submit(venue, body)`; the host resolves the venue id to the installed adapter instance, calls its `derive-header`, runs guard policy on the result (see below), and only then forwards to the adapter's `submit`. Adapters never see keys, never import `identity`, and hold no unscoped transport, so a hostile adapter can misdescribe or grief (drop, delay, leak order details the venue would see anyway) but cannot steal.

Transport is entirely the adapter's concern. A venue reachable over HTTPS, Swarm PSS, Waku, raw libp2p, or an on-chain contract call presents the same four exports; the module and the host router are transport-blind. This is the same shape as `chain::request` (the module says what, the host decides how) extended to one more decision layer.

### Curation and consent

Adapters install like modules, with two provenance tiers:

- **Curated registry (default):** a platform-signed list of adapter content hashes. Installing from it shows the standard consent sheet (publisher, venue, transport scopes).
- **ENS escape hatch:** any ENS-published adapter installs behind a stronger warning. Header trust then equals publisher trust, and the consent copy says exactly that.

Adapters are always separate artifacts from strategy modules: one adapter per venue, shared by all modules, separately consented. A module author who also authors a venue needs two visible installs, which keeps collusion observable.

### What stays where

The strategy versus protocol boundary from ADR-0006 is preserved, not repealed. Strategy stays guest-side in modules: polling cadence, condition evaluation, revert-taxonomy interpretation, when to give up. What moves out of the engine and into adapters is encoding, transport, and observation. The SDK gains a venue-generic chassis for the machinery both flagship modules already implement by hand: watch-set persistence, gate keys, idempotency journals keyed on receipts, retry classification. Porting the pattern to a new venue means implementing the adapter plus a thin typed SDK crate; the tested machinery travels.

## The egress guard

### One pipeline for all value egress

Three event classes produce the same fact-bundle shape and flow through the same spine:

1. **Intent submission** (from the pool router, header derived by the venue adapter),
2. **Typed-data signing** (EIP-712 requests arriving at `identity::sign-typed-data`),
3. **Transaction signing** (raw transactions arriving at `identity` via `chain::request` signing methods).

```mermaid
flowchart LR
    E["egress event<br/>(intent / typed-data / tx)"] --> F["fact assembly:<br/>decode + simulate"]
    F --> A["analysers<br/>(deadline-bounded)"]
    A --> P["policy engine<br/>(binding, user override)"]
    P --> C["consent surface /<br/>auto-allow / block"]
```

The wallet embedding is a host profile where transaction and typed-data events dominate and the consent surface is the wallet UI (driven over the embedding API). The server runtime is a profile where intent submissions dominate and policy is operator configuration. Same pipeline, same analysers, same vocabulary.

### Fact assembly and the `simulate` primitive

The host assembles a typed fact bundle per event: the decoded payload (EIP-712 struct and domain, transaction fields, or intent header plus venue id), simulation results (balance diffs and approvals granted, expressed in `nexum:value` types), and context metadata (counterparty contract, chain, requesting component).

Simulation is a pluggable host primitive, additive alongside `clock` and `http`:

- **Server and desktop:** a local EVM (revm) over the provider pool's state access.
- **Mobile:** cold-state simulation over mobile RPC can take seconds, so the host may use an operator- or user-configured remote simulation backend. That trades transaction privacy for latency and the trade is made explicitly in configuration and surfaced in consent, never silently.

One WIT contract either way; analysers and policy are backend-blind.

### Analysers

Analysers are request/response components (the `query-module` lineage): the host calls them with a fact bundle and a deadline, they return a verdict. Capabilities are tiered:

- **Pure core (default):** no imports at all. The analyser computes on the facts it is handed. Deterministic, fast, and nothing to exfiltrate: the natural home for heuristics, decoder cross-checks, and known-bad-pattern matching.
- **Granted extras:** an analyser may request `chain` (its own reads) or scoped `http` (a vendor reputation feed). The consent sheet states the consequence plainly: this analyser sends what you sign to vendor.example. Everything the user signs is exactly the data a network-capable analyser could leak, so the tier boundary is the privacy boundary.

Verdicts carry a severity and a typed subject (which `gives` entries they concern). They are policy-binding with per-event user override: high-severity findings block by default, the user can override with friction. Analyser timeout or crash during an interactive prompt resolves per policy profile: a wallet profile fails closed for high-value egress, a server profile may fail open with logging. The choice is explicit configuration, not an accident of scheduling.

## Trust model summary

| Guarantee | Enforced by | Trust required |
|---|---|---|
| Adapter cannot move value | Sandbox: no `identity` import, no keys, scoped transport | None (structural) |
| Spend limits, consent summaries | Guard policy on host-routed, adapter-derived headers | Adapter publisher (curated registry or explicit ENS consent) |
| Theft prevention on signed egress | Guard at the `identity` boundary: EIP-712 and tx payloads are self-describing, the host decodes and simulates them itself | Host only |
| Contract-authorised flows (e.g. EIP-1271 conditional orders) | Consented on-chain when the commitment was created; guests can only materialise what the contract permits | On-chain approval hygiene |
| Threat verdicts | Analyser components under deadline, tiered capabilities | Analyser publisher, proportional to granted tier |

Honest limitations, carried deliberately: policy on `external` (RWA) assets is adapter-attested rather than host-verified; adapter misbehaviour of the griefing grade (delay, drop, leak) is handled by curation and reputation rather than mechanism; and `wants` is display-grade.

## Sequencing

Each step is independently shippable and the earlier steps are pure wins even if later ones change shape.

1. **Hygiene:** move the `cow_orderbook` backend out of the engine behind the RuntimeTypes extension seam; remove `CowApiHost` from the SDK supertrait. The engine becomes domain-free; shepherd is the distribution that bundles the CoW integration.
2. **SDK chassis:** extract the conditional-commitment machinery (watch sets, gate keys, idempotency journals, retry ledgers) from twap-monitor and ethflow-watcher into venue-generic SDK traits. This delivers watch-tower-parity transportability with no WIT change.
3. **Intent core:** `nexum:value` and `nexum:intent` at 0.x, the `venue-adapter` world, the host router with supervisor reuse, and the CoW adapter built as a component from day one (bundled with the shepherd distribution; bundled is not compiled-in). Port both flagship modules; ethflow proves observe-only and the status-event path.
4. **Guard, first cut:** the `simulate` primitive (local backend), fact assembly, the `analyzer` world, policy binding with override, and the identity-boundary checkpoint for typed-data and transaction signing.
5. **Postage adapter (N=2):** proves `service` wants, non-HTTP transport thinking, and settlement variance. Freeze the vocabulary at 1.0 only after this round-trips submit, status, and policy.
6. **Registry and consent:** the curated adapter/analyser registry, publisher display, the ENS escape hatch, and the wallet-profile consent surface over the embedding API.

## Open questions

- **Vocabulary freeze discipline:** `nexum:value` becomes forward-compatibility-critical for three consumers at once. The N=2 gate (step 5) is the guard, but the versioning policy for post-1.0 additions (new asset variants) needs its own note.
- **Analyser composition:** multiple analysers with overlapping findings need aggregation rules (max severity wins is the obvious start) and a story for contradictory verdicts.
- **A `tx` venue:** transactions are covered by the guard at the identity boundary, not modelled as an intent venue. Whether a transaction-shaped venue adapter (batching, private orderflow) is ever worth registering stays open; the policy hooks are shaped so it could be.
- **Adapter reputation:** beyond curation, whether observed adapter behaviour (submission latency, status accuracy) feeds a local score.
