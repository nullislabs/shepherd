# SDK Design: The Two-Persona SDK Plan

This document describes the guest-side SDK crates and the plan that
shapes them: a **module-author persona** and a **venue-adapter
persona**, each with its own crate pair and attribute macro. The
module-author persona is shipped and is what this document mostly
describes; the venue-adapter persona is design intent tracked by a
separate epic and is called out explicitly as such wherever it
appears below.

For the architectural decision behind the host-trait seam that the
module-author persona builds on, see [ADR-0009](adr/0009-host-trait-surface.md).
For the rustdoc-level API reference (the source of truth once you are
writing module code), see [`sdk.md`](sdk.md) and the rustdoc under
`crates/nexum-sdk/`, `crates/shepherd-sdk/`, and
`crates/nexum-module-macros/`.

## The two personas

The runtime has two kinds of guest authors, and they need different
things from the SDK:

1. **Module author.** Writes an automation module against
   `nexum:host/event-module` (or the CoW-extended `shepherd:cow/shepherd`
   world): react to blocks, chain logs, ticks, or messages; read and
   write local state; submit orders. This persona is served today by
   `nexum-sdk` (+ the `#[nexum::module]` macro, spelled
   `#[nexum_sdk::module]` in code) and, for CoW-specific modules,
   `shepherd-sdk` on top.

2. **Venue adapter author.** Writes an adapter that exposes a trading
   venue (CoW Protocol, a DEX, a lending market, ...) to modules
   through a common intent surface, so a module author does not need
   to know the venue's wire format. This persona is planned but not
   yet shipped: the crate (`videre-sdk`), the per-venue crates
   (e.g. a `cow-venue` crate carrying CoW's intent-body codec), the
   `#[nexum::venue]` macro, and the `videre-test` conformance kit
   are all tracked by the SDK-surfaces epic and have no code in the
   tree yet. See [Venue-adapter persona (planned)](#venue-adapter-persona-planned)
   below for the shape of the plan.

Each persona has its own proc-macro crate (`nexum-module-macros` for
modules, `videre-macros` for venue adapters) and both share the
same host-trait philosophy: guest code is written against small Rust
traits that mirror the WIT interfaces one-for-one, so strategy logic
can be unit-tested against an in-memory mock without a `wasm32-wasip2`
toolchain or a running wasmtime instance.

## Module-author persona (shipped): `nexum-sdk` + `shepherd-sdk`

### Crate structure

```
nexum-sdk/
├── Cargo.toml
└── src/
    ├── lib.rs                # crate docs, `pub use nexum_module_macros::module`
    ├── prelude.rs            # alloy primitive re-exports (Address, B256, Bytes, U256, keccak256)
    ├── host.rs                # ChainHost / LocalStoreHost / LoggingHost + supertrait Host; Fault, ChainError, RpcError
    ├── wit_bindgen_macro.rs  # bind_host_via_wit_bindgen! - generates WitBindgenHost + converters
    ├── keeper.rs             # WatchSet, Gates, Journal, ConditionalSource, Retrier
    ├── chain/                # eth_call_params, parse_eth_call_result, chainlink AggregatorV3 reader
    ├── events.rs              # native alloy Log assembly from the wire ChainLog record
    ├── config.rs              # (key, value) config-table lookups, decimal scaling
    ├── address.rs             # EVM address parsing with typed errors
    ├── http.rs                # Fetch trait seam, WasiFetch, FetchError (wasi:http)
    ├── tracing.rs             # guest tracing facade + panic hook over a LogSink seam
    └── proptests.rs           # cfg(test) property tests (not part of the public surface)

nexum-module-macros/
├── Cargo.toml                 # proc-macro = true
└── src/
    └── lib.rs                 # #[module] attribute macro

shepherd-sdk/
├── Cargo.toml
└── src/
    ├── lib.rs                 # crate docs; no re-export of nexum-sdk
    ├── prelude.rs             # cowprotocol order/signing/orderbook re-exports
    ├── wit_bindgen_macro.rs   # bind_cow_host_via_wit_bindgen! - layers CowApiHost onto WitBindgenHost
    ├── cow/                   # CowApiHost trait, gpv2_to_order_data, Verdict, LegacyRevertAdapter,
    │                           # RetryAction classifiers, run() (poll -> gate/journal/submit)
    └── proptests.rs           # cfg(test) property tests (not part of the public surface)
```

`nexum-sdk` is host-neutral and domain-free: any module targeting the
runtime pulls helpers and canonical primitive types from it regardless
of which world it exports. `shepherd-sdk` depends on `nexum-sdk` and
layers the CoW Protocol domain on top; modules that touch the
orderbook import both crates directly (nothing is re-exported between
them). `shepherd-sdk` has not been retired - the clean break described
in the SDK epic (folding its CoW surface into a future `cow-venue`
crate) is deferred to a follow-on train, sequenced after the
venue-adapter persona lands.

Companion mock crates: `nexum-sdk-test` (in-memory `MockHost` over
`ChainHost` / `LocalStoreHost` / `LoggingHost`) and `shepherd-sdk-test`
(composes those mocks with `MockCowApi`). See
[Testing](#testing-nexum-sdk-test-and-shepherd-sdk-test) below.

### The host-trait seam

Neither crate calls `wit_bindgen`-generated functions directly.
Instead `nexum-sdk::host` exposes small traits that mirror the WIT
interfaces:

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

`shepherd-sdk` adds a fourth trait, `CowApiHost` (`submit_order`,
`cow_api_request`), and
its own supertrait `CowHost: Host + CowApiHost`. Strategy code takes
`&impl Host` (or a narrower `<H: ChainHost + LocalStoreHost>` bound
when it only needs part of the surface) so tests inject
`nexum_sdk_test::MockHost` while the compiled module injects the
wit-bindgen-backed adapter. See [ADR-0009](adr/0009-host-trait-surface.md)
for the full rationale (four traits over one fat trait, the
`strategy.rs` / `lib.rs` split, and the world-neutral `HostError`
predecessor that per-interface typed errors later replaced - see
[ADR-0011](adr/0011-per-interface-typed-errors.md)).

### The wit-bindgen adapter: `bind_host_via_wit_bindgen!`

Every module still keeps its own `wit_bindgen::generate!` call (the
macro emits types into the calling crate; re-exporting wit-bindgen
output from a library crate would duplicate symbols and break the
component-export contract). What the SDK removes is the ~80 lines of
mechanical glue that used to sit next to it: the `nexum_sdk::bind_host_via_wit_bindgen!()`
declarative macro emits a `WitBindgenHost` struct, the `ChainHost` /
`LocalStoreHost` / `LoggingHost` impls over the generated import
shims, the `Fault` / `ChainError` converters in both directions, a
`Level` <-> wit-bindgen `logging::Level` converter, a
`From<ChainLog> for nexum_sdk::events::Log` impl, and an
`install_tracing()` helper that routes `tracing::info!(...)` through
the bound host logging call. The adapter is capability-selected: the
zero-argument form emits the full set for blanket-world modules, and
the `caps: [chain, logging]` form (what `#[nexum_sdk::module]`
generates from the manifest) emits only the pieces whose imports the
module's world carries. `shepherd-sdk::bind_cow_host_via_wit_bindgen!`
layers the `CowApiHost` impl on top of the same `WitBindgenHost` type.

### The `#[nexum::module]` macro

`nexum-module-macros` ships one attribute macro, re-exported as
`nexum_sdk::module`. Apply it to an inherent `impl` block whose
methods are named event handlers - `init`, `on_block`,
`on_chain_logs`, `on_tick`, `on_message` - and the macro reads the
crate's `module.toml`, synthesizes the per-module world from its
`[capabilities]`, and generates the `wit_bindgen::generate!` call for
that world, the capability-selected `bind_host_via_wit_bindgen!`
invocation, a `Guest` implementation whose `on_event` dispatches to
whichever handlers are present (absent handlers become a no-op for
that event), and `export!`:

```rust
// modules/examples/http-probe/src/lib.rs (shipped)
mod strategy;

use nexum::host::types;

struct HttpProbe;

#[nexum_sdk::module]
impl HttpProbe {
    fn init(config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        let cfg = strategy::parse_config(&config)?;
        // ...
        Ok(())
    }

    fn on_block(block: types::Block) -> Result<(), Fault> {
        strategy::on_block(&nexum_sdk::http::WasiFetch, /* ... */ block.number)
            .map_err(Into::into)
    }
}
```

Two things worth being precise about, since they differ from earlier
drafts of this plan:

- **One macro, not two.** There is no separate `#[shepherd::module]`.
  The macro reads the crate's `module.toml` and generates against a
  per-module world whose imports are exactly the
  `[capabilities].required`/`optional` declarations (a chain +
  local-store module simply has no `cow-api` or `identity` bindings to
  call). This retires the import-elision dependency ADR-0009 flagged
  for macro-built modules: their imports equal their declarations by
  construction, and the runtime's capability check is a backstop
  rather than a consumer of toolchain dead-import elision. Declaring
  `cow-api` (or `pool`) pulls that import into the world, and the
  module layers its own domain adapter (a `CowApiHost` impl over the
  generated shims) on top of the emitted core one.
- **Handlers are synchronous.** `init` and the named handlers are
  plain `fn`, called directly with no `block_on` wrapper. There is no
  `async fn` handler support and no injected `&RootProvider` - modules
  call `host.request(chain_id, method, params_json)` (or the
  `chain::eth_call_params` / `parse_eth_call_result` helpers) directly
  against `ChainHost`, per [doc 07](07-rpc-namespace-design.md).

The `Guest`/`export!` shape the macro emits still follows the
`strategy.rs` (pure logic, tested against `&impl Host`) / `lib.rs`
(handlers plus the macro attribute) split from ADR-0009. The keeper
helpers in `nexum_sdk::keeper` - `WatchSet`, `Gates`, `Journal`,
`ConditionalSource`, `Retrier` - give conditional-commitment
modules (watchers that poll a set of pending commitments) a shared set
of `LocalStoreHost` conventions instead of hand-rolled key schemes.

### Testing: `nexum-sdk-test` and `shepherd-sdk-test`

```rust
use nexum_sdk::host::*;
use nexum_sdk_test::MockHost;

let host = MockHost::new();
host.chain.respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));

assert_eq!(host.request(1, "eth_blockNumber", "[]").unwrap(), "\"0x1\"");
assert_eq!(host.chain.calls().len(), 1);
```

`MockHost` composes one mock per trait (`chain`, `store`, `logging`
in `nexum-sdk-test`; `shepherd-sdk-test` adds a `cow_api` field on the
`shepherd:cow/cow-api` seam, backed by `MockCowApi` by default or the
per-call-scriptable `MockVenue` via `MockHost::with_venue()`), each
recording calls and letting tests program responses. Tests run as
plain native Rust against the traits - no `wasm32-wasip2` target, no
wasmtime instance, no network round-trip. This is the whole SDK-side
testing story today. (The runtime crate separately ships a
feature-gated component-level harness - `nexum-runtime`'s
`test_utils::TestRuntime`, behind the `test-utils` feature - that
loads a compiled `.wasm` plus manifest under real wasmtime and
dispatches events to it; that is runtime-internal tooling, not part
of the module-author SDK contract.)

## Venue-adapter persona (planned)

The venue-adapter persona is the other half of the two-persona plan
and is **not shipped**. It is tracked by a set of open issues under
the SDK-surfaces epic and depends on a venue-adapter WIT world that
does not exist yet either. Nothing below this heading describes code
in this repository; it is recorded here so the module-author persona
above is read in the context of where the SDK is going, not as a
competing vision.

The planned shape:

- **`videre-sdk`** - a new crate carrying the guest-side
  `VenueAdapter` trait over the (also planned) adapter-world bindgen,
  a `borsh`-backed `IntentBody` derive that enforces a per-venue
  version enum (an adapter rejects an intent body tagged with an
  unknown version rather than misinterpreting it), and typed wrappers
  over the scoped transport imports (`http`, `messaging`, `chain`) an
  adapter is granted.
- **Per-venue crates** - e.g. a `cow-venue` crate that would carry
  CoW Protocol's intent-body codec and become the eventual home for
  the CoW helpers `shepherd-sdk::cow` carries today, once the clean
  break happens.
- **`#[nexum::venue]`** - a second attribute macro, in `videre-macros`,
  parallel to `#[nexum::module]`: it would emit the per-cdylib export
  glue for an adapter and a per-component world matching the
  manifest's declared capabilities (retiring the import-elision
  dependency for the venue side from day one, rather than as
  follow-on work).
- **`videre-test`** - a conformance kit: published codec
  round-trip vectors (so a non-Rust adapter author can prove
  byte-exact `IntentBody` encoding without linking Rust), header-
  derivation golden fixtures, and a `MockTransport` for adapter unit
  tests.

Once this lands, `shepherd-sdk`'s CoW surface is expected to move into
the `cow-venue` crate as a single clean-break migration - the same
no-deprecation-window reasoning [ADR-0011](adr/0011-per-interface-typed-errors.md)
gives for pre-1.0 wire breaks applies here - and this document should
be revisited to describe the venue-adapter persona as shipped rather
than planned.

## Non-Rust module and adapter authors

For **non-Rust** authors (JavaScript, Python, Go, C++), neither SDK is
relevant - they generate bindings directly from the WIT package for
their target world with their language's `wit-bindgen`. The WIT is
the universal contract; both Rust SDKs are an ergonomics layer on top
of it, not a requirement.

## Where to go next

- [`sdk.md`](sdk.md) - the day-to-day API reference and rustdoc entry
  point for module authors.
- [ADR-0009](adr/0009-host-trait-surface.md) - the host-trait seam
  decision this document builds on.
- [ADR-0011](adr/0011-per-interface-typed-errors.md) - the typed
  error model (`Fault`, `ChainError`, `CowApiError`) the host traits
  return.
- [doc 07](07-rpc-namespace-design.md) - the `chain` RPC passthrough
  design and why module authors call `host.request` directly rather
  than through an injected provider.
