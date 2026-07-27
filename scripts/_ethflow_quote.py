#!/usr/bin/env python3
"""ethflow quote + tuple-encode helper.

Called by scripts/e2e-onchain.sh. Hits the CoW Sepolia orderbook
`/api/v1/quote` endpoint for a native-ETH sell, then ABI-encodes the
EthFlowOrder.Data tuple the EthFlow contract expects as the
`createOrder` argument, plus the msg.value the operator must send.

Output (stdout, two lines):

    CALLDATA=0x<hex>
    VALUE_WEI=<integer>

The script is deliberately fail-loud: any non-200 from cow.fi or a
quote shape we don't recognise aborts with a non-zero exit.
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

from eth_abi import encode
from eth_utils import keccak

COW_API     = "https://api.cow.fi/sepolia/api/v1"
# CoW's quote endpoint rejects the native-ETH sentinel
# (`InvalidNativeSellToken`). EthFlow orders are quoted with the
# wrapped form (WETH9 Sepolia) as the sell side and the EthFlow
# contract handles the wrap on `createOrder` from `msg.value`.
WETH_SEPOLIA = "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14"
BUY_TOKEN    = "0x0625aFB445C3B6B7B929342a04A22599fd5dBB59"  # COW Sepolia

EMPTY_APP_DATA_JSON = "{}"
EMPTY_APP_DATA_HASH = "0x" + keccak(EMPTY_APP_DATA_JSON.encode()).hex()


def fetch_quote(eoa: str, sell_amount_wei: int) -> dict:
    body = {
        "sellToken":            WETH_SEPOLIA,
        "buyToken":             BUY_TOKEN,
        "from":                 eoa,
        "receiver":             eoa,
        "sellAmountBeforeFee":  str(sell_amount_wei),
        "kind":                 "sell",
        "partiallyFillable":    False,
        "sellTokenBalance":     "erc20",
        "buyTokenBalance":      "erc20",
        "signingScheme":        "eip1271",
        "onchainOrder":         True,
        "appData":              EMPTY_APP_DATA_JSON,
        "appDataHash":          EMPTY_APP_DATA_HASH,
    }
    req = urllib.request.Request(
        f"{COW_API}/quote",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Accept": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        sys.exit(f"cow.fi /quote returned {e.code}: {e.read().decode(errors='replace')}")


def main() -> None:
    eoa             = sys.argv[1]
    sell_amount_wei = int(sys.argv[2])

    q = fetch_quote(eoa, sell_amount_wei)
    inner = q["quote"]
    quote_id     = int(q["id"])
    fee_amount   = int(inner["feeAmount"])
    buy_amount   = int(inner["buyAmount"])
    valid_to     = int(inner["validTo"])
    # The quote endpoint may have rebalanced sellAmount to reflect the
    # fee; for an EthFlow order we honour the rebalanced value.
    sell_amount  = int(inner["sellAmount"])

    # EthFlowOrder.Data:
    #   address buyToken;
    #   address receiver;
    #   uint256 sellAmount;
    #   uint256 buyAmount;
    #   bytes32 appData;
    #   uint256 feeAmount;
    #   uint32  validTo;
    #   bool    partiallyFillable;
    #   int64   quoteId;
    encoded = encode(
        ["(address,address,uint256,uint256,bytes32,uint256,uint32,bool,int64)"],
        [(
            BUY_TOKEN,
            eoa,
            sell_amount,
            buy_amount,
            bytes.fromhex(EMPTY_APP_DATA_HASH[2:]),
            fee_amount,
            valid_to,
            False,
            quote_id,
        )]
    )
    selector = keccak(b"createOrder((address,address,uint256,uint256,bytes32,uint256,uint32,bool,int64))")[:4]
    calldata = selector + encoded
    value_wei = sell_amount + fee_amount

    print(f"CALLDATA=0x{calldata.hex()}")
    print(f"VALUE_WEI={value_wei}")
    print(f"# fee_amount={fee_amount} buy_amount={buy_amount} valid_to={valid_to} quote_id={quote_id} sell_amount={sell_amount}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
