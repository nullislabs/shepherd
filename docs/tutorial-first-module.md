# Build your first Shepherd module

This is the cold-start guide for an external developer. Target
completion time: **under four hours** from "I cloned the repo" to
"I see my module's first event in the engine log".

Scenario: a **stop-loss** module that watches a Chainlink price
oracle on every block and submits a CoW Protocol order when the
price drops below a configured trigger. It combines every
load-bearing pattern in the SDK:

| Pattern | Where this tutorial uses it | Already shown in |
|---|---|---|
| Block subscription | "react every block" | [`price-alert`](../modules/examples/price-alert) |
| `chain::request` + ABI decode | read the oracle | [`price-alert`](../modules/examples/price-alert) |
| `local-store` | dedup submitted orders | [`balance-tracker`](../modules/examples/balance-tracker) |
| `cow_api::submit_order` | submit the order | [`twap-monitor`](../modules/twap-monitor) |
| Host-free tests via `MockHost` | unit tests | [`shepherd-sdk-test`](../crates/shepherd-sdk-test) |

If you would rather read working code than a walkthrough, those
four crates are the worked examples. The rest of this guide
sequences the build so the patterns are introduced one at a time.

## 0. Prerequisites (15 minutes)

You need a recent Rust toolchain (`rustc 1.91+`, ships with `cargo`)
and the WASM Component Model target. From the repo root:

```sh
rustup target add wasm32-wasip2
```

Verify the engine builds and runs against the example module that
ships in the workspace:

```sh
cargo build --target wasm32-wasip2 --release -p example
cargo run -p nexum-cli -- \
  target/wasm32-wasip2/release/example.wasm \
  modules/example/nexum.toml
```

You should see two log lines from the example module - one in
`init`, one on the synthetic block event. Stop here and triage if
the build fails or those log lines do not appear; the rest of the
tutorial assumes a working local engine.

## 1. Scaffold the workspace member (15 minutes)

Create a new crate under `modules/examples/`:

```sh
mkdir -p modules/examples/stop-loss/src
```

The `Cargo.toml` follows the same template as `price-alert`:

```toml
# modules/examples/stop-loss/Cargo.toml
[package]
name = "stop-loss"
version = "0.1.0"
edition.workspace = true
license.workspace = true
repository.workspace = true

[lib]
crate-type = ["cdylib"]

[dependencies]
shepherd-sdk = { path = "../../../crates/shepherd-sdk" }
cowprotocol = { version = "1.0.0-alpha.3", default-features = false }
alloy-primitives = { version = "1.5", default-features = false, features = ["std"] }
alloy-sol-types = { version = "1.5", default-features = false, features = ["std"] }
serde_json = { version = "1", default-features = false, features = ["alloc"] }
wit-bindgen = { version = "0.57", default-features = false, features = ["macros", "realloc"] }

[dev-dependencies]
shepherd-sdk-test = { path = "../../../crates/shepherd-sdk-test" }
```

Note the four key features:

- **`crate-type = ["cdylib"]`** - produces a WASM Component when
  built for `wasm32-wasip2`.
- **`shepherd-sdk` path dep** - brings in the helpers (`cow::`,
  `chain::`, `host::`, `prelude`).
- **`shepherd-sdk-test` as a dev-dep** - `MockHost` + assertion
  helpers, only linked under `cargo test`.
- **No direct `nexum-runtime` dep** - modules never link the engine;
  they communicate via wit-bindgen-generated shims.

Add the new crate to the workspace `members` list in `Cargo.toml`
at the repo root:

```toml
[workspace]
members = [
    # ... existing members
    "modules/examples/stop-loss",
]
```

`cargo check --target wasm32-wasip2 -p stop-loss` should fail with
"no library targets found" - expected, you have not written any
source yet.

## 2. Author the manifest (10 minutes)

`module.toml` declares the capabilities, subscriptions, and
operator-supplied config. Drop this next to `Cargo.toml`:

```toml
# modules/examples/stop-loss/module.toml
[module]
name = "stop-loss"
version = "0.1.0"
component = "sha256:0000000000000000000000000000000000000000000000000000000000000000"

[capabilities]
required = ["logging", "chain", "local-store", "cow-api"]
optional = []

[capabilities.http]
allow = []

[[subscription]]
kind = "block"
chain_id = 11155111  # Sepolia

[config]
# Chainlink AggregatorV3Interface address (ETH/USD on Sepolia).
oracle_address = "0x694AA1769357215DE4FAC081bf1f309aDC325306"
decimals = "8"
# Trigger price in the oracle's native decimal units. Below this,
# we sell.
trigger_price = "2500.00"
# CoW order parameters (signed by the owner off-chain ahead of
# time, then the module submits the pre-signed body on trigger).
owner = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
sell_token = "0x6810e776880C02933D47DB1b9fc05908e5386b96"  # GNO on Sepolia
buy_token = "0xfff9976782d46cc05630d1f6ebab18b2324d6b14"   # WETH on Sepolia
sell_amount_wei = "1000000000000000000"  # 1 GNO
buy_amount_wei  = "300000000000000000"   # 0.3 ETH
valid_to_seconds = "4294967295"          # u32::MAX (no expiry)
```

Two patterns worth noting:

- **`required` matches the WIT imports the module uses.** The
  engine enforces this at instantiation - declaring a capability
  the module does not use is fine; missing a capability the module
  does use is a hard error.
- **`[config]` values are stringly-typed in 0.2.** Your `init`
  parses them; the M3 SDK's `OnceLock<Settings>` pattern (see
  `price-alert`) is the recommended idiom.

## 3. Write the strategy (60 minutes)

The strategy logic splits into two layers:

- A pure function that takes `&impl Host` and runs the decision
  tree. This is what your tests exercise - no `wit-bindgen`, no
  `wasmtime`, fast iteration.
- A thin `Guest` impl in `lib.rs` that adapts the wit-bindgen-
  generated host imports into a struct implementing
  `shepherd_sdk::host::Host`.

### 3a. The pure strategy (30 minutes)

Sketch in `src/strategy.rs`:

```rust
use alloy_primitives::{Address, I256};
use alloy_sol_types::{SolCall, sol};
use shepherd_sdk::chain::{eth_call_params, parse_eth_call_result};
use shepherd_sdk::host::{Host, HostError, LogLevel};
use shepherd_sdk::prelude::*;

sol! {
    interface AggregatorV3 {
        function latestRoundData() external view returns (
            uint80, int256 answer, uint256, uint256, uint80
        );
    }
}

pub struct Settings {
    pub oracle_address: Address,
    pub trigger_price_scaled: I256,
    pub owner: Address,
    pub sell_token: Address,
    pub buy_token: Address,
    pub sell_amount: U256,
    pub buy_amount: U256,
    pub valid_to: u32,
}

pub fn on_block<H: Host>(
    host: &H,
    chain_id: u64,
    settings: &Settings,
) -> Result<(), HostError> {
    // 1. Read the oracle.
    let call = AggregatorV3::latestRoundDataCall {};
    let params = eth_call_params(&settings.oracle_address, &call.abi_encode());
    let result_json = host.request(chain_id, "eth_call", &params)?;
    let Some(bytes) = parse_eth_call_result(&result_json) else {
        host.log(LogLevel::Warn, "stop-loss: cannot decode oracle result");
        return Ok(());
    };
    let decoded = AggregatorV3::latestRoundDataCall::abi_decode_returns(&bytes)
        .map_err(|e| HostError {
            domain: "stop-loss".into(),
            kind: shepherd_sdk::host::HostErrorKind::InvalidInput,
            code: 0,
            message: format!("oracle decode: {e}"),
            data: None,
        })?;
    let price = decoded.answer;

    // 2. Are we above trigger? Stay idle.
    if price > settings.trigger_price_scaled {
        host.log(LogLevel::Info, &format!("stop-loss idle (price={price})"));
        return Ok(());
    }

    // 3. Dedup: did we already submit?
    let dedup_key = format!("submitted:{:#x}", settings.owner);
    if host.get(&dedup_key)?.is_some() {
        host.log(LogLevel::Info, "stop-loss: already submitted, skipping");
        return Ok(());
    }

    // 4. Build the OrderCreation. (See `twap-monitor` for the full
    //    helper; for tutorial brevity we elide the JSON encoding.)
    let body = build_order_body(settings)?;
    let uid = host.submit_order(chain_id, &body)?;

    // 5. Persist + log.
    host.set(&dedup_key, uid.as_bytes())?;
    host.log(LogLevel::Warn, &format!("stop-loss triggered, uid={uid}"));
    Ok(())
}

fn build_order_body(_s: &Settings) -> Result<Vec<u8>, HostError> {
    // Cross-reference: `modules/twap-monitor/src/lib.rs::build_order_creation`
    // shows the full assembly path using cowprotocol::OrderCreation::
    // from_signed_order_data + serde_json::to_vec.
    todo!("see modules/twap-monitor for the canonical assembly")
}
```

The shape to internalise:

- **Every interaction with the world goes through `host`.** No
  global wit-bindgen functions in the strategy; everything is a
  method on `&impl Host`.
- **The function is pure-ish:** the only effects are through the
  host trait. Tests in §3c run this function against `MockHost`
  and assert on the side effects (calls + log lines + state writes).
- **Errors propagate but the loop should not abort on transient
  failure.** Wrap upstream calls so a single bad event does not
  poison the supervisor - see `price-alert`'s warn-and-return
  pattern.

### 3b. The Guest adapter (15 minutes)

`src/lib.rs` adapts wit-bindgen's free functions into a struct that
implements `Host`. This is mechanical and almost identical across
modules:

```rust
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../../wit/nexum-host", "../../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

mod strategy;

use std::sync::OnceLock;
use shepherd_sdk::host::{
    ChainHost, CowApiHost, HostError as SdkHostError, HostErrorKind as SdkHostErrorKind,
    LocalStoreHost, LogLevel as SdkLogLevel, LoggingHost,
};

static SETTINGS: OnceLock<strategy::Settings> = OnceLock::new();

struct WitBindgenHost;

impl ChainHost for WitBindgenHost {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, SdkHostError> {
        nexum::host::chain::request(chain_id, method, params).map_err(convert_err)
    }
}

impl LocalStoreHost for WitBindgenHost {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SdkHostError> {
        nexum::host::local_store::get(key).map_err(convert_err)
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<(), SdkHostError> {
        nexum::host::local_store::set(key, value).map_err(convert_err)
    }
    fn delete(&self, key: &str) -> Result<(), SdkHostError> {
        nexum::host::local_store::delete(key).map_err(convert_err)
    }
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, SdkHostError> {
        nexum::host::local_store::list_keys(prefix).map_err(convert_err)
    }
}

impl CowApiHost for WitBindgenHost {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, SdkHostError> {
        shepherd::cow::cow_api::submit_order(chain_id, body).map_err(convert_err)
    }
}

impl LoggingHost for WitBindgenHost {
    fn log(&self, level: SdkLogLevel, message: &str) {
        nexum::host::logging::log(convert_level(level), message);
    }
}

fn convert_err(e: HostError) -> SdkHostError {
    SdkHostError {
        domain: e.domain,
        kind: match e.kind {
            HostErrorKind::Unsupported => SdkHostErrorKind::Unsupported,
            HostErrorKind::Unavailable => SdkHostErrorKind::Unavailable,
            HostErrorKind::Denied => SdkHostErrorKind::Denied,
            HostErrorKind::RateLimited => SdkHostErrorKind::RateLimited,
            HostErrorKind::Timeout => SdkHostErrorKind::Timeout,
            HostErrorKind::InvalidInput => SdkHostErrorKind::InvalidInput,
            HostErrorKind::Internal => SdkHostErrorKind::Internal,
        },
        code: e.code,
        message: e.message,
        data: e.data,
    }
}

fn convert_level(l: SdkLogLevel) -> nexum::host::logging::Level {
    use nexum::host::logging::Level::*;
    match l {
        SdkLogLevel::Trace => Trace,
        SdkLogLevel::Debug => Debug,
        SdkLogLevel::Info => Info,
        SdkLogLevel::Warn => Warn,
        SdkLogLevel::Error => Error,
    }
}

struct StopLoss;

impl Guest for StopLoss {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        let parsed = strategy::Settings::from_config(&config)
            .map_err(|e| HostError {
                domain: "stop-loss".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: e,
                data: None,
            })?;
        let _ = SETTINGS.set(parsed);
        nexum::host::logging::log(
            nexum::host::logging::Level::Info,
            "stop-loss: init ok",
        );
        Ok(())
    }

    fn on_event(event: nexum::host::types::Event) -> Result<(), HostError> {
        let Some(s) = SETTINGS.get() else {
            return Ok(());
        };
        if let nexum::host::types::Event::Block(b) = event {
            strategy::on_block(&WitBindgenHost, b.chain_id, s).map_err(|e| HostError {
                domain: e.domain,
                kind: match e.kind {
                    SdkHostErrorKind::Unsupported => HostErrorKind::Unsupported,
                    SdkHostErrorKind::Unavailable => HostErrorKind::Unavailable,
                    SdkHostErrorKind::Denied => HostErrorKind::Denied,
                    SdkHostErrorKind::RateLimited => HostErrorKind::RateLimited,
                    SdkHostErrorKind::Timeout => HostErrorKind::Timeout,
                    SdkHostErrorKind::InvalidInput => HostErrorKind::InvalidInput,
                    SdkHostErrorKind::Internal => HostErrorKind::Internal,
                },
                code: e.code,
                message: e.message,
                data: e.data,
            })?;
        }
        Ok(())
    }
}

export!(StopLoss);
```

The conversion code looks heavy but is one-time boilerplate. Copy
it verbatim into every new module; only the `Guest` impl and
`SETTINGS` initialisation change per module.

### 3c. Unit tests against `MockHost` (15 minutes)

In `src/strategy.rs`, append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_sdk::host::*;
    use shepherd_sdk_test::MockHost;

    fn settings(trigger_scaled: i64) -> Settings {
        Settings {
            oracle_address: "0x694AA1769357215DE4FAC081bf1f309aDC325306".parse().unwrap(),
            trigger_price_scaled: I256::try_from(trigger_scaled).unwrap(),
            owner: "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".parse().unwrap(),
            sell_token: Address::ZERO,
            buy_token: Address::ZERO,
            sell_amount: U256::ZERO,
            buy_amount: U256::ZERO,
            valid_to: 0xffff_ffff,
        }
    }

    /// Encode a Chainlink `latestRoundData` return for tests.
    fn oracle_returns(answer: i64) -> String {
        let returns = AggregatorV3::latestRoundDataCall::abi_encode_returns(&(
            0u128,
            I256::try_from(answer).unwrap(),
            U256::ZERO,
            U256::ZERO,
            0u128,
        ));
        let hex = alloy_primitives::hex::encode_prefixed(returns);
        format!("\"{hex}\"")
    }

    #[test]
    fn idle_when_price_above_trigger() {
        let host = MockHost::new();
        let s = settings(/*trigger*/ 1_000);
        // Oracle returns 2000 (above the 1000 trigger).
        host.chain.respond_to(
            "eth_call",
            &shepherd_sdk::chain::eth_call_params(
                &s.oracle_address,
                &AggregatorV3::latestRoundDataCall {}.abi_encode(),
            ),
            Ok(oracle_returns(2000)),
        );

        on_block(&host, 11_155_111, &s).unwrap();

        assert_eq!(host.cow_api.call_count(), 0);
        assert!(host.logging.contains("stop-loss idle"));
    }

    #[test]
    fn triggers_below_threshold_once() {
        let host = MockHost::new();
        let s = settings(/*trigger*/ 1_000);
        host.chain.respond_to(
            "eth_call",
            &shepherd_sdk::chain::eth_call_params(
                &s.oracle_address,
                &AggregatorV3::latestRoundDataCall {}.abi_encode(),
            ),
            Ok(oracle_returns(500)),
        );
        host.cow_api.respond(Ok("0xdeadbeef".into()));

        // First block: submits.
        on_block(&host, 11_155_111, &s).unwrap();
        assert_eq!(host.cow_api.call_count(), 1);
        assert!(host.logging.contains("triggered"));

        // Second block at the same price: dedup'd by the
        // `submitted:` key.
        on_block(&host, 11_155_111, &s).unwrap();
        assert_eq!(host.cow_api.call_count(), 1);
        assert!(host.logging.contains("already submitted"));
    }
}
```

Run with `cargo test -p stop-loss`. Both tests should pass on a
plain host - no wasm toolchain involved.

The takeaway: any time you can express a behaviour as "given this
host state, do that", the `MockHost` route is faster to iterate
than a full engine restart.

## 4. Build the `.wasm` artefact (5 minutes)

```sh
cargo build --target wasm32-wasip2 --release -p stop-loss
ls -lh target/wasm32-wasip2/release/stop_loss.wasm
```

Expected size: 250–350 KB. If it ballooned past ~500 KB, look at
`cargo tree -p stop-loss --target wasm32-wasip2` - usually a fresh
dependency pulled `reqwest` or `tokio` into the wasm graph.

## 5. Wire `engine.toml` and run it (10 minutes)

Add an RPC endpoint for Sepolia in `engine.toml`:

```toml
[chains.11155111]
rpc_url = "wss://ethereum-sepolia-rpc.publicnode.com"
```

WebSocket is required because the `[[subscription]]` is `kind =
"block"` and block subscriptions ride `eth_subscribe`.

Run the engine pointed at your new module:

```sh
cargo run -p nexum-cli -- \
  target/wasm32-wasip2/release/stop_loss.wasm \
  modules/examples/stop-loss/module.toml
```

Expected output on first run (one log per:

- `init`: `stop-loss: init ok`
- on each new block: either `stop-loss idle` (price above trigger)
  or `stop-loss triggered, uid=0x...` then `already submitted`
  on subsequent blocks.

If the engine reports `unsupported` for any capability, double-
check that the module's `[capabilities].required` list matches the
imports the strategy actually uses.

## 6. Where to go from here (10 minutes)

- **Production hardening**: replace the synthetic `init` with the
  per-module fuel + memory limits in `engine.toml::[engine.limits]`
  (see [`docs/deployment.md`](./deployment.md)).
- **Real order assembly**: the `build_order_body` `todo!` in §3a
  is the only piece this tutorial elided. Cross-reference
  [`modules/twap-monitor/src/lib.rs::build_order_creation`]  - 
  it's the canonical assembly path
  (`cowprotocol::OrderCreation::from_signed_order_data` +
  `serde_json::to_vec`).
- **Tests for the adapter layer**: the wit-bindgen ↔ `Host`
  conversion functions are mechanical but worth a smoke test that
  forces each enum variant through. See `shepherd-sdk-test`'s own
  tests for the pattern.
- **Multi-chain operation**: change `[[subscription]].chain_id` and
  the `engine.toml::[chains.<id>]` entry. The strategy stays
  unchanged because every host call already passes `chain_id`
  through.

## Time-budget check

If a section ran much longer than the rough estimate above, please
file an issue tagged `docs/tutorial` with the section that dragged.
The target is **<4h cold from a fresh checkout to a successful run
in §5**, and we tighten the prose against feedback.

## Reference index

- SDK overview: [`docs/sdk.md`](./sdk.md)
- Deployment runbook: [`docs/deployment.md`](./deployment.md)
- ADR-0001 (`engine.toml` vs `module.toml` split)
- ADR-0006 (TWAP / EthFlow as guest modules, no specialised
  WIT interfaces)
- ADR-0007 (push protocol primitives to `cow-rs` first)
- Worked examples: [`price-alert`](../modules/examples/price-alert/),
  [`balance-tracker`](../modules/examples/balance-tracker/),
  [`twap-monitor`](../modules/twap-monitor/),
  [`ethflow-watcher`](../modules/ethflow-watcher/)
