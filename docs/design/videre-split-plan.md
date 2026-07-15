# The videre split — next-push execution plan

**Splitting `nullislabs/shepherd` (`dev/m1` @ `ddfb2b9`, design-doc baseline `aed942c`) into three repos: `nexum-runtime` ← `videre` ← `CoW-on-videre`.**

Status: decision-ready blueprint. Extends [`docs/design/venue-platform-architecture.md`](./venue-platform-architecture.md) (`9fca43c:docs/design/venue-platform-architecture.md`) — read §2 (three-layer target), §6 (R1–R8), §7 (migration plan), §8 (the 7+1 decisions) first. This plan does **not** restate the layer model or the red-team; it consumes them and turns them into an ordered set of moves with concrete crate/WIT/file targets.

---

## 1. Executive summary

**End state — three repos, one acyclic edge each:**

- **`nexum-runtime`** — the universal host: runtime engine, generic supervisor, capability model, the `Extension<T>` seam, the module SDK/macro, and the single leaf WIT package `nexum:host`. **Zero** intent/venue/cow knowledge.
- **`videre`** — the venue-neutral intent-settlement + quoting layer: the intent WIT packages, the venue-adapter world + component kind, the venue SDK + `#[venue]` + conformance kit, the pool router (as a host-side `videre-host` crate that plugs into the runtime seam), and the generic keeper *assembler*.
- **`CoW-on-videre`** — the concrete CoW venue adapter (cdylib), the CoW bodies + classification, and the composable-cow *keeper* strategy module — all riding videre's `pool` contract.

**The single biggest sequencing decision:** land the **R6 host↔intent WIT decouple (§8 decision 2)** as move #1, before any crate moves. `wit/nexum-host/types.wit:8` does `use nexum:intent/types@0.1.0.{receipt, intent-status}` — the L1 host world literally imports the L2 intent package. Until that `use` is gone (host `event` carries opaque status bytes), `nexum-runtime` cannot compile without videre's WIT, and **an acyclic split is physically impossible.** Everything else is downstream of this one line.

**Go / no-go:** **GO** on the enabling reshape and refactors — do them *now, in the monorepo*, while nothing is pinned (§8 decision 8 makes every WIT change a free recompile, not a wire break). **NO-GO** on the physical repo cut until three gates hold: (a) R6 landed and `nexum-runtime` proven intent-free; (b) a real `cow-venue` **cdylib** exists and the keeper is ported off the legacy `cow-api` host extension onto `pool`; (c) a genuine **second** venue compiles against `videre-sdk` alone. Cutting repos before (b)/(c) freezes a "generic" L2 contract that today only the `echo-venue` toy exercises and that the live CoW path bypasses entirely (`shepherd-sdk/src/cow/run.rs:138` submits via `CowApiHost`, never `pool`). That is R1 unretired — do not codify it into a repo boundary.

**The long pole is not a file move.** Making `nexum-runtime` venue-agnostic requires generalizing the `Extension<T>` seam (today `LinkerHook` + `NamespaceCaps`) to host a *second component kind* with its own supervised store/actor lifecycle, and lifting `PoolRouter` + the privileged `HostState.pool_router` field out of the core. This has **no ADR or design-doc decision behind it** — it exists only to satisfy the split's acyclicity/zero-knowledge directive. Scope it as real runtime surgery, prove it with `echo-venue` before cow, and make *deleting the `pool_router` field* the forcing-function acceptance test.

---

## 2. Target: the three repos

### 2.1 Layer / dependency diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│  CoW-on-videre   (L3, app-level, may not publish)                     │
│    crates:  cow-venue (cdylib adapter)  · shepherd-sdk (the keeper)   │
│             shepherd-cow-host (LEGACY read path) · shepherd-sdk-test  │
│             shepherd-backtest · shepherd (the concrete bin)           │
│    wit:     shepherd:cow  (legacy host-ext surface; retiring)         │
│    vendors: nexum:host + videre:*                                     │
└───────────────┬─────────────────────────────────────────────────────┘
                │  Rust: git-tag/published dep     WIT: wit-deps(videre:*, nexum:host)
                ▼
┌─────────────────────────────────────────────────────────────────────┐
│  videre          (L2, published)                                      │
│    crates:  videre-sdk · videre-test · videre-macros                  │
│             videre-host (NEW: PoolRouter + adapter supervision,       │
│                          registered via the runtime seam)            │
│    wit:     videre:value-flow ← videre:intent ← videre:adapter        │
│    vendors: nexum:host                                                 │
│    examples: echo-venue · echo-client                                 │
└───────────────┬─────────────────────────────────────────────────────┘
                │  Rust: videre-host depends on nexum-runtime (legal L2→L1 crate edge)
                │  WIT:  videre:adapter `use`s nexum:host/{types,chain,messaging}
                ▼
┌─────────────────────────────────────────────────────────────────────┐
│  nexum-runtime   (L1, published)                                      │
│    crates:  nexum-runtime · nexum-sdk · nexum-sdk-test                │
│             nexum-module-macros · nexum-world (plain lib)             │
│             nexum-launch (lib) + nexum (bare Ext=() engine bin)       │
│    wit:     nexum:host   (LEAF — after R6)                            │
└─────────────────────────────────────────────────────────────────────┘
```

WIT DAG (post-R6): `nexum:host` (leaf) ← `videre:value-flow` ← `videre:intent` ← `videre:adapter` ← `shepherd:cow`. Every edge points down. The only edge that is *up* in Rust — `videre-host → nexum-runtime` — is legal because it is a host-side crate depending on the engine, exactly as `shepherd-cow-host` depends on `nexum-runtime` today.

### 2.2 `nexum-runtime` (L1) — charter, crates, surface

**Charter:** run WASI components under a capability model with a supervised lifecycle, and expose a generic `Extension<T>` seam so *anything* venue/intent/cow-shaped is added from outside. The engine never learns what a venue is.

| Owns | Detail |
|---|---|
| `nexum-runtime` | engine, `RuntimeBuilder`/`bootstrap::run`/`with_extensions`, generic supervisor, capability registry, `host::extension::Extension<T>`, per-interface host linker primitives (`nexum::host::chain/messaging/logging/identity/local-store/remote-store`), the generic **component-kind + host-actor** facility extracted from `AdapterActor` (fuel refuel, trap→error projection, async-mutex serialization, restart/poison-sweep membership — R8) |
| `nexum-sdk`, `nexum-sdk-test` | guest host SDK (`ChainHost`/`Fault`/`Fetch`), **and the world-neutral keeper primitives** `keeper.rs` (`WatchSet`/`Gates`/`Journal`/`Retrier`/`ConditionalSource`) — verified intent-agnostic, pure local-store, they stay here |
| `nexum-world` (new plain lib) | the `world.rs` synthesis + the KNOWN capability table, now **registry-driven** (no baked `pool`/`cow-api` rows) |
| `nexum-module-macros` | `#[module]` only |
| `nexum-launch` + `nexum` bin | generic launcher lib + a bare `Ext=()` engine binary composing from an empty extension list |
| `wit/nexum-host` | the sole L1 WIT package; **leaf after R6** |

**Public surface (the seam it exposes upward):** `RuntimeTypes` + the `Ext` slot + `ExtState`; `Extension<T> = { link, capabilities, component_kind, install_predicate }` (the last two are the generalization); per-interface `add_to_linker` primitives; `CapabilityRegistry`/`NamespaceCaps`; `RuntimeBuilder::with_extensions`. Nothing named `pool`, `venue`, `intent`, or `cow` appears anywhere.

**Zero-leak CI gate (post-refactor, permanent):** `cargo tree -p nexum-runtime` must not reach `videre-*` or any intent/cow crate; `rg 'nexum:intent|value-flow|VenueAdapter|synthesize_venue|nexum:adapter|PoolRouter' crates/nexum-runtime/src` must return empty.

### 2.3 `videre` (L2) — charter, crates, surface

**Charter:** the generic, venue-neutral intent-settlement + quoting abstraction. A venue author codes against videre; videre is CoW-blind. Named *videre* ("to see") — price/liquidity discovery across venues is the quoting half; it names the intent/venue layer without colliding with the rejected watch/observer vocab.

| Owns | Detail |
|---|---|
| `videre-sdk` (← `nexum-venue-sdk`) | `VenueAdapter` trait, `IntentBody` versioned-borsh codec, `IntentClient<P>` over the byte-level pool seam, typed transport wrappers, fault folds; **plus the generic `Keeper::sweep` assembler (§5.3) and the `Sweep` outcome** resolving the dangling `ConditionalSource::Outcome` |
| `videre-test` (← `nexum-venue-test`) | `CodecVectors`/`HeaderGoldens`/`MockTransport` conformance kit — the cargo-test-fails-if-wire-drifts gate |
| `videre-macros` | `#[venue]` (the single blessed `impl VenueAdapter` path, R4) + `#[derive(IntentBody)]` + `synthesize_venue`; depends on `nexum-world` |
| `videre-host` (**new**) | `PoolRouter` + `impls/pool.rs` + adapter supervision + guard seam + the `venue-adapter`/`pool-host` bindgens, registered into `nexum-runtime` via the generalized seam. **Host-side crate, depends on `nexum-runtime` — the split's core enabling work** |
| `wit/videre-{value-flow,intent,adapter}` | the intent contract (renamed from `nexum:*`, §8-dec-8 fold); `videre:adapter` `use`s `nexum:host/{types,chain,messaging}` = the dependency edge onto L1 |
| examples | `echo-venue` + `echo-client` — the canonical venue-neutral demos, so videre ships a working end-to-end example independent of CoW |

**Public surface (the "author a venue" front door):** `#[videre::venue] impl VenueAdapter`, `#[derive(IntentBody)]`, `IntentClient::quote(&body)?.submit()?` typestate, the `videre-test` golden gate, `VenueId` zero-cost newtype + Order-style builder (§5.2/§5.4). Ships with quoting (`videre:intent/quote` + `adapter.quote`) in 0.1.0 — thin, value-flow-typed, EVM-only (§8 decision 5) — because a settlement layer without quoting is only half its thesis.

### 2.4 `CoW-on-videre` (L3) — charter, crates, surface

**Charter:** one concrete CoW venue on videre, plus the composable-cow keeper that drives it. This repo is where all cow protocol knowledge lives; L1 and L2 never compile it.

| Owns | Detail |
|---|---|
| `cow-venue` | grown from today's body-only `[lib]` to a **cdylib** adapter targeting `videre:adapter/venue-adapter`; `#[videre::venue] impl VenueAdapter for CowVenue`; caps `[chain, http]`; owns `CowIntentBody`/`ComposableBody`, the borsh codec, and `classification.toml` (projects orderbook `errorType → venue-error`) |
| `shepherd-sdk` | the **keeper**: `ConditionalSource`/`Verdict`/`Retrier` + `cow::run`, consuming videre's generic `Keeper::sweep`; a strategy event-module importing `videre:intent/pool` |
| `shepherd-cow-host` | the **legacy** `cow-api` host extension = the read path; retires down to a shim as the keeper ports onto `pool` |
| `shepherd-sdk-test`, `shepherd-backtest` | tests + backtest |
| `shepherd` bin | the concrete composition root (today's `nexum-cli` cow-wiring logic) |
| `wit/shepherd-cow` | legacy host-ext surface only; deleted at wire-swap |

**Modules:** ethflow-watcher, twap-monitor, stop-loss, orderbook-mock, and the composable-cow keeper module.

### 2.5 DX walkthrough A — author a venue on videre

```rust
// my-venue/Cargo.toml → [lib] crate-type = ["cdylib"]; deps: videre-sdk
use videre_sdk::{VenueAdapter, IntentBody, prelude::*};

#[derive(IntentBody)]                     // versioned borsh codec + goldens
enum MyBody { V1(MyIntentV1) }

#[videre::venue]                          // the single blessed path (R4)
impl VenueAdapter for MyVenue {
    fn derive_header(&self, body: &[u8]) -> Result<IntentHeader, VenueError> { … }
    fn quote(&self, body: &[u8]) -> Result<Quote, VenueError> { … }   // 0.1.0 surface
    fn submit(&self, body: &[u8]) -> Result<SubmitOutcome, VenueError> { … }
    fn status(&self, id: IntentId) -> Result<IntentStatus, VenueError> { … }
}
```
`module.toml`: `capabilities = ["chain", "http"]` (transport-only — §8 decision 1). Then `cargo test` runs the venue against `videre-test` golden vectors; a wire drift fails the build. No host code, no runtime dep, no cow knowledge.

### 2.6 DX walkthrough B — run a keeper on a videre venue

```rust
// A strategy event-module (targets nexum:host/event-module, imports videre:intent/pool)
struct CowSource { /* polls ComposableCoW.getTradeableOrderWithSignature */ }
impl ConditionalSource<Host> for CowSource {
    fn poll(&self, ctx: &mut Ctx) -> Sweep {        // generic outcome (videre)
        match self.verdict(ctx) {                    // CoW Verdict → Sweep
            Verdict::Post(order) => Sweep::Submit(CowIntentBody::v1(order).encode()),
            Verdict::WaitBlock   => Sweep::WaitBlock,
            …
        }
    }
}
// The runtime boots this module; videre's Keeper::sweep assembles
// WatchSet → Gates → source.poll → Retrier → Journal, and routes
// Sweep::Submit(bytes) through pool.submit(CowVenue::ID, bytes) → cow-venue adapter.
```

---

## 3. The videre abstraction

### 3.1 Naming and the `videre:*` WIT rename

`nexum:value-flow / nexum:intent / nexum:adapter → videre:*`, folded into the §8-decision-8 `@0.1.0` normalization pass. **Not load-bearing for acyclicity — gate nothing on it.** Rationale for folding it in: these packages physically move to the videre repo, so the `nexum:` prefix would misattribute them to the host; and the normalization `git-filter-repo`/`jj` fold is *already pending* (versions are unnormalized — `nexum:host@0.2.0`, `shepherd:cow@0.2.0`), so the marginal cost is one extra `sed` inside a pass that must run anyway. Keep `nexum:host` (L1 brand) and `shepherd:cow` (L3). Caveat: `value-flow` is the hardest-freezing contract — regenerate all `videre-test` goldens under the new namespace and re-assert the byte-identical tip oracle.

### 3.2 The venue SDK + `#[venue]`

`#[videre::venue]` is the single blessed authoring path (§8 decision 6 / R4), emitting `impl VenueAdapter`; `export_venue_adapter!` demotes to the internal codegen it expands to. This kills the two forked Guest impls (`nexum-macros/src/lib.rs:422` raw Guest vs `:426` adapter Guest) that today fork "the one clear arrangement of traits." Pull R4 forward into the refactor phase so videre ships **one** authoring surface at carve time, not two across a repo boundary.

### 3.3 Quoting + the install-time schema handshake

- **Quoting (new in 0.1.0):** `videre:intent/quote.wit` + `adapter.quote` + an `IntentClient.quote(&body)?.submit()?` typestate. Keep the quote record thin and value-flow-typed until a second real venue exercises it (guards against enshrining a CoW/EVM-shaped quote, R1). EVM-only (§8 decision 5). Free now (§8 decision 8), a wire break later.
- **Install-time `body_version` handshake (§8 decision 4 / R7):** the adapter/module manifest declares a supported `body_version` set; the supervisor asserts agreement at install. The **schema is videre's** but `Supervisor::install` is in `nexum-runtime`, which must stay venue-agnostic — so `videre-host` supplies the *install predicate* through the generalized seam. No WIT gate.

### 3.4 How videre consumes `nexum-runtime` host worlds

Two distinct edges, both legal and both downward-in-contract:

1. **WIT:** `videre:adapter/venue-adapter` `use`s `nexum:host/{types,chain,messaging}` and imports host `chain`+`messaging`+`wasi:http`. This is videre's WIT dependency on L1 — correct direction, acyclic *after R6*.
2. **Rust:** `videre-host` depends on `nexum-runtime` (the crate) to reach `wasmtime::Store`/`HostState<T>`/`RuntimeTypes` and to register through `with_extensions`. This resolves the L6 objection ("the router is irreducible host code so it must stay in L1"): a host-side crate that *depends on* the engine is a legal L2→L1 crate edge, exactly like `shepherd-cow-host` today. The router's *contract* (WIT + guest SDK) is L2; its *implementation* is a host-side L2 crate; neither forces `nexum:intent` back into the L1 core.

**Reconciliation with the 7 decisions:** dec-1 (transport-only adapters) = videre's adapter linker exposes only generic host interfaces, never a cow interface; dec-2 (opaque status) = the acyclicity gate that lets `nexum:host` version independently of videre; dec-3 (advisory guard) = videre advertises `derive→guard→submit` whose teeth are deferred, and videre docs **must** say the guard is advisory-only for M1 (`AllowAllGuard`); dec-4 = the install predicate above; dec-5 = EVM-only quoting/bodies; dec-6 = `#[videre::venue]`; dec-8 = the free rename+quote window that closes at the cut.

---

## 4. CoW-on-videre + the composable-cow keeper

### 4.1 The R2 category error, resolved explicitly

There is no "cow-protocol WASI" world, and **a component targets exactly one world.** So "composable-cow as a keeper on videre" is **not** a separate component or world. Cow enters the system as exactly three artifacts on **two** component worlds plus one host extension — never as world-composition:

| Piece | Kind | World it targets | How cow enters |
|---|---|---|---|
| **(a) `cow-venue` adapter** | cdylib component | `videre:adapter/venue-adapter` | exports `videre:intent/adapter`; imports `chain`+`wasi:http`; decodes `CowIntentBody`, POSTs `OrderCreation`, projects orderbook `errorType → venue-error` |
| **(b) composable-cow keeper** | strategy component | `nexum:host/event-module` | imports `videre:intent/pool`; runs the ADR-0013 poll loop; drives the adapter with **opaque** bodies |
| **(c) `shepherd-cow-host`** | host extension | (linker seam, not a world) | the **legacy** read path; retires to a shim |

Composable orders are just a `ComposableBody` variant of `CowIntentBody` (R2) — no separate "composable" component. `Verdict::Post` is half-populated today (`LegacyRevertAdapter` never produces it, `composable.rs`) and `NeedsInput` is dead until the fork — so the keeper-on-videre path is *architecturally* coherent but *not yet instantiable*; naming a repo around it is ahead of the code, which is exactly why the physical cut waits on gate (b).

### 4.2 How they compose via `pool`

Build-time: the adapter depends on `videre-sdk` + `cow-venue::body`; the keeper depends on `nexum-sdk` + `cow-venue::body` (to encode `CowIntentBody`) + videre's `pool` bindings. **Neither imports the other's world.** Runtime: the supervisor boots both as guests; `videre-host`'s `PoolRouter` wires `pool.submit("cow", bytes) → resolve venue-id → derive-header → guard → cow-venue.submit`. The port that flips `run.rs:138` `host.submit_order(...)` (legacy `CowApiHost`) to `pool.submit(CowVenue::ID, cow_body_bytes)` is the concrete embodiment of gap #2 — it retires the `CowApiHost` trait and (eventually) the `shepherd-cow-host` extension.

### 4.3 The clean re-split of responsibility

- **Keeper** = poll ComposableCoW + decide + emit an opaque `CowIntentBody` + classify the coarse `venue-error` via `Retrier`.
- **Adapter** = `CowIntentBody → OrderCreation` JSON + `wasi:http` POST + orderbook-`errorType → venue-error`. `build_order_creation`/`order_uid_hex`/`gpv2_to_order_data` (today in `shepherd-sdk/src/cow/{run,order}.rs`) move **into** the adapter's `submit`. `classification.toml` moves into the adapter.
- **Consequence (R1 reshape, do in Phase 0):** the coarse `venue-error` must gain `rate-limited{retry-after-ms}` + `denied` so the retry hint survives the collapse (`faults.rs` already folds `unavailable|rate-limited|timeout`).
- **Idempotency seam (settle before the port):** `run.rs` today derives the client-side UID and checks the `submitted:` Journal *before* the network call. Once assembly moves to the adapter, the keeper can't derive the UID pre-submit → double-post risk. Fix: have the adapter's `derive-header` (already called pre-submit by the router) return a deterministic intent-id the keeper journals, or make `SubmitOutcome` carry `receipt = UID`.

### 4.4 ADR-0013 gate

The **Rust seam** re-split (keeper ↔ adapter) has zero contract dependency and lands **now**, independent of the fork. Only the **poll wire-swap** — deleting `composable.rs`/`LegacyRevertAdapter` — is hard-blocked until the fork's `deployments/networks.json` is non-empty on a shepherd target chain. **Do not couple them**, or the whole CoW-on-videre split freezes behind a third-party deployment clock.

---

## 5. Sequencing — the next push

Reshape-then-extract, decisively. Do everything free while one repo; cut repos only at the end, gated.

### Phase 0 — the free WIT fold (one repo, one oracle-validated pass)

The design doc's Phase-0 cluster **plus two split-enablers**, all in one `git-filter-repo`/`jj` fold with the byte-identical tip oracle and regenerated goldens:

| Move | Target | Why here |
|---|---|---|
| **R6 host↔intent decouple** (§8 dec-2) | drop `wit/nexum-host/types.wit:8` `use nexum:intent/types.{receipt,intent-status}`; host `event` → opaque status bytes with a **versioned destructuring contract** (specify the wording — still open in §8) | **THE master gate.** Until this lands, L1 can't compile without L2's WIT. |
| version normalize | all packages → `@0.1.0` (§8 dec-8) | closes the free window; must precede the cut |
| `videre:*` rename | `nexum:{intent,value-flow,adapter}` → `videre:*` | free now, a fold later; gate nothing on it |
| `venue-error` reshape (R1) | add `rate-limited{retry-after-ms}` + `denied` | so classification survives the adapter split |
| the rest of doc Phase-0 | `valid-until-ms`, named ERC records, codec version discriminator, doc caveats, migration-cruft deletion | free contract reshape |

**Exit test:** `cargo tree -p nexum-runtime` no longer reaches any intent crate; the WIT DAG builds acyclically in one repo.

### Phase 1 — finish M1 cleanly (one repo, no fold)

Land the doc's Phase-1 Rust amends (#249/#250/#251/#296), the Wave-1 #334 verdict-seam fixes, the R7 install-time `body_version` handshake (manifest + supervisor, no WIT gate), and the approved cars — to a single **green linear** `dev/m1` tip. Do not begin the carve until this tip exists (carving mid-train triples the fold surgery across three repos).

### Phase S1 — the generalization (one repo, the long pole, NOT free)

The only non-free work in the plan; validated by `echo-venue` boot tests as the oracle, **before** any cow involvement.

1. **Generalize the seam.** Grow `host/extension.rs` `Extension<T>` from imports-only to register a **component kind** (its store/actor/install lifecycle) + a **host actor** + an install predicate. Extract the generic supervised-component primitive from `AdapterActor` (fuel refuel, trap projection, async-mutex serialization, restart/poison-sweep per R8) into `nexum-runtime`.
2. **Move the router to `videre-host`.** Lift `host/pool_router.rs`, `host/impls/pool.rs`, the `venue_adapter`/`pool_host` bindgens (`bindings.rs:21-24` etc.), `build_adapter_linker`, the adapter path of `synthesize_venue`, and `INTENT_/ADAPTER_` namespaces into the new `videre-host` crate. **Forcing function: delete the `HostState.pool_router` field (`state.rs:54`) and carry the router in a composite `Ext` lattice** — if that field is gone and echo still boots, L1 is intent-free.
3. **Split the macros.** Factor `world.rs` synthesis + a **registry-driven** KNOWN table (delete the baked `pool` row at `world.rs:74` **and** the `cow-api → shepherd:cow` row at `:80` — an L1→L3 name leak) into `nexum-world` (plain lib, L1). `#[module]` → `nexum-module-macros` (L1); `#[venue]`+`IntentBody`+`synthesize_venue` → `videre-macros` (L2). Rewrite `find_wit_root` (`lib.rs:512`) from workspace-ancestor-walk to crate-local `wit/`+`wit/deps` resolution. Pull R4 forward (single blessed `#[venue]`).
4. **Split the CLI.** Generic launch + bare `nexum` bin stay in L1; the cow composition root (`launch.rs:16,47` `shepherd_cow_host::extension` / `with_extensions`) becomes the `shepherd` bin destined for L3 — fixing today's backwards `nexum-cli → shepherd-cow-host` dep.

### Phase S2 — flip WIT resolution, then carve (still one repo → three)

- **S2a (one repo):** introduce `wit-deps` (`deps.toml`) per prospective repo; flip every `bindgen!` path list and the macro WIT-root off `../../wit/*` to crate-local `wit/` + `wit/deps/`. Prove the acyclic graph still builds. Source cross-package WIT from git tags initially (no registry exists); check in resolved lockfiles.
- **S2b (three repos):** three `git-filter-repo --path` extractions preserving history — reuse the keeper-rename template from memory (range-limited `git-filter-repo` + byte-identical tip oracle + `jj`/`mergiraf`), per-repo. Wire cross-repo Rust via git-tag pins, cross-repo WIT via `wit-deps` git tags, held together by a **transitional umbrella superproject** with path-deps during stabilization. Converge to published crates.io (L1/L2) + `wkg`/OCI (`ghcr.io/nullislabs`) once stable; L3 stays app-level.

### Phase S3 — de-risk R1 (videre repo)

**Acceptance gate for calling the split done:** build the first real **non-cow** venue (rfq or amm-router) against `videre-sdk` alone. `echo-venue` is a toy and cannot prove venue-neutrality; the live CoW path bypasses videre, so without a second real venue the split ships an unproven L2.

### Do-now / do-during-split / defer triage

| Do NOW (Phase 0/1, monorepo, free) | Do DURING split (S1/S2) | Defer (M7 / design-partner / fork-gated) |
|---|---|---|
| R6 host↔intent decouple | seam generalization + delete `pool_router` field | `cow-venue` cdylib fully replacing the legacy path |
| `@0.1.0` normalize + `videre:*` rename | `videre-host` extraction | keeper clean-break off `CowApiHost` (needs idempotency seam) |
| `venue-error` reshape (R1) | macro split + registry-driven KNOWN table | poll wire-swap / delete `composable.rs` (ADR-0013, fork-gated) |
| quoting stub (`videre:intent/quote`) | CLI split → `shepherd` bin | de-EVM (dec-5 EVM-only for 0.1) |
| R7 install-handshake shape | `wit-deps` flip + three `git-filter-repo` carves | real egress guard w/ teeth (dec-3 advisory for M1) |
| finish M1 to a green linear tip | second-venue acceptance (S3) | `Keeper<Source,Venue>` materialiser (M7) |

---

## 6. Red-team & open decisions

### 6.1 Ranked risks + mitigations

1. **Seam generalization is the long pole and has no design-doc mandate.** If `Extension<T>` can't be grown to host a component kind, the router stays in L1, `nexum:intent` stays bound in the core, and the zero-knowledge invariant fails → no acyclic split. **Mitigate:** prove pool-router-as-extension with `echo-venue` only, in-monorepo; make *deleting `HostState.pool_router`* the acceptance test; do it before any cow work and before any carve.
2. **R6 not landed + its contract still open.** `types.wit:8` is live; §8 still lists "the exact wording + versioning of the opaque-status destructuring contract" as open. An under-specified status contract blocks Phase 0. **Mitigate:** specify the versioned discriminator first, land dec-2, CI-gate `nexum-runtime` intent-free.
3. **Freezing videre on a toy (R1 unretired).** Sole real consumer is `echo-venue`; `cow-venue` has no cdylib; the live keeper bypasses `pool`; dec-5 scopes it EVM-only → high odds it's CoW/EVM-mis-shaped. **Mitigate:** gate the *repo cut* (not the in-repo refactor) on a real second venue + a real cow cdylib + the keeper-on-`pool` port.
4. **Cross-repo WIT versioning has no teeth during transition.** `wit-deps` git-tag sourcing gives no semver enforcement; a mispinned tag silently drifts the contract until a bindgen error. And the split *creates* the external consumers that make a host WIT change a real break — the free-reshape window closes at the cut. **Mitigate:** complete all Phase-0 reshapes before S2b; pin exact tags; check in resolved lockfiles; adopt independent per-package semver (`nexum:host@0.1.x`, `videre:*@0.1.x`, `shepherd:cow`) with caret ranges.
5. **Forced cycles if steps are mis-ordered.** Three: WIT (`nexum:host→nexum:intent`, broken by R6), Rust crate (`nexum-cli→shepherd-cow-host`, broken by moving the bin to L3), bindgen (core binds `nexum:intent`, broken only *after* R6 by the router extraction). **Mitigate:** the phase order *is* the mitigation — R6 → seam+router → CLI move → carve. Verify each cycle broken and green in one repo before S2b.
6. **Idempotency regression on the keeper port** (double orderbook posts). **Mitigate:** settle the deterministic intent-id / `receipt=UID` seam before moving `OrderCreation`/UID assembly into the adapter.
7. **DX regression from three repos.** Loses the single hoisted dep table (built specifically to stop cowprotocol alpha-vs-alpha.3 drift), the shared `Cargo.lock`, and atomic folds. **Mitigate:** transitional umbrella superproject with path-deps through S1–S3; published-and-pinned WIT packages + a CI dep/WIT-sync check; never permanent cross-repo path-deps.
8. **Naming.** `videre:*` forks the wire brand and stacks a rename fold. **Mitigate:** fold the rename into the pass that must run anyway; gate nothing on it; keep `nexum:host` + `shepherd:cow`.

### 6.2 Decisions the team must make before starting

| # | Decision | Recommendation |
|---|---|---|
| D1 | Router placement | **Extract to `videre-host` (L2 host-side crate) via the generalized seam.** Not a relabel — requires seam generalization. Alternative (keep in L1) violates zero-knowledge. |
| D2 | Seam generalization vs special-case | **Generalize** `Extension<T>` to register a component kind + host actor + install predicate. The special-case path leaves intent shape in L1. |
| D3 | Macro split shape | **`world.rs` → `nexum-world` (L1 plain lib); `nexum-module-macros` (L1) + `videre-macros` (L2)**, KNOWN table registry-driven (de-hardcode `pool` **and** `cow-api`). |
| D4 | Keeper location | **Primitives stay in `nexum-sdk`** (verified world-neutral); only the not-yet-written `Keeper::sweep` assembler → `videre-sdk`; CoW `ConditionalSource`/`Verdict` → L3. (Corrects the "whole keeper to videre" framing.) |
| D5 | `videre:*` rename timing | **Fold into the dec-8 normalization pass; gate nothing on it.** |
| D6 | Quoting in 0.1.0 | **Ship it** (thin, value-flow-typed, EVM-only) — it's half the thesis and free now. |
| D7 | `nexum-cli` placement | **Split:** generic `nexum` + launcher lib stay L1; the cow composition root → `shepherd` bin in L3. (Corrects the starting map.) |
| D8 | Cut timing | **Gate the physical repo cut on: R6 landed + real `cow-venue` cdylib + keeper-on-`pool` + a real second venue.** Refactor now, cut later. |
| D9 | Cross-repo dep medium | **Git-tag pins (Rust) + `wit-deps` git tags (WIT) first; crates.io + `wkg`/OCI once stable.** |
| D10 | Transitional workspace | **Yes** — one workspace / path-deps through S1–S2a, umbrella superproject S2b–S3; split physically only at S2b. |

---

*Grounded against `dev/m1` (`ddfb2b9`) and `9fca43c:docs/design/venue-platform-architecture.md`. Every load-bearing path/line was verified in-tree.*

---

## 7. Pinned design — the platform seam, videre WIT, and CoW-on-videre

_Decided interactively 2026-07-15; this refines §5's sequencing at the end._

### 7.1 Composition model — PLATFORM (decided)

Two models were on the table: a **host-routed platform** (venues install as components; the host dispatches keeper→venue) vs. **`wac` static composition** (compose keeper + adapter into one binary, no host router). **Chosen: the platform** — because a **shared orderbook connection + rate-limit** to CoW genuinely matters in production, and only a shared, installed venue gives one quota / one guard seam / one connection. The cost (a host-side venue runtime) is accepted, offset by **macro-driven, reth/alloy-grade DX** on top.

### 7.2 The runtime seam — finish the `Extension` seam with worker/provider roles

The runtime already has `Extension<T> { link, capabilities }` (host/extension.rs) — it adds host interfaces a module imports (that's how `cow-api` works). The "long pole" is that it does only half the job: it can't register a **component kind** (`ModuleKind` is a hardcoded `EventModule | VenueAdapter` enum) or a **host service** (the `PoolRouter` is a privileged field on the supervisor). Both are welded into `nexum-runtime`.

**Fix:** the runtime knows only two generic **roles** — **worker** (the host pushes events at it; modules, keepers) and **provider** (the host holds it behind a serialized actor; others call it; venue adapters). An `Extension` grows to contribute all four things:

```rust
pub trait Extension<T: RuntimeTypes>: Send + Sync + 'static {
    fn namespace(&self) -> &'static str;                              // "videre"
    fn capabilities(&self) -> NamespaceCaps;                          // (kept)
    fn link(&self, l: &mut Linker<HostState<T>>) -> Result<()>;       // worker-imported ifaces (kept)
    fn service(&self)  -> Option<Arc<dyn HostService>>     { None }   // NEW: the ex-PoolRouter, extension-owned
    fn provider(&self) -> Option<Box<dyn ProviderKind<T>>> { None }   // NEW: a provider kind this ext installs
}
pub trait HostService: Any + Send + Sync + 'static {}                 // type-erased onto HostState.services[ns]
#[async_trait]
pub trait ProviderKind<T: RuntimeTypes>: Send + Sync + 'static {
    fn kind(&self) -> &'static str;                                   // "venue-adapter"
    fn link(&self, l: &mut Linker<HostState<T>>) -> Result<()>;       // provider-imported ifaces (chain, http)
    async fn install(&self, c: &Component, s: Store<HostState<T>>, svc: &Arc<dyn HostService>) -> Result<()>;
}
```

`videre` becomes **one extension** — `builder.with_extension(videre::platform())` — that registers the venue-adapter provider-kind + the `VenueRegistry` service (the renamed, un-privileged `PoolRouter`) + the `videre:venue/client` interface. The supervisor's `match kind { VenueAdapter => … }` collapses to a generic kind loop. **After this, `nexum-runtime` compiles with zero venue/intent/cow symbols**, and a second platform is just another `impl Extension`.

| Concept | Today (welded in) | After (plugged in) |
|---|---|---|
| host interfaces | `Extension.link` | kept |
| host service | `supervisor.pool_router` field | `Extension::service` → `HostState.services[ns]` |
| component kind | `enum ModuleKind` + `match` | `Extension::provider` → `ProviderKind` |
| the router | `PoolRouter` | `VenueRegistry` (videre-owned) |
| the actor | `AdapterActor<T>` | `VenueActor` (videre) |
| the guard | `GuardPolicy`/`AllowAll` | `EgressGuard` (videre) |

### 7.3 MSRV 1.94 / async

Native `async fn` in traits is stable but **not `dyn`-compatible** on 1.94. So: **native AFIT** for the hot, static-dispatch guest traits (`Venue`, `Keeper`, `VenueClient` — macros emit concrete impls, zero boxing); **`#[async_trait]`** only for the one `dyn`, cold-path boot trait `ProviderKind::install` (per-provider boxing at boot is free); **neither** for `HostService`/`EgressGuard` (kept sync, so `dyn`-compatible as-is). Drop the `async_trait` when `async_fn_in_dyn_trait` stabilizes.

### 7.4 The pinned `videre:*` WIT surface

Renamed off `nexum:intent`; `quote` in; the maker-side "offer" deferred to **#355**; EVM-only in 0.1; install-time schema handshake in.

```wit
package videre:types@0.1.0;
interface types {
  use videre:value-flow/types.{asset-amount};
  record intent-header { gives: asset-amount, wants: asset-amount, settlement: settlement, authorisation: auth-scheme }
  variant auth-scheme { eip1271, eip712 }               // non-EVM → 0.2
  record settlement   { chain: u64 }                    // EVM-only in 0.1
  type receipt = list<u8>;
  variant submit-outcome { accepted(receipt), requires-signing(unsigned-tx) }
  record unsigned-tx { chain: u64, to: list<u8>, value: list<u8>, data: list<u8> }
  enum intent-status { pending, open, fulfilled, cancelled, expired }
  variant venue-error { unknown-venue, invalid-body(string), unsupported, denied(string),
                        rate-limited(rate-limit), unavailable(string), timeout }
  record rate-limit { retry-after-ms: option<u64> }
  record quote { gives: asset-amount, wants: asset-amount, fee: asset-amount, valid-until-ms: u64 }  // firm/RFQ → #355
}

package videre:venue@0.1.0;
interface client {   // WORKER (keeper) face
  use videre:types/types.{quote, receipt, intent-status, submit-outcome, venue-error};
  quote:  func(venue: string, body: list<u8>) -> result<quote, venue-error>;
  submit: func(venue: string, body: list<u8>) -> result<submit-outcome, venue-error>;
  status: func(venue: string, receipt: receipt) -> result<intent-status, venue-error>;
  cancel: func(venue: string, receipt: receipt) -> result<_, venue-error>;
}
interface adapter {  // PROVIDER (venue) face — mirror; one adapter = one venue
  use videre:types/types.{intent-header, quote, receipt, intent-status, submit-outcome, venue-error};
  body-versions: func() -> list<u32>;                   // install-time schema handshake (R7)
  derive-header: func(body: list<u8>) -> result<intent-header, venue-error>;
  quote:  func(body: list<u8>) -> result<quote, venue-error>;
  submit: func(body: list<u8>) -> result<submit-outcome, venue-error>;
  status: func(receipt: receipt) -> result<intent-status, venue-error>;
  cancel: func(receipt: receipt) -> result<_, venue-error>;
}

package videre:value-flow@0.1.0;
interface types {
  record asset-amount { asset: asset, amount: u256 }    // named records (fixes the old anonymous tuples)
  variant asset { native, erc20(erc20) }                // erc721/1155/offchain/service → additive later
  record erc20 { token: address }
  type address = list<u8>;   // 20 bytes
  type u256    = list<u8>;   // 32 bytes LE
}
```

### 7.5 DX — the macros (reth/alloy-grade)

- `#[nexum::module]` — a worker that reacts to host events (exists).
- `#[videre::venue]` — a provider: write `impl Venue for CowVenue { … }`, the macro emits the `videre:venue/adapter` export + manifest `kind` (the single blessed authoring path, decision Q6).
- `#[videre::keeper]` — a worker that drives a venue: write logic against a typed `VenueClient<V>` (wraps `videre:venue/client`, alloy-style, typed not `list<u8>`); the macro wires the event subs.
- Newtypes throughout: `VenueId` (was `venue: string`), `Receipt` — no stringly typing.

### 7.6 CoW-on-videre end-to-end + the venue↔keeper boundary

**THE RULE (load-bearing): the cow venue is *only* the CoW orderbook.** It `submit`/`quote`/`status`/`cancel`s an `OrderBody` on `api.cow.fi` and maps orderbook errors to `venue-error`. That is its entire charter — it has never heard of ComposableCoW, `getTradeableOrderWithSignature`, revert selectors, TWAP, or EthFlow.

**All composable-cow specifics live in the composable-cow keeper and leak nowhere else:** `ComposableBody`/`ConditionalOrderParams` (keeper-internal, used to *poll*, never submitted), the `COMPOSABLE_COW` address + `ConditionalOrderCreated` topic-0, the `getTradeableOrderWithSignature` call/decode, and the revert-selector decoding + `LegacyRevertAdapter` + `Verdict` seam (ADR-0013). The keeper's job: *watch the conditional orders, poll them, produce a plain `OrderBody`* → `cow.submit(order)`.

```
composable-cow keeper                              cow VENUE (orderbook only)
──────────────────────                             ──────────────────────────
watch ConditionalOrderCreated                      submit(OrderBody) → /api/v1/orders
poll getTradeableOrderWithSignature                quote / status / cancel
revert→Verdict (LegacyRevertAdapter, ADR-0013)     classify orderbook errors → venue-error
  └─► produces OrderBody ──cow.submit(order)──────► (no idea composable-cow exists)

ethflow keeper                                     (same shared venue)
watch EthFlow.OrderPlacement; EthFlow consts here
compute UID ──────────────cow.status(uid)─────────► observe/verify path
```

**ethflow falls out for free** as a second keeper on the same venue — it doesn't `submit`, it `status`es a computed UID to verify the orderbook indexed the on-chain EthFlow order. Same venue, different verb.

**The cleave (real refactor):** today's `crates/cow-venue` mixes both — it has `OrderBody` *and* `ComposableBody`/`composable.rs`. Split it into (a) the venue (orderbook + `OrderBody` + classification; the venue body is `OrderBody`-only, drop the `Composable` variant) and (b) the composable-cow keeper. **CI gate:** the venue crate has zero `Composable*` / `getTradeableOrder` / revert-selector symbols. Consequence: anyone can write a new CoW keeper (limit-order, milkman-style) that produces `OrderBody`s without importing composable-cow's machinery.

**CoW-on-videre repo owns:** the `cow` adapter cdylib (venue), cow bodies (`OrderBody`) + classification, the composable-cow keeper (bodies + poll + Verdict + revert), the ethflow keeper, and the `shepherd-cow` event-ABI WITs. Depends only on `videre` + `nexum-runtime` host worlds — no cycle. `CowApiHost`/`cow-api`/`cow-ext` retire (the design-doc's "biggest lever").

## 8. Pinned sequencing (refines §5)

**Phase 0 — reshape in the monorepo (free; nothing is pinned, decision-8):**
- **P0.1 — R6 decouple (MASTER GATE, move #1):** `wit/nexum-host/types.wit` stops `use nexum:intent/types.{receipt,intent-status}`; host emits **opaque status bytes** + a documented destructuring contract. Until this lands, an acyclic split is physically impossible.
- **P0.2 — `videre:*` rename:** `nexum:intent`→`videre:venue` (`client`+`adapter`) + `videre:types`; `nexum:value-flow`→`videre:value-flow`; fold the readability renames (`pool`→`venue/client`, `PoolRouter`→`VenueRegistry`, `AdapterActor`→`VenueActor`, `GuardPolicy`→`EgressGuard`, `venue: string`→`VenueId`).
- **P0.3 — normalize all WIT to a single `@0.1.0`;** delete the `0.1-to-0.2` cruft; value-flow named records.
- **P0.4 — add `quote`** to `videre:venue` (client + adapter) + the `body-versions` handshake.

**Phase S1 — the seam (monorepo; the real long pole):**
- **S1.1** grow `Extension<T>` → `{link, capabilities, service, provider}`; add `ProviderKind` + `HostService`; make `HostState.services` a typed map.
- **S1.2** move `PoolRouter`→`VenueRegistry` as an extension-owned service; delete the privileged `supervisor.pool_router` field; collapse the `match kind` to the generic role loop.
- **S1.3** `videre::platform()` registers the provider-kind + service + `videre:venue/client`. **CI gate:** `nexum-runtime` has zero venue/intent/cow symbols.
- **S1.4** prove it with `echo-venue`: a venue installs + a worker submits through the generic seam.

**Phase S1b — CoW on the generic seam (monorepo):**
- **S1b.1** cleave `cow-venue` → venue (orderbook + `OrderBody` + classification) vs composable-cow keeper. CI gate on the venue crate (no `Composable*`/`getTradeableOrder`/revert).
- **S1b.2** build the `cow` adapter cdylib (`#[venue] impl Venue` over `wasi:http`); retire `CowApiHost`/`cow-api`/`cow-ext` (the "biggest lever").
- **S1b.3** add `#[videre::keeper]` + typed `VenueClient`; port composable-cow + ethflow onto `videre:venue/client`.
- **S1b.4** green on `dev/m1`.

**Phase S2 — the repo cut (gated):**
- **Gate:** (a) `nexum-runtime` venue-agnostic (S1), (b) CoW on the generic seam with a real adapter (S1b), (c) a **genuine second-protocol venue** compiles against `videre-sdk` alone (de-risk R1 — not just a second cow keeper).
- Transitional cargo workspace with path deps in the three groupings → verify build → three history-preserving `git-filter-repo` carves (`nexum-runtime` / `videre` / `CoW-on-videre`); flip WIT path-deps → registry/wit-deps.

**Phase S3 — second-venue acceptance:** the real second-protocol venue merged; videre proven venue-neutral.

**Deferred:** maker-side "offer" / provide-liquidity (**#355**); RFQ firm-quote (additive on `quote`); the real egress guard (egress epic); `Materialiser<Source,Venue>` (M7).
