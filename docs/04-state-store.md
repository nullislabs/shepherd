# Local Store Architecture

Every Nexum module has a persistent key-value store that survives restarts, crashes, and module updates, backed by **redb** (v3.1, pure Rust, embedded, ACID, MVCC) and exposed through the `local-store` WIT interface. It is the only durable memory a module has: WASM linear memory is wiped on every restart, so modules reconstruct working state from the store on `init`.

## redb Fundamentals

| Property | Detail |
|----------|--------|
| Engine | Copy-on-write B-tree |
| Concurrency | MVCC: concurrent readers, single writer |
| Durability | Crash-safe (fsync on commit) |
| Transactions | Full ACID |

## Isolation Model

A single redb file lives under `EngineConfig.engine.state_dir`. Every key is namespaced host-side by a fixed 32-byte prefix `keccak256(module_name)` prepended before the raw key, so modules sharing a key string see disjoint data and cannot forge a key into another module's range. keccak256 matches ENS node derivation (see [ADR-0003](adr/0003-local-store-namespacing.md)). The module never observes the prefix.

Module identity is `name` from `module.toml`. Two instances sharing a name share a namespace (intentional: hot-reload with state continuity).

## WIT Interface

```wit
interface local-store {
    use nexum:host/types.{fault};

    get: func(key: string) -> result<option<list<u8>>, fault>;
    set: func(key: string, value: list<u8>) -> result<_, fault>;
    delete: func(key: string) -> result<_, fault>;
    list-keys: func(prefix: string) -> result<list<string>, fault>;
    contains: func(key: string) -> result<bool, fault>;
    len: func(key: string) -> result<option<u64>, fault>;
    count: func(prefix: string) -> result<u64, fault>;
}
```

`contains`, `len`, and `count` answer existence, value length, and prefix cardinality without transferring the value or materialising the key list. Errors are the shared `fault` vocabulary; the interface is its own failure domain, so it reports `fault` directly. Keys are UTF-8 strings; values are opaque bytes.

`list-keys` and `count` enable prefix-based namespacing within a module:

```
orders/active/0x1234  →  [serialised order]
orders/active/0x5678  →  [serialised order]

list_keys("orders/active/") → ["orders/active/0x1234", "orders/active/0x5678"]
```

## Transaction Semantics

Each host call is its own redb transaction: a `get` / `contains` / `len` / `count` / `list-keys` opens a read transaction, and a `set` / `delete` opens a write transaction and commits (fsync-durable) before returning. There is no transaction spanning a whole `on_event`, so a module that traps midway through processing an event keeps whatever writes already committed; per-event atomicity is the module's responsibility (checkpoint a "last processed" key last, and make `init` idempotent). A `set` rejected for quota aborts its own write untouched.

## Size Enforcement

A module's namespace is capped at the engine-global `[limits].state_bytes` quota (default 50 MiB). `set` charges the on-disk footprint (prefix + key + value + a fixed per-entry overhead), summed across the namespace's keys, and rejects an over-quota write with `fault.invalid-input`, leaving the store untouched. The footprint is tracked by an incremental per-namespace counter, seeded once by a prefix-range scan.

## State Lifecycle

- **Cold start.** On first load the namespace is empty; `init` seeds any initial keys.
- **Restart.** A crash yields a fresh WASM instance over the same namespace. The last committed write is intact. `init` reads its checkpoint and resumes.
- **Update.** A new version (same `name`) inherits the namespace; its `init` handles any schema migration.
- **Removal.** A removed module's keys are retained by default.

## Summary

| Concern | Design |
|---------|--------|
| Backend | redb v3.1 (pure Rust, ACID, MVCC) |
| File layout | Single redb file under `state_dir` |
| Isolation | 32-byte `keccak256(module_name)` key prefix (ADR-0003) |
| Key / value | UTF-8 string / opaque bytes (`list<u8>`) |
| Namespacing within module | Slash-separated prefixes + `list-keys` / `count` |
| Transaction scope | Per host call, committed on return |
| Size limit | Per-namespace quota from `[limits].state_bytes` |
| Survives restart | Yes, external to the WASM instance |
