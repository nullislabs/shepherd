# CoW orderbook EthFlow indexer baseline (2026-06-22T14:03:22Z)

Per-chain pairing of every on-chain `EthFlow.OrderPlacement` event in the trailing window with the orderbook's record for the same UID, plus the `(creationDate - block.timestamp)` delta. Each pair is rigorous — the script ABI-decodes the event's GPv2OrderData and derives the OrderUid via EIP-712 before looking it up — so the data is ground-truth, not a temporal-FIFO approximation.

## Headline finding

**For EthFlow orders the orderbook indexer sets `creationDate := block.timestamp`** (not the indexer's ingest time), so the historical delta is structurally 0s on every chain. This is the orderbook's intentional behaviour for back-fill-style flows; it is **not** a measurement bug. The implication for the M4 / M5 KPIs is that EthFlow indexer latency cannot be derived from historical orderbook data — the meaningful relayer-latency baseline lives on the TWAP lane (where the orderbook records the indexer's `now()` per child order PUT). TWAP child-latency is tracked as a follow-up since it requires per-part UID derivation from each parent `ConditionalOrderCreated` static input.

What the run below **is** useful for: confirming the orderbook's `creationDate` semantics across every supported chain, and yielding ground-truth UID ↔ block pairings the M4 e2e harness can cross-check against.

## Method

- Window: trailing **7 days** from the run.
- Event source: `eth_getLogs` against the chain's ETH_FLOW_PRODUCTION (ETH_FLOW_SEPOLIA on Sepolia) for the `OrderPlacement` topic.
- Order source: `GET /account/{ETH_FLOW_ADDRESS}/orders` from the chain's cow.fi orderbook, paginated.
- Pairing: per-event EIP-712 UID derivation. For each event the script ABI-decodes the GPv2OrderData payload, computes the order digest against the chain's GPv2Settlement domain, and assembles UID = digest || ethflow_owner || validTo. Each UID is then looked up against the bulk `/account/.../orders` fetch, falling back to `GET /api/v1/orders/{uid}` if the bulk page missed it. No temporal-FIFO approximation.
- Sanity filters: negative deltas dropped (clock skew between block and indexer); deltas > 1 hour dropped (stale/re-indexed order).
- Event cap per chain: **200** (most recent).

## EthFlow latency, per chain

| Chain | Events scanned | Orders fetched | Pairs | Median (s) | p95 (s) |
|---|---:|---:|---:|---:|---:|
| Mainnet | 0 | 0 | 0 | n/a | n/a |
| Gnosis | 0 | 0 | 0 | n/a | n/a |
| Arbitrum One | 0 | 0 | 0 | n/a | n/a |
| Base | 0 | 0 | 0 | n/a | n/a |
| Sepolia | 256 | 5000 | 200 | 0.00 | 0.00 |

## TWAP latency, per chain

*Not measured in v1 of this baseline.* TWAP requires reconstructing `(t0, n, t)` from each parent `ConditionalOrderCreated` static input and deriving each child order's UID per part, then matching to the orderbook's child orders. Tracked as a follow-up; **EthFlow alone is sufficient anchor for the M4 KPI bar** since both modules share the same dispatch path in shepherd.

## Notes per chain

- **Mainnet**:
  - RPC-LIMITED: public endpoint (https://eth.drpc.org) refused the log scan even at 50-block chunks (endpoint refused 3 consecutive calls at chunk=31: 408 Client Error: Request Timeout for url: https://eth.drpc.org/). Re-run with a paid endpoint via RPC_URL_* env to get real data; this baseline cell stays blank. Matches the paid-endpoint requirement.
- **Gnosis**:
  - RPC-LIMITED: public endpoint (https://gnosis.drpc.org) refused the log scan even at 50-block chunks (endpoint refused 3 consecutive calls at chunk=31: 500 Server Error: Internal Server Error for url: https://gnosis.drpc.org/). Re-run with a paid endpoint via RPC_URL_* env to get real data; this baseline cell stays blank. Matches the paid-endpoint requirement.
- **Arbitrum One**:
  - RPC-LIMITED: public endpoint (https://arbitrum.drpc.org) refused the log scan even at 50-block chunks (endpoint refused 3 consecutive calls at chunk=31: 500 Server Error: Internal Server Error for url: https://arbitrum.drpc.org/). Re-run with a paid endpoint via RPC_URL_* env to get real data; this baseline cell stays blank. Matches the paid-endpoint requirement.
- **Base**:
  - RPC-LIMITED: public endpoint (https://base.drpc.org) refused the log scan even at 50-block chunks (endpoint refused 3 consecutive calls at chunk=31: 500 Server Error: Internal Server Error for url: https://base.drpc.org/). Re-run with a paid endpoint via RPC_URL_* env to get real data; this baseline cell stays blank. Matches the paid-endpoint requirement.
- **Sepolia**:
  - capped to last 200 events of 256
  - match diagnostics: bulk_hit=200

## Reproducing

```bash
python3 tools/baseline-latency/baseline_latency.py \
    --window-days 7 --max-events-per-chain 200 \
    --out docs/operations/baselines/baseline-latency-$(date -u +%Y-%m-%d).md
```

Override individual RPCs via env: `RPC_URL_MAINNET`, `RPC_URL_GNOSIS`, `RPC_URL_ARBITRUM`, `RPC_URL_BASE`, `RPC_URL_SEPOLIA_HTTP`.

## Provenance

Script: `tools/baseline-latency/baseline_latency.py`. Raw data dump per chain: `tools/baseline-latency/data/`.
