# E2E run-prep punch list

Companion to `docs/operations/e2e-testnet-runbook.md`. This file
captures every **pinned value** for the 2026-06-18 dry run so the operator can copy-paste through the on-chain
actions without re-deriving any UID, address, or calldata.

If you are running a *later* E2E run (different EOA, different
Safe, different config), do not reuse the UIDs / calldatas — they
are a function of all the pinned config below. Either re-derive
via the Python recipes in this doc, or re-run
`cargo test -p stop-loss --lib cow_1064` to lock the new UID.

---

## 0. Pinned identities (2026-06-18 run)

| Role | Address | Network | Notes |
|---|---|---|---|
| Test EOA | `0x7bF140727D27ea64b607E042f1225680B40ECa6A` | Sepolia | Bruno-controlled. Funds itself via faucet. |
| Test Safe (single-sig, threshold 1) | `0x14995a1118Caf95833e923faf8Dd155721cd53c2` | Sepolia | EOA is the sole owner. Submits TWAP order. |
| ComposableCoW | `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74` | Sepolia | Where `create((address,bytes32,bytes),bool)` lands. |
| TWAP handler | `0x6cF1e9cA41f7611dEf408122793c358a3d11E5a5` | Sepolia | `ConditionalOrderParams.handler`. |
| CoWSwapEthFlow | `0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC` | Sepolia | EthFlow's production deployment; emits `OrderPlacement`. |
| GPv2Settlement | `0x9008D19f58AAbD9eD0D60971565AA8510560ab41` | Sepolia | `setPreSignature(orderUid, signed)` lives here. |
| GPv2VaultRelayer | `0xc92e8bdf79f0507f65a392b0ab4667716bfe0110` | Sepolia | Spender for sell-token ERC-20 approvals. |
| WETH9 | `0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14` | Sepolia | `deposit()` payable wraps ETH; `balanceOf(EOA)` is the sell-side balance. |
| COW Token | `0x0625aFB445C3B6B7B929342a04A22599fd5dBB59` | Sepolia | name="CoW Protocol Token", symbol="COW", decimals=18. |
| GPv2 domain separator | `0xdaee378bd0eb30ddf479272accf91761e697bc00e067a268f95f1d2732ed230b` | Sepolia | EIP-712 domain digest queried from chain. |

All addresses verified via `eth_getCode > 0` on
`https://ethereum-sepolia-rpc.publicnode.com` as of run prep.

---

## 1. Per-module config pinning

### stop-loss

`modules/examples/stop-loss/module.toml` is checked in on the
`feat/e2e-run-config-cow-1064` branch with the production-ready
config for this run. Effective values:

| Field | Value | Notes |
|---|---|---|
| `oracle_address` | `0x694AA1769357215DE4FAC081bf1f309aDC325306` | Chainlink ETH/USD Sepolia. |
| `decimals` | `8` | Chainlink USD-pair convention. |
| `trigger_price` | `2000.00` | Above the live Sepolia mocked answer (~$1681), `direction=below` → triggers on first block. |
| `owner` | `0x7bF1...Ca6A` | Test EOA. |
| `sell_token` | `0xfFf9...6B14` | WETH9 Sepolia. |
| `buy_token` | `0x0625...BB59` | COW Sepolia. |
| `sell_amount_wei` | `5000000000000000` | 0.005 WETH. |
| `buy_amount_wei` | `20000000000000000000` | 20 COW. Conservative quote at run-prep time. |
| `valid_to_seconds` | `4294967295` | uint32::MAX. |

### Resulting OrderUid

The strategy's `build_creation` is pinned by the
`cow_1064_e2e_settings_yield_expected_uid` regression test
(`crates/.../stop-loss/src/strategy.rs`). The canonical UID:

```
0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be87bf140727d27ea64b607e042f1225680b40eca6affffffff
```

Decomposition (per `packOrderUidParams`):

| Offset | Bytes | Field | Value |
|---|---|---|---|
| 0..32 | 32 | `orderDigest` (EIP-712) | `0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be8` |
| 32..52 | 20 | `owner` | `0x7bf140727d27ea64b607e042f1225680b40eca6a` |
| 52..56 | 4 | `validTo` (uint32) | `0xffffffff` |

### balance-tracker

Pinned to the EOA + Safe so the run sees ETH-balance diffs:

| Field | Value |
|---|---|
| `addresses` | `0x7bF1...Ca6A,0x1499...53c2` |
| `change_threshold` | `1000000000000000` (0.001 ETH) |

---

## 2. On-chain actions for the run window

> Order: action 1 can be done at any time before/during the run.
> Actions 2-4 should fire **after** the engine prints
> `INFO supervisor ready modules=5 chains=1` so the modules
> observe the events. They are independent; do them in any order.

### Action 1 (optional, pre-run): wrap 0.01 ETH → 0.01 WETH

Without WETH, stop-loss will hit `TransferSimulationFailed` ->
`backoff:` write (which is itself a valid terminal-marker per
the acceptance bar). To get the **`submitted:`** path,
wrap first then do action 2.

- Etherscan: https://sepolia.etherscan.io/address/0xfff9976782d46cc05630d1f6ebab18b2324d6b14#writeContract
- Connect Web3 from the EOA in Metamask
- Function `deposit` → payable value `0.01` ETH → Write

Verify: `balanceOf(EOA)` returns `10000000000000000` post-tx.

### Action 2 (optional, only if action 1 done): pre-sign stop-loss order

- Etherscan: https://sepolia.etherscan.io/address/0x9008d19f58aabd9ed0d60971565aa8510560ab41#writeProxyContract
- Connect Web3 from the EOA
- Function `setPreSignature(bytes orderUid, bool signed)`:
  - `orderUid`:
    ```
    0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be87bf140727d27ea64b607e042f1225680b40eca6affffffff
    ```
  - `signed`: `true`
- Write

Also approve WETH → GPv2VaultRelayer so the settle path is real:

- Etherscan: https://sepolia.etherscan.io/address/0xfff9976782d46cc05630d1f6ebab18b2324d6b14#writeContract
- Function `approve(address guy, uint256 wad)`:
  - `guy`: `0xc92e8bdf79f0507f65a392b0ab4667716bfe0110`
  - `wad`: `5000000000000000` (0.005 WETH — matches the order's sell_amount)
- Write

### Action 3: TWAP conditional order via Safe TX Builder

Triggers `ConditionalOrderCreated` → twap-monitor writes
`watch:{orderHash}`. The Safe pays the gas (~0.003 ETH); the
order will TRY to settle later but the Safe holds no WETH so
settlement will fail. **That's fine** — only the `create()`
event is required for the acceptance marker.

- Safe app: https://app.safe.global/transactions/queue?safe=sep:0x14995a1118Caf95833e923faf8Dd155721cd53c2
- New transaction → Transaction Builder
- Enter contract address: `0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74`
- Toggle "Use custom data (hex encoded)" ON
- Generate the calldata locally (do NOT paste a pinned blob):

```bash
python3 scripts/_twap_calldata.py
```

The helper backdates `t0` by 60 s on every invocation so part 0 is
Ready immediately. The constants (sell/buy tokens, amounts, n, t,
salt) mirror section 4.2; edit there + in the helper in lockstep
if the TWAP shape changes.

Copy the helper's stdout into the Transaction Builder's custom-data
field. The blob is ~516 bytes - the `create(ConditionalOrderParams,
bool dispatch)` call with a 2-part TWAP from WETH → COW, 0.001 WETH
per part, 600 s between parts, salt pinned to `0x...6670f000`.

> Historical note: a previously-pinned variant of this calldata
> hardcoded `t0 = 0`, which silently produced an
> `AFTER_TWAP_FINISHED` revert on every poll because
> `calculateValidTo` divided `block.timestamp` by `t` and exceeded
> `n`. Surfaced in the 2026-06-18 dry run. Always derive
> via the helper.

- ETH value: `0`
- Create batch → Send batch → sign with the EOA

Expected log within 1-2 Sepolia blocks:

```
INFO twap-monitor watch:0x<orderHash>  chain_id=11155111
```

### Action 4: EthFlow swap via cow-swap UI

Triggers `OrderPlacement` → ethflow-watcher writes
`submitted:{uid}` (or `dropped:{uid}` if the orderbook rejects;
both are valid terminal markers).

Easiest path is the cow-swap UI:

1. https://swap.cow.fi/#/11155111/swap/ETH/COW (Sepolia)
2. Connect Metamask, EOA selected, network=Sepolia
3. Sell amount: `0.005` ETH
4. Click "Swap" → it builds the EthFlow `createOrder` tx
5. Approve in Metamask

The UI handles `quoteId` resolution + `appData` IPFS pinning +
EthFlow contract call. Sell amount is small enough to fit in the
~0.05 ETH budget plus gas.

Expected log within 1-2 Sepolia blocks:

```
INFO ethflow-watcher submitted:0x<uid>
```

If the UI errors out (Sepolia orderbook can be flaky), fallback
to calling EthFlow directly via Etherscan:

- https://sepolia.etherscan.io/address/0xba3cb449bd2b4adddbc894d8697f5170800eadec#writeContract
- Function `createOrder((address,address,uint256,uint256,bytes32,uint256,uint32,bool,int64))`
- The shape of the tuple needs the orderbook quote endpoint hit
  first to get `feeAmount` + `quoteId` — easier to defer to the
  UI for the run.

---

## 3. Validation snippets for the operator

Run these in a separate shell while the engine is up:

```bash
RPC="wss://eth-sepolia.g.alchemy.com/v2/<YOUR_KEY>"   # replace
EOA="0x7bF140727D27ea64b607E042f1225680B40ECa6A"
SAFE="0x14995a1118Caf95833e923faf8Dd155721cd53c2"
WETH="0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14"

# EOA + Safe balances
cast balance $EOA  --rpc-url $RPC
cast balance $SAFE --rpc-url $RPC

# EOA WETH balance + GPv2VaultRelayer allowance
cast call $WETH "balanceOf(address)(uint256)" $EOA --rpc-url $RPC
cast call $WETH "allowance(address,address)(uint256)" \
    $EOA 0xc92e8bdf79f0507f65a392b0ab4667716bfe0110 --rpc-url $RPC

# Did setPreSignature land?
cast call 0x9008D19f58AAbD9eD0D60971565AA8510560ab41 \
    "preSignature(bytes)(uint256)" \
    0xc2b9cb4ea1ee5a86d8049ac09d8f494bf04cca0a68407285f31e2e6379800be87bf140727d27ea64b607e042f1225680b40eca6affffffff \
    --rpc-url $RPC
# Returns 1 if pre-signed, 0 otherwise.

# Mine the supervisor log for terminal markers in real time
journalctl -u shepherd -f --output=json \
    | jq -r '.MESSAGE | fromjson? | select(.fields.message | test("watch:|submitted:|dropped:|backoff:|TRIGGERED")) | "\(.fields.module): \(.fields.message)"'
```

(If you don't have `cast` installed: `curl -L https://foundry.paradigm.xyz | bash && foundryup`.)

---

## 4. Recipes for re-deriving the pinned values

If anything in section 0 drifts, regenerate from these recipes.

### 4.1 OrderUid

Either:

```bash
cargo test -p stop-loss --lib cow_1064 -- --nocapture
```

(asserts against the same constants pinned in `module.toml`,
fails loudly if the EIP-712 type-hash or domain separator
shifts).

Or with raw Python:

```python
from eth_utils import keccak

# Replace these 8 values to re-derive
DOMAIN_SEP  = bytes.fromhex("daee378bd0eb30ddf479272accf91761e697bc00e067a268f95f1d2732ed230b")
SELL_TOKEN  = bytes.fromhex("fFf9976782d46CC05630D1f6eBAb18b2324d6B14")
BUY_TOKEN   = bytes.fromhex("0625aFB445C3B6B7B929342a04A22599fd5dBB59")
OWNER       = bytes.fromhex("7bF140727D27ea64b607E042f1225680B40ECa6A")
RECEIVER    = OWNER
SELL_AMOUNT = 5_000_000_000_000_000
BUY_AMOUNT  = 20_000_000_000_000_000_000
VALID_TO    = 4_294_967_295

APP_DATA  = bytes.fromhex("b48d38f93eaa084033fc5970bf96e559c33c4cdc07d889ab00b4d63f9590739d")  # keccak("{}")
KIND_SELL = keccak(b"sell")
ERC20     = keccak(b"erc20")
TYPE_HASH = keccak(b"Order(address sellToken,address buyToken,address receiver,uint256 sellAmount,uint256 buyAmount,uint32 validTo,bytes32 appData,uint256 feeAmount,string kind,bool partiallyFillable,string sellTokenBalance,string buyTokenBalance)")
pad32 = lambda b: bytes(32-len(b)) + b
uint  = lambda v: v.to_bytes(32, "big")
struct_hash = keccak(
    TYPE_HASH + pad32(SELL_TOKEN) + pad32(BUY_TOKEN) + pad32(RECEIVER)
    + uint(SELL_AMOUNT) + uint(BUY_AMOUNT) + uint(VALID_TO)
    + APP_DATA + uint(0) + KIND_SELL
    + b"\x00"*32 + ERC20 + ERC20  # partiallyFillable=false
)
order_digest = keccak(b"\x19\x01" + DOMAIN_SEP + struct_hash)
uid = order_digest + OWNER + VALID_TO.to_bytes(4, "big")
print("0x" + uid.hex())
```

### 4.2 ComposableCoW.create() calldata

```python
import time
from eth_utils import keccak
from eth_abi import encode

selector = keccak(b"create((address,bytes32,bytes),bool)")[:4]
# Edit these 10 fields to retarget the TWAP
static = encode(
    ["(address,address,address,uint256,uint256,uint256,uint256,uint256,uint256,bytes32)"],
    [(
        "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14",   # sellToken
        "0x0625aFB445C3B6B7B929342a04A22599fd5dBB59",   # buyToken
        "0x14995a1118Caf95833e923faf8Dd155721cd53c2",   # receiver
        1_000_000_000_000_000, 500_000_000_000_000_000, # partSellAmount, minPartLimit
        int(time.time()) - 60, 2, 600, 0,               # t0 (NEVER 0 - see note above), n, t, span
        b"\x00" * 32,                                   # appData
    )]
)
calldata = selector + encode(
    ["(address,bytes32,bytes)", "bool"],
    [(
        "0x6cF1e9cA41f7611dEf408122793c358a3d11E5a5",   # TWAP handler
        bytes.fromhex("000000000000000000000000000000000000000000000000000000006670f000"),  # salt
        static,
    ), True]
)
print("0x" + calldata.hex())
```

---

## 5. Acceptance checklist for THIS run

Hand-check at the end of the run (also goes in
`e2e-report-YYYY-MM-DD.md` section 7):

- [ ] EOA at `0x7bF1...Ca6A` still has ≥ 0.03 ETH remaining
- [ ] twap-monitor logged `watch:0x...` after action 3
- [ ] ethflow-watcher logged `submitted:0x...` after action 4
- [ ] stop-loss logged `backoff:` or `TRIGGERED + submitted:` (depending on whether action 1+2 ran)
- [ ] price-alert logged `TRIGGERED` on first block
- [ ] balance-tracker logged a `last:0x7bf1...` write on first block + at least one Warn diff log over the run window
- [ ] `shepherd_module_poisoned{...} == 0` for all 5 modules at end
- [ ] `shepherd_module_errors_total{error_kind="trap"} == 0` for all modules
- [ ] ≥ 1500 Sepolia blocks dispatched (`block delta` in report section 2)

If all green the run is complete and the 7-day soak can start.
