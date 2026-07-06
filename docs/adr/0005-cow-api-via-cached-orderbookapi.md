---
status: proposed
implemented-in: nullislabs/shepherd#8
---

# `cow-api` host backend routes both `request` and `submit-order` through `cowprotocol::OrderBookApi`

## Context

`shepherd:cow/cow-api` exposes two operations: a generic REST passthrough (`request`) and a typed order submission (`submit-order`). Either could be implemented with raw `reqwest` against `api.cow.fi/{slug}/api/v1`, but the published `cowprotocol` crate already ships an `OrderBookApi` client that knows the chain-specific base URL, the canonical paths, and the `post_order` codec.

## Decision

At engine boot, construct one `cowprotocol::OrderBookApi` per `cowprotocol::Chain` variant (currently Mainnet, Gnosis, Sepolia, ArbitrumOne, Base) into a `BTreeMap<u64, OrderBookApi>` keyed by EVM chain id. "Cached" here means built once during boot and reused for the engine's lifetime; clients are not lazy-constructed on each call nor LRU-evicted. The pool implements `Default` so callers instantiate it as `OrderBookPool::default()`; the trait impl populates the map with one entry per `cowprotocol::Chain` variant.

Both `cow-api` operations consult this pool:

- `request` resolves the chain's `OrderBookApi`, reads `api.base_url()` for the prefix, joins the module-supplied path, and dispatches via a shared `reqwest::Client`.
- `submit-order` deserialises the JSON `OrderCreation` and calls `OrderBookApi::post_order` directly. The crate handles signing-scheme encoding, error mapping, and `OrderUid` extraction.

Chains not in `cowprotocol::Chain` return `cow-api-error` carrying `fault.unsupported` at the host call boundary.

## Considered options

- **Raw `reqwest` for both.** Rejected: forces us to maintain the chain → base-URL table (drifts whenever cowprotocol adds a chain) and reimplement `post_order`'s body codec and error mapping, the exact duplication the cow-rs SDK already eliminates.
- **`OrderBookApi` for `submit-order`, raw `reqwest` for `request`.** Tempting (request is opaque to the crate) but means two separate chain-resolution paths, two HTTP clients, and a second place to keep the chain set in sync.
- **Build `OrderBookApi` lazily on first call per chain.** Rejected: hides config errors at runtime. Up-front boot construction surfaces unknown chains immediately and amortises away the per-call cost.

## Consequences

- Operator-supplied custom orderbook URLs (barn, staging, forked deployments) are out of scope for the default constructor and require a follow-on `OrderBookApi::with_base_url(chain_id, base_url)` constructor in the cow-rs crate (ADR-0007 item 2, not vendored locally).
- Adding a chain means a `cowprotocol::Chain` variant lands in cow-rs first; the engine inherits it on the next patched rev bump.
- The shared `reqwest::Client` enables connection pooling across both `request` and `submit-order` paths.
- Guest-side TWAP and EthFlow modules (ADR-0006) submit orders through this `cow-api` interface; no specialised host helpers wrap it.
