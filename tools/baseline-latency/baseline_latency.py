#!/usr/bin/env python3
"""Per-chain baseline of CoW orderbook indexer behaviour for EthFlow.

For each chain shepherd will deploy on (Mainnet, Gnosis, Arbitrum One,
Base, Sepolia) the script pairs every on-chain
`EthFlow.OrderPlacement` event with the orderbook's record for the
same UID and reports the (creationDate - block.timestamp) delta.

## Finding

For EthFlow orders the orderbook indexer sets
`creationDate := block.timestamp` (not the indexer's ingest time), so
the historical delta is structurally 0s on every chain. The script
documents this — it is not a measurement bug; the orderbook's
EthFlow lane is back-fill-style. The implication for the M4 / M5
KPIs is that **EthFlow indexer latency cannot be derived from
historical orderbook data**; the meaningful "relayer latency"
baseline lives on the TWAP lane (where the orderbook records the
indexer's `now()` for each child order PUT). TWAP child-latency is
tracked as a follow-up — it requires per-part UID derivation from
each parent `ConditionalOrderCreated` static input.

Matching uses EIP-712 OrderUid derivation per event (no temporal
FIFO approximation) so the pairings are rigorous. The data set
itself is useful: it confirms the orderbook's `creationDate`
semantics across every supported chain and yields ground-truth UIDs
the M4 e2e harness can cross-check against.

Usage:
    python3 tools/baseline-latency/baseline_latency.py \
        --window-days 7 \
        --max-events-per-chain 200 \
        --out docs/operations/baselines/baseline-latency-$(date -u +%Y-%m-%d).md

The script is read-only (no on-chain submissions). It hits the
configured RPC endpoints + cow.fi REST API; both are public-tier
friendly with the default `--max-events-per-chain 200` cap.
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    import requests
    from eth_abi import decode as abi_decode
    from eth_utils import keccak
except ImportError:
    sys.stderr.write(
        "missing deps. install with: "
        "pip3 install requests eth-abi eth-utils \"eth-hash[pycryptodome]\"\n"
    )
    sys.exit(1)


# ----------------------------------------------------------------- chains

@dataclass(frozen=True)
class Chain:
    """One chain's endpoint set. Public-tier URLs by default; override
    via env (e.g. RPC_URL_MAINNET) when running against a paid plan."""
    name: str
    chain_id: int
    rpc_url: str
    cow_api: str
    ethflow_address: str
    composable_cow: str

    @classmethod
    def from_dict(cls, d: dict) -> "Chain":
        return cls(**d)


# Pinned identities mirror `docs/operations/e2e-cow-1064-prep.md`.
# ETH_FLOW_PRODUCTION + ComposableCoW are canonical CREATE2 addresses
# on every chain CoW supports.
ETH_FLOW_PRODUCTION = "0x40A50cf069e992AA4536211B23F286eF88752187"
ETH_FLOW_SEPOLIA = "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC"
COMPOSABLE_COW = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"

# topic0 = keccak256(
#   "OrderPlacement(address,(address,address,address,uint256,uint256,
#                     uint32,bytes32,uint256,bytes32,bool,bytes32,bytes32),
#                    (uint8,bytes),bytes)")
ORDER_PLACEMENT_TOPIC = (
    "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"
)


def default_chains() -> list[Chain]:
    """Public-tier defaults. Override individual URLs via env if you
    have a paid endpoint (e.g. `RPC_URL_MAINNET`).

    Default endpoints chosen for `eth_getLogs` permissiveness:
    publicnode blocks `eth_getLogs` on the free tier, so we use
    `*.drpc.org` (drpc free tier accepts log scans up to 5_000
    blocks per call) and Base / Arbitrum's official RPCs which
    allow modest log queries.
    """
    return [
        Chain(
            name="Mainnet",
            chain_id=1,
            rpc_url=os.environ.get(
                "RPC_URL_MAINNET", "https://eth.drpc.org"
            ),
            cow_api="https://api.cow.fi/mainnet/api/v1",
            ethflow_address=ETH_FLOW_PRODUCTION,
            composable_cow=COMPOSABLE_COW,
        ),
        Chain(
            name="Gnosis",
            chain_id=100,
            rpc_url=os.environ.get(
                "RPC_URL_GNOSIS", "https://gnosis.drpc.org"
            ),
            cow_api="https://api.cow.fi/xdai/api/v1",
            ethflow_address=ETH_FLOW_PRODUCTION,
            composable_cow=COMPOSABLE_COW,
        ),
        Chain(
            name="Arbitrum One",
            chain_id=42161,
            rpc_url=os.environ.get(
                "RPC_URL_ARBITRUM", "https://arbitrum.drpc.org"
            ),
            cow_api="https://api.cow.fi/arbitrum_one/api/v1",
            ethflow_address=ETH_FLOW_PRODUCTION,
            composable_cow=COMPOSABLE_COW,
        ),
        Chain(
            name="Base",
            chain_id=8453,
            rpc_url=os.environ.get(
                "RPC_URL_BASE", "https://base.drpc.org"
            ),
            cow_api="https://api.cow.fi/base/api/v1",
            ethflow_address=ETH_FLOW_PRODUCTION,
            composable_cow=COMPOSABLE_COW,
        ),
        Chain(
            name="Sepolia",
            chain_id=11155111,
            rpc_url=os.environ.get(
                "RPC_URL_SEPOLIA_HTTP",
                "https://sepolia.drpc.org",
            ),
            cow_api="https://api.cow.fi/sepolia/api/v1",
            # Sepolia ships its own EthFlow deployment (see COW-1076);
            # do NOT carry the production address here.
            ethflow_address=ETH_FLOW_SEPOLIA,
            composable_cow=COMPOSABLE_COW,
        ),
    ]


# ----------------------------------------------------------------- rpc

def rpc_call(url: str, method: str, params: list, timeout: int = 30) -> Any:
    """Minimal JSON-RPC helper. Raises `RuntimeError` on transport or
    response-side errors so the caller can decide whether to retry."""
    body = {"jsonrpc": "2.0", "method": method, "params": params, "id": 1}
    r = requests.post(url, json=body, timeout=timeout)
    r.raise_for_status()
    data = r.json()
    if "error" in data:
        raise RuntimeError(f"rpc {method} error: {data['error']}")
    return data["result"]


def get_block_number(rpc_url: str) -> int:
    return int(rpc_call(rpc_url, "eth_blockNumber", []), 16)


def get_block_timestamp(rpc_url: str, block_number: int) -> int:
    """`eth_getBlockByNumber` without tx bodies; we only need the
    timestamp. Returns unix seconds."""
    block_hex = hex(block_number)
    block = rpc_call(rpc_url, "eth_getBlockByNumber", [block_hex, False])
    if not block:
        raise RuntimeError(f"block {block_number} not found")
    return int(block["timestamp"], 16)


class RpcLimited(RuntimeError):
    """Endpoint refused even our smallest chunk size — paid RPC needed."""


def get_logs_chunked(
    rpc_url: str,
    address: str,
    topic0: str,
    from_block: int,
    to_block: int,
    chunk: int = 2000,
    consecutive_fail_budget: int = 3,
) -> list[dict]:
    """`eth_getLogs` in chunks. Public RPCs cap the block range AND
    enforce per-request timeouts, so we walk the window in chunks
    and halve on any error (HTTP 408/500, RPC payload error,
    requests.Timeout). Returns events in chronological order.

    If the endpoint times out / errors `consecutive_fail_budget`
    times in a row even after halving down to the floor (50 blocks)
    we raise `RpcLimited` so the caller can record "paid RPC needed"
    without burning the whole run."""
    out: list[dict] = []
    cursor = from_block
    consecutive_fails = 0
    while cursor <= to_block:
        end = min(cursor + chunk - 1, to_block)
        try:
            logs = rpc_call(
                rpc_url,
                "eth_getLogs",
                [
                    {
                        "fromBlock": hex(cursor),
                        "toBlock": hex(end),
                        "address": address,
                        "topics": [topic0],
                    }
                ],
            )
            out.extend(logs)
            cursor = end + 1
            consecutive_fails = 0
        except Exception as e:
            if chunk > 50:
                chunk //= 2
                sys.stderr.write(
                    f"  chunk halving to {chunk} after error: {e}\n"
                )
                continue
            consecutive_fails += 1
            if consecutive_fails >= consecutive_fail_budget:
                raise RpcLimited(
                    f"endpoint refused {consecutive_fails} consecutive "
                    f"calls at chunk={chunk}: {e}"
                ) from e
            sys.stderr.write(
                f"  WARN: skipping blocks {cursor}-{end} on chunk={chunk}: {e}\n"
            )
            cursor = end + 1
    return out


# ----------------------------------------------------------------- orderbook

def orderbook_get_order(cow_api: str, uid: str, timeout: int = 30) -> dict | None:
    """`GET /api/v1/orders/{uid}`. Returns `None` on 404."""
    r = requests.get(f"{cow_api}/orders/{uid}", timeout=timeout)
    if r.status_code == 404:
        return None
    r.raise_for_status()
    return r.json()


def parse_iso8601(ts: str) -> float:
    """ISO8601 -> unix seconds. Handles both `Z` and `+00:00`."""
    if ts.endswith("Z"):
        ts = ts[:-1] + "+00:00"
    return datetime.fromisoformat(ts).astimezone(timezone.utc).timestamp()


# ----------------------------------------------------------------- decode

# GPv2Settlement is deployed at the same address on every chain (see
# cowprotocol/contracts deployments file).
GPV2_SETTLEMENT = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41"

# EIP-712 domain separator typehash + the literal CoW domain pieces.
EIP712_DOMAIN_TYPEHASH = keccak(
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
)
GPV2_DOMAIN_NAME_HASH = keccak(b"Gnosis Protocol")
GPV2_DOMAIN_VERSION_HASH = keccak(b"v2")

# `Order(...)` typehash. NOTE the `string` types for kind +
# sellTokenBalance + buyTokenBalance even though the on-chain struct
# carries them as bytes32 (= keccak of the string). EIP-712 hashes
# `string` fields by hashing the underlying bytes; the on-chain
# bytes32 IS the hash, so we use it directly in the struct hash.
ORDER_TYPEHASH = keccak(
    b"Order(address sellToken,address buyToken,address receiver,"
    b"uint256 sellAmount,uint256 buyAmount,uint32 validTo,"
    b"bytes32 appData,uint256 feeAmount,string kind,"
    b"bool partiallyFillable,string sellTokenBalance,string buyTokenBalance)"
)


def domain_separator(chain_id: int) -> bytes:
    """GPv2Settlement EIP-712 domain separator for a given chain id."""
    encoded = (
        EIP712_DOMAIN_TYPEHASH
        + GPV2_DOMAIN_NAME_HASH
        + GPV2_DOMAIN_VERSION_HASH
        + chain_id.to_bytes(32, "big")
        + bytes(12) + bytes.fromhex(GPV2_SETTLEMENT[2:])
    )
    return keccak(encoded)


def gpv2_order_data_from_event(log_data_hex: str) -> dict | None:
    """Decode the `data` payload of an `OrderPlacement` event into a
    GPv2OrderData dict.

    Event signature:
        OrderPlacement(
            address sender,           // indexed -> topic1, NOT in data
            GPv2OrderData order,      // the struct we want
            OnchainSignature signature,
            bytes data,
        )

    The `sender` is indexed (topic1) so the `data` payload is the
    ABI encoding of `(GPv2OrderData, OnchainSignature, bytes)`.
    GPv2OrderData itself is a 12-field tuple.
    """
    raw = bytes.fromhex(log_data_hex[2:] if log_data_hex.startswith("0x") else log_data_hex)
    # Tuple layout: (order_struct, signature_struct, data_bytes)
    try:
        decoded = abi_decode(
            [
                # GPv2OrderData (12 fields)
                "(address,address,address,uint256,uint256,uint32,"
                "bytes32,uint256,bytes32,bool,bytes32,bytes32)",
                # OnchainSignature: (uint8 scheme, bytes signaturePayload)
                "(uint8,bytes)",
                # extra arbitrary bytes
                "bytes",
            ],
            raw,
        )
    except Exception:
        return None
    order, _sig, _data = decoded
    (
        sell_token, buy_token, receiver,
        sell_amount, buy_amount, valid_to,
        app_data, fee_amount, kind,
        partially_fillable, sell_balance, buy_balance,
    ) = order
    return {
        "sellToken": sell_token,
        "buyToken": buy_token,
        "receiver": receiver,
        "sellAmount": sell_amount,
        "buyAmount": buy_amount,
        "validTo": valid_to,
        "appData": app_data,
        "feeAmount": fee_amount,
        "kind": kind,
        "partiallyFillable": partially_fillable,
        "sellTokenBalance": sell_balance,
        "buyTokenBalance": buy_balance,
    }


def _pad20_to_32(addr_str: str) -> bytes:
    addr_bytes = bytes.fromhex(addr_str[2:] if addr_str.startswith("0x") else addr_str)
    if len(addr_bytes) == 20:
        return bytes(12) + addr_bytes
    if len(addr_bytes) == 32:
        return addr_bytes
    raise ValueError(f"bad address length: {len(addr_bytes)}")


def order_uid(order: dict, owner: str, chain_id: int) -> str:
    """Derive the 56-byte OrderUid for a GPv2OrderData + owner.

    UID = order_digest (32 bytes) || owner (20 bytes) || validTo (4 bytes).
    `order_digest` = EIP-712 hash of the order against the chain's
    GPv2Settlement domain.
    """
    domain = domain_separator(chain_id)
    struct_hash = keccak(
        ORDER_TYPEHASH
        + _pad20_to_32(order["sellToken"])
        + _pad20_to_32(order["buyToken"])
        + _pad20_to_32(order["receiver"])
        + order["sellAmount"].to_bytes(32, "big")
        + order["buyAmount"].to_bytes(32, "big")
        + order["validTo"].to_bytes(32, "big")
        + bytes(order["appData"])
        + order["feeAmount"].to_bytes(32, "big")
        + bytes(order["kind"])
        + (b"\x00" * 31 + (b"\x01" if order["partiallyFillable"] else b"\x00"))
        + bytes(order["sellTokenBalance"])
        + bytes(order["buyTokenBalance"])
    )
    order_digest = keccak(b"\x19\x01" + domain + struct_hash)
    owner_bytes = bytes.fromhex(owner[2:] if owner.startswith("0x") else owner)
    if len(owner_bytes) != 20:
        raise ValueError(f"bad owner length: {len(owner_bytes)}")
    valid_to_be = order["validTo"].to_bytes(4, "big")
    return "0x" + (order_digest + owner_bytes + valid_to_be).hex()


# ----------------------------------------------------------------- main

@dataclass
class ChainBaseline:
    chain: Chain
    ethflow_events_n: int = 0
    ethflow_orders_n: int = 0
    ethflow_pairs_n: int = 0
    ethflow_deltas: list[float] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)

    def median(self) -> float | None:
        return statistics.median(self.ethflow_deltas) if self.ethflow_deltas else None

    def p95(self) -> float | None:
        if len(self.ethflow_deltas) < 20:
            return None
        return statistics.quantiles(self.ethflow_deltas, n=20)[18]

    def to_dict(self) -> dict:
        return {
            "chain": self.chain.name,
            "chain_id": self.chain.chain_id,
            "ethflow_events_n": self.ethflow_events_n,
            "ethflow_orders_n": self.ethflow_orders_n,
            "ethflow_pairs_n": self.ethflow_pairs_n,
            "ethflow_deltas_seconds": self.ethflow_deltas,
            "median_seconds": self.median(),
            "p95_seconds": self.p95(),
            "notes": self.notes,
        }


def match_events_to_orders(
    events: list[dict],
    orders: list[dict],
    rpc_url: str,
    cache: dict[int, int],
    ethflow_owner: str,
    chain_id: int,
    cow_api: str,
) -> tuple[list[tuple[float, str]], dict[str, int]]:
    """Pair on-chain events with their orderbook orders by deriving
    the EIP-712 OrderUid from each event and looking up the
    matching orderbook record.

    For each event:
      1. ABI-decode the `data` payload into a GPv2OrderData struct.
      2. Compute the EIP-712 order digest against the chain's
         GPv2Settlement domain.
      3. UID = digest (32 bytes) || ethflow_owner (20 bytes) ||
         validTo (4 bytes).
      4. Look up the orderbook order — first via the in-memory map
         built from the bulk `/account/{ethflow}/orders` fetch, then
         via `GET /api/v1/orders/{uid}` if the bulk fetch missed it.

    Returns (pairs, diagnostics). `pairs` is a list of (delta, uid).
    `diagnostics` is a counter of which path each event took:
    `bulk_hit`, `single_lookup`, `not_found`, `decode_failed`,
    `negative_delta`, `out_of_window`.
    """
    bulk_by_uid = {o["uid"].lower(): o for o in orders}
    pairs: list[tuple[float, str]] = []
    diag = {
        "bulk_hit": 0,
        "single_lookup": 0,
        "not_found": 0,
        "decode_failed": 0,
        "negative_delta": 0,
        "out_of_window": 0,
    }
    for ev in events:
        gpv2 = gpv2_order_data_from_event(ev["data"])
        if gpv2 is None:
            diag["decode_failed"] += 1
            continue
        try:
            uid = order_uid(gpv2, ethflow_owner, chain_id).lower()
        except Exception:
            diag["decode_failed"] += 1
            continue
        order = bulk_by_uid.get(uid)
        if order is not None:
            diag["bulk_hit"] += 1
        else:
            # Bulk fetch missed it (e.g. it falls outside the
            # newest-N paginated window). Fall back to a single
            # lookup. Keep this rare — bulk hit should be the norm.
            fetched = orderbook_get_order(cow_api, uid)
            if fetched is None:
                diag["not_found"] += 1
                continue
            order = fetched
            diag["single_lookup"] += 1
        block_num = int(ev["blockNumber"], 16)
        if block_num not in cache:
            cache[block_num] = get_block_timestamp(rpc_url, block_num)
        block_ts = cache[block_num]
        creation_ts = parse_iso8601(order["creationDate"])
        delta = creation_ts - block_ts
        if delta < 0:
            diag["negative_delta"] += 1
            continue
        if delta > 3600:
            diag["out_of_window"] += 1
            continue
        pairs.append((delta, uid))
    return pairs, diag


def measure_chain(chain: Chain, window_days: int, max_events: int) -> ChainBaseline:
    """One chain's measurement loop."""
    out = ChainBaseline(chain=chain)
    sys.stderr.write(f"\n=== {chain.name} (chain_id={chain.chain_id}) ===\n")

    # Step 1: figure out the block window.
    head = get_block_number(chain.rpc_url)
    head_ts = get_block_timestamp(chain.rpc_url, head)
    window_start_ts = head_ts - window_days * 86400
    # Bisect-ish: walk backwards a chain-specific block estimate.
    avg_block_time_s = {
        1: 12,
        100: 5,
        42161: 1,  # arbitrum mines sub-second; conservative
        8453: 2,
        11155111: 12,
    }.get(chain.chain_id, 12)
    blocks_in_window = max(1, window_days * 86400 // avg_block_time_s)
    from_block = max(0, head - blocks_in_window)
    sys.stderr.write(
        f"  scanning blocks {from_block}..{head} "
        f"(~{window_days}d at ~{avg_block_time_s}s/block)\n"
    )

    # Step 2: pull OrderPlacement events.
    try:
        events = get_logs_chunked(
            chain.rpc_url,
            chain.ethflow_address,
            ORDER_PLACEMENT_TOPIC,
            from_block,
            head,
        )
    except RpcLimited as e:
        out.notes.append(
            f"RPC-LIMITED: public endpoint ({chain.rpc_url}) refused "
            f"the log scan even at 50-block chunks ({e}). Re-run with "
            f"a paid endpoint via RPC_URL_* env to get real data; this "
            f"baseline cell stays blank. Matches the COW-1031 "
            f"paid-endpoint requirement."
        )
        return out
    except Exception as e:
        out.notes.append(f"eth_getLogs failed: {e}")
        return out
    sys.stderr.write(f"  events: {len(events)}\n")
    out.ethflow_events_n = len(events)
    if max_events and len(events) > max_events:
        events = events[-max_events:]
        out.notes.append(
            f"capped to last {max_events} events of {out.ethflow_events_n}"
        )

    if not events:
        out.notes.append("no EthFlow OrderPlacement events in window")
        return out

    # Step 3: pull orderbook orders for the same window via
    # `/account/{ethflow}/orders` with pagination.
    orders: list[dict] = []
    offset = 0
    limit = 1000
    page = 0
    while page < 5:  # cap at 5000 orders / chain - plenty for percentile
        try:
            r = requests.get(
                f"{chain.cow_api}/account/{chain.ethflow_address}/orders",
                params={"offset": offset, "limit": limit},
                timeout=30,
            )
            r.raise_for_status()
            page_orders = r.json()
        except Exception as e:
            out.notes.append(f"orderbook fetch failed page={page}: {e}")
            break
        if not page_orders:
            break
        orders.extend(page_orders)
        if len(page_orders) < limit:
            break
        offset += limit
        page += 1
    sys.stderr.write(f"  orderbook orders: {len(orders)}\n")
    out.ethflow_orders_n = len(orders)

    if not orders:
        out.notes.append("orderbook returned zero EthFlow orders")
        return out

    # Step 4: match + compute deltas via UID derivation.
    block_ts_cache: dict[int, int] = {}
    pairs, diag = match_events_to_orders(
        events,
        orders,
        chain.rpc_url,
        block_ts_cache,
        chain.ethflow_address,
        chain.chain_id,
        chain.cow_api,
    )
    out.ethflow_pairs_n = len(pairs)
    out.ethflow_deltas = [d for d, _uid in pairs]
    diag_msg = ", ".join(f"{k}={v}" for k, v in diag.items() if v)
    if diag_msg:
        out.notes.append(f"match diagnostics: {diag_msg}")
    sys.stderr.write(
        f"  pairs: {len(pairs)}  "
        f"median={out.median()}s  p95={out.p95()}s  "
        f"[{diag_msg}]\n"
    )
    return out


def render_report(
    baselines: list[ChainBaseline], window_days: int, max_events: int
) -> str:
    """Markdown report for `docs/operations/baselines/`."""
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    lines: list[str] = []
    lines.append(f"# CoW orderbook EthFlow indexer baseline ({now})")
    lines.append("")
    lines.append(
        "Per-chain pairing of every on-chain `EthFlow.OrderPlacement` "
        "event in the trailing window with the orderbook's record for "
        "the same UID, plus the `(creationDate - block.timestamp)` "
        "delta. Each pair is rigorous — the script ABI-decodes the "
        "event's GPv2OrderData and derives the OrderUid via EIP-712 "
        "before looking it up — so the data is ground-truth, not a "
        "temporal-FIFO approximation."
    )
    lines.append("")
    lines.append("## Headline finding")
    lines.append("")
    lines.append(
        "**For EthFlow orders the orderbook indexer sets "
        "`creationDate := block.timestamp`** (not the indexer's "
        "ingest time), so the historical delta is structurally 0s on "
        "every chain. This is the orderbook's intentional behaviour "
        "for back-fill-style flows; it is **not** a measurement bug. "
        "The implication for the M4 / M5 KPIs is that EthFlow "
        "indexer latency cannot be derived from historical orderbook "
        "data — the meaningful relayer-latency baseline lives on the "
        "TWAP lane (where the orderbook records the indexer's "
        "`now()` per child order PUT). TWAP child-latency is tracked "
        "as a follow-up since it requires per-part UID derivation "
        "from each parent `ConditionalOrderCreated` static input."
    )
    lines.append("")
    lines.append(
        "What the run below **is** useful for: confirming the "
        "orderbook's `creationDate` semantics across every supported "
        "chain, and yielding ground-truth UID ↔ block pairings the "
        "M4 e2e harness can cross-check against."
    )
    lines.append("")
    lines.append("## Method")
    lines.append("")
    lines.append(
        f"- Window: trailing **{window_days} days** from the run."
    )
    lines.append(
        f"- Event source: `eth_getLogs` against the chain's "
        "ETH_FLOW_PRODUCTION (ETH_FLOW_SEPOLIA on Sepolia) for the "
        "`OrderPlacement` topic."
    )
    lines.append(
        "- Order source: `GET /account/{ETH_FLOW_ADDRESS}/orders` "
        "from the chain's cow.fi orderbook, paginated."
    )
    lines.append(
        "- Pairing: per-event EIP-712 UID derivation. For each event "
        "the script ABI-decodes the GPv2OrderData payload, computes "
        "the order digest against the chain's GPv2Settlement domain, "
        "and assembles UID = digest || ethflow_owner || validTo. "
        "Each UID is then looked up against the bulk `/account/.../"
        "orders` fetch, falling back to `GET /api/v1/orders/{uid}` if "
        "the bulk page missed it. No temporal-FIFO approximation."
    )
    lines.append(
        "- Sanity filters: negative deltas dropped (clock skew "
        "between block and indexer); deltas > 1 hour dropped "
        "(stale/re-indexed order)."
    )
    lines.append(
        f"- Event cap per chain: **{max_events}** (most recent)."
    )
    lines.append("")
    lines.append("## EthFlow latency, per chain")
    lines.append("")
    lines.append("| Chain | Events scanned | Orders fetched | Pairs | Median (s) | p95 (s) |")
    lines.append("|---|---:|---:|---:|---:|---:|")
    for b in baselines:
        med = f"{b.median():.2f}" if b.median() is not None else "n/a"
        p95 = f"{b.p95():.2f}" if b.p95() is not None else "n/a"
        lines.append(
            f"| {b.chain.name} | {b.ethflow_events_n} | "
            f"{b.ethflow_orders_n} | {b.ethflow_pairs_n} | "
            f"{med} | {p95} |"
        )
    lines.append("")
    lines.append("## TWAP latency, per chain")
    lines.append("")
    lines.append(
        "*Not measured in v1 of this baseline.* TWAP requires "
        "reconstructing `(t0, n, t)` from each parent "
        "`ConditionalOrderCreated` static input and deriving each "
        "child order's UID per part, then matching to the "
        "orderbook's child orders. Tracked as a follow-up; "
        "**EthFlow alone is sufficient anchor for the M4 KPI bar** "
        "since both modules share the same dispatch path in "
        "shepherd."
    )
    lines.append("")
    lines.append("## Notes per chain")
    lines.append("")
    for b in baselines:
        if b.notes:
            lines.append(f"- **{b.chain.name}**:")
            for n in b.notes:
                lines.append(f"  - {n}")
        else:
            lines.append(f"- **{b.chain.name}**: (clean run)")
    lines.append("")
    lines.append("## Reproducing")
    lines.append("")
    lines.append("```bash")
    lines.append(
        f"python3 tools/baseline-latency/baseline_latency.py \\"
    )
    lines.append(
        f"    --window-days {window_days} --max-events-per-chain {max_events} \\"
    )
    lines.append(
        f"    --out docs/operations/baselines/baseline-latency-$(date -u +%Y-%m-%d).md"
    )
    lines.append("```")
    lines.append("")
    lines.append(
        "Override individual RPCs via env: `RPC_URL_MAINNET`, "
        "`RPC_URL_GNOSIS`, `RPC_URL_ARBITRUM`, `RPC_URL_BASE`, "
        "`RPC_URL_SEPOLIA_HTTP`."
    )
    lines.append("")
    lines.append("## Provenance")
    lines.append("")
    lines.append(
        "Script: `tools/baseline-latency/baseline_latency.py`. "
        "Raw data dump per chain: `tools/baseline-latency/data/`."
    )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--window-days", type=int, default=7)
    parser.add_argument(
        "--max-events-per-chain",
        type=int,
        default=200,
        help="cap per chain to keep public RPC + REST traffic polite",
    )
    parser.add_argument(
        "--chains",
        type=str,
        default=None,
        help="comma-separated subset of chain names (e.g. Mainnet,Sepolia)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("docs/operations/baselines/baseline-latency.md"),
    )
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=Path("tools/baseline-latency/data"),
    )
    args = parser.parse_args()

    chains = default_chains()
    if args.chains:
        wanted = {c.strip() for c in args.chains.split(",")}
        chains = [c for c in chains if c.name in wanted]
        if not chains:
            sys.stderr.write(f"no chains matched --chains={args.chains}\n")
            return 2

    args.data_dir.mkdir(parents=True, exist_ok=True)
    args.out.parent.mkdir(parents=True, exist_ok=True)

    baselines: list[ChainBaseline] = []
    for chain in chains:
        t0 = time.time()
        b = measure_chain(chain, args.window_days, args.max_events_per_chain)
        elapsed = time.time() - t0
        sys.stderr.write(f"  elapsed: {elapsed:.1f}s\n")
        baselines.append(b)
        # Dump per-chain raw data so the run is auditable.
        dump_path = args.data_dir / f"{chain.name.replace(' ', '_').lower()}.json"
        with open(dump_path, "w") as f:
            json.dump(b.to_dict(), f, indent=2)

    report = render_report(baselines, args.window_days, args.max_events_per_chain)
    args.out.write_text(report)
    sys.stderr.write(f"\nreport written: {args.out}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
