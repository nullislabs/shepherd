# SDK Design: The Two-Persona SDK

This document describes the guest-side SDK crates. There are two personas, and both are shipped: the **module author**, served by `nexum-sdk`, and the **venue persona**, served by `videre-sdk` - which covers both sides of a venue: the adapter author who speaks one venue's protocol, and the keeper author who drives venues through the typed client.

For the architectural decision behind the host-trait seam both personas build on, see [ADR-0009](adr/0009-host-trait-surface.md). For the rustdoc-level API reference, see [`sdk.md`](sdk.md) and the rustdoc under `nexum/crates/nexum-sdk/` and `videre/crates/videre-sdk/`.

## The two personas

1. **Module author.** Writes an automation module against
`nexum:host/event-module`: react to blocks, chain logs, ticks, or messages; read and write local state. Served by `nexum-sdk` plus the `#[nexum_sdk::module]` attribute macro (from `nexum-module-macros`).

2. **Venue author.** Writes the component that exposes a trading venue
(CoW Protocol, a DEX, a lending market, ...) to modules through the `videre:venue` intent surface, so a module author never sees the venue's wire format. Served by `videre-sdk`: the `VenueAdapter` trait under `#[videre_sdk::venue]`, the `IntentBody` derive, and the `videre-test` conformance kit.

A keeper - a module that drives venues - sits between the two: it is a module by world, but it authors with `#[videre_sdk::keeper]` and calls venues through the typed `VenueClient`. The domain itself (CoW, a DEX) lives in the venue adapter, never in the host: see [doc 08](08-platform-generalisation.md#layer-3-domain-extensions-venue-adapters).

Each persona has its own proc-macro crate (`nexum-module-macros`, `videre-macros`), reached through the SDK re-exports. Both share the same host-trait philosophy: guest code is written against small Rust traits that mirror the WIT interfaces one-for-one, so module logic can be unit-tested against an in-memory mock without a `wasm32-wasip2` toolchain or a running wasmtime instance.

## Crate layout

```
nexum-sdk/                     # universal module SDK (host-neutral, domain-free)
└── src/
    ├── lib.rs                 # crate docs, `pub use nexum_module_macros::module`
    ├── prelude.rs             # alloy primitive re-exports (Address, B256, Bytes, U256, keccak256)
    ├── host.rs                # ChainHost / LocalStoreHost / LoggingHost + supertrait Host; Fault, ChainError, RpcError
    ├── wit_bindgen_macro.rs   # bind_host_via_wit_bindgen! - generates WitBindgenHost + converters
    ├── keeper.rs              # WatchSet, Gates, Journal, Poller, Retrier
    ├── chain/                 # eth_call_params, parse_eth_call_result, chainlink AggregatorV3 reader
    ├── events.rs              # native alloy Log assembly from the wire ChainLog record
    ├── config.rs              # (key, value) config-table lookups, decimal scaling
    ├── address.rs             # EVM address parsing with typed errors
    ├── http.rs                # Fetch trait seam, WasiFetch, FetchError (wasi:http)
    └── tracing.rs             # guest tracing facade + panic hook over a LogSink seam

nexum-module-macros/           # #[module] attribute macro (proc-macro)

videre-sdk/                    # venue SDK: both venue sides
└── src/
    ├── lib.rs                 # crate docs, macro re-exports (venue, keeper, IntentBody)
    ├── adapter.rs             # VenueAdapter trait (init + the five intent functions)
    ├── body.rs                # IntentBody trait + BodyError (versioned borsh codec)
    ├── client.rs              # Venue, VenueId, VenueClient, VenueTransport, HostVenues; poll_once completes async handlers on the sync guest boundary
    ├── keeper.rs              # Keeper::run - the generic run assembler; Outcome, RunReport
    ├── transport.rs           # HostChain, HostMessaging, http, BoundedFetch
    ├── faults.rs              # VenueFault + conversions across wire fault / SDK fault / VenueError
    ├── event.rs               # intent-status event decoding for on_intent_status handlers
    ├── value_flow.rs          # value-flow asset helpers (erc20 amounts)
    └── bindings.rs            # the shared import-only bindgen the macros remap onto

videre-macros/                 # #[venue], #[keeper], derive(IntentBody) (proc-macro)

videre-test/                   # venue conformance kit
└── src/                       # CodecVectors, HeaderGoldens, MockTransport, reference venue

cow-venue/                     # the CoW venue, as feature slices
composable-cow/                # ComposableCoW keeper machinery (body, poll seam, run)
```

`nexum-sdk` is host-neutral and domain-free: any module targeting the runtime pulls helpers and canonical primitive types from it regardless of which world it exports. `videre-sdk` layers the venue platform's guest surface on top. Domain crates (`cow-venue`, `composable-cow`) depend on the SDKs; nothing is re-exported between layers.

Companion mock crates: `nexum-sdk-test` (in-memory `MockHost` over `ChainHost` / `LocalStoreHost` / `LoggingHost`) for modules and keepers, and `videre-test` for adapters. See [Testing](#testing-nexum-sdk-test-and-videre-test) below.

## Module-author persona: `nexum-sdk`

### The host-trait seam

`nexum-sdk` never calls `wit_bindgen`-generated functions directly. Instead `nexum_sdk::host` exposes small traits that mirror the WIT interfaces:

```rust
pub trait ChainHost {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, ChainError>;
}
pub trait LocalStoreHost {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, Fault>;
    fn set(&self, key: &str, value: &[u8]) -> Result<(), Fault>;
    fn delete(&self, key: &str) -> Result<(), Fault>;
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, Fault>;
}
pub trait LoggingHost {
    fn log(&self, level: Level, message: &str);
}
pub trait Host: ChainHost + LocalStoreHost + LoggingHost {}
impl<T: ChainHost + LocalStoreHost + LoggingHost> Host for T {}
```

Module code takes `&impl Host` (or a narrower `<H: ChainHost + LocalStoreHost>` bound when it only needs part of the surface) so tests inject `nexum_sdk_test::MockHost` while the compiled module injects the wit-bindgen-backed adapter. See [ADR-0009](adr/0009-host-trait-surface.md) for the full rationale and [ADR-0011](adr/0011-per-interface-typed-errors.md) for the typed error model.

### The wit-bindgen adapter: `bind_host_via_wit_bindgen!`

Every module keeps its own `wit_bindgen::generate!` call (the macro emits types into the calling crate; re-exporting wit-bindgen output from a library crate would duplicate symbols and break the component-export contract). What the SDK removes is the ~80 lines of mechanical glue that used to sit next to it: the `nexum_sdk::bind_host_via_wit_bindgen!()` declarative macro emits a `WitBindgenHost` struct, the trait impls over the generated import shims, the `Fault` / `ChainError` converters in both directions, a `Level` converter, a `From<ChainLog> for nexum_sdk::events::Log` impl, and an `install_tracing()` helper that routes `tracing::info!(...)` through the bound host logging call. The adapter is capability-selected: the zero-argument form emits the full set for blanket-world modules, and the `caps: [chain, logging]` form (what `#[nexum_sdk::module]` generates from the manifest) emits only the pieces whose imports the module's world carries.

### The `#[nexum_sdk::module]` macro

`nexum-module-macros` ships one attribute macro, re-exported as `nexum_sdk::module`. Apply it to an inherent `impl` block whose methods are named event handlers - `init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message` - and the macro reads the crate's `module.toml`, synthesizes the per-module world from its `[capabilities]`, and generates the `wit_bindgen::generate!` call for that world, the capability-selected `bind_host_via_wit_bindgen!` invocation, a `Guest` implementation whose `on_event` dispatches to whichever handlers are present (absent handlers become a no-op for that event), and `export!`:

```rust
// nexum/modules/examples/http-probe/src/lib.rs (shipped)
mod logic;

use nexum::host::types;

struct HttpProbe;

#[nexum_sdk::module]
impl HttpProbe {
    fn init(config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        let cfg = logic::parse_config(&config)?;
        // ...
        Ok(())
    }

    fn on_block(block: types::Block) -> Result<(), Fault> {
        logic::on_block(&nexum_sdk::http::WasiFetch, /* ... */ block.number)
            .map_err(Into::into)
    }
}
```

Two things worth being precise about:

- **The world is derived from the manifest.** The macro generates
against a per-module world whose imports are exactly the `[capabilities].required`/`optional` declarations: a chain + local-store module simply has no `identity` bindings to call. Its imports equal its declarations by construction, and the runtime's capability check is a backstop rather than a consumer of toolchain dead-import elision.
- **Handlers are synchronous.** `init` and the named handlers are
plain `fn`, called directly with no `block_on` wrapper. Modules call `host.request(chain_id, method, params_json)` (or the `chain::eth_call_params` / `parse_eth_call_result` helpers) directly against `ChainHost`, per [doc 07](07-rpc-namespace-design.md). (Keeper handlers are the exception: see below.)

The `Guest`/`export!` shape the macro emits follows the `logic.rs` (pure logic, tested against `&impl Host`) / `lib.rs` (handlers plus the macro attribute) split from ADR-0009. The keeper primitives in `nexum_sdk::keeper` - `WatchSet`, `Gates`, `Journal`, `Poller`, `Retrier` - give conditional-commitment modules a shared set of `LocalStoreHost` conventions instead of hand-rolled key schemes; `videre_sdk::keeper` assembles them into the generic run.

## Venue persona: `videre-sdk`

### The venue contract

An adapter targets the `videre:venue/venue-adapter` world: it exports the `adapter` interface (`body-versions`, `derive-header`, `quote`, `submit`, `status`, `cancel`) plus `init`, and imports scoped transport only - `chain`, `messaging`, and allowlisted `wasi:http`. No local-store, remote-store, identity, or logging import: an adapter structurally cannot touch host key material or persistent state. Keepers reach venues through the host-implemented `videre:venue/client` interface, which mirrors `adapter` with a venue selector per call. The types (`intent-header`, `quotation`, `submit-outcome`, `receipt`, `intent-status`, `venue-error`) live in `videre:types`, over the `videre:value-flow` asset vocabulary.

### Bodies: the `IntentBody` derive

A venue's intent body is opaque on the wire; typing is a guest-side agreement between keeper and adapter, spelled as an outer per-venue version enum under `#[derive(videre_sdk::IntentBody)]`:

```rust
#[derive(videre_sdk::IntentBody)]
enum EchoBody {
    V1(u64),
}
```

The wire form is the borsh enum layout: a one-byte version tag (the variant's declaration index) then the borsh payload - so the tag order is the schema: append new versions, never reorder. Decoding an unknown tag fails typedly as `BodyError::UnknownVersion` rather than as a stringly decode error. The versions an adapter decodes are declared in its manifest `[venue] body_versions` and asserted at install against its `body-versions` export; a keeper declares the single `[venue] body_version` it encodes and install refuses it unless every installed adapter decodes that version.

### `#[videre_sdk::venue]`

The single blessed venue authoring path. Apply it to the adapter's `impl VenueAdapter for MyVenue` block: the macro reads the crate's `module.toml`, asserts its `[module] kind` is `venue-adapter`, synthesizes a per-component world exporting the `videre:venue/adapter` face and importing exactly the manifest's declared scoped transport, then emits the `wit_bindgen::generate!` call, the untouched trait impl, and the export glue. An undeclared capability's bindings do not exist (using one is a compile error), and a capability outside the venue-permitted set (`chain`, `messaging`, `http`) is rejected at expansion. The generated world remaps the shared interfaces onto `videre_sdk::bindings`, so the impl speaks `videre_sdk` types directly and shares type identity with the conformance kit and the client core.

### The typed client and `#[videre_sdk::keeper]`

The wire carries opaque bodies and a stringly venue selector; typing is recovered in `videre_sdk::client`. A keeper names a venue once, as a `Venue` marker carrying its `VenueId` and body schema:

```rust
struct CowVenue;
impl Venue for CowVenue {
    const ID: VenueId = VenueId::from_static("cow");
    type Body = CowIntentBody;
}
```

`VenueClient<V>` then drives the venue with typed bodies - `quote` returns a `Quoted` typestate whose `submit` sends exactly the priced bytes - encoding through `IntentBody` before the byte-level, native-AFIT `VenueTransport` seam. `HostVenues` binds the seam to the module's own `videre:venue/client` import; tests implement `VenueTransport` in memory.

`#[videre_sdk::keeper]` is the keeper mirror of `#[module]`: apply it to an `impl` block whose associated functions are the event handlers (`init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`, `on_intent_status`). It requires the `client` capability (the `videre:venue/client` import is what makes a keeper a keeper), wires that import onto the SDK's shared shims, and lets handlers be `async` so they can await the typed client directly; `videre_sdk::client::poll_once` completes the futures on the synchronous guest boundary. A `From<ClientError>` impl onto the wire fault is emitted, so `?` applies to client calls inside handlers.

`videre_sdk::keeper::Keeper::run` assembles the world-neutral `nexum_sdk::keeper` stores - `WatchSet` to `Gates` to `Poller::poll` to `Retrier` to `Journal` - over the `VenueTransport` seam, so a conditional-commitment keeper writes one `poll` producing the shared `Outcome` outcome and inherits the whole gate/journal/retry pass.

### Testing: `nexum-sdk-test` and `videre-test`

Keeper logic tests exactly as module logic does: against the host traits with `nexum_sdk_test::MockHost`, plus an in-memory `VenueTransport` for the client seam. Tests run as plain native Rust - no `wasm32-wasip2` target, no wasmtime instance, no network.

```rust
use nexum_sdk::host::*;
use nexum_sdk_test::MockHost;

let host = MockHost::new();
host.chain.respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));

assert_eq!(host.request(1, "eth_blockNumber", "[]").unwrap(), "\"0x1\"");
assert_eq!(host.chain.calls().len(), 1);
```

Adapters are held to the `videre-test` conformance kit instead:

- **`CodecVectors`** - the venue's `IntentBody` wire bytes as a JSON
file (bytes as lowercase hex). A Rust adapter checks its derived enum with `assert_conforms`; a non-Rust author reads the same file and proves byte-exactness without linking Rust.
- **`HeaderGoldens`** - published bodies paired with the header a
conforming `derive-header` projects from them.
- **`MockTransport`** - the three transports an adapter is granted
(chain, messaging, outbound HTTP) as programmable in-memory mocks behind the SDK's own seams.

(The runtime crate separately ships a feature-gated component-level harness - `nexum-runtime`'s `test_utils::TestRuntime` - that loads a compiled `.wasm` plus manifest under real wasmtime; that is runtime-internal tooling, not part of either SDK contract.)

## Walkthrough: authoring a venue on videre

The shipped reference pair is `videre/modules/examples/echo-venue` (adapter) and `videre/modules/examples/echo-keeper` (driver); the production instance of the same shape is `shepherd/crates/cow-venue` driven by `shepherd/modules/twap-monitor`.

1. **Declare the manifest.** A venue adapter is a component with a
`module.toml` whose kind names it:

   ```toml
   [module]
   name = "echo-venue"          # the venue id the registry installs it under
   version = "0.1.0"
   kind = "venue-adapter"
   component = "sha256:..."

   [capabilities]
   required = ["chain"]          # scoped transport only: chain, messaging, http

   [capabilities.http]
   allow = []

   [venue]
   body_versions = [1]           # must equal the body-versions export; install asserts it
   ```

2. **Implement `VenueAdapter`.** One impl block under the macro; the
five intent functions plus `init` and `body_versions`:

   ```rust
   use videre_sdk::{IntentHeader, IntentStatus, Quotation, SubmitOutcome, VenueAdapter, VenueError};

   struct EchoVenue;

   #[videre_sdk::venue]
   impl VenueAdapter for EchoVenue {
       fn init(_config: Config) -> Result<(), Fault> { Ok(()) }
       fn body_versions() -> Vec<u32> { vec![1] }
       fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> { /* pure, no I/O */ }
       fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> { /* ... */ }
       fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> { /* ... */ }
       fn status(receipt: Vec<u8>) -> Result<IntentStatus, VenueError> { /* ... */ }
       fn cancel(receipt: Vec<u8>) -> Result<(), VenueError> { /* ... */ }
   }
   ```

`derive_header` is the policy seam: it projects the guard-facing `IntentHeader` (`gives`, `wants`, `settlement`, `authorisation`) from a body, pure and I/O-free, and the host's egress guard runs on it before every submit.

3. **Publish the fixtures.** Ship the venue's codec vectors and header
goldens, and hold the adapter to them with `videre-test` in the crate's tests. Non-Rust keeper authors read the same files.

4. **Build and install.** Build the cdylib for `wasm32-wasip2`
(`cargo build --target wasm32-wasip2 --release -p echo-venue`) and install it via the engine config:

   ```toml
   [[adapters]]
   path = "target/wasm32-wasip2/release/echo_venue.wasm"
   manifest = "videre/modules/examples/echo-venue/module.toml"
   http_allow = []               # the operator's outbound-HTTP grant
   ```

Install boots the component, checks the `body-versions` handshake against the manifest, and registers the venue under its manifest name in the `VenueRegistry` ([doc 08](08-platform-generalisation.md)).

5. **Drive it from a keeper.** A keeper declares the `client`
capability plus its `[venue] body_version`, names the venue as a `Venue` marker, and speaks types end to end:

   ```rust
   #[videre_sdk::keeper]
   impl EchoKeeper {
       async fn on_block(block: types::Block) -> Result<(), Fault> {
           let venue = VenueClient::<EchoVenue>::new();
           let quoted = venue.quote(&EchoBody::V1(block.number)).await?;
           if let SubmitOutcome::Accepted(receipt) = quoted.submit().await? {
               let _status = venue.status(&receipt).await?;
           }
           Ok(())
       }
   }
   ```

An accepted submit is watched implicitly: the registry polls the adapter's `status` and fans transitions back as `intent-status` events, which the keeper subscribes to in its manifest and handles in `on_intent_status`.

## The CoW venue

CoW ships as the production instance of the persona, in two crates so the venue stays orderbook-only:

- **`cow-venue`** - feature slices. `body` (default, `no_std`): the
order intent body types and codec, light enough for any keeper or adapter to carry. `client`: the typed `CowClient` bound to the CoW venue, the deterministic `intent_id` journal key, and the table-driven retry classification generated from the shipped `data/classification.toml`. `assembly`: the chain-edge order projections and orderbook submission bodies. `adapter`: the venue adapter component itself (`CowAdapter` under `#[videre_sdk::venue]`, manifest at `shepherd/crates/cow-venue/module.toml`).
- **`composable-cow`** - the ComposableCoW keeper machinery, kept out
of the venue: the conditional-order `ComposableBody`, the structured poll seam (`Verdict`, with the deployed 1.x reverting wire quarantined behind `LegacyRevertAdapter`, per [ADR-0013](adr/0013-composable-cow-structured-poll.md)), and the `run` slice composing the poll loop over the typed `CowClient`.

The shipped CoW keepers - `shepherd/modules/twap-monitor`, `shepherd/modules/ethflow-watcher` - are ordinary `#[videre_sdk::keeper]` modules on this surface.

## Non-Rust module and adapter authors

For **non-Rust** authors (JavaScript, Python, Go, C++), neither SDK is relevant - they generate bindings directly from the WIT package for their target world with their language's `wit-bindgen`, and prove body conformance against the venue's published `videre-test` fixture files. The WIT is the universal contract; both Rust SDKs are an ergonomics layer on top of it, not a requirement.

## Where to go next

- [`sdk.md`](sdk.md) - the day-to-day API reference and rustdoc entry
point.
- [ADR-0009](adr/0009-host-trait-surface.md) - the host-trait seam
decision this document builds on.
- [ADR-0011](adr/0011-per-interface-typed-errors.md) - the typed
error model the host traits return.
- [doc 07](07-rpc-namespace-design.md) - the `chain` RPC passthrough
design and why module authors call `host.request` directly.
- [doc 08](08-platform-generalisation.md) - the layered WIT and why
venue adapters are the domain-extension mechanism.
