# Testing the Runtime: the engine-side `test-utils` harness

Two entirely separate mock surfaces exist in this repo, for testing two
entirely separate things. Conflating them wastes effort - either testing
module logic through the slow, heavy engine harness, or trying (and
failing) to reach engine correctness through a guest-side mock. Read this
before writing a runtime test.

## The guardrail

- **Module business logic is tested in plain Rust, no wasm.** A module's
  decision logic lives in a host-generic `strategy.rs`
  (`fn on_block<H: Host>(host: &H, ...)`), and its tests drive it against
  `nexum-sdk-test::MockHost` (CoW modules: `shepherd-sdk-test::MockHost`).
  No wasmtime, no component boundary, no engine crate at all. This is
  already the dominant pattern across every shipped module (twap-monitor,
  ethflow-watcher, stop-loss, price-alert, balance-tracker) - see
  [docs/sdk.md](sdk.md#companions-nexum-sdk-test-and-shepherd-sdk-test).
  **New module-logic tests belong here.**
- **The engine harness (this page) is reserved for engine, host, and
  boundary correctness**: supervision (poison, restart, resource traps),
  dispatch isolation across chains and modules, fault and log capture, the
  WASI clock override a real guest observes, capability wiring, stream
  reconnect. These need a real compiled `.wasm` component over async mock
  backends and genuinely cannot be faked in-process.
- **Do not test module business logic through the wasm harness.** If a
  test only needs "given input X the strategy does Y", it belongs in
  `strategy.rs` against `MockHost`, not in a booted fixture here. A
  harness test that could be rewritten as a plain-Rust `MockHost` test
  without losing coverage is in the wrong place.

## What `test-utils` provides

`crates/nexum-runtime`'s `test_utils` module (gated behind the
`test-utils` cargo feature) ships two layers:

- **The bare mock backends** - `MockChainProvider`, `MockStateStore`, and
  `MockTypes` implement the engine's component-seam traits with no
  network and no disk. `mock_components` / `mock_components_from` bundle
  them into a `Components` ready for `Supervisor::boot`. Use these when a
  test needs to drive the supervisor directly - multi-module scenarios,
  custom extensions, or checking host-interface wiring the harness below
  doesn't expose.
- **`TestRuntime` / `TestRuntimeBuilder`** - a higher-level harness over
  the same mocks: launch *one* module through the real public
  `RuntimeBuilder` path, inject events, and read back logs and store
  writes, with no supervisor ceremony. This is what most engine-level
  tests want.

### Feature gate and the self dev-dependency

`test_utils` only compiles under the `test-utils` feature (it pulls
`tempfile`, needed for manifest staging):

```toml
# crates/nexum-runtime/Cargo.toml
[features]
test-utils = ["dep:tempfile"]

[dev-dependencies]
# Self dev-dependency enabling `test-utils` for this crate's own test, doc,
# and example targets, so `cargo test -p nexum-runtime` sees the mocks
# without every invocation passing `--features test-utils`.
nexum-runtime = { path = ".", features = ["test-utils"] }
```

A crate outside `nexum-runtime` that wants the harness - an extension
crate testing its own backend against a real supervisor, for instance -
depends on `nexum-runtime` with `features = ["test-utils"]` directly; no
self-dependency trick needed there.

## Worked example: `TestRuntime`

Build the module fixture once (`cargo build --target wasm32-wasip2
--release -p example`, or `just build-module`), then:

```rust
use alloy_rpc_types_eth::Header;
use nexum_runtime::test_utils::TestRuntime;

#[tokio::test]
async fn example_dispatches_a_block_and_logs_it() {
    let wasm = "target/wasm32-wasip2/release/example.wasm";

    let mut rt = TestRuntime::builder(wasm)
        .manifest_inline(
            r#"
[module]
name = "example"

[capabilities]
required = ["logging"]

[[subscription]]
kind     = "block"
chain_id = 1
"#,
        )
        .launch()
        .await
        .expect("launch the example module");

    // Inject a block header. Dispatch runs on the spawned event-loop
    // task, so inject then await an observable effect - don't assume
    // synchronous delivery.
    let mut header: Header = Header::default();
    header.inner.number = 42;
    rt.push_block(header);

    // Notification-driven, not a sleep: resolves as soon as the
    // dispatched event's log record lands (5s failure backstop).
    rt.wait_for_log("example", "block 42 on chain")
        .await
        .expect("the module logged the block");

    rt.shutdown();
    rt.wait().await.expect("clean shutdown");
}
```

Beyond the happy path above:

- **Chain-log injection** mirrors blocks: `rt.push_chain_log(log)` where
  `log: alloy_rpc_types_eth::Log`.
- **Chain-request programming**, for a module that calls `chain::request`
  (e.g. an `eth_call` oracle read): `rt.chain().on_method(ChainMethod::EthCall,
  r#""0x...""#)` before `launch`, or `on_request` for a full
  `(method, params)` match.
- **Error and stream-end injection**, to exercise reconnect and fault
  paths: `rt.chain().push_block_err(err)` delivers a transport error to
  the open block stream; `rt.chain().close_block_stream()` simulates the
  upstream ending the subscription, so the test can assert the event
  loop's reconnect logic re-opens it.
- **Store assertions**: `rt.store()` exposes the same `MockStateStore`
  the module wrote through `local-store` - read back what a dispatched
  event persisted.
- **Guest time**: `rt.clock()` is the `ManualClock` installed as the
  module's WASI clock override; advance it to test time-dependent logic
  without a real sleep.

## If you landed here looking for module tests

You want `nexum-sdk-test::MockHost` (or `shepherd-sdk-test::MockHost` for
CoW modules) instead - no wasm build, no engine crate, runs in
milliseconds. See [docs/sdk.md](sdk.md) and any shipped module's
`strategy.rs` test module for the pattern.
