---
status: superseded-in-part
---

# Host trait surface: per-capability traits plus a supertrait, with a `logic.rs` / `lib.rs` split

> **The error envelope is superseded by [ADR-0011](0011-per-interface-typed-errors.md):** the single `HostError` / `HostErrorKind` record is replaced by per-interface typed errors over a shared `fault` vocabulary. The trait-surface and `logic.rs` / `lib.rs` decisions below still hold; read `HostError` as its successor types.

## Context

The runtime needs a testable host abstraction so module logic compiles against an in-memory mock without a `wasm32-wasip2` toolchain. `wit_bindgen::generate!` emits per-cdylib types, so a single shared SDK type cannot cross the WIT boundary; the mocks live in their own crate (`nexum-sdk-test`) and compile for the host target.

## Decision

Three coupled choices:

1. **Four per-capability traits (`ChainHost`, `LocalStoreHost`, `CowApiHost`, `LoggingHost`) with a blanket-implemented supertrait `Host`.** Module code takes `&impl Host` and calls any interface uniformly; tests inject `nexum_sdk_test::MockHost`, production injects `WitBindgenHost`. The four-trait split lets a test mock only the calls its module makes.
2. **An SDK-side error type mirroring the WIT struct field-for-field**, its own type (the WIT-generated one is per-cdylib), bridged by a one-line `From` in each module's `lib.rs`. This keeps the traits world-neutral so `nexum-sdk-test` needs no wasm toolchain. Superseded by ADR-0011's typed errors.
3. **Per-module `logic.rs` (pure, wit-independent logic, unit tests against `MockHost`) plus `lib.rs` (per-cdylib `wit_bindgen::generate!` glue, the `WitBindgenHost` impl, and the `Guest` dispatch).** Colocating hundreds of lines of logic with the mechanical adapter obscures both.

## Consequences

- Module code is testable in native Rust without `wasm32-wasip2`; every module ships a `MockHost` unit-test suite.
- New capabilities add a new trait plus a `MockX` in `nexum-sdk-test`; modules that do not use a capability bound only on the subset they touch.
- The `#[nexum_sdk::module]` macro derives a per-module world from the manifest's `[capabilities]`, so a macro-built component's imports equal its declarations by construction and an undeclared capability is a compile-time error. `enforce_capabilities` (`crates/nexum-runtime/src/manifest/capabilities.rs`) is the boot-time backstop; a hand-rolled module compiled against the supertype world that imports an undeclared capability fails there rather than at compile time.
