---
status: superseded
---

# `cow-api` host backend routes both `request` and `submit-order` through `cowprotocol::OrderBookApi`

> **Superseded by the videre venue-adapter architecture.** The `shepherd:cow/cow-api` host extension and its `OrderBookApi` backend are retired: orderbook submission and status ride the `cow-venue` adapter component over `wasi:http`, driven through the `videre:venue/client` pool seam.

## Context

`shepherd:cow/cow-api` exposed a generic REST passthrough (`request`) and a typed order submission (`submit-order`). The `cowprotocol` crate already shipped an `OrderBookApi` client that knew the chain base URL, canonical paths, and `post_order` codec.

## Decision (retired)

At boot, build one `cowprotocol::OrderBookApi` per `cowprotocol::Chain` variant into a `BTreeMap<u64, OrderBookApi>` keyed by chain id, reused for the runtime's lifetime. `request` resolved the chain client and joined the module-supplied path; `submit-order` deserialized the JSON `OrderCreation` and called `OrderBookApi::post_order`. Chains outside `cowprotocol::Chain` returned `fault.unsupported`.
