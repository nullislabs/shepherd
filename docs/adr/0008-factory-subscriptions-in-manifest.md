---
status: deferred
---

# Dynamic address registration for log subscriptions

## Status

**Deferred.** No current module needs it, and the schema and host-function surface add runtime complexity nothing exercises yet. Preserved as a record of the design space; the shape is revisited when a module actually requiring dynamic address registration emerges.

## Context

Some module archetypes track contracts deployed by a factory (for example Uniswap V3 pools). Static `[[subscription]]` declarations in `module.toml` cannot express this: the child addresses are unknown at manifest authorship. The current CoW modules do not need it; each subscribes to a single well-known contract per chain.

`eth_getLogs` already accepts a topic-only filter (no `address` field), so a module can subscribe to a topic across all addresses and filter module-side, covering the common case without a new manifest-and-host mechanism.

## Design space (not adopted)

- **Topic-only `[[subscription]]`**: no address field, module filters client-side. Simplest, no new host functions; trade-off is firehose volume for common topics.
- **Dynamic register-address**: a `[[subscription.template]]` block plus `chain.register-address` / `unregister-address` host functions maintaining a per-chain aggregated `eth_subscribe logs` whose address set the module mutates at runtime. Envio HyperIndex's `register()` is the closest existing pattern.
- **Runtime-extracted factory child addresses**: declarative ABI-aware extraction rules; schema complexity grows with exotic factory shapes.

The choice depends on what the first consumer needs.

## Consequences

- The `nexum:host` WIT surface and the `module.toml` schema stay unchanged: no `[[subscription.template]]`.
- Current modules ship against static `[[subscription]]` (one address per subscription, known at authorship).
