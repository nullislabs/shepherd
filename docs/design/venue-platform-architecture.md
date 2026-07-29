# Venue Platform Architecture

**Status:** Architecture report — decision-ready
**Baseline:** `origin/refactor/rename-chassis-to-keeper` (last car #260 + Wave-1 #334/#335), M1 train mid-flight
**Audience:** nullislabs/shepherd platform team
**Author:** lead architect

> This report synthesises five grounded lens sweeps and an adversarial review against the
> actual tree. Where a lens and the code disagreed, the code wins; where the adversary
> corrected a lens, the correction is folded in. The sharpest verified risks are carried
> into the migration plan.

---

## 1. Executive summary

We have built a genuinely three-layer platform, and two of the three layers are real and
wired end to end. The **universal host layer** (`nexum:host@0.2.0`) is clean, versioned,
and typed-error-idiomatic: six interfaces exist and are all linked into the `event-module`
world, with `chain`, `local-store`, and `logging` live and `identity`, `messaging`,
`remote-store` intentionally stubbed to 0.3 — matching the target vision almost exactly.
The **generic settlement layer** (`nexum:intent@0.1.0` + `nexum:value-flow` +
`nexum:adapter/venue-adapter`) is venue-neutral by construction (opaque `list<u8>` bodies,
mirror `pool`/`adapter` faces, a real host `PoolRouter`), and `echo-venue` proves the seam
is implementable without CoW. The venue-author SDK persona (`nexum-venue-sdk`,
`#[venue]`, `nexum-venue-test`) is **de facto shipped and well-built**, contradicting the
design docs that still call it "planned."

But the thesis is only half-proven, and the half that is frozen is shaped wrong. **Quoting
does not exist** — the vision says "settlement + quoting" but the contract exposes only
submit/status/cancel. The **intent ontology is EVM/CoW/single-tx-shaped** while the
value-flow vocabulary it depends on is broad — an internal contradiction. **No concrete CoW
venue component exists** (`cow-venue` ships bodies + client only), so venue-neutrality is
asserted by a toy echo, never a real second venue. The **egress guard that justifies the
whole router shape is an `AllowAllGuard` no-op**, runs on an adapter-attested header rather
than the settled bytes, and does not cover the signing path at all. The generic
`keeper.run` orchestrator the branch is *named for* is not yet in the new stack — strategy
authors get boxes of parts with no assembler. The single highest-leverage move is to fix
the `nexum:intent@0.1.0` WIT ontology **now**, while only `echo-venue` pins it and the cost
is near-zero — the whole interface set is pre-release cruft, so reshaping it costs an
internal recompile, never a wire break, right up until the true 0.1.0 release is cut.

---

## 2. The target architecture

Three layers, each a distinct versioning and trust boundary.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ LAYER 3 — CONCRETE VENUES (components; one per protocol)                   │
│                                                                            │
│   cow-venue          rfq-venue          amm-router-venue     echo-venue   │
│   ─ impl VenueAdapter over protocol codec + typed client                  │
│   ─ targets ONE world: exports nexum:intent/adapter@0.1.0                  │
│   ─ imports ONLY scoped transport (chain, messaging, http+allowlist)      │
│   ─ reaches its protocol as opaque bytes over wasi:http / nexum:host/chain │
└───────────────────────────────┬──────────────────────────────────────────┘
                                 │ exports/implements
┌───────────────────────────────▼──────────────────────────────────────────┐
│ LAYER 2 — GENERIC INTENT / VENUE WORLDS (venue-agnostic contract)          │
│                                                                            │
│   nexum:value-flow  ── settlement / asset / asset-amount vocabulary        │
│   nexum:intent      ── intent-header, auth-scheme, submit-outcome, status  │
│        pool.wit  (strategy face: venue named per call)  ◀─ modules         │
│        adapter.wit (venue face: no venue arg)           ◀─ venues          │
│        quote.wit (MISSING — the other half of the thesis)                  │
│                                                                            │
│   host PoolRouter: resolve venue-id → AdapterActor → guard → submit        │
└───────────────────────────────┬──────────────────────────────────────────┘
                                 │ imports
┌───────────────────────────────▼──────────────────────────────────────────┐
│ LAYER 1 — UNIVERSAL NEXUM HOST INTERFACES (venue-agnostic, HOST-provided)  │
│                                                                            │
│   nexum:host@0.2.0  world event-module:                                    │
│     chain ● local-store ● logging ●   (LIVE backends)                      │
│     identity ○ messaging ○ remote-store ○   (linked, backend → 0.3)        │
│   + WASI: clocks, random, wasi:http (per-module [capabilities.http])       │
└────────────────────────────────────────────────────────────────────────────┘

● live backend + SDK trait + Mock + linked    ○ linked + host impl, backend deferred
```

### Layer 1 — universal host interfaces

HOST-provided, implemented by the runtime, imported by everyone above. The contract is
per-capability WIT interfaces plus a single shared error vocabulary. The guest ergonomic
seam is one Rust trait per capability with a supertrait bundle and a `Mock*` for host-free
unit testing (ADR-0009), and one typed-error type mirroring the WIT `fault` variant
(ADR-0011).

```rust
// nexum-sdk — the provider-pattern seam (target shape)
pub trait ChainHost      { fn request(&self, chain: Chain, method: ChainMethod, params: &str) -> Result<String, ChainError>; }
pub trait LocalStoreHost { fn get(&self, k: &[u8]) -> Result<Option<Vec<u8>>, Fault>; /* set/delete/list_keys */ }
pub trait LoggingHost    { /* tracing facade */ }
pub trait IdentityHost   { /* accounts / sign / sign_typed_data */ }     // target: trait + MockIdentity
pub trait MessagingHost  { /* publish / query */ }                       // target: trait + MockMessaging
pub trait RemoteStoreHost{ /* upload / download / read_feed / write_feed */ }

pub trait Host: ChainHost + LocalStoreHost + LoggingHost
             + IdentityHost + MessagingHost + RemoteStoreHost {}          // target: all six
```

### Layer 2 — generic intent / venue worlds

The venue-neutral settlement (and, per vision, quoting) contract. Bodies are opaque
`list<u8>` at both faces so no venue schema leaks into the interface; the typed edges are
`nexum:value-flow` headers/quotes the router and guard can read uniformly.

```wit
// nexum:intent/adapter.wit — venue face (target, with quoting)
interface adapter {
  use nexum:value-flow/types.{asset-amount};
  derive-header: func(body: list<u8>) -> result<intent-header, venue-error>; // pure projection
  quote:        func(body: list<u8>) -> result<quote, venue-error>;          // MISSING today
  submit:       func(body: list<u8>) -> result<submit-outcome, venue-error>;
  status:       func(id: list<u8>)   -> result<intent-status, venue-error>;
  cancel:       func(id: list<u8>)   -> result<_, venue-error>;
}
```

### Layer 3 — concrete venues

A component per protocol. Each `impl VenueAdapter`, targets exactly one world exporting
`nexum:intent/adapter@0.1.0`, and reaches its protocol as opaque bytes over the scoped
transport it declares. CoW is the flagship: a `composable-cow` adapter over the existing
`OrderBody`/`ComposableBody` codec, capabilities `[chain, http]`.

### DX walkthrough A — author a new venue

```rust
// modules/venues/my-rfq/src/lib.rs
use nexum_venue_sdk::{venue, VenueAdapter, IntentBody, VenueError, HostChain};

#[derive(IntentBody)]                    // versioned borsh codec, typed BodyError
struct RfqBody { /* … */ }

struct MyRfq;

#[venue]                                 // synthesizes a per-manifest narrowed world
impl VenueAdapter for MyRfq {            // TARGET: macro emits impl over the typed trait
    fn derive_header(body: RfqBody) -> Result<IntentHeader, VenueError> { /* pure */ }
    fn quote(body: RfqBody)         -> Result<Quote, VenueError> { /* http round-trip */ }
    fn submit(body: RfqBody)        -> Result<SubmitOutcome, VenueError> { /* … */ }
    fn status(id: &[u8])            -> Result<IntentStatus, VenueError> { /* … */ }
    fn cancel(id: &[u8])            -> Result<(), VenueError> { /* … */ }
}
```

```toml
# module.toml — capabilities are the world by construction (compile error if outside {chain,messaging,http})
[capabilities]
required = ["chain", "http"]
```

The conformance kit (`nexum-venue-test`) holds it to portable JSON codec vectors and
header goldens; `cargo test` fails if the wire shape drifts.

### DX walkthrough B — call the host from a strategy

```rust
// pure strategy logic, generic over the host, host-free testable
fn on_tick<H: Host>(host: &H, tick: Tick) -> Result<(), Fault> {
    let block = host.provider().get_block_number()?;          // TARGET: alloy Provider seam
    let price = Chainlink::new(host, FEED).latest_answer()?;   // sol!-typed reader (exists)
    let last  = host.local_store().get(b"last")?;              // typed error, `?` folds to Fault
    // …decide, then submit via the venue-bound client…
    Ok(())
}
```

---

## 3. Where we are today

Grounded at `origin/refactor/rename-chassis-to-keeper`, last car #260 + Wave-1 #334/#335.

### Capability matrix

| Interface / capability | WIT? | Host impl? | Backend live? | SDK trait? | Mock? | Wired into world? |
|---|---|---|---|---|---|---|
| **L1** chain | ✅ `nexum:host/chain` | ✅ `ProviderPool` | ✅ | ✅ `ChainHost` | ✅ `MockChain` | ✅ event-module + adapter |
| **L1** local-store | ✅ | ✅ redb | ✅ | ✅ `LocalStoreHost` | ✅ | ✅ event-module |
| **L1** logging | ✅ | ✅ `LogPipeline` | ✅ | ✅ `LoggingHost` | ✅ | ✅ event-module |
| **L1** identity | ✅ | ✅ (empty roster) | ❌ → 0.3 | ❌ | ❌ | ✅ linked, ❌ no seam |
| **L1** messaging | ✅ | ✅ (scope enforced) | ❌ → 0.3 | ❌ | ❌ | ✅ event-module + adapter |
| **L1** remote-store | ✅ | ✅ (`unsupported`) | ❌ → 0.3 | ❌ | ❌ | ✅ linked, ❌ no seam |
| **L1** query-module world | ✅ (EXPERIMENTAL) | ❌ no linker | ❌ | ❌ | ❌ | ❌ published-unhosted |
| **L2** value-flow types | ✅ `nexum:value-flow` | n/a | n/a | ✅ (venue SDK) | ✅ goldens | ✅ |
| **L2** intent settlement | ✅ `nexum:intent` pool/adapter | ✅ `PoolRouter` | ✅ | ✅ `IntentClient<P>` | ✅ `nexum-venue-test` | ✅ |
| **L2** intent quoting | ❌ absent | ❌ | ❌ | ❌ | ❌ | ❌ |
| **L2** egress guard | ✅ `GuardPolicy` trait | ⚠️ `AllowAllGuard` no-op | ❌ | n/a | n/a | ✅ seam only |
| **L2** adapter supervision | ✅ | ✅ boot/install | ⚠️ no restart/poison | n/a | n/a | ✅ partial |
| **L3** CoW venue component | ⚠️ codec/`CowClient`, plain `[lib]` | ❌ **no cdylib / no `VenueAdapter` impl** | ❌ | ⚠️ body/client only | — | ❌ |
| **L3** echo-venue | ✅ | ✅ | ✅ | ✅ `#[venue]` | ✅ golden | ✅ (only real adapter) |
| **L3** legacy `shepherd:cow/cow-api` | ✅ | ✅ (module cap) | ✅ | — | — | ✅ event-module only (**live CoW submit**) |

### Per-layer narrative

**Layer 1 (host).** Strong. All six interfaces exist, are linked into `event-module`, and
have host impls; the real/stub split is *backend liveness*, matching the vision (chain +
local-store today; messaging + remote-store later; identity a fourth deferred). The typed-
error model (ADR-0011) is fully realised: one `fault` variant (7 cases incl.
`rate-limited{retry-after-ms}`), mirrored as `Fault` (thiserror + `IntoStaticStr` snake_case
+ `#[non_exhaustive]`), `From<ChainError> for Fault` so aggregated calls fold through `?`.
Capability enforcement is hardened: `#[nexum_sdk::module]` synthesises a per-module world
from the manifest, so undeclared caps are a compile error — **except `http`** (see R-C2).
The gaps are the guest seam: the `Host` supertrait bundles only 3 of 6, and `chain` is raw
stringly JSON-RPC with no alloy `Provider`.

**Layer 2 (generic).** The settlement surface is venue-neutral and works end to end:
three independent-cadence packages, opaque bodies, mirror faces, a real `PoolRouter`
(resolve → derive-header → guard → charge → submit → status-watch), per-component worlds
via `synthesize_venue`. What is missing is structural: **no quoting**, a **no-op guard**,
adapters **outside the restart/poison sweeps**, and a residual EVM/CoW shape in the intent
ontology (below).

**Layer 3 (concrete).** The persona is shipped but **unexercised by a real venue**.
`cow-venue` is a plain `[lib]` crate — **no `crate-type = ["cdylib"]`** — carrying only its
`body` + `client` slices (grep for `export_venue_adapter!`/`#[venue]`/`VenueAdapter` is
empty); `lib.rs:12-13` names the typed-client-plus-adapter component an explicit "later
slice." The only working adapter is `echo-venue`. Crucially, the **live CoW submit path
never touches the generic seam**: it still runs module → the `shepherd:cow/cow-api@0.2.0`
host extension → the `cowprotocol` crate, assembling `OrderCreation` JSON on the strategy
side (`shepherd-sdk/src/cow`) and **bypassing `nexum:intent/pool` entirely**. `mod.rs:36`
already re-exports the cow-venue body types as a shim "while the module ports move off the
legacy surface," but that port has not happened — so `docs/08`, which documents *only* the
older host-extension model, still matches the hot path even though the code is meant to
migrate away from it.

---

## 4. Gap analysis (ranked by leverage)

Ranked across all layers; leverage = (blast radius if unfixed) × (cheapness now vs later).

1. **[L2, contract] The `nexum:intent@0.1.0` ontology is mis-shaped and
   unexercised.** Missing quoting; `submit-outcome.requires-signing` is a *single* EVM
   `unsigned-tx`; `auth-scheme` is EVM-only; `venue: string` is stringly; `venue-error`
   lacks `rate-limited`. Every fix costs only an internal recompile + a train fold, and only
   `echo-venue` pins the world today — nothing is released. **Highest leverage bar none.**
2. **[L3, spec] No coherent composition path for the flagship CoW venue.** The vision's
   "compose cow-protocol WASI + Nexum venue world" is a category error (B1): a component
   targets one world, and no "cow-protocol WASI" interface exists. Must be a spec decision
   *before* the cow-venue adapter slice is built.
3. **[L2, security] The egress guard is advertised but absent and mis-shaped.** Only
   `AllowAllGuard` ships; it runs on the adapter-attested header, not the settled bytes
   (TOCTOU), and does not cover the `requires-signing` signing path at all (B3).
4. **[L1, DX] The chain seam has no alloy `Provider`.** Raw `request(u64,&str,&str)->String`;
   authors hand-build JSON and parse strings. `doc-08` promises a `HostTransport: alloy
   Transport` shim that is not in the SDK. Largest single DX gap from the alloy target.
5. **[L1, DX] Three of six host interfaces have no guest seam.** `identity`, `messaging`,
   `remote-store` have `adapter:None` in the macro KNOWN table, no `*Host` trait, no
   `Mock*` — modules reach them only via raw wit-bindgen and cannot host-free unit-test.
   ADR-0009's "each interface becomes a trait + MockX" is unfulfilled for all three.
6. **[L2, DX] No generic `Keeper` orchestrator.** The branch is named for `keeper.run`, but
   only the parts ship (`WatchSet`/`Gates`/`Journal`/`Retrier`/`ConditionalSource`); there
   is no `Keeper::sweep(tick)` assembling them, and `ConditionalSource::Outcome` is a
   dangling associated type nothing consumes.
7. **[L3, DX] Two authoring paths fork the "one clear arrangement."** `#[venue]` emits a
   `Guest` impl over raw bindgen types and *bypasses* the `VenueAdapter` trait;
   `export_venue_adapter!` routes through it on a differently-named world. Each `#[venue]`
   adapter also hand-copies ~80 lines of `*_to_golden` bridges.
8. **[L1↔L2, versioning] Asymmetric host→intent freeze coupling.** `nexum:host@0.2.0`
   `use nexum:intent/types@0.1.0.{receipt, intent-status}` in its `event` variant, so a new
   `intent-status` case breaks the host and recompiles every event-module.
9. **[L2, DX] `venue-error` is raw wire, not mirrored** (no `Display`, formatted via
   `{0:?}`); the fault fold drops `retry-after-ms`. **[L3, DX] `OrderBody` is a bare
   12-field literal**; CoW body aliases `Address`/`U256` drop the newtype guarantee.
10. **[docs] `docs/05` says the venue persona is "not shipped"; `docs/08` documents only
    the deprecated Layer-3 host-extension model.** The source-of-truth docs actively
    mislead a new author.

---

## 5. DX / reth-alloy idioms

Concrete before/after for the roughest surfaces.

### 5.1 Chain: stringly JSON-RPC → alloy Provider seam

```rust
// BEFORE — hand-rolled JSON in, string out
let params = eth_call_params(to, calldata, "latest");
let raw = host.request(1u64, "eth_call", &params)?;      // ChainHost::request
let out = parse_eth_call_result(&raw)?;

// AFTER — alloy Provider over a HostTransport: alloy Transport shim
let provider = host.provider(Chain::mainnet());          // zero-cost, typed Chain
let out = provider.call(&tx).block(BlockId::latest()).await?;
let head = provider.get_block_number().await?;
```

`ChainMethod` (the closed `IntoStaticStr` RPC enum) already exists host-side — carry the
same typed surface to the guest instead of `method:&str`.

### 5.2 Venue id: stringly → zero-cost newtype

```rust
// BEFORE — three disconnected string definitions agree by convention
const VENUE: &str = "cow";                       // cow-venue/src/client.rs
[[adapters]] name = "cow"                         // engine.toml
venue_id = adapter_namespace                      // supervisor.rs (manifest name)
// mismatch → runtime venue-error.unknown-venue

// AFTER — one source, compile-time linkage
pub struct VenueId(&'static str);                 // zero-cost newtype
impl CowVenue { pub const ID: VenueId = VenueId("cow"); }
let client = IntentClient::builder().venue(CowVenue::ID).connect(pool);
```

### 5.3 Generic keeper: parts box → assembler

```rust
// BEFORE — parts only; every author re-hand-rolls list→parse→is_ready→get→poll→match→apply
// AFTER — the keeper.run the branch is named for, venue-neutral
pub struct Keeper<'h, H, S> { host: &'h H, source: S }
impl<'h, H: Host, S: ConditionalSource<H>> Keeper<'h, H, S> {
    pub fn sweep(&self, tick: &Tick) -> Result<SweepReport, Fault> { /* WatchSet→Gates→Retrier */ }
}
// give ConditionalSource a shared Outcome the keeper drives:
pub enum Sweep { Submit(Vec<u8>), WaitBlock(u64), WaitEpoch(u64), Drop, TryNextBlock }
let keeper = Keeper::new(host).source(src).submit_via(client);
```

### 5.4 Order construction: 12-field literal → typestate builder

```rust
// BEFORE — name all 12 fields at every call site
let order = OrderBody { sell_token, buy_token, sell_amount, buy_amount, valid_to,
    receiver, kind, partially_fillable, sell_token_balance, buy_token_balance, app_data, fee };
// AFTER — alloy TransactionRequest idiom
let order = Order::sell(sell_token, sell_amount)
    .buy(buy_token, buy_amount)
    .valid_to(t)
    .partially_fillable()
    .build();                                    // receiver=None, balances/kind defaulted
```

### The systemic moves worth making

- **Provider pattern:** the alloy `Provider` seam (5.1) is the flagship; `IntentClient`
  gains a `ProviderBuilder`-style `builder().venue(id).connect(pool)` and a
  `client.quote(&body)?.submit()?` typestate once quoting lands.
- **Zero-cost newtypes:** `VenueId`, a guest-side `Chain`/`ChainId`, and CoW
  `SellToken(Address)`/`BuyToken(Address)` so sell/buy cannot silently swap.
- **Sealed traits:** seal the blanket-impl / extension-point traits (`Host`, `HostFault`,
  `RuntimeTypes`, `Runtime`, `IntentPool`) with a private `Sealed` supertrait — the reth
  idiom for "traits you implement, not us," and it lets the SDK grow their method sets.
- **`#[non_exhaustive]` uniformly** across every public error/label enum (`BodyError`,
  `ClientError`, `ConfigError`, `ChainError`, `BuildError` currently lack it).
- **Derive the mirrors:** the `fault` vocabulary is hand-mirrored in three places
  (`types.wit`, SDK `Fault`, macro round-trip) and the KNOWN capability table is duplicated
  across `nexum-macros` and the runtime `CapabilityRegistry`. Both should emit from one
  source-of-truth const so they cannot skew.
- **Mirror `venue-error`** the way `chain-error` is mirrored (a `VenueFault` with
  `Display` + `IntoStaticStr` label + `From<bindings>`), so operator logs stop
  `{0:?}`-formatting and the `rate-limited` case survives.

---

## 6. Red-team findings

Ranked by what sinks the architecture, each with the failure it causes and where to fix it.
Verified against the tree by the adversarial pass.

### R1 — `nexum:intent@0.1.0` is mis-shaped and unexercised *(sink #1)*
**Failure:** a real second venue (RFQ / AMM-router) immediately hits five walls at once —
no place to express a **quote**; `submit-outcome.requires-signing(unsigned-tx)` is exactly
one EVM call so approve+swap or any tx *sequence* is unrepresentable and a non-EVM venue
cannot express settlement at all (B2); `auth-scheme` is EVM-only; venue id is stringly; and
`venue-error` lacks `rate-limited`, so the `faults.rs` fold collapses
`unavailable|rate-limited|timeout → unavailable(string)` and destroys the `retry-after-ms`
a throttling-heavy RFQ/AMM API needs. **Where:** `wit/nexum-intent/{pool,adapter,types}.wit`
and `wit/nexum-value-flow/types.wit`. **When:** now — only `supervisor/tests.rs:306`
(`echo-venue`) instantiates the world, and the whole interface set is pre-release cruft, so a
reshape costs an internal recompile + a train fold, never a wire break. Do it before the true
0.1.0 release is cut.
**Decided (2026-07-14):** 0.1 is **EVM-only** as a *scoping* choice; non-EVM settlement
(de-EVM `auth-scheme`/`unsigned-tx`) lands later. Nothing is pinned, so this reshape — and
quoting — can land now or later at will; there is no freeze to preserve additive
extensibility across (see decision 8).

### R2 — the flagship CoW venue has no coherent composition path *(sink #2)*
**Failure:** the vision's "concrete venue brings in the cow-protocol WASI + the Nexum venue
world" is a **category error** — a component targets one world, and no "cow-protocol WASI"
interface exists. The only CoW WIT (`shepherd:cow/cow-api@0.2.0`) is a *host extension linked
into event-modules only*: adapters **cannot import it** (`ADAPTER_CAPABILITIES = ["chain",
"messaging"]`, `manifest/capabilities.rs`; `build_adapter_linker`, `supervisor.rs:1316`,
comments "Extensions are not linked into adapters"). So a CoW adapter reaches the orderbook
over `wasi:http` (gated by `[[adapters]].http_allow`) + `nexum:host/chain` — and needs **no
separate "composable-cow module,"** because composable orders are already just a
`ComposableBody` payload variant of `CowIntentBody`. **Unresolved (ADR-0013):** the
`Verdict::Post` seam is **half-populated** — it is the one variant `LegacyRevertAdapter` never
produces (`composable.rs:340`), so decode/classify never yields a submittable order; and
`Verdict::NeedsInput` is **dead surface** until `IOrderModule`/the fork lands (`run.rs:143`
parks it). **Where:** spec decision + `docs/08` + the adapter linker/capability story.
**When:** before the cow-venue adapter slice.

### R3 — the egress guard is advertised, absent, and mis-shaped *(sink #3)*
**Failure:** the router's entire `derive→guard→submit` shape is justified by a checkpoint
that (a) is `AllowAllGuard`, a no-op (`pool_router.rs:104-110`); (b) inspects the adapter's
*own* `derive-header` output while `submit` re-decodes the body independently, so a
buggy/hostile adapter shows a benign `gives` and settles something else (TOCTOU); and (c)
does not cover the `requires-signing` class at all — the real value movement is in the
`unsigned-tx` calldata *returned by submit* and signed on the `identity` path, which is a
0.3 stub (`accounts()->Ok(vec![])`). Shipping the `pool` import in the default build
advertises a boundary that does not exist. **Where:** `pool_router.rs` (move the guard to
the signed-tx boundary; pass the derived header *into* submit for single-decode);
`intent/types.wit` (soften "host-verified `gives`" to "adapter-attested"). **When:** before
any non-echo adapter is installable and before identity signing lands.
**Decided (2026-07-14):** advisory-only for M1 — keep `AllowAllGuard` + feature-gate the
`pool` import + document the boundary as **not yet enforcing**; the real guard (its shape and
where it runs) is deferred wholly to the egress-guard epic. No teeth land during M1.

### R4 — two authoring paths fork the "one clear arrangement of traits" *(sink #4)*
**Failure:** `#[venue]` emits `impl exports::nexum::intent::adapter::Guest` over raw
bindgen types and **bypasses** the typed `VenueAdapter` trait (`macros/lib.rs:426`;
`echo-venue` impls inherent fns), while `export_venue_adapter!` routes through the trait —
the flagship typed trait is bypassed by the flagship macro. Note the **corrected** framing
(C1): the two world *names* (`nexum:venue-world` vs `nexum:adapter`) are a harmless local
alias — both export `nexum:intent/adapter@0.1.0` and are loadable by the same host. The
real divergence is **import-narrowing**: `export_venue_adapter!` imports chain+messaging
unconditionally and leans on wasm-tools dead-import elision to pass capability enforcement,
whereas `synthesize_venue` narrows by construction. **Where:** make `#[venue]` emit an
`impl VenueAdapter` shim; demote `export_venue_adapter!`; unify the import-narrowing
guarantee. **When:** before the cow-venue adapter, or the fork is enshrined in the flagship.
**Decided (2026-07-14):** `#[nexum::venue]` is the single blessed authoring path, fixed to
emit `impl VenueAdapter`; `export_venue_adapter!` is demoted to the internal codegen detail
the macro expands to (not a public second path).

### R5 — the `http` capability escapes the compile-time guarantee *(corrected from lens 1)*
**Failure:** in the KNOWN table `http` has `import: None` (`world.rs:88-92`) — declaring or
omitting `http` changes *no* world import line; `wasi:http` is linked out-of-band and gated
only by the `engine.toml` allowlist. So the "undeclared cap is a compile error" guarantee
covers chain/messaging/logging but **not** the one venue capability most likely to carry
egress. **Where:** either bring `http` under the synthesised-world guarantee or document
loudly that http egress is allowlist-gated only. **When:** before venues start making
external calls in production.

### R6 — asymmetric host→intent freeze coupling *(corrected from lens 2)*
**Failure:** `intent/types.wit` claims freeze independence, but `nexum-host/types.wit:8`
`use nexum:intent/types@0.1.0.{receipt, intent-status}` — the decoupling is
*one-directional*. Bumping `nexum:intent` (a new lifecycle `intent-status` case a new venue
needs) is a breaking change to `nexum:host` and recompiles every event-module. **Where:**
decide whether the host `event` stream should carry `intent-status` at all, or a host-owned
opaque status projection. **When:** now, before more `intent-status` cases are demanded.
**Decided (2026-07-14):** the host `event` stream carries **opaque status bytes**, decoupled
from `nexum:intent` (drop `use nexum:intent/types.{intent-status}` in `nexum-host/types.wit`),
with a documented, versioned contract for how those bytes destructure. This folds in with the
same pre-release reshape — the `nexum:host` package version carries no maturity or compat
weight (it is cruft, normalizing to `@0.1.0`; see decision 8), so there is no version reason
to sequence it apart. If ordering matters, it is on technical merit alone.

### R7 — no module↔venue schema-version handshake *(blind spot B4)*
**Failure:** bodies are opaque `list<u8>` with a guest-side borsh version tag; if a strategy
module and the installed adapter disagree on body version, the only signal is `invalid-body`
at runtime. For a platform whose thesis is "opaque bodies + typed edges," schema agreement
is never a checked property. **Where:** a version/feature field on the pool face or a
capability handshake at install. **When:** can defer past M1, but decide the mechanism now.
**Decided (2026-07-14):** an **install-time capability handshake** — a `body_version` (or
version-set) field in the module and adapter manifests; `Supervisor::install` asserts the
module's version is in the adapter's supported set and refuses to boot a mismatched pair
(fail fast, logged). This is a manifest + supervisor change (**not** WIT-freeze-gated), so it
builds as a concrete Phase 1-2 step.

### R8 — adapters are outside the supervisor lifecycle sweeps
**Failure:** adapters boot once and install but are not in the restart/poison-recovery
sweeps (`supervisor.rs:61-66`); a trapped adapter stays dead until process restart, and the
router only projects the trap to `internal-error`. **Where:** fold adapters into the sweeps;
expose `adapters_alive` so a strategy distinguishes `unknown-venue` from
`venue-temporarily-dead`. **When:** before production multi-venue.

---

## 7. Migration plan from mid-train

We are mid-M1 with the last car at #260 and Wave-1 (#334/#335) in flight.
`Materialiser<Source, Venue>` is the M7 destination. The governing constraint:
**the whole WIT interface set is pre-release**, pinned only by the `echo-venue`/`echo-client`
demo pair and the host router — no external consumer, no released version. The package
version strings on the WIT (`@0.1.0`, `@0.2.0`, …) are accumulated cruft, not compat
boundaries; they normalize to a single `@0.1.0` at the true initial release (decision 8).
Until that release is cut, a breaking WIT change costs only an internal recompile + a train
fold — never a wire break — so this clean-slate window is exactly when to reshape aggressively.
The WIT-debt cluster leads the plan for that reason, not because of any looming freeze.

### The fold-vs-amend rule

The **keeper-rename fold** is the proven stack-surgery template: a range-limited
`git-filter-repo` pass replayed across the 21-car stack (#239→#260) + Wave-1 (#334/#335),
validated against a **byte-identical tip oracle** (rebuild the tip two ways, diff, identical
trees prove no drift), then a single force-push of every branch; `jj` drives the per-car
rebases (immutable-heads override for pushed cars) and `mergiraf` resolves the WIT/Rust
conflicts. That template gives the organizing rule for the whole plan:

- **Fold** — any change that must appear *identically in every car's WIT* is a train-wide
  fold, never a per-car edit (a WIT type touched in #247 is imported by #248→#260; editing it
  car-by-car desyncs the stack). Folds go through the oracle-validated `git-filter-repo` +
  force-push machinery above, and every WIT-touching step regenerates the `nexum-venue-test`
  goldens and re-asserts the tip oracle.
- **Amend-in-place** — a correctness fix that lives in *exactly one car* is the opposite:
  amend that car directly and ripple-rebase the rest with `jj`.

### Phase 0 — reshape the pre-release contract (one WIT fold; echo-venue sole pin)

The whole WIT interface set is pre-release and echo-only-pinned, so this is a **clean-slate
window**: a breaking change costs an internal recompile + a train fold, never a wire break,
because there is no external consumer and no released version to break (decision 8). That
makes reshaping *free* — do it aggressively now, before the true 0.1.0 release is cut. Do the
entire WIT-debt cluster (#247/#248/#297) as **one fold** on `refactor/intent-contract-reshape`,
oracle-validated exactly like the keeper rename:

- **train-wide version normalization** — reset every WIT package and every `use …@<ver>`
  reference (`nexum:host@0.2.0`, `nexum:intent@0.1.0`, `nexum:value-flow`, `nexum:adapter`,
  `shepherd:cow@0.2.0`, …) to a single **`@0.1.0`** as the true initial release, done as one
  `git-filter-repo`/`jj` fold like the keeper rename (byte-identical tip oracle across the
  stack). The old numbers are accumulated cruft, not compat boundaries;
- rename `valid-until` → `valid-until-ms` (kills the silent ms-vs-`Tick.epoch_s`-seconds
  `/1000` mismatch);
- MUST-NOT-retry doc on `venue-error.denied()`;
- lift the anonymous `erc20`/`erc721`/`erc1155` positional tuples to **named records**;
- add a **version discriminator + reject-unknown** to the cross-language codec goldens, plus a
  non-empty-vector assertion (today's empty-vector golden passes vacuously);
- `derive-header` purity doc-caveat / sub-world note;
- **fold in the R6 host↔intent decoupling** — drop `nexum-host/types.wit`'s
  `use nexum:intent/types.{intent-status}` so the host `event` stream carries **opaque status
  bytes** plus a documented, versioned destructuring contract. The host package version is
  cruft too, so there is no maturity reason to sequence it apart from the intent reshape; it
  rides the same fold (order it later only if a technical dependency demands it);
- **delete/rewrite the migration cruft** — `docs/migration/0.1-to-0.2.md` and the "Migration
  from 0.1" prose in `docs/08-platform-generalisation.md` describe a version transition that
  never happened; there is no released 0.1 to migrate *from*, so delete the file and drop the
  prose;
- fold in the co-located nits (`none`/`unsigned` keyword note, BZZ-is-ERC20, missing
  `amount`-field doc).

**This is the free window, not a one-way door:** nothing is pinned, so quoting, de-EVM, and
any other breaking reshape can land now or later at will — echo-only pinning means the blast
radius today is a demo recompile. The reason to do it *now* is cleanliness before the true
0.1.0 release, not a looming freeze.

### Phase 1 — per-car Rust correctness amends (no fold)

None of these touch WIT, so each is an amend-in-place with a `jj` ripple-rebase, no fold:

- **#249** — supervisor missing-manifest error (distinguish *missing* from *wrong-kind*);
- **#250** — guard-deny must charge quota (closes the busy-loop DoS; latent while only
  `AllowAllGuard` ships, but cheap now);
- **#251** — add the `RateLimited` fold test (currently drops `retry_after_ms` untested);
- **#296** — bump `echo-venue` to `wit-bindgen` 0.59 (de-dupes `Cargo.lock`).
- **R7 install-time handshake** (shape decided 2026-07-14; manifest + supervisor, **no
  WIT-freeze dependency** so it need not ride Phase 0) — add a `body_version` (or version-set)
  field to the module and adapter manifests and have `Supervisor::install` assert the module's
  version is in the adapter's supported set, refusing to boot a mismatched pair (fail fast,
  logged). May land as a new car in Phase 1 or Phase 2.

Clearing Phase 1 lets the approved cars (#252/#253/#256/#257/#259/#260) merge clean.

### Phase 1-Wave-1 — Verdict seam hardening (#334)

Both fixes are latent-until-fork, so free now; amend #334 directly:

- model `Verdict::Post.next_poll_timestamp` as `Option`/`NextPoll` instead of a `0`-sentinel
  (the sentinel collides with the fork wire; no-op today, breaking once the fork deploys);
- add a dispatch test for the `NeedsInput` arm (today it info-logs and busy-polls every tick).

### Phase 2 — egress-guard epic + adapter/guard fidelity (new cars off Wave-1 tip / post-M1)

A real (non-`AllowAll`) guard makes a latent cluster live, so these are new cars gated behind
the egress-guard epic owner:

- the **material** facet of #249 `derive-header`-before-guard — the router runs adapter
  derivation *before* `guard.check`, so side effects escape policy; the honest fix is a
  guarded sub-world or moving derivation behind the checkpoint, not just the Phase-0 doc
  caveat;
- **#250** `GuardPolicy::check` sync → async (breaking; lands with the egress-guard epic,
  which needs async I/O);
- **#249** `messaging.query` scope-check hole (goes live with the 0.3 Waku backend);
- **#297** mock-grant fidelity divergence and **#296** blanket chain+messaging shims / two
  coexisting adapter contracts — canonicalise **one** adapter contract as the real guard
  replaces the shims.

**M1 guard posture (advisory-only, decided 2026-07-14):** M1 ships no guard teeth. Keep
`AllowAllGuard`, feature-gate the `pool` import, and document the checkpoint as **not yet a
boundary**. The real guard — its shape *and* where it runs (signed-`unsigned-tx` boundary,
single-decode) — is deferred **wholly** to the egress-guard epic; nothing enforcing lands
during M1.

Fold the remaining red-team hardening in here as the epic lands: move the guard checkpoint to
the signed-`unsigned-tx` boundary and soften the `gives`-is-host-verified claim to
"adapter-attested" (R3); fold adapters into the restart/poison sweeps and expose
`adapters_alive` (R8); bring `http` under the world guarantee or document the allowlist-only
story (R5).

### Phase 3 — module/backtest correctness follow-ons

Low blast radius, non-contract; land whenever the owning car reopens, none blocks freeze or
train merge:

- **#242** — backtest `classify_ok` misclassification + stop-loss dedup-key asymmetry
  (`server_uid` write vs `uid_hex` read → re-POST on UID drift);
- **#243** — macro name-only handler match (an async `on_block` compiles `.await`-less).

### Phase 4 — SDK / DX build-out + docs (fold as convenient)

With the reshaped contract underneath, these are the good-but-non-blocking build-outs; fold
WIT touches when convenient, oracle-re-validated:

- **alloy `Provider` seam** — `HostTransport: alloy Transport` over `ChainHost::request`
  (Gap #4, §5.1);
- **`IdentityHost`/`MessagingHost`/`RemoteStoreHost` + `Mock*` + bind-macro slices** wired to
  the stub backends, widening the `Host` supertrait (or opt-in subset supertraits) so guest DX
  decouples from 0.3 backend readiness (Gap #5);
- **unify the authoring path (R4, decided 2026-07-14):** make `#[nexum::venue]` emit an
  `impl VenueAdapter` and demote `export_venue_adapter!` to the internal codegen the macro
  expands to — `#[venue]` is the single blessed path, no public second path, ambiguity removed;
- **DX polish:** generic `Keeper::sweep` + shared `Sweep` outcome (§5.3); `Order` builder
  (§5.4); mirror `VenueError` → `VenueFault` (§4.9); uniform `#[non_exhaustive]`; seal the
  extension traits; derive the `fault` mirror + KNOWN table from one source-of-truth const;
  kill the `*_to_golden` bridge boilerplate (§4.7);
- **docs (blocking, cheap):** rewrite `docs/05` (venue persona is **shipped** — document the
  crate layout + a step-by-step "author a venue") and `docs/08` (venue adapters are **the**
  domain-extension mechanism; mark `shepherd:cow/cow-api` as legacy read-path). Highest
  discovery-return change, pure prose.

### The full m1-review-sweep triage (item → phase → action → branch)

| Sweep item | Phase | Action | Branch |
|---|---|---|---|
| version-string cruft → single `@0.1.0` (#247) | 0 | train-wide normalization fold | fold `refactor/intent-contract-reshape` |
| `valid-until` ms vs `epoch_s` (#248) | 0 | rename `valid-until-ms` | fold `refactor/intent-contract-reshape` |
| `denied()` no MUST-NOT-retry (#248) | 0 | doc caveat | fold |
| ERC anonymous tuples (#247) | 0 | → named records | fold |
| codec no version discriminator (#297) | 0 | add discriminator + reject-unknown | fold (vector car) |
| `derive-header` purity claim (#249) | 0 + 2 | doc/sub-world caveat now; reorder vs guard later | fold; then Phase-2 car |
| codec empty-vector vacuous pass (#297) | 0 | non-empty vector assertion | fold (vector car) |
| supervisor missing-manifest error (#249) | 1 | branch missing vs wrong-kind | amend car #249 |
| guard-deny no quota / DoS (#250) | 1 | charge quota on deny | amend car #250 |
| `RateLimited` fold untested (#251) | 1 | add test | amend car #251 |
| wit-bindgen 0.58/0.59 skew (#296) | 1 | bump to 0.59 | amend car #296 |
| `Verdict::Post` 0-sentinel (#334) | 1-W1 | model `Option`/`NextPoll` | amend #334 |
| `NeedsInput` busy-poll / no test (#334) | 1-W1 | add dispatch test | amend #334 |
| `GuardPolicy::check` sync (#250) | 2 | async trait | new car off Wave-1 (egress-guard epic) |
| `messaging.query` not scoped (#249) | 2 | scope-check | egress-guard epic (0.3 Waku) |
| mock grant fidelity diverges (#297) | 2 | align mock to host | egress-guard epic |
| blanket shims / 2 adapter contracts (#296) | 2 | canonicalise one contract | egress-guard epic |
| backtest `classify_ok` (#242) | 3 | tighten bucketing | car #242 |
| stop-loss dedup-key asymmetry (#242) | 3 | unify write/read key | car #242 |
| macro name-only match (#243) | 3 | match on async-ness | car #243 |

### Triage

**Do next (this week):** all of Phase 0 as one oracle-validated fold, then
Phase 1 + the two #334 fixes. *Rationale:* Phase 0 is the **clean-slate pre-release window** —
nothing is pinned, so every WIT-debt item (including the version-string normalization) costs
only an internal recompile + a fold, and echo-only pinning means the blast radius is a demo;
do it now for a clean true-0.1.0 release, not because of any freeze. Phase 1/#334 are one-car
amends with no fold cost, and clearing them lets the approved cars
(#252/#253/#256/#257/#259/#260) + #334/#335 merge clean.

**Do later (post-M1, epic-gated):** Phase 2 — reachable only once a real egress guard replaces
`AllowAllGuard`, so fixing it now is untestable/speculative; ride the egress-guard epic + the
0.3 Waku backend. Also gated: the **ADR-0013 poll wire-swap**, merge-blocked until the fork's
`deployments/networks.json` is non-empty on a shepherd target chain; until then
`LegacyRevertAdapter` maps the upstream reverting selectors to a structured `Verdict`.

**Decided, build in a concrete phase (no longer decide-later):** the **authoring-path unify**
(R4 — `#[venue]` emits `impl VenueAdapter`, `export_venue_adapter!` internal) lands in Phase 4
DX; the **R7 module↔venue handshake** (install-time `body_version` assertion in
`Supervisor::install`) lands as a manifest+supervisor car in Phase 1-2 (no WIT-freeze gate).

**Don't yet (defer to M7 / post-freeze / epic-gated):** the venue-neutral
`Materialiser<Source, Venue>` (explicitly M7); the concrete CoW venue component + the cow-venue
clean-break migration of `shepherd-sdk::cow`; **quoting** in the intent contract + de-EVM-ing
the CoW/single-tx ontology (0.1 is **EVM-only**, decided 2026-07-14); and the **real egress
guard** (M1 is advisory-only — the guard's teeth and location are the egress-guard epic's).
These want a named design partner and a *second* real venue so the true 0.1.0 does not enshrine
guesses. *Note:* deferring these is a *scoping* choice, not a compatibility one — nothing is
pinned, so quoting or a de-EVM reshape can land whenever a design partner arrives, at the cost
of an internal recompile + a fold. There is no freeze to preserve additive extensibility
across, so no additive down-payment is needed.

### Grounded caveats

- The triage file `docs/design/m1-review-sweep-triage.md` is **on disk only** — not committed
  at `origin/refactor/rename-chassis-to-keeper`.
- The WIT (`wit/nexum-intent/{types,adapter,pool}.wit`, `wit/nexum-value-flow/types.wit`) is
  confirmed `@0.1.0` **unfrozen**, pinned only by the echo demo pair + the host router.
- ADR-0013's **merge gate** (fork `deployments/networks.json` non-empty) is the hard blocker on
  the poll wire-swap.

---

## 8. Decisions (2026-07-14)

The open questions are resolved. Each decision below states the resolution and its one-line
consequence for the architecture/plan; the R-item mapping is retained so cross-references
still resolve.

1. **(Q1 / R2) No venue-specific host interfaces — adapters are transport-only.** Adapters
   reach venues over the generic Nexum host interfaces + `wasi:http`; the host interface set
   must be kept ample enough for venues. *Consequence:* the `shepherd:cow/cow-api`-as-adapter-
   extension ambiguity is **deleted** from the docs (it survives only as the legacy
   event-module read path); no extension-namespace machinery is added to `synthesize_venue` /
   the adapter linker.
2. **(Q2 / R6) The host `event` stream carries opaque status bytes.** Drop
   `nexum-host/types.wit`'s `use nexum:intent/types.{intent-status}` coupling; the host emits
   opaque bytes with a documented, versioned destructuring contract. *Consequence:* the host
   package version carries no maturity or compat weight (it is cruft, normalizing to `@0.1.0`
   per decision 8), so this folds in with the same pre-release reshape in Phase 0 — order it
   apart only if a technical dependency demands it, never for version-maturity reasons.
3. **(R3) The egress guard is advisory-only for M1.** Keep `AllowAllGuard`, feature-gate the
   `pool` import, and document the boundary as **not yet enforcing**. *Consequence:* the real
   guard — trust model, single-decode, and where it runs — is deferred wholly to the
   egress-guard epic; no teeth land during the M1 timeline (consistent with decision 7).
4. **(R7) Install-time capability handshake for body-schema agreement.** A `body_version` (or
   version-set) field in the module and adapter manifests; `Supervisor::install` asserts the
   module's version is in the adapter's supported set and refuses to boot a mismatched pair
   (fail fast, logged). *Consequence:* a manifest + supervisor change (**not** WIT-freeze-
   gated), so it lands as a concrete Phase 1-2 step — not "decide the shape now, build later."
5. **(Q5 / R1-B2) 0.1 is EVM-only** — a *scoping* choice, not a compatibility one. Non-EVM
   settlement (de-EVM `auth-scheme`/`unsigned-tx`) lands later. *Consequence:* nothing is
   pinned (decision 8), so non-EVM + quoting can land whenever a design partner arrives, at the
   cost of an internal recompile + a fold — no additive down-payment or wire-break avoidance is
   needed; the named-records + version-discriminator work stands on its own hygiene merits.
6. **(Q6 / R4) `#[nexum::venue]` is the single blessed authoring path,** fixed to emit
   `impl VenueAdapter` (not a raw `Guest` impl). *Consequence:* `export_venue_adapter!` is
   demoted to the internal codegen detail the macro expands to — not a public second path; the
   "which is blessed?" ambiguity is removed.
7. **(R3) Identity signing lands with the guard, later (Phase 3).** *Consequence:* the "chain
   delegates to identity for signing" claim in `chain.wit`/`doc-08` stays unrealised until
   then; module authors should not build against the signing seam before Phase 3.
8. **(new, 2026-07-14) WIT package versions are pre-release cruft — all normalize to a single
   `@0.1.0`.** The accumulated version strings (`nexum:host@0.2.0`, `nexum:intent@0.1.0`,
   `nexum:value-flow`, `nexum:adapter`, `shepherd:cow@0.2.0`, …) are not meaningful
   compatibility boundaries; no cross-version compatibility exists or is preserved, and no
   external consumer pins any of them. *Consequence:* every WIT package and `use …@<ver>`
   reference resets to `@0.1.0` (the true initial release) via one `git-filter-repo`/`jj` fold
   in Phase 0; until that release is cut, a "breaking" change is internal-only (recompile +
   fold), never a wire break — which is what makes the Phase-0 reshape free. Cross-referenced
   by the reframed Phase 0, R1, and decisions 2/5.

- the exact wording + versioning scheme of the documented **opaque-status destructuring
  contract** the host `event` stream commits to (decision 2);
- the precise **manifest key name** (`body_version` vs a version-set field) and the
  supported-set match semantics for the install-time handshake (decision 4).
