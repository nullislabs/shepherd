# shepherd-sdk

Guest-side SDK for [Shepherd](https://github.com/nullislabs/shepherd) modules.

`shepherd-sdk` is the shared companion to each module's
`wit_bindgen::generate!` invocation: the module keeps its own
wit-bindgen call (which emits the world-specific `Guest` trait and
host-import shims into the module's own crate) and pulls helpers,
typed primitives, and the host trait seam from here.

## Quick tour

```rust
use shepherd_sdk::prelude::*;
use shepherd_sdk::cow::{gpv2_to_order_data, classify_api_error, RetryAction};
use shepherd_sdk::chain::{eth_call_params, parse_eth_call_result};
```

| Module | What it provides |
|---|---|
| `prelude` | One-liner `use ::*` for alloy primitives + cowprotocol order / signing / orderbook surface. |
| `cow::order` | `gpv2_to_order_data` - `GPv2OrderData` -> typed `OrderData`. |
| `cow::composable` | `sol! IConditionalOrder` errors + `PollOutcome` + `decode_revert`. |
| `cow::error` | `RetryAction` enum + `classify_api_error` + `try_decode_api_error`. |
| `chain::eth_call` | `eth_call_params`, `parse_eth_call_result`, `decode_revert_hex`. |
| `host` | `Host` trait seam (`ChainHost` / `LocalStoreHost` / `CowApiHost` / `LoggingHost`) + host-neutral `HostError`. |
| `http` | Synchronous `fetch` over wasi:http (guest target only) + `Fetch` trait seam + `FetchError` distinguishing allowlist denials from transport failures. |

## Testing modules host-free

Add the companion `shepherd-sdk-test` crate as a dev-dep and write
your strategy function against `&impl shepherd_sdk::host::Host`:

```rust,ignore
use shepherd_sdk::host::*;

pub fn handle_block<H: Host>(host: &H, chain_id: u64) -> Result<(), HostError> {
    let result = host.request(chain_id, "eth_blockNumber", "[]")?;
    host.log(LogLevel::Info, &format!("got {result}"));
    Ok(())
}
```

Tests against `MockHost` then run without `wit-bindgen` or
`wasmtime`:

```rust,ignore
let host = MockHost::new();
host.chain.respond_to("eth_blockNumber", "[]", Ok("\"0x1\"".into()));
handle_block(&host, 1).unwrap();
assert_eq!(host.chain.call_count(), 1);
```

## Why no `wit_bindgen::generate!` in the SDK

The macro emits types into the calling crate (the module's cdylib).
Re-exporting wit-bindgen output from a library would duplicate
symbols and break the component-export contract. Helpers in this
SDK take primitive arguments (`&[u8]`, `&str`, `Option<&str>`) so
the SDK stays world-neutral; modules unpack their wit-bindgen
`HostError` / `Log` into primitives at the call site. Trade-off
documented in ADR-0006 and ADR-0007 in `docs/adr/`.

## Layout

```
crates/shepherd-sdk/
├── src/
│   ├── lib.rs           crate root + intra-doc links
│   ├── prelude.rs       bulk re-exports
│   ├── cow/
│   │   ├── mod.rs
│   │   ├── order.rs     gpv2_to_order_data
│   │   ├── composable.rs IConditionalOrder + PollOutcome + decode_revert
│   │   └── error.rs     RetryAction + classify_api_error
│   ├── chain/
│   │   ├── mod.rs
│   │   └── eth_call.rs  eth_call_params + parse_eth_call_result
│   ├── host.rs          trait seam + SDK HostError
│   └── http.rs          wasi:http fetch helper + Fetch seam + FetchError
└── README.md            you are here
```

## Generating docs locally

```sh
RUSTDOCFLAGS="-D warnings -D missing-docs" cargo doc -p shepherd-sdk --no-deps --open
```

The CI gate `cargo doc -p shepherd-sdk --no-deps` runs under those
flags, so all public items carry doc comments and intra-doc links
resolve.
