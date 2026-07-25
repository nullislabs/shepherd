---
status: accepted
---

# Per-chain alloy provider transport selected by URL scheme

## Context

`nexum:host/chain` covers both generic JSON-RPC dispatch (`request`) and event subscriptions (`subscribe-blocks`, `subscribe-logs`). Subscriptions require a duplex transport; request/response works on either HTTP or WebSocket. The operator configures one `rpc_url` per chain in `engine.toml`, and the runtime picks the alloy transport from it.

## Decision

`ProviderPool::from_config` switches on the URL scheme:

- `ws://` / `wss://` connect via `connect_ws`. Pubsub transport: subscriptions and request/response both work. Recommended for any chain a module subscribes to.
- `http://` / `https://` connect via `connect_http`. Request/response only; `subscribe-blocks` and `subscribe-logs` return `fault.unsupported` to the guest.

Both erase to `DynProvider`, so the rest of the runtime is transport-agnostic. Alloy can emulate `eth_subscribe` on HTTP by polling; this is deliberately not enabled.

## Non-goals

RPC failover, load balancing, and retry policy are out of scope. Alloy ships tower-style middleware for timeout, retry, rate-limit, and fallback endpoints; operators configure it on the provider builder, or rely on their provider's server-side fallback.

## Consequences

- Operators needing subscriptions supply WSS URLs; HTTP-only chains downgrade to request-only at the host call boundary.
- Connection failure at boot is fatal: the runtime refuses to start with a broken chain rather than masking misconfiguration a module rediscovers at first event.
- Adding IPC is additive: extend the scheme match with `file://` and call `connect_ipc`.
