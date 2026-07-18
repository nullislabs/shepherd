#!/usr/bin/env python3
"""Emit ComposableCoW.create() calldata for the E2E TWAP order.

Why this exists: the prior version of `e2e-onchain.sh` pinned
a 516-byte hex blob with `t0 = 0` in the static-input tuple. TWAP
handler's `validateData` does NOT reject `t0 = 0` (it only checks
`t0 >= type(uint32).max`), so the `create()` tx succeeded - but
`TWAPOrderMathLib.calculateValidTo` then computes:

    part = (block.timestamp - 0) / t  =  ~3,300,000

which is >> the configured `n = 2`, triggering `AFTER_TWAP_FINISHED`
reverts on every `getTradeableOrderWithSignature` poll. The order
was permanently dead at submission.

The fix is to derive `t0` from wall-clock just before the create()
call. `t0 = now() - 60` makes part 0 immediately tradeable (the
60-second backdate covers Sepolia block lag without breaking the
TWAP math).

Anyone reading this: do NOT hardcode `t0` again. The whole point of
this helper is to keep `t0` derived from the current run.

Outputs a single hex string on stdout; the shell script captures it
into `twap_calldata`. Exits non-zero on any internal error (missing
deps, encoder failure).

Constants below mirror `docs/operations/e2e-prep.md` section
4.2. Edit there + here in lockstep if the TWAP shape changes.
"""

import sys
import time

try:
    from eth_abi import encode
    from eth_utils import keccak
except ImportError:
    sys.stderr.write(
        "missing Python deps. Run: pip3 install eth-abi eth-utils "
        '"eth-hash[pycryptodome]"\n'
    )
    sys.exit(1)


def main() -> int:
    # TWAP handler (Sepolia) - keep in sync with e2e-prep.md
    twap_handler = "0x6cF1e9cA41f7611dEf408122793c358a3d11E5a5"
    # Static-input fields. Edit in lockstep with the prep doc.
    sell_token = "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14"  # WETH
    buy_token = "0x0625aFB445C3B6B7B929342a04A22599fd5dBB59"  # COW
    receiver = "0x14995a1118Caf95833e923faf8Dd155721cd53c2"  # Safe
    part_sell_amount = 1_000_000_000_000_000  # 0.001 WETH per part
    min_part_limit = 500_000_000_000_000_000  # 0.5 COW per part (min out)
    n = 2  # number of parts
    t = 600  # seconds between parts
    span = 0  # full part window (no early-completion clamp)
    app_data = b"\x00" * 32  # empty app_data hash
    salt = bytes.fromhex(
        "000000000000000000000000000000000000000000000000000000006670f000"
    )

    # The whole point of this helper: t0 is derived from wall-clock,
    # backdated 60s so part 0 is Ready immediately. See module
    # docstring for why hardcoding t0=0 was a prior bug.
    t0 = int(time.time()) - 60

    selector = keccak(b"create((address,bytes32,bytes),bool)")[:4]
    static = encode(
        [
            "(address,address,address,uint256,uint256,"
            "uint256,uint256,uint256,uint256,bytes32)"
        ],
        [
            (
                sell_token,
                buy_token,
                receiver,
                part_sell_amount,
                min_part_limit,
                t0,
                n,
                t,
                span,
                app_data,
            )
        ],
    )
    calldata = selector + encode(
        ["(address,bytes32,bytes)", "bool"],
        [(twap_handler, salt, static), True],
    )
    sys.stdout.write("0x" + calldata.hex() + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
