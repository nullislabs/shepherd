# Build your first module

A walkthrough of the shipped `price-alert` example: a module that polls a Chainlink oracle on every block and logs when the price crosses a threshold. It exercises the three load-bearing patterns of a module: a block subscription, a `chain` read with ABI decode, and `[config]`-driven behaviour. Read the real files alongside this page under [`modules/examples/price-alert`](../modules/examples/price-alert).

Venue submission (signing and posting an order to a venue such as CoW) is a separate concern layered on `videre-sdk`; see [Where to go from here](#where-to-go-from-here).

## Prerequisites

A Rust toolchain and the WASM Component Model target:

```sh
rustup target add wasm32-wasip2
```

Build and run the minimal `example` module to confirm the engine works:

```sh
cargo build --target wasm32-wasip2 --release -p example
cargo run -p nexum-cli -- \
  target/wasm32-wasip2/release/example.wasm \
  modules/example/module.toml
```

You should see the example module's `init` log line. Triage the build before continuing if it does not appear.

## Crate layout

A module is a `cdylib` crate that compiles to a WASM Component. `price-alert`'s `Cargo.toml`:

```toml
[package]
name = "price-alert"
version = "0.1.0"
edition.workspace = true

[lib]
crate-type = ["cdylib"]

[dependencies]
nexum-sdk = { path = "../../../crates/nexum-sdk" }
alloy-primitives = { version = "1.6", default-features = false, features = ["std"] }
tracing = { version = "0.1", default-features = false }
wit-bindgen = { version = "0.59", default-features = false, features = ["macros", "realloc"] }

[dev-dependencies]
nexum-sdk-test = { path = "../../../crates/nexum-sdk-test" }
alloy-sol-types = { version = "1.6", default-features = false, features = ["std"] }
```

The load-bearing points:

- `crate-type = ["cdylib"]` produces a Component when built for `wasm32-wasip2`.
- `nexum-sdk` supplies the generic helpers (`chain`, `host`, `config`, `prelude`). A module that talks to a venue also depends on `videre-sdk`; `price-alert` does not.
- `nexum-sdk-test` is a dev-dep only: its `MockHost` links under `cargo test`, never into the artefact.
- Modules never depend on `nexum-runtime`. They reach the host through wit-bindgen-generated imports the SDK macro wires up.

The crate is a workspace member; add its path to `members` in the root `Cargo.toml`.

## The manifest

`module.toml` declares capabilities, subscriptions, and operator config:

```toml
[module]
name = "price-alert"
version = "0.1.0"
component = "sha256:0000000000000000000000000000000000000000000000000000000000000000"

[capabilities]
required = ["logging", "chain"]
optional = []

[capabilities.http]
allow = []

[[subscription]]
kind = "block"
chain_id = 11155111  # Sepolia

[config]
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"  # ETH/USD on Sepolia
decimals = "8"
threshold = "2500.00"
direction = "below"
every_n_blocks = "1"
```

- `required` lists the host capabilities the module imports. The engine enforces the list at instantiation: missing a used capability is a hard error.
- `[capabilities.http].allow` is empty because `price-alert` makes no outbound HTTP. A module that needs it declares the `http` capability, lists the hosts it may contact, and calls `nexum_sdk::http::fetch`; an off-list host returns the matchable `FetchError::Denied`. See [`modules/examples/http-probe`](../modules/examples/http-probe).
- `[config]` values are strings. `init` parses them into a typed `Settings`.

## The pure strategy

Decision logic lives in `strategy.rs` as a host-generic function. It never names `wit-bindgen` or `wasmtime`, so tests drive it directly:

```rust
use nexum_sdk::chain::chainlink::read_latest_answer;
use nexum_sdk::host::{ChainHost, Fault, LoggingHost};

pub fn on_block<H: ChainHost + LoggingHost>(
    host: &H,
    chain_id: u64,
    settings: &Settings,
    block_number: u64,
) -> Result<(), Fault> {
    if !block_number.is_multiple_of(settings.every_n_blocks) {
        return Ok(());
    }
    let Some(answer) =
        read_latest_answer(host, chain_id, settings.oracle_address, "price-alert")
    else {
        return Ok(()); // read_latest_answer already logged the failure
    };
    if classify(answer, settings.threshold_scaled, settings.direction) {
        tracing::warn!(answer = %answer, "price-alert: TRIGGERED");
    } else {
        tracing::info!(answer = %answer, "price-alert: ok");
    }
    Ok(())
}
```

The shape to internalise:

- Every interaction with the world goes through `host`, bounded by the traits the module actually imports (`ChainHost + LoggingHost` here, matching the two declared capabilities).
- The function recovers from transient upstream failure by logging and returning `Ok`, so one bad event does not poison the supervisor.

Config parsing follows the same one-shot style: `parse_config(&[(String, String)]) -> Result<Settings, Fault>`, using the `nexum_sdk::config` helpers (`get_required`, `scale_decimal`). See the full source in `strategy.rs`.

## The glue

`lib.rs` declares the handlers and defers all per-cdylib glue to `#[nexum_sdk::module]`:

```rust
#![allow(clippy::too_many_arguments)]

mod strategy;

use std::sync::OnceLock;
use nexum::host::types;

static SETTINGS: OnceLock<strategy::Settings> = OnceLock::new();

struct PriceAlert;

#[nexum_sdk::module]
impl PriceAlert {
    fn init(config: Vec<(String, String)>) -> Result<(), Fault> {
        install_tracing();
        let cfg = strategy::parse_config(&config)?;
        let _ = SETTINGS.set(cfg);
        Ok(())
    }

    fn on_block(block: types::Block) -> Result<(), Fault> {
        let Some(cfg) = SETTINGS.get() else { return Ok(()) };
        strategy::on_block(&WitBindgenHost, block.chain_id, cfg, block.number)
            .map_err(Into::into)
    }
}
```

Apply the attribute to an inherent `impl` whose functions are the named handlers (`init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`; absent handlers no-op). The macro generates the `wit_bindgen::generate!` call, the `WitBindgenHost` adapter, the `Fault` conversions, `install_tracing`, and the `Guest`/`export!` glue. Call `install_tracing()` once in `init`; after that the `tracing` macros reach the host log from anywhere with no `Host` value to thread through.

## Tests against `MockHost`

Because the strategy is host-generic, tests run in plain Rust with no wasm toolchain, driving it against `nexum_sdk_test::MockHost` and capturing the log output:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use nexum_sdk_test::{MockHost, capture_tracing};

    #[test]
    fn triggers_below_threshold() {
        let host = MockHost::new();
        let settings = sample_settings(250_050_000_000, Direction::Below);
        programmed_eth_call(&host, settings.oracle_address, Ok(oracle_response_json(200_000_000_000)));

        let (result, logs) = capture_tracing(|| on_block(&host, 11_155_111, &settings, 100));
        result.unwrap();

        let ev = logs.expect_one(|e| e.level == Level::WARN);
        assert_eq!(ev.message, "price-alert: TRIGGERED");
    }
}
```

Run with `cargo test -p price-alert`. See `strategy.rs` for the full test module, including the `MockHost` chain programming (`host.chain.respond_to`) and the throttle and error-path cases.

Any behaviour expressible as "given this host state, do that" belongs here, not in the engine harness. See [testing-runtime-harness.md](testing-runtime-harness.md) for the guardrail between the two.

## Build and run

Build the artefact:

```sh
cargo build --target wasm32-wasip2 --release -p price-alert
ls -lh target/wasm32-wasip2/release/price_alert.wasm
```

Block subscriptions ride `eth_subscribe`, so the chain needs a WebSocket endpoint. Point `engine.toml` at one:

```toml
[chains.11155111]
rpc_url = "wss://ethereum-sepolia-rpc.publicnode.com"
```

Run the engine over the module, supplying the chain config:

```sh
cargo run -p nexum-cli -- \
  target/wasm32-wasip2/release/price_alert.wasm \
  modules/examples/price-alert/module.toml \
  --engine-config engine.toml
```

You should see the `init` line, then one `price-alert: ok` or `price-alert: TRIGGERED` line per new block. An `unsupported` fault means the module imports a capability its `[capabilities].required` list omits.

## Where to go from here

- Venue submission: a module that signs and posts orders to a venue depends on `videre-sdk` and a venue adapter crate (`cow-venue` for CoW), and uses `#[videre_sdk::keeper]` rather than `#[nexum_sdk::module]`. See [`modules/twap-monitor`](../modules/twap-monitor) and [docs/sdk.md](sdk.md).
- Local state: declare `local-store` and persist through `host.set`/`host.get`; see [`modules/examples/balance-tracker`](../modules/examples/balance-tracker).
- Outbound HTTP: [`modules/examples/http-probe`](../modules/examples/http-probe).
- Resource limits: [docs/deployment.md](deployment.md).

## Reference

- SDK overview: [docs/sdk.md](sdk.md)
- Deployment: [docs/deployment.md](deployment.md)
- ADR-0001 (`engine.toml` vs `module.toml` split)
- ADR-0006 (TWAP and EthFlow as guest modules, no specialised WIT interfaces)
- Worked examples: [`price-alert`](../modules/examples/price-alert/), [`balance-tracker`](../modules/examples/balance-tracker/), [`twap-monitor`](../modules/twap-monitor/), [`ethflow-watcher`](../modules/ethflow-watcher/)
