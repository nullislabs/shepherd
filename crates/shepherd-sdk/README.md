# shepherd-sdk

CoW-domain guest SDK for [Shepherd](https://github.com/nullislabs/shepherd) modules.

`shepherd-sdk` layers the CoW Protocol surface on top of the generic
`nexum-sdk`: the module keeps its own `wit_bindgen::generate!` call
(which emits the world-specific `Guest` trait and host-import shims
into the module's own crate), pulls the host trait seam and generic
helpers from `nexum-sdk`, and pulls the CoW types and helpers from
here. Nothing is re-exported between the two crates; modules import
each directly.

## Quick tour

```rust
use nexum_sdk::prelude::*;
use shepherd_sdk::prelude::*;
use shepherd_sdk::cow::{gpv2_to_order_data, classify_api_error, RetryAction};
```

| Module | What it provides |
|---|---|
| `prelude` | One-liner `use ::*` for cowprotocol order / signing / orderbook surface (alloy primitives come from `nexum_sdk::prelude`). |
| `cow` | `CowApiHost` trait for `shepherd:cow/cow-api` + the `CowHost` bound over the core `nexum_sdk::host::Host`. |
| `cow::order` | `gpv2_to_order_data` - `GPv2OrderData` -> typed `OrderData`. |
| `cow::composable` | `sol! IConditionalOrder` errors + `PollOutcome` + `decode_revert` + `decode_revert_hex`. |
| `cow::error` | `CowApiError` (mirror of `cow-api-error`: `Fault` / `Http` / `Rejected`) + `RetryAction` enum + `classify_api_error` over an `OrderRejection`. |
| `cow::app_data` | `resolve_app_data` - appData hash -> canonical JSON document. |
| `wit_bindgen_macro` | `bind_cow_host_via_wit_bindgen!` - the generic `WitBindgenHost` adapter plus the `CowApiHost` impl. |

## Testing modules host-free

Add the companion `shepherd-sdk-test` crate as a dev-dep and write
your strategy function against `&impl shepherd_sdk::cow::CowHost`
(or `&impl nexum_sdk::host::Host` if it never touches the
orderbook). Tests against `MockHost` then run without `wit-bindgen`
or `wasmtime`:

```rust,ignore
let host = shepherd_sdk_test::MockHost::new();
host.cow_api.respond(Ok("0xuid".into()));
submit_watch(&host, 1).unwrap();
assert_eq!(host.cow_api.call_count(), 1);
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
│   ├── lib.rs               crate root + intra-doc links
│   ├── prelude.rs           cowprotocol bulk re-exports
│   ├── cow/
│   │   ├── mod.rs           CowApiHost + CowHost
│   │   ├── order.rs         gpv2_to_order_data
│   │   ├── composable.rs    IConditionalOrder + PollOutcome + decode_revert(_hex)
│   │   ├── error.rs         RetryAction + classify_api_error
│   │   └── app_data.rs      resolve_app_data
│   └── wit_bindgen_macro.rs bind_cow_host_via_wit_bindgen!
└── README.md                you are here

(The generic surface - host trait seam, chain / config / address
helpers, http, tracing - lives in the sibling `nexum-sdk` crate.)
```

## Generating docs locally

```sh
RUSTDOCFLAGS="-D warnings -D missing-docs" cargo doc -p shepherd-sdk -p nexum-sdk --no-deps --open
```

The CI gate `cargo doc -p shepherd-sdk --no-deps` runs under those
flags, so all public items carry doc comments and intra-doc links
resolve.
