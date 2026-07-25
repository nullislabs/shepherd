---
status: accepted
---

# Per-interface typed errors over a shared fault vocabulary

## Context

The host once returned one flat envelope (`host-error`: a stringly `domain`, a `host-error-kind` enum, a numeric `code`, a message, an optional `data` blob) from every function and export. It conflated the shared cross-domain failure vocabulary (unavailable, timeout, denied) with per-interface structured detail (a JSON-RPC revert's node code and decoded bytes, an orderbook rejection's typed `{errorType, description}`). Every interface paid for fields it did not use and lost the fields it did, and modules restated their own identity in every error they built.

## Decision

Follow the WASI idiom: each interface declares its own typed error, and the errors share one payload-bearing `fault` vocabulary for the cross-domain cases.

`fault` has seven cases: `unsupported(string)`, `unavailable(string)`, `denied(string)`, `rate-limited(rate-limit)`, `timeout`, `invalid-input(string)`, and `internal(string)`. A richer interface embeds `fault` as one case of its own variant and adds only the cases it needs: `chain-error` adds an `rpc` case carrying the node code and decoded revert bytes; `cow-api-error` adds `http` and `rejected`. Interfaces with nothing to add report `fault` directly.

The module exports (`init`, `on-event`, `evaluate`) return `result<_, fault>`. Module identity is the supervisor's business, so the `domain` self-naming and message-prefix duplication are gone; the supervisor derives its metric label and log `kind` from the fault case via the `HostFault` label. `host-error` and `host-error-kind` are deleted from `types.wit` and every mirror. The SDK exposes `Fault` plus the `HostFault` trait, with `From<ChainError> for Fault` folding a chain error into the shared vocabulary.

This is a pre-1.0 wire break; CI rebuilds every module wasm on a world change, so no shim is warranted.

## Consequences

- A caller dispatches on the structured cause by matching the typed variant, with no stringly `domain`/`code` cross-check.
- The shared cases yield one stable label vocabulary for metrics and logs; interfaces carry exactly the detail they have.
- The numeric `code` and opaque `data` fields are gone; structured detail lives in a typed case instead.
