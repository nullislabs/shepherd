---
status: deferred (target design; deferred wholesale to the egress-guard epic)
reconciled: 2026-07-15 (against the shipped M1 tree + the 7 platform decisions of 2026-07-14)
---

# One egress guard pipeline: intent submissions, typed-data signing, and transaction signing share facts, analysers, and policy

> **M1 status — advisory only; teeth deferred.** None of the pipeline below ships in M1.
> This ADR is the **target** design. What actually ships is an `AllowAllGuard` — a no-op
> `GuardPolicy` on the host `PoolRouter` that admits every egress (`pool_router.rs`). It runs
> on the adapter's *own* `derive-header` output (which `submit` re-decodes independently, so
> it is advisory even in principle — a TOCTOU gap), and it does **not** cover the signing path
> at all: there is no `simulate` primitive, no fact assembly, no `analyzer` world, no policy
> engine, and the `identity` boundary it names as the theft anchor is a 0.3 stub. Per platform
> decision **R3 (2026-07-14)** the M1 guard posture is advisory-only: keep `AllowAllGuard`,
> feature-gate the `pool` import, and document the checkpoint as **not yet a boundary**. The
> real guard — its trust model, its single-decode fix, and *where* it runs — is deferred
> wholesale to the egress-guard epic; per decision 7, **identity signing lands with the guard,
> later**, not before. Read everything below as the design that epic will build, not as
> present behaviour. See `docs/design/venue-platform-architecture.md` §6 (R3) for the
> red-team detail.

## Context

Two requirements arrived together. First, the intent layer (ADR-0010) needs a policy checkpoint: consent, spend limits, and audit on what modules submit to venues. Second, the same engine embeds in a wallet, where EIP-712 typed data and transactions must be decoded, simulated, and analysed for threats before the user signs, and where the threat analysis should be performed by installable wasm components so security vendors ship analysers the way strategy authors ship modules.

These are the same problem. An intent submission, a typed-data signature, and a transaction signature are all value egress events: something leaves the user's control. Designing two pipelines (venue policy in the runtime, signing analysis in the wallet) would duplicate the vocabulary, drift, and leave each surface weaker than the union.

A further fact anchors the trust model: EIP-712 typed data and transaction payloads are self-describing. The host can decode and simulate them without trusting any guest component's metadata. The identity boundary is therefore the one checkpoint no guest code can route around.

## Decision

*(Target. The M1 tree ships the `derive → guard → submit` router seam and an `AllowAllGuard`
no-op in that guard slot; everything the Decision describes below is the shape the egress-guard
epic fills in. Where the shipped seam already exists, it is called out.)*

One guard pipeline handles all value egress:

```
egress event -> fact assembly (decode + simulate) -> analysers (deadline-bounded) -> policy (binding, user override) -> consent / allow / block
```

**Egress events.** Intent submissions (header derived by the venue adapter, routed by the pool), EIP-712 requests at `identity::sign-typed-data`, and transaction signing at the identity boundary. The wallet embedding is a host profile where signing events dominate and consent renders in the wallet UI over the embedding API; the server runtime is a profile where intent submissions dominate and policy is operator configuration.

**Fact assembly.** The host builds a typed fact bundle per event: decoded payload, simulation results (balance diffs, approvals granted), and context metadata. All value flows are expressed in the shared `nexum:value-flow` vocabulary, the same types intent headers use, so one asset-delta dialect serves headers, simulations, and verdicts.

**`simulate` is a pluggable host primitive**, additive alongside `clock` and `http`. Server and desktop hosts run a local EVM (revm) over provider-pool state. Mobile hosts may configure a remote simulation backend because cold-state simulation over mobile RPC is interactively too slow; that trades transaction privacy for latency, and the trade is explicit operator/user configuration surfaced in consent, never a silent default. Analysers and policy are backend-blind.

**Analysers** are request/response components (the `query-module` lineage): called with a fact bundle and a deadline, they return verdicts with severity and typed subjects (which `gives` entries they concern). Capabilities are tiered:

- Pure core (default): no imports; compute on the facts handed over. Deterministic, fast, nothing to exfiltrate.
- Granted extras: `chain` reads or scoped `http` (vendor reputation feeds) behind explicit consent that states the consequence plainly: this analyser sends what you sign to the vendor. The tier boundary is the privacy boundary, because everything the user signs is exactly what a network-capable analyser could leak.

**Egress events are classified by authorisation source.** Host-signed events (EIP-712 via `identity`, transaction signing) get the full pipeline and are blocking-capable; they are the only class where host-held keys act. Pre-authorised events (EIP-1271 contract signatures, contract-owner schemes) default to non-interactive audit plus advisory analysis: the consent already happened on-chain at commitment creation (itself a guarded transaction), venue submission is permissionless so local blocking prevents nothing (any third party can submit the same materialised part), and per-part prompts would be interruption without protection. These flows never reach the identity checkpoint at all; the signature comes back from the chain. For this class, spend limits are observability rather than enforcement, and advisory findings are detection rather than prevention, though still actionable: the user can invalidate the commitment on-chain before the next part.

**Policy is binding with per-event user override.** High-severity verdicts block by default; the user can override with friction. Analyser timeout or crash during an interactive prompt resolves per policy profile: wallet profiles fail closed for high-value egress, server profiles may fail open with logging. Fail-open versus fail-closed is explicit configuration.

**Transactions are covered by the guard, not modelled as an intent venue.** The earlier deferral of "does the intent capability swallow `eth_sendTransaction`" resolves here: unification happens at the guard layer, where all egress meets the same facts and policy, while the intent pool stays scoped to venue submissions. The policy hooks are shaped so a transaction-shaped venue adapter could register later if batching or private orderflow ever wants one.

**Capability least-privilege is the enforcement mechanism, not hygiene.** Guard soundness requires that guarded components have no unguarded egress path: a strategy module that submits intents receives the `intent` capability and does not also hold unscoped `messaging` or `http`. Consent UX must present capability combinations accordingly (a module holding `chain` plus `identity` can egress value outside intent policy only through the identity checkpoint, which is guarded).

## Considered options

- **Two pipelines (runtime venue policy, wallet signing guard) with a shared vocabulary.** Rejected: the fact shapes, analyser world, and policy engine would be near-duplicates that drift; the wallet profile would re-implement intent policy the day a wallet hosts strategy modules.
- **Advisory-only verdicts.** Rejected as the default: it puts the entire burden on consent-sheet reading. Retained as the effective behaviour for low-severity findings.
- **A uniform blocking posture for all egress regardless of authorisation source.** Rejected: for pre-authorised (EIP-1271) intents, blocking is security theatre because submission is permissionless, and prompting per TWAP part is unusable. The enforcement point for those flows is commitment creation, which the guard already covers as a transaction.
- **Hard veto for any installed analyser.** Rejected: one broken or compromised analyser bricks signing, and it forces fail-closed on every timeout regardless of stakes.
- **Analysers bring their own simulation** via `chain`/`http` grants. Rejected as the model: every analyser duplicates the work, the fact bundle thins to uselessness for pure analysers, and the privacy tier collapses (all analysers would need network). Network-capable analysers may still enrich beyond the host bundle.
- **Host-only heuristics, no analyser components.** Rejected: it forecloses the security-vendor ecosystem and hard-codes threat logic into host releases, the same mistake the venue layer just escaped.

## Consequences

### What M1 actually ships (advisory only)

- **A no-op guard on a real seam.** The host `PoolRouter` implements the `derive → guard → submit` sequence with a `GuardPolicy` trait and an `AllowAllGuard` default that admits every egress. The seam and its quota gate are real and tested (a `DenyGuard` test proves a deny blocks submit after the header is derived), but nothing enforcing is wired in.
- **Advisory, and on the wrong bytes even in principle.** The guard inspects the adapter's *own* `derive-header` output; `submit` re-decodes the body independently, so a buggy or hostile adapter can show a benign `gives` and settle something else (TOCTOU). The fix — pass the derived header *into* submit for a single decode, and move the checkpoint to the signed-`unsigned-tx` boundary — is part of the deferred epic.
- **The signing path is uncovered.** There is no `simulate` primitive, no fact assembly, no `analyzer` world, no policy engine. The `identity` interface exists but its backend is a 0.3 stub (`accounts() -> Ok(vec![])`), so the theft boundary this ADR anchors on is not yet a live control. Per decision 7, identity signing lands *with* the guard.
- **`pool` import feature-gated; boundary documented as absent.** Per decision R3 the honest posture is to keep `AllowAllGuard`, feature-gate the strategy-facing `pool` import, and state plainly that shipping it advertises no boundary yet.

### What the epic will build (target consequences)

- The theft-prevention anchor is host-only trust: the identity-boundary guard decodes and simulates what it signs regardless of what adapters or modules claim. Spend-limit accuracy on venue submissions remains adapter-publisher trust (ADR-0010); the two layers degrade independently and the trust table in the design doc states both.
- Interactive signing acquires a latency budget shared by simulation and analysers. Deadlines are enforced with the existing metering machinery (fuel, epoch interruption); partial verdicts render as "analysis incomplete" per profile.
- The engine grows three surfaces: the `simulate` primitive with a backend seam, the fact-bundle assembly, and the analyser host-call path with deadline scheduling. The `analyzer` world finally gives the experimental `query-module` lineage a shipping consumer (the world is published-but-unhosted today: WIT exists, no linker).
- `GuardPolicy::check` goes sync → async (a breaking trait change) because a real guard needs I/O; it lands with the epic, not during M1.
- Verdict aggregation across multiple analysers starts as max-severity-wins; contradictions and reputation are open questions recorded in the design doc.
- Every embedder profile (server, wallet, super-app) must declare its fail-open/fail-closed matrix; the embedding API exposes verdicts and consent hooks so wallets render them natively.
- **Related M1 hardening the epic subsumes:** guard-deny must charge the caller's quota (else a denied module busy-loops the router — a latent DoS while only `AllowAllGuard` ships); `http` egress is allowlist-gated only and escapes the compile-time capability guarantee, so it needs either the world guarantee or a loud doc caveat; and adapters must fold into the restart/poison sweeps before production multi-venue.
