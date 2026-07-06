---
status: proposed
implemented-in: nullislabs/shepherd#8, nullislabs/shepherd#9
---

# Per-chain alloy provider transport selected by URL scheme

## Context

`nexum:host/chain` covers both generic JSON-RPC dispatch (`request`) and event subscriptions (`subscribe-blocks`, `subscribe-logs`). Subscriptions require a duplex transport (`eth_subscribe` is push-only over a long-lived connection); request/response works on either HTTP or WebSocket. The operator configures one RPC endpoint per chain in `engine.toml`; the engine has to decide which alloy transport to use.

## Decision

The `ProviderPool::from_config` constructor reads each chain's `rpc_url` and switches by URL scheme prefix:

- `ws://` or `wss://` → `ProviderBuilder::new().connect_ws(WsConnect::new(url))`. Pubsub transport. Subscriptions and request/response both work. **This is the recommended configuration for any chain a module subscribes to.**
- `http://` or `https://` → `ProviderBuilder::new().connect_http(parsed)`. HTTP transport. Request/response only; `subscribe-blocks` and `subscribe-logs` surface as `fault.unsupported` to the guest.

Both transports erase to `DynProvider` so the rest of the engine is transport-agnostic.

Alloy is capable of emulating `eth_subscribe` on HTTP via polling, but this is intentionally **not** enabled. The engine takes an opinionated stance favouring WebSockets for subscriptions; operators who want push-based events configure WSS endpoints. HTTP-only chains are supported for `request` traffic but not for subscriptions.

## Non-goals

- **RPC failover, load balancing, and retry policies are explicitly out of scope for the engine.** This logic lives in upstream crates (alloy ships tower-style middleware for timeout / retry / rate-limit / fallback endpoint). The engine does not roll its own. Operators wanting failover configure it via alloy provider builders before passing them through, or rely on the provider's own fallback (Alchemy, Infura, etc. handle it server-side).
- Re-routing requests across chains, rebalancing across pools within a chain, and similar provider-management concerns are likewise alloy's responsibility.

## Considered options

- **Force WSS everywhere.** Rejected: many providers (Alchemy, Infura, self-hosted RPC) expose HTTP-only on free tiers, and modules that only need `request` (no subscriptions) shouldn't be blocked by a WSS requirement.
- **Explicit `transport = "ws" | "http"` field per chain in `engine.toml`.** Rejected for 0.2: redundant with the URL scheme, and operators already distinguish `wss://` from `https://` endpoints when copying them from their RPC provider's dashboard. Revisit if we add IPC (`/path/to/geth.ipc`) - scheme alone won't carry that.
- **Open both an HTTP and a WSS connection per chain.** Rejected: doubles connection count for the common case where one endpoint serves both, and forces operators to provide two URLs even when their provider returns identical data on both.

## Consequences

- Operators that need subscriptions must supply WSS URLs; HTTP-only chains downgrade to request-only mode at the host call boundary.
- Connection failures at boot are fatal (the engine refuses to start with a broken chain). This is intentional - silent fall-back to a half-functioning state masks misconfiguration that a module then rediscovers at first event.
- Adding IPC support is additive: extend the scheme match with `/` / `file://` and call `connect_ipc`.
- The `DynProvider` erasure costs a virtual dispatch per call - a measurable concern at scale, deferred to M4 if profiling shows it.
