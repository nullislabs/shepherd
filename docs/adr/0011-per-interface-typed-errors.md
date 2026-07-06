---
status: accepted
---

# Per-interface typed errors over a shared fault vocabulary

## Context

The host once returned one unified envelope, `host-error`, from every imported function and every module export: a record of `domain` (a stringly subsystem tag), a `host-error-kind` enum, a numeric `code` (a JSON-RPC code or an HTTP status, depending on the caller), a `message`, and an optional opaque `data` blob. Dispatch on the failure cause meant reading `kind`, sometimes cross-checking `domain` and `code`. Modules re-named their own domain in every error they built and prefixed each message with the module name, duplicating context the runtime already had (the module name, the interface).

The envelope conflated two things that want to move independently: the shared cross-domain failure vocabulary (unavailable, timeout, denied, ...) and the per-interface structured detail (a JSON-RPC revert carries a node code and decoded revert bytes; an orderbook rejection carries a typed `{errorType, description}`). Squeezing both through one flat record meant every interface paid for fields it did not use and lost the fields it did.

## Decision

Adopt the WASI idiom: each interface declares its own typed error, and the errors share one payload-bearing `fault` vocabulary for the cross-domain cases.

`fault` has seven cases: `unsupported(string)`, `unavailable(string)`, `denied(string)`, `rate-limited(rate-limit)`, `timeout`, `invalid-input(string)`, and `internal(string)`. A richer interface embeds `fault` as one case of its own variant and adds the cases only it needs: `chain-error` adds an `rpc` case carrying the node code and decoded revert bytes; `cow-api-error` adds `http` and `rejected`. Interfaces with nothing to add report `fault` directly (local-store, and now the module exports).

The module exports (`init`, `on-event`, `evaluate`) return `result<_, fault>`. Module identity is the supervisor's business: it holds the module name and does not need each fault to re-declare it, so the `domain` self-naming and the message-prefix duplication in modules are gone. The supervisor derives its error metric label and structured-log `kind` field from the fault case (via the `HostFault` label), which drops a `format!("{:?}")` allocation per erroring dispatch.

`host-error` and `host-error-kind` are deleted from `types.wit` and from every mirror: the runtime host constructors, both guest SDKs, both SDK test crates, and the module glue. The SDK exposes `Fault` (mirroring the wire vocabulary) plus the `HostFault` trait that recovers an embedded fault and a stable snake_case label; `From<ChainError> for Fault` folds a chain error into the shared vocabulary so a strategy aggregating store and chain calls returns one `Fault`.

This is a pre-1.0 wire break. CI rebuilds every module wasm on a world change, so no compatibility shim is warranted.

## Consequences

- A caller dispatches on the structured cause by matching the typed variant, with no stringly `domain`/`code` cross-check.
- Interfaces carry exactly the detail they have; the shared cases stay uniform across interfaces and yield one stable label vocabulary for metrics and logs.
- Modules no longer restate their identity or prefix messages; the runtime supplies both.
- The numeric `code` and opaque `data` fields are gone. An interface that needs structured detail (the JSON-RPC code, decoded revert bytes) carries it in a typed case instead.
- Old bindings do not interoperate with the new world. Because this predates 1.0 and CI rebuilds all module wasms per world change, that break is accepted rather than shimmed.
