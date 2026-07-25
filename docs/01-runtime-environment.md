# Runtime Environment: wasmtime + Component Model

## Version Target

**wasmtime 45.x**, requiring **Rust 1.90.0+**. Guest bindings pin `wit-bindgen` 0.57.x.

## Component Model

The engine targets the Component Model, not raw core modules, for:

- **Structural sandboxing.** A component compiled against a WIT world with no filesystem import cannot access the filesystem: enforced at the type level, not by omission of host functions.
- **Type-safe contract.** The WIT definition is the API spec; host and guest get generated bindings (`wasmtime::component::bindgen!`, `wit_bindgen::generate!`).
- **Resource types.** Opaque handles with lifecycle management via `ResourceTable`.
- **Multi-language guests.** Rust, C/C++, Go, JavaScript, Python all produce valid components against the same WIT world.
- **No WASI required.** The pure `nexum:host` world imports exactly the host APIs; zero WASI imports means zero implicit capabilities.

The engine uses wasmtime's basic async host functions (`func_wrap_async`), not the still-evolving Component Model native async (`stream<T>`, `future<T>`).

## Core Concepts

### Engine

Global, thread-safe compilation environment. One per process.

```rust
let mut config = Config::new();
config.async_support(true);
config.consume_fuel(true);
config.epoch_interruption(true);
let engine = Engine::new(&config)?;
```

### Store

Per-module execution context. Holds component instances, host state (`NexumHostState`), fuel counters, resource limits, and the `ResourceTable` for handle management.

```rust
let mut store = Store::new(&engine, NexumHostState {
    table: ResourceTable::new(),
    rpc: alloy_provider,
    db: redb_handle,
    // ...
});
store.set_fuel(10_000)?;
store.epoch_deadline_async_yield_and_update(10); // yield after 10 epochs (~1s at 100ms tick)
```

### Component -> InstancePre -> Instance

1. **Component**: compiled from `.wasm` component binary (expensive, cacheable, thread-safe).
2. **Linker**: binds host implementations of our WIT interfaces.
3. **InstancePre**: pre-validated component + linker (reusable across stores).
4. **Instance**: a live component in a specific store, from which we call exports.

```rust
let component = Component::from_file(&engine, "twap_monitor.wasm")?;
let mut linker = Linker::new(&engine);
EventModule::add_to_linker(&mut linker, |state| state)?;

// Pre-validate once, instantiate many times (one per store)
let pre = linker.instantiate_pre(&component)?;
let bindings = EventModule::instantiate_pre(&mut store, &pre)?;
```

## WIT Worlds

The **universal** package `nexum:host` defines platform-agnostic interfaces and the `event-module` world. CoW Protocol support layers on top: `shepherd:cow` carries the `cow-events` enum, and order submission is the `videre:venue` venue-adapter contract (below).

### Universal Package: `nexum:host@0.1.0`

The `nexum:host` package is the single source of truth for the universal host-guest contract. It defines a custom world with **no WASI imports**:

```wit
package nexum:host@0.1.0;

interface types {
    type chain-id = u64;

    record block {
        chain-id: chain-id,
        number: u64,
        hash: list<u8>,
        timestamp: u64,           // milliseconds since Unix epoch, UTC
    }

    record chain-log {
        address: list<u8>,
        topics: list<list<u8>>,
        data: list<u8>,
        block-hash: option<list<u8>>,        // block-scoped fields absent on a pending log
        block-number: option<u64>,
        block-timestamp: option<u64>,
        transaction-hash: option<list<u8>>,
        transaction-index: option<u64>,
        log-index: option<u64>,
        removed: bool,
    }

    // A batch of logs from one subscription; the alloy log carries no chain
    // id, so it sits here once and every log shares the subscription's chain.
    record chain-logs {
        chain-id: chain-id,
        logs: list<chain-log>,
    }

    record tick {
        fired-at: u64,            // milliseconds since Unix epoch, UTC
    }

    record message {
        content-topic: string,
        payload: list<u8>,
        timestamp: u64,           // milliseconds since Unix epoch, UTC
        sender: option<list<u8>>,
    }

    variant event {
        block(block),
        chain-logs(chain-logs),
        tick(tick),
        message(message),
    }

    /// Opaque config from module.toml [config] section. All TOML scalars are
    /// flattened to their string form by the host. A typed `config-value`
    /// variant is on the 0.3 roadmap, bundled with the manifest parser work.
    type config = list<tuple<string, string>>;

    /// The cross-domain failure vocabulary richer interfaces embed as a
    /// case. Each payload-bearing case carries a human-readable detail;
    /// `rate-limited` carries structured backoff guidance instead.
    variant fault {
        unsupported(string),       // capability declared but not provisioned
        unavailable(string),       // capability exists, backend is down/offline
        denied(string),            // user or policy rejected
        rate-limited(rate-limit),
        timeout,
        invalid-input(string),
        internal(string),
    }

    /// Backoff guidance for a `fault.rate-limited`.
    record rate-limit {
        retry-after-ms: option<u64>,
    }
}

interface chain {
    use types.{chain-id, fault};

    /// A JSON-RPC error response from the upstream node: `code` is the
    /// node-reported numeric (typically -32000 for an `eth_call` revert)
    /// and `data` is the decoded revert payload, hex-decoded host-side.
    record rpc-error {
        code: s32,
        message: string,
        data: option<list<u8>>,
    }

    /// Failure of a chain call: either a shared host `fault` (transport
    /// down, timed out, denied, ...) or a structured JSON-RPC error.
    variant chain-error {
        fault(fault),
        rpc(rpc-error),
    }

    /// Execute a JSON-RPC request against the specified chain.
    ///
    /// The host forwards the request to the configured alloy provider for
    /// the given chain, applying timeout/retry/rate-limit/fallback middleware
    /// transparently. Method includes the namespace prefix (e.g. "eth_call").
    ///
    /// `params` and the success return value are JSON-encoded strings matching
    /// the JSON-RPC spec. The host handles id/jsonrpc framing; the guest only
    /// provides method + params and receives the `result` field.
    ///
    /// See doc 07 (RPC Namespace Design) for the full design rationale: a
    /// single generic function replaces per-method WIT functions, enabling
    /// the SDK to implement alloy's Transport trait and expose the full
    /// alloy Provider API (80+ methods) to guest modules with zero WIT churn.
    ///
    /// Note: signing RPC methods (eth_sendTransaction, eth_accounts,
    /// eth_signTypedData_v4, personal_sign) are intercepted by the host and
    /// delegated to the identity backend. The module does not need to handle
    /// key material directly when using chain for transactions.
    request: func(chain-id: chain-id, method: string, params: string)
        -> result<string, chain-error>;

    /// A single JSON-RPC request to be executed as part of a batch.
    record rpc-request {
        method: string,
        params: string,
    }

    /// Result of a single request inside a batch. Each entry is independent;
    /// one failing call does not abort the others.
    variant rpc-result {
        ok(string),
        err(chain-error),
    }

    /// Additive 0.2 method: batched JSON-RPC. The alloy-backed HostTransport
    /// routes RequestPacket::Batch through this - `provider.multicall(...)`
    /// actually batches on the wire in 0.2. Hosts that cannot batch natively
    /// MUST fall back to sequential `request` calls; the returned list is
    /// the same length as `requests` and in the same order.
    request-batch: func(chain-id: chain-id, requests: list<rpc-request>)
        -> result<list<rpc-result>, chain-error>;
}

interface identity {
    use types.{fault};

    /// Get available signing accounts (20-byte Ethereum addresses).
    accounts: func() -> result<list<list<u8>>, fault>;

    /// Sign a message with `personal_sign` semantics. The host MUST prepend
    /// the EIP-191 prefix (`\x19Ethereum Signed Message:\n<len>`) before
    /// hashing and signing. Hosts MUST NOT expose a raw-bytes signing path
    /// through this function - a raw signer can be tricked into signing
    /// EIP-155 transactions or EIP-712 payloads disguised as plain bytes.
    ///
    /// Returns a 65-byte ECDSA secp256k1 signature (r || s || v).
    ///
    /// A separate raw-bytes signing primitive, gated by an explicit
    /// capability, is on the 0.3 roadmap.
    sign: func(account: list<u8>, message: list<u8>) -> result<list<u8>, fault>;

    /// Sign EIP-712 typed data with the specified account.
    /// `typed-data` is the JSON-encoded EIP-712 TypedData structure.
    sign-typed-data: func(account: list<u8>, typed-data: string) -> result<list<u8>, fault>;
}

interface local-store {
    use types.{fault};
    get: func(key: string) -> result<option<list<u8>>, fault>;
    set: func(key: string, value: list<u8>) -> result<_, fault>;
    delete: func(key: string) -> result<_, fault>;
    list-keys: func(prefix: string) -> result<list<string>, fault>;
}

interface logging {
    enum level { trace, debug, info, warn, error }
    log: func(level: level, message: string);
}

/// The universal event-driven module world. Platform-agnostic: no CoW,
/// no domain-specific imports. Suitable for any web3 automation.
///
/// In 0.2 this imports all six primitives - the identity import was
/// missing from the 0.1 WIT despite being part of the documented primitive
/// taxonomy, and is now present.
world event-module {
    import chain;
    import identity;
    import local-store;
    import remote-store;
    import messaging;
    import logging;

    /// Called once on load. Receives typed config from module.toml.
    export init: func(config: types.config) -> result<_, fault>;

    /// Called for each subscribed event.
    export on-event: func(event: types.event) -> result<_, fault>;
}
```

In addition to the six core imports, 0.2 publishes one additive optional capability - `http` (allowlisted outbound HTTP) - which modules declare in their `module.toml` `[capabilities]` section. The declaration is a manifest concern only: the capability is serviced by the standard `wasi:http/outgoing-handler` interface, not a `nexum:host` one. 0.2 also publishes the experimental **`query-module`** world for request/response modules; the WIT is stable but no host implementation ships in 0.2, so it's a target for `MockHost` testing only.

### CoW Protocol packages

`shepherd:cow@0.1.0` carries a single interface, `cow-events`: the canonical decoded on-chain event enum whose variants pin each CoW Solidity signature and its topic-0 hash. Keeper constants and module manifests are parity-tested against it.

```wit
package shepherd:cow@0.1.0;

interface cow-events {
    enum cow-event {
        conditional-order-created,   // ComposableCoW registration
        conditional-order-removed,   // ComposableCoW v2 removal
        order-placement,             // CoWSwapOnchainOrders (EthFlow)
    }
}
```

Order submission is not a host interface. It is the `videre:venue@0.1.0` venue-adapter contract: a keeper calls `videre:venue/client` (`quote` / `submit` / `observe` / `status` / `cancel`) naming a venue by string, and each installed adapter component exports the provider face for one venue over scoped transport only. The CoW venue is the `cow-venue` crate. See doc 08 for the venue layer.

### Key properties

- **Constrained WASI** - the WASI p2 surface linked into every store includes `wasi:clocks` and `wasi:random` ambiently; there is no filesystem grant and no inbound network. The only network path is allowlisted outbound HTTP through `wasi:http/outgoing-handler`, available to modules that declare the `http` capability in the manifest's `[capabilities]` section. The `wasi:sockets` bindings are linked as part of the p2 surface but stay inert because the WASI context grants no network.
- **All I/O through host interfaces** - RPC reads, identity/signing, local-store, messaging, logging; venue submission through `videre:venue/client`.
- **Generic JSON-RPC passthrough** - the `chain` interface exposes a single `request` function (plus an additive `request-batch`). The SDK implements alloy's `Transport` trait on top of it, giving modules the full alloy `Provider` API. See doc 07 for details.
- **Identity as a first-class primitive** - the `identity` interface provides key management and signing. The `chain` host implementation depends on `identity`: signing RPC methods (`eth_sendTransaction`, `eth_accounts`, `eth_signTypedData_v4`, `personal_sign`) are intercepted and delegated to the identity backend. Modules can also import `identity` directly for `personal_sign` message signing, EIP-712 typed data signing, and listing accounts. `sign` prepends the EIP-191 prefix; a raw-bytes signing primitive is a 0.3 direction.
- **Per-interface typed errors over a shared `fault` vocabulary** - each interface declares its own error type; the cross-domain cases share one payload-bearing `fault` (`unsupported`, `unavailable`, `denied`, `rate-limited`, `timeout`, `invalid-input`, `internal`). Interfaces with nothing to add return `fault` directly (identity, local-store, remote-store, messaging, the module exports); `chain-error` embeds `fault` and adds an `rpc` case. Modules match on the typed variant for retry/backoff decisions. See ADR-0011.
- **`list<u8>` for raw bytes** - local-store values, signatures, accounts, order bodies. The SDK provides typed wrappers.
- **Worlds** - `nexum:host/event-module` for automation modules; `videre:venue/venue-adapter` for venue adapter components. The experimental `nexum:host/query-module` world is published but not yet hosted.

## Host-Side Embedding

The host uses `wasmtime::component::bindgen!` to generate Rust traits from the WIT; the generated traits for universal interfaces live under `nexum::host::`.

```rust
wasmtime::component::bindgen!({
    path: "wit/nexum-host",
    world: "event-module",
    async: true,
});
```

### Identity Host Trait

The `Identity` trait abstracts key management and signing. Platform implementations vary (server uses keystore/KMS/HSM, mobile uses device keychain, WebView uses wallet extensions), but the trait is uniform:

```rust
trait Identity {
    fn accounts(&self) -> Result<Vec<Address>>;
    fn sign(&self, account: Address, data: &[u8]) -> Result<Signature>;
    fn sign_typed_data(&self, account: Address, typed_data: &str) -> Result<Signature>;
}
```

### Chain depends on Identity

The `chain` host implementation depends on `Identity` internally. When a module calls a signing RPC method through `chain::request` (e.g. `eth_sendTransaction`, `eth_accounts`, `eth_signTypedData_v4`, `personal_sign`), the host intercepts the call and delegates to the identity backend instead of forwarding to the RPC provider:

```rust
impl nexum::host::chain::Host for NexumHostState {
    async fn request(
        &mut self,
        chain_id: u64,
        method: String,
        params: String,
    ) -> Result<Result<String, ChainError>> {
        // Signing methods are intercepted and delegated to identity.
        match method.as_str() {
            "eth_accounts" => {
                let accounts = self.identity.accounts()?;
                let json = serde_json::to_string(&accounts)?;
                return Ok(Ok(json));
            }
            "eth_sendTransaction" => {
                // Parse tx, sign via identity, then submit signed tx to provider
                let tx = serde_json::from_str(&params)?;
                let signature = self.identity.sign(tx.from, &tx.signing_hash())?;
                let signed = tx.with_signature(signature);
                let provider = self.provider_for(chain_id)?;
                let hash = provider.send_raw_transaction(&signed.encoded()).await?;
                return Ok(Ok(serde_json::to_string(&hash)?));
            }
            "eth_signTypedData_v4" | "personal_sign" => {
                // Delegate to identity for signing
                let (account, data) = parse_sign_params(&method, &params)?;
                let sig = self.identity.sign(account, &data)?;
                return Ok(Ok(serde_json::to_string(&sig)?));
            }
            _ => {}
        }

        if !self.is_method_allowed(&method) {
            return Ok(Err(ChainError::Fault(Fault::Denied(format!(
                "method not allowed: {method}"
            )))));
        }

        let provider = self.provider_for(chain_id)?;
        let raw_params: Box<RawValue> = RawValue::from_string(params)?;

        // One function handles the entire eth_ namespace - alloy's provider
        // stack (timeout, retry, rate-limit, fallback) applies transparently.
        // A structured JSON-RPC error folds to ChainError::Rpc (node code +
        // decoded revert bytes); a transport failure folds to a Fault.
        match provider.raw_request_dyn(method.into(), &raw_params).await {
            Ok(result) => Ok(Ok(result.get().to_string())),
            Err(e) => Ok(Err(e.into())),
        }
    }
}
```

### Identity Host Implementation

The `identity::Host` implementation delegates to the platform-specific `Identity` trait. The interface is the failure domain, so errors are a plain `Fault`:

```rust
impl nexum::host::identity::Host for NexumHostState {
    async fn accounts(&mut self) -> Result<Result<Vec<Vec<u8>>, Fault>> {
        match self.identity.accounts() {
            Ok(addrs) => Ok(Ok(addrs.into_iter().map(|a| a.to_vec()).collect())),
            Err(e) => Ok(Err(Fault::Internal(e.to_string()))),
        }
    }

    async fn sign(
        &mut self,
        account: Vec<u8>,
        data: Vec<u8>,
    ) -> Result<Result<Vec<u8>, Fault>> {
        let address = Address::from_slice(&account);
        match self.identity.sign(address, &data) {
            Ok(sig) => Ok(Ok(sig.to_vec())),
            Err(IdentityBackendError::UserRejected) => Ok(Err(Fault::Denied("user rejected".into()))),
            Err(e) => Ok(Err(Fault::Internal(e.to_string()))),
        }
    }

    // sign_typed_data follows the same pattern.
}
```

### Local Store Host Implementation

```rust
impl nexum::host::local_store::Host for NexumHostState {
    async fn get(&mut self, key: String) -> Result<Result<Option<Vec<u8>>, Fault>> {
        // Read from the in-flight WriteTransaction (not a new ReadTransaction)
        // so the module sees its own uncommitted writes within a single on_event.
        let table = self.write_txn.open_table(self.local_store_table())?;
        Ok(Ok(table.get(key.as_str())?.map(|v| v.value().to_vec())))
    }
    // ...
}
```

See doc 07 for the full `chain` host implementation, method allowlisting, and the `HostTransport` that bridges it to alloy's `Provider` API on the guest side.

## Guest-Side (Module Author) Experience

Modules ship using the host-trait seam from [ADR-0009](adr/0009-host-trait-surface.md): a `logic.rs` (`keeper.rs` in a keeper) of pure logic against `&impl Host`, plus a `lib.rs` `WitBindgenHost` adapter that bridges to `wit-bindgen::generate!`. Build with `cargo build --target wasm32-wasip2 --release`. See [`sdk.md`](sdk.md), doc 05, and the example modules under `modules/examples/`.

## Multi-Language Guest Support

| Language | Tooling | Maturity |
|----------|---------|----------|
| **Rust** | `wit-bindgen` + `cargo-component` | Mature |
| **C/C++** | `wit-bindgen c` + WASI SDK | Mature |
| **Go** | `wit-bindgen` Go generator | Maturing |
| **JavaScript** | ComponentizeJS (SpiderMonkey) | Maturing |
| **Python** | componentize-py (CPython) | Maturing |
| **C#** | `wit-bindgen-csharp` | Emerging |

All produce valid components against the `nexum:host/event-module` world.

## Execution Metering

### Fuel (deterministic cost accounting)

- `Config::consume_fuel(true)` - each WASM op consumes fuel; exhaustion traps.
- Use for **per-invocation budgets**: cap a single `on_event` callback.

### Epoch Interruption (cooperative time-slicing)

- `Config::epoch_interruption(true)` - background Tokio task calls `engine.increment_epoch()` on a fixed interval.
- Stores yield at epoch boundaries via `epoch_deadline_async_yield_and_update`.
- Use for **wall-clock fairness**: prevent one module from starving others.

Both are needed: fuel for correctness, epochs for liveness.

## Resource Limits

A `ResourceLimiter` caps linear-memory growth per module store, enforced synchronously on every `memory.grow`. The cap is `[limits].memory_bytes` from `engine.toml` (default 64 MiB). Fuel, the per-dispatch wall-clock deadline, and the local-store byte quota are the other resolved caps (`fuel_per_event` 1B, `event_deadline_secs` 120, `state_bytes` 50 MiB); all live in `crates/nexum-runtime/src/engine_config.rs` and apply uniformly, per-module overrides being a 0.3 direction.

## Async Integration

All RPC and outbound I/O is async (alloy / reqwest on the host). wasmtime bridges this:

- `Config::async_support(true)`.
- Host functions registered with `func_wrap_async` (or via `async: true` in `bindgen!`).
- Guest exports called with `call_async`.
- wasmtime runs WASM on a separate native stack; `Future::poll` drives execution.
- Epoch yielding ensures cooperation with the Tokio scheduler.

**Note:** We use wasmtime's basic async support (stable), *not* the Component Model native async (`stream<T>`, `future<T>`) which is still evolving.

## WASI: Constrained Surface

- WASI 0.2.1 is stable in wasmtime. WASI 0.3 (native async) is in preview.
- The `event-module` world imports **zero WASI interfaces**; the WASI surface a module actually sees is what the engine links into its store, and that surface is deliberately constrained.
- `wasi:clocks` and `wasi:random` are ambient: every module reads time and draws CSPRNG bytes with no capability declaration.
- There is no filesystem grant and no inbound network. `wasi:sockets` is linked as part of the p2 surface but inert - the WASI context grants no network, so the HTTP gate below is the only live network path.
- Outbound HTTP is the standard `wasi:http/outgoing-handler` interface, linked for modules that declare the `http` capability in their manifest. Every outgoing request is checked against the module's `[capabilities.http].allow` list - exact host match or `*.suffix` wildcard, case-insensitive, name-based - and an off-list host is denied with the `HTTP-request-denied` error code before any connection is made. The host does not follow redirects, so a redirect a guest chooses to follow re-enters the gate as a fresh request.
- Every admitted request is also bounded by the engine's `[limits.http]` knobs: ceilings on the guest-settable connect / first-byte / between-bytes timeouts (which double as the effective values when a guest sets none), an unconditional total deadline over the whole exchange, and a cap on the response body size. `engine.example.toml` documents the knobs and their defaults.

## Summary: Nexum <-> wasmtime Mapping

| Nexum Concept | wasmtime Primitive |
|------------------|--------------------|
| Runtime process | `Engine` (one, shared) |
| Universal API contract | WIT world (`nexum:host/event-module`) |
| Venue adapter contract | WIT world (`videre:venue/venue-adapter`) |
| Compiled module | `Component` (cached, thread-safe) |
| Pre-validated module | `InstancePre` (linker + component) |
| Running instance | `Store<NexumHostState>` + `Instance` |
| Host API impl | Traits generated by `bindgen!` |
| Host identity | `Identity` trait (keystore/KMS/HSM on server) |
| Opaque handles | `Resource<T>` + `ResourceTable` |
| Per-call budget | Fuel |
| Wall-clock fairness | Epoch interruption |
| Memory/table caps | `ResourceLimiter` |
| Async RPC / outbound I/O | `func_wrap_async` + Tokio |
| Persistent state | redb (per-module database file, via `local-store` interface host fns) |
