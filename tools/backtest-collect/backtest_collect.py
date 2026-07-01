#!/usr/bin/env python3
"""Collect a Sepolia event window for the pre-soak backtest.

Pulls every on-chain
- `CoWSwapEthFlow.OrderPlacement` (EthFlow lane), and
- `ComposableCoW.ConditionalOrderCreated` (TWAP lane)

in the trailing `--days` window on Sepolia, ABI-decodes the payloads,
derives the EthFlow `OrderUid` via EIP-712, resolves any non-empty
`appData` hashes via the orderbook's `/api/v1/app_data/{hash}` lookup,
and emits a single fixtures JSON the Rust replay harness
(`crates/shepherd-backtest`) consumes.

The script is read-only (no on-chain submissions, no orderbook PUTs).
It only hits the configured RPC endpoint + `GET` against the cow.fi
orderbook.

## Scope

Phase 1 MVP collects events + decoded payloads + app_data only. It
does NOT walk every TWAP watch with `eth_call(getTradeableOrderWith
Signature)` per block — that requires an archive-tier RPC plan.
The replay harness will perform that walk on demand
once a paid endpoint is wired; until then the TWAP replay is bounded
to "would the strategy assemble a child body on the first `Ready`
window?" and the EthFlow replay is fully exercisable from the
collected fixtures alone.

## Output shape

```
{
  "metadata": {
    "collected_at": "2026-06-22T15:00:00Z",
    "chain_id": 11155111,
    "chain_name": "Sepolia",
    "window_days": 7,
    "from_block": 11065713,
    "to_block": 11116113,
    "rpc_url": "https://sepolia.drpc.org",
    "cow_api": "https://api.cow.fi/sepolia/api/v1",
    "ethflow_owner": "0xba3cb449bd2b4adddbc894d8697f5170800eadec",
    "composable_cow": "0xfdafc9d1902f4e0b84f65f49f244b32b31013b74"
  },
  "ethflow_orders": [ { "uid": "0x...", "block_number": ..., "block_timestamp": ..., "tx_hash": "0x...", "log_index": ..., "sender": "0x...", "contract": "0x...", "gpv2_order": {...}, "signature": {"scheme": 0, "payload": "0x..."}, "extra_data": "0x...", "app_data_resolved": null | {"hash": "0x...", "document": "..."} } ],
  "twap_conditionals": [ { "owner": "0x...", "block_number": ..., "block_timestamp": ..., "tx_hash": "0x...", "log_index": ..., "params": {"handler": "0x...", "salt": "0x...", "static_input": "0x..."} } ]
}
```

Usage:

    python3 tools/backtest-collect/backtest_collect.py \
        --days 7 \
        --out tools/backtest-collect/fixtures-$(date -u +%Y-%m-%d).json
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from dataclasses import dataclass
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


# ----------------------------------------------------------------- pinned identities

# EthFlow contract Sepolia deployment (see docs/operations/e2e-prep.md).
ETH_FLOW_SEPOLIA = "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC"

# ComposableCoW is CREATE2'd to the same address on every chain.
COMPOSABLE_COW = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"

# GPv2Settlement is also identical across chains.
GPV2_SETTLEMENT = "0x9008D19f58AAbD9eD0D60971565AA8510560ab41"

# topic0 = keccak("OrderPlacement(address,(...12 GPv2Order fields...),(uint8,bytes),bytes)")
ORDER_PLACEMENT_TOPIC = (
    "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"
)

# topic0 = keccak("ConditionalOrderCreated(address,(address,bytes32,bytes))")
CONDITIONAL_ORDER_CREATED_TOPIC = (
    "0x2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361"
)


# ----------------------------------------------------------------- EIP-712

EIP712_DOMAIN_TYPEHASH = keccak(
    b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
)
GPV2_DOMAIN_NAME_HASH = keccak(b"Gnosis Protocol")
GPV2_DOMAIN_VERSION_HASH = keccak(b"v2")
ORDER_TYPEHASH = keccak(
    b"Order(address sellToken,address buyToken,address receiver,"
    b"uint256 sellAmount,uint256 buyAmount,uint32 validTo,"
    b"bytes32 appData,uint256 feeAmount,string kind,"
    b"bool partiallyFillable,string sellTokenBalance,string buyTokenBalance)"
)


def domain_separator(chain_id: int) -> bytes:
    """GPv2Settlement EIP-712 domain separator for a given chain id."""
    return keccak(
        EIP712_DOMAIN_TYPEHASH
        + GPV2_DOMAIN_NAME_HASH
        + GPV2_DOMAIN_VERSION_HASH
        + chain_id.to_bytes(32, "big")
        + bytes(12) + bytes.fromhex(GPV2_SETTLEMENT[2:])
    )


def _pad20(addr_str: str) -> bytes:
    raw = bytes.fromhex(addr_str[2:] if addr_str.startswith("0x") else addr_str)
    if len(raw) == 20:
        return bytes(12) + raw
    if len(raw) == 32:
        return raw
    raise ValueError(f"bad address length: {len(raw)}")


def order_uid(order: dict, owner: str, chain_id: int) -> str:
    """Derive the 56-byte OrderUid for a GPv2OrderData + owner."""
    struct_hash = keccak(
        ORDER_TYPEHASH
        + _pad20(order["sellToken"])
        + _pad20(order["buyToken"])
        + _pad20(order["receiver"])
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
    order_digest = keccak(b"\x19\x01" + domain_separator(chain_id) + struct_hash)
    owner_b = bytes.fromhex(owner[2:] if owner.startswith("0x") else owner)
    if len(owner_b) != 20:
        raise ValueError(f"bad owner length: {len(owner_b)}")
    return "0x" + (order_digest + owner_b + order["validTo"].to_bytes(4, "big")).hex()


# ----------------------------------------------------------------- decoding

def decode_order_placement(log_data_hex: str) -> dict | None:
    """ABI-decode `OrderPlacement.data` into GPv2OrderData + signature + extra.

    Event signature:
        OrderPlacement(
            address indexed sender,                  // topic1
            GPv2Order order,
            OnchainSignature signature,              // (uint8 scheme, bytes payload)
            bytes data,
        )

    The data payload encodes `(order, signature, data)`.
    """
    raw = bytes.fromhex(log_data_hex[2:] if log_data_hex.startswith("0x") else log_data_hex)
    try:
        order, sig, extra = abi_decode(
            [
                "(address,address,address,uint256,uint256,uint32,"
                "bytes32,uint256,bytes32,bool,bytes32,bytes32)",
                "(uint8,bytes)",
                "bytes",
            ],
            raw,
        )
    except Exception:
        return None
    return {
        "order": {
            "sellToken": order[0],
            "buyToken": order[1],
            "receiver": order[2],
            "sellAmount": order[3],
            "buyAmount": order[4],
            "validTo": order[5],
            "appData": order[6],
            "feeAmount": order[7],
            "kind": order[8],
            "partiallyFillable": order[9],
            "sellTokenBalance": order[10],
            "buyTokenBalance": order[11],
        },
        "signature": {"scheme": sig[0], "payload": "0x" + sig[1].hex()},
        "extra_data": "0x" + extra.hex(),
    }


def decode_conditional_order_params(log_data_hex: str) -> dict | None:
    """ABI-decode `ConditionalOrderCreated.data` into the ConditionalOrderParams tuple.

    Event signature:
        ConditionalOrderCreated(
            address indexed owner,                   // topic1
            ConditionalOrderParams params,           // (address handler, bytes32 salt, bytes staticInput)
        )
    """
    raw = bytes.fromhex(log_data_hex[2:] if log_data_hex.startswith("0x") else log_data_hex)
    try:
        (params,) = abi_decode(["(address,bytes32,bytes)"], raw)
    except Exception:
        return None
    handler, salt, static_input = params
    return {
        "handler": handler,
        "salt": "0x" + salt.hex(),
        "static_input": "0x" + static_input.hex(),
    }


def order_to_json(order: dict) -> dict:
    """Re-serialise a decoded GPv2Order as JSON-safe types."""
    return {
        "sellToken": order["sellToken"],
        "buyToken": order["buyToken"],
        "receiver": order["receiver"],
        "sellAmount": str(order["sellAmount"]),
        "buyAmount": str(order["buyAmount"]),
        "validTo": order["validTo"],
        "appData": "0x" + order["appData"].hex(),
        "feeAmount": str(order["feeAmount"]),
        "kind": "0x" + order["kind"].hex(),
        "partiallyFillable": order["partiallyFillable"],
        "sellTokenBalance": "0x" + order["sellTokenBalance"].hex(),
        "buyTokenBalance": "0x" + order["buyTokenBalance"].hex(),
    }


# ----------------------------------------------------------------- rpc

def rpc_call(url: str, method: str, params: list, timeout: int = 30) -> Any:
    """Minimal JSON-RPC helper. Raises on transport or response errors."""
    r = requests.post(
        url,
        json={"jsonrpc": "2.0", "method": method, "params": params, "id": 1},
        timeout=timeout,
    )
    r.raise_for_status()
    data = r.json()
    if "error" in data:
        raise RuntimeError(f"rpc {method} error: {data['error']}")
    return data["result"]


def get_block_number(url: str) -> int:
    return int(rpc_call(url, "eth_blockNumber", []), 16)


def get_block_timestamp(url: str, block_number: int) -> int:
    block = rpc_call(url, "eth_getBlockByNumber", [hex(block_number), False])
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
    """`eth_getLogs` in chunks with halving retry. Mirrors the
    baseline-latency tool's behaviour (PR #57): if the endpoint
    rejects a chunk we halve it down to a 50-block floor; if we hit
    `consecutive_fail_budget` failures even at the floor we raise
    `RpcLimited` so the caller can record the constraint."""
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
                sys.stderr.write(f"  chunk halving to {chunk} after error: {e}\n")
                continue
            consecutive_fails += 1
            if consecutive_fails >= consecutive_fail_budget:
                raise RpcLimited(
                    f"endpoint refused {consecutive_fails} consecutive calls at chunk={chunk}: {e}"
                ) from e
            sys.stderr.write(
                f"  WARN: skipping blocks {cursor}-{end} on chunk={chunk}: {e}\n"
            )
            cursor = end + 1
    return out


# ----------------------------------------------------------------- app_data

def fetch_app_data(cow_api: str, app_data_hash_hex: str) -> dict | None:
    """`GET /api/v1/app_data/{hash}`. Returns the resolved JSON
    document (a dict with `fullAppData` etc.), or `None` on 404.

    The orderbook's `app_data` endpoint exists specifically so
    relayers can look up the user-supplied app_data JSON
    associated with a given hash (the on-chain order only carries
    the hash, not the JSON). Replays that re-submit need the JSON
    so the digest matches; the live equivalent is
    in twap-monitor / ethflow-watcher."""
    r = requests.get(f"{cow_api}/app_data/{app_data_hash_hex}", timeout=30)
    if r.status_code == 404:
        return None
    r.raise_for_status()
    return r.json()


# ----------------------------------------------------------------- main

EMPTY_BYTES32_HEX = "0x" + "00" * 32


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--days", type=int, default=7)
    parser.add_argument(
        "--rpc",
        default=os.environ.get(
            "RPC_URL_SEPOLIA_HTTP", "https://sepolia.drpc.org"
        ),
    )
    parser.add_argument(
        "--cow-api",
        default="https://api.cow.fi/sepolia/api/v1",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("tools/backtest-collect")
        / f"fixtures-{datetime.now(timezone.utc):%Y-%m-%d}.json",
    )
    parser.add_argument(
        "--max-events-per-stream",
        type=int,
        default=500,
        help="cap per event type so app_data resolution stays bounded",
    )
    args = parser.parse_args()

    chain_id = 11155111  # Sepolia
    sys.stderr.write(f"=== backtest-collect (Sepolia, days={args.days}) ===\n")
    sys.stderr.write(f"  rpc: {args.rpc}\n")
    sys.stderr.write(f"  cow-api: {args.cow_api}\n")

    head = get_block_number(args.rpc)
    head_ts = get_block_timestamp(args.rpc, head)
    from_block = max(0, head - args.days * 86400 // 12)
    sys.stderr.write(f"  scanning blocks {from_block}..{head}\n")

    # ---- EthFlow OrderPlacement ----
    sys.stderr.write("\n[ethflow] fetching OrderPlacement logs\n")
    notes: list[str] = []
    try:
        ethflow_logs = get_logs_chunked(
            args.rpc, ETH_FLOW_SEPOLIA, ORDER_PLACEMENT_TOPIC, from_block, head
        )
    except RpcLimited as e:
        notes.append(f"ethflow eth_getLogs RPC-LIMITED: {e}")
        ethflow_logs = []
    sys.stderr.write(f"  events: {len(ethflow_logs)}\n")
    if args.max_events_per_stream and len(ethflow_logs) > args.max_events_per_stream:
        notes.append(
            f"ethflow capped to last {args.max_events_per_stream} of {len(ethflow_logs)}"
        )
        ethflow_logs = ethflow_logs[-args.max_events_per_stream:]

    # ---- ComposableCoW ConditionalOrderCreated ----
    sys.stderr.write("\n[twap] fetching ConditionalOrderCreated logs\n")
    try:
        twap_logs = get_logs_chunked(
            args.rpc, COMPOSABLE_COW, CONDITIONAL_ORDER_CREATED_TOPIC, from_block, head
        )
    except RpcLimited as e:
        notes.append(f"twap eth_getLogs RPC-LIMITED: {e}")
        twap_logs = []
    sys.stderr.write(f"  events: {len(twap_logs)}\n")
    if args.max_events_per_stream and len(twap_logs) > args.max_events_per_stream:
        notes.append(
            f"twap capped to last {args.max_events_per_stream} of {len(twap_logs)}"
        )
        twap_logs = twap_logs[-args.max_events_per_stream:]

    # ---- block timestamp cache (one eth_getBlockByNumber per unique block) ----
    block_ts_cache: dict[int, int] = {head: head_ts}

    def block_ts(b: int) -> int:
        if b not in block_ts_cache:
            block_ts_cache[b] = get_block_timestamp(args.rpc, b)
        return block_ts_cache[b]

    # ---- EthFlow fixtures ----
    sys.stderr.write("\n[ethflow] decoding + UID derivation\n")
    ethflow_fixtures: list[dict] = []
    decode_failed = 0
    app_data_hashes_seen: set[str] = set()
    for log in ethflow_logs:
        decoded = decode_order_placement(log["data"])
        if decoded is None:
            decode_failed += 1
            continue
        # The OrderPlacement.sender is the indexed topic1 (32 bytes,
        # right-padded address).
        sender_topic = log["topics"][1] if len(log["topics"]) > 1 else None
        sender = "0x" + sender_topic[-40:] if sender_topic else None
        # Derive UID via EIP-712 against the EthFlow contract owner.
        try:
            uid = order_uid(decoded["order"], ETH_FLOW_SEPOLIA, chain_id).lower()
        except Exception as e:
            sys.stderr.write(f"  uid derive failed for {log.get('transactionHash')}: {e}\n")
            decode_failed += 1
            continue
        block_num = int(log["blockNumber"], 16)
        app_data_hex = "0x" + decoded["order"]["appData"].hex()
        if app_data_hex.lower() != EMPTY_BYTES32_HEX:
            app_data_hashes_seen.add(app_data_hex.lower())
        ethflow_fixtures.append(
            {
                "uid": uid,
                "block_number": block_num,
                "block_timestamp": block_ts(block_num),
                "tx_hash": log.get("transactionHash"),
                "log_index": int(log.get("logIndex", "0x0"), 16),
                "contract": ETH_FLOW_SEPOLIA.lower(),
                "sender": sender,
                "gpv2_order": order_to_json(decoded["order"]),
                "signature": decoded["signature"],
                "extra_data": decoded["extra_data"],
                "app_data_hash": app_data_hex,
                "app_data_resolved": None,  # filled in below
                # Raw eth_getLogs payload so the Rust replay harness
                # can reconstruct an exact `LogView` (topics + data
                # bytes) without re-encoding from the decoded
                # fields. The strategy decodes from raw bytes; fidelity
                # matters when the goal is "would the strategy have
                # done the same thing it does live?"
                "raw_log": {
                    "topics": log["topics"],
                    "data": log["data"],
                },
            }
        )
    if decode_failed:
        notes.append(f"ethflow: {decode_failed} events failed to decode/derive")
    sys.stderr.write(
        f"  fixtures: {len(ethflow_fixtures)} (failed: {decode_failed})\n"
    )

    # ---- TWAP fixtures ----
    sys.stderr.write("\n[twap] decoding ConditionalOrderParams\n")
    twap_fixtures: list[dict] = []
    twap_decode_failed = 0
    for log in twap_logs:
        params = decode_conditional_order_params(log["data"])
        if params is None:
            twap_decode_failed += 1
            continue
        owner_topic = log["topics"][1] if len(log["topics"]) > 1 else None
        owner = "0x" + owner_topic[-40:] if owner_topic else None
        block_num = int(log["blockNumber"], 16)
        twap_fixtures.append(
            {
                "owner": owner,
                "block_number": block_num,
                "block_timestamp": block_ts(block_num),
                "tx_hash": log.get("transactionHash"),
                "log_index": int(log.get("logIndex", "0x0"), 16),
                "params": params,
                "raw_log": {
                    "topics": log["topics"],
                    "data": log["data"],
                },
            }
        )
    if twap_decode_failed:
        notes.append(f"twap: {twap_decode_failed} events failed to decode")
    sys.stderr.write(
        f"  fixtures: {len(twap_fixtures)} (failed: {twap_decode_failed})\n"
    )

    # ---- app_data resolution (EthFlow only — TWAP staticInput carries its own data) ----
    if app_data_hashes_seen:
        sys.stderr.write(
            f"\n[app_data] resolving {len(app_data_hashes_seen)} unique hashes\n"
        )
        resolved: dict[str, dict | None] = {}
        for h in sorted(app_data_hashes_seen):
            doc = fetch_app_data(args.cow_api, h)
            resolved[h] = doc
            if doc is None:
                sys.stderr.write(f"  {h[:14]}.. 404\n")
        not_found = sum(1 for v in resolved.values() if v is None)
        if not_found:
            notes.append(
                f"app_data: {not_found}/{len(app_data_hashes_seen)} hashes 404'd "
                f"(not mirrored by orderbook — expected for some external app_data flows)"
            )
        # Stitch the resolved documents back into each fixture row.
        for fx in ethflow_fixtures:
            fx["app_data_resolved"] = resolved.get(fx["app_data_hash"])
        sys.stderr.write(
            f"  resolved: {len(resolved) - not_found}/{len(app_data_hashes_seen)}\n"
        )

    # ---- write fixtures file ----
    out_doc = {
        "metadata": {
            "collected_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "chain_id": chain_id,
            "chain_name": "Sepolia",
            "window_days": args.days,
            "from_block": from_block,
            "to_block": head,
            "rpc_url": args.rpc,
            "cow_api": args.cow_api,
            "ethflow_owner": ETH_FLOW_SEPOLIA.lower(),
            "composable_cow": COMPOSABLE_COW.lower(),
            "notes": notes,
        },
        "ethflow_orders": ethflow_fixtures,
        "twap_conditionals": twap_fixtures,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(out_doc, indent=2))
    sys.stderr.write(
        f"\nfixtures written: {args.out}\n"
        f"  ethflow_orders: {len(ethflow_fixtures)}\n"
        f"  twap_conditionals: {len(twap_fixtures)}\n"
        f"  notes: {len(notes)}\n"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
