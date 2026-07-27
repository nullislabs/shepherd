# RPC Namespace Design: the `chain` interface

Modules reach chain state through one host function, `chain.request`, plus a batch form `chain.request-batch`. A single generic JSON-RPC entry point means no WIT change per method: the guest SDK layers an alloy `Provider` on top, so every read method on the permitted surface works without host-side per-method plumbing.

## The WIT interface

`nexum:host/chain` (`wit/nexum-host/chain.wit`):

```wit
interface chain {
    use types.{chain-id, fault};

    record rpc-error { code: s32, message: string, data: option<list<u8>> }
    variant chain-error { fault(fault), rpc(rpc-error) }

    record rpc-request { method: string, params: string }
    variant rpc-result { ok(string), err(chain-error) }

    request: func(chain-id: chain-id, method: string, params: string)
        -> result<string, chain-error>;
    request-batch: func(chain-id: chain-id, requests: list<rpc-request>)
        -> result<list<rpc-result>, chain-error>;
}
```

`method` carries the namespace prefix (`eth_call`). `params` and the success value are JSON strings; the host frames the id/jsonrpc envelope. A failure is a `chain-error`: either a shared host `fault` (`unavailable`, `rate-limited`, `timeout`, `denied`, `invalid-input`, ...) that a module matches for retry and backoff, or a structured `rpc` case carrying the node code and the host-decoded revert bytes so a revert reads without parsing numeric JSON-RPC codes by hand. `request-batch` runs several calls against one chain in a single round trip where the transport supports it, falling back to sequential `request` otherwise; the result list matches `requests` in length and order, each entry independently `ok` or `err`.

## Permitted method surface

The reference server host forwards only a closed read-only set, the `ChainMethod` enum in `nexum-world`. Host dispatch and the guest-side allowlist re-export the same type, so the two cannot drift. A method outside the set, which is every signing or mutating method, is refused with a `denied` fault before it reaches the provider.

The surface is `eth_blockNumber`, `eth_call`, `eth_chainId`, `eth_estimateGas`, `eth_feeHistory`, `eth_gasPrice`, `eth_maxPriorityFeePerGas`, `eth_getBalance`, `eth_getBlockByHash`, `eth_getBlockByNumber`, `eth_getBlockReceipts`, `eth_getCode`, `eth_getLogs`, `eth_getProof`, `eth_getStorageAt`, `eth_getTransactionByHash`, `eth_getTransactionCount`, `eth_getTransactionReceipt`, and `net_version`.

Enforcement is host-side string-to-`ChainMethod` resolution, not a compile-time guarantee. The Component Model already sandboxes I/O, so a chain-capable module can only call `chain.request`, and the closed surface adds method-level defence in depth on top.

## Signing

`chain.request` neither signs nor delegates signing; signing methods simply fall outside the read surface. Signing is the separate `nexum:host/identity` interface:

```wit
interface identity {
    use types.{fault};
    accounts: func() -> result<list<list<u8>>, fault>;
    sign: func(account: list<u8>, message: list<u8>) -> result<list<u8>, fault>;
    sign-typed-data: func(account: list<u8>, typed-data: string) -> result<list<u8>, fault>;
}
```

`accounts` returns the 20-byte addresses the host will sign for (an empty list means no signing capability); `sign` applies `personal_sign` semantics (the EIP-191 prefix) and returns a 65-byte signature; `sign-typed-data` signs an EIP-712 JSON payload. `nexum-sdk`'s `IdentityHost` trait mirrors the interface one-for-one.

## Guest SDK: the alloy provider seam

`nexum_sdk::chain` fronts `chain.request` with an alloy `Provider`, so module code calls typed provider methods instead of hand-building JSON-RPC:

- `HostTransport<H>` implements alloy's `Service<RequestPacket>` over any `ChainHost`, dispatching single requests through `chain.request` and batches through `chain.request-batch`.
- `ProviderHost::provider(chain)` mints an alloy `RootProvider` over that transport, blanket-implemented for every cloneable `ChainHost`.
- `block_on` drives the returned futures. The transport is a synchronous WIT import, so a future resolves on its first poll; a `Pending` panics, signalling that an alloy layer awaiting a reactor or timer has been introduced.

```rust
use nexum_sdk::chain::{Chain, ProviderHost, block_on};

let provider = host.provider(Chain::mainnet());
let block = block_on(provider.get_block_number())?;
```

A module that only needs raw JSON calls `host.request(chain_id, method, params)` directly, or reaches for the `chain::eth_call_params` and `parse_eth_call_result` helpers.

## Handlers are synchronous

`#[nexum_sdk::module]` dispatches events to synchronous named handlers (`init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`, `on_custom`); an absent handler is a no-op for that event. There is no `block_on` wrapper around a handler and no provider injection: a handler that wants the alloy provider builds it with `host.provider(chain)` and drives calls with `block_on` itself. Keeper handlers are the exception, where `#[videre_sdk::keeper]` allows `async fn` completed by `videre_sdk::client::poll_once`; see [doc 05](05-sdk-design.md).

## Order submission

Submitting an order or intent is not a chain namespace. It is the `videre:venue` venue-adapter contract: a keeper calls `videre:venue/client`, and the installed venue adapter (for CoW, `shepherd/crates/cow-venue`) speaks the orderbook wire. See [doc 08](08-platform-generalisation.md) for the layer model and [doc 05](05-sdk-design.md) for the venue SDK.

## Testing

`nexum_sdk_test::MockHost` implements the host traits (`ChainHost`, `IdentityHost`, `LocalStoreHost`, ...); module logic tests against `&impl Host` as plain native Rust, with no `wasm32-wasip2` target and no wasmtime instance. Provider-level tests wrap a stub `ChainHost` in `HostTransport` and drive it with `block_on`.
