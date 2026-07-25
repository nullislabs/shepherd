# E2E testnet runbook

Runs all 5 production modules (twap-monitor, ethflow-watcher, price-alert, balance-tracker, stop-loss) on a live Sepolia host simultaneously for 4-6 h and captures a structured report under `docs/operations/e2e-reports/`. This is the correctness step between unit-test coverage and the 7-day soak.

Acceptance bar:

- >= 1500 Sepolia blocks (~5 h at 12 s block time).
- Each of the 5 modules writes at least one terminal-state marker (`submitted:` / `dropped:` / `backoff:` / `TRIGGERED` / `last:`).
- 0 unexpected errors in the supervisor log.
- 0 well-behaved modules trapped or poisoned at end of run.
- A committed report.

## 0. Prerequisites

### Toolchain

Same as the M2 + M3 runbooks (`rustup target add wasm32-wasip2`, `just`, a Sepolia WS RPC).

### RPC

The public Sepolia node throttles `eth_subscribe` and `eth_call` under sustained load. The run holds 1 block subscription (shared across 4 modules), 2 log subscriptions (twap-monitor `ConditionalOrderCreated`, ethflow-watcher `OrderPlacement`), and >= 4 `eth_call` per block. Override `rpc_url` in `engine.e2e.toml` with an Alchemy / Infura WS:

```toml
[chains.11155111]
rpc_url = "wss://eth-sepolia.g.alchemy.com/v2/<KEY>"
```

### On-chain prep (operator)

The acceptance bar requires real on-chain submissions. Prepare:

1. A funded test EOA on Sepolia (>= 0.05 ETH for gas; also covers the EthFlow swap + stop-loss `setPreSignature`).
2. A Safe (or direct caller) that can call ComposableCoW, for the TWAP conditional-order submission.
3. stop-loss config aligned with that EOA: set `[config].owner` in `modules/examples/stop-loss/module.toml` to the EOA, and pick a `sell_token` / `buy_token` pair the EOA holds and has approved to the GPv2VaultRelayer (M3 runbook section 2 has the pre-sign + allowance recipe).

The run boots without (1)/(2)/(3), but the acceptance bar needs one `submitted:` marker on each of twap-monitor / ethflow-watcher / stop-loss, which only on-chain triggers produce.

## 1. Boot

```bash
just run-e2e
```

Long form:

```bash
just build-e2e         # builds the 5 module .wasm artefacts
cargo run -p shepherd -- --engine-config engine.e2e.toml
```

Expected boot (~5 s) ends with:

```
INFO  metrics exporter listening at /metrics addr=127.0.0.1:9100
INFO  init succeeded module=twap-monitor
INFO  init succeeded module=ethflow-watcher
INFO  init succeeded module=price-alert
INFO  init succeeded module=balance-tracker
INFO  init succeeded module=stop-loss
INFO  supervisor ready modules=5 chains=1
INFO  log subscription open chain_id=11155111 module=twap-monitor
INFO  log subscription open chain_id=11155111 module=ethflow-watcher
```

If `modules=5` or either log subscription is missing, stop the run and triage before committing to 4-6 h.

## 2. The run

### 2.1 Start the clock and baseline

```bash
just run-e2e 2>&1 | tee -a docs/operations/e2e-reports/engine-$(date -u +%Y%m%dT%H%M%SZ).log
curl -s http://127.0.0.1:9100/metrics > docs/operations/e2e-reports/metrics-start.txt
```

Record `date -u --iso-8601=seconds` and `git rev-parse HEAD` in section 1 of the report.

### 2.2 Trigger each on-chain action

Run as soon as the supervisor is `ready`:

1. **TWAP order**: call ComposableCoW from the Safe. Within 1-2 blocks: `INFO twap-monitor watch:{orderHash}`.
2. **EthFlow swap**: execute a small ETH-flow swap from the EOA via the CoW Swap front-end on Sepolia. Within 1-2 blocks: `INFO ethflow-watcher submitted:{uid}` (or a typed `dropped:{uid}`, both terminal markers).
3. **stop-loss trigger**: once the owner EOA has called `setPreSignature` and approved the sell token, lower `trigger_price` in `modules/examples/stop-loss/module.toml` to <= the current Chainlink ETH/USD answer and reload. Within 1 block: `INFO stop-loss TRIGGERED` then `submitted:{uid}`.

### 2.3 Idle until end of run

Once all three markers are observed, leave the engine undisturbed for the remainder of the window. Red flags (each is a defect for report section 6):

| Red flag | Why it matters |
|---|---|
| `ERROR` from `nexum_runtime::*` | Acceptance: zero ERROR lines |
| `module ... trapped:` for a non-fixture module | Trapping a production module is a defect |
| `module ... poisoned` | Quarantine of a real module is a defect |
| `stream reconnect attempt=N` with N rising | WS flapping. One reconnect per chain is fine |
| `chain::request` `err` rate > 5% | RPC degraded. Switch keys / providers |

### 2.4 Capture deltas and shut down

```bash
curl -s http://127.0.0.1:9100/metrics > docs/operations/e2e-reports/metrics-end.txt
# Ctrl-C: graceful shutdown logs `dispatched_blocks=N dispatched_logs=M uptime_secs=K`.
diff <(grep '^shepherd_' docs/operations/e2e-reports/metrics-start.txt) \
     <(grep '^shepherd_' docs/operations/e2e-reports/metrics-end.txt)
```

## 3. Report

Copy the template at the start of the run and fill it as the run progresses:

```bash
DATE=$(date -u +%Y-%m-%d)
cp docs/operations/e2e-reports/e2e-report.template.md docs/operations/e2e-reports/e2e-report-${DATE}.md
```

Every acceptance box in the template's section 7 must be `[x]` for the run to pass. Commit the filled report on this branch.

## 4. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `supervisor ready modules=4 chains=1` at boot | A module manifest failed to load, likely a missing wasm artefact | Re-run `just build-e2e`; verify all 5 `.wasm` present |
| Only one `log subscription open` | One log-subscribing module failed init | Check the preceding `init failed module=...`; usual culprit is `[capabilities]` or the subscription `address` |
| RPC drops every ~30 min on `publicnode.com` | Public node rate limits | Switch to Alchemy / Infura |
| `stop-loss TRIGGERED` fires immediately | Default `trigger_price` above the feed with `direction = below` | Tune `trigger_price` lower |
| `twap-monitor` never logs `watch:` | No `ConditionalOrderCreated` observed | Submit the TWAP order (2.2 step 1) |
| `ethflow-watcher` never logs `submitted:` | No `OrderPlacement` observed | Execute the EthFlow swap (2.2 step 2) |

## 5. Known Sepolia constraint: EthFlow `validTo = u32::MAX`

EthFlow on-chain orders carry `validTo = type(uint32).max` by design (cancellation is operator-controlled via the EthFlow contract). The Sepolia orderbook's max-validTo cap rejects this shape with `errorType = "ExcessiveValidTo"`, so every EthFlow placement on Sepolia terminates as `Drop`. The keeper recognises this and degrades gracefully:

- `ethflow dropped <uid> (400): orderbook error (ExcessiveValidTo)...` at Info level.
- `dropped:{uid}` written once per placement.
- `shepherd_cow_api_submit_total{outcome="err"}` grows by exactly the EthFlow placement count, then stops.

This is a testnet orderbook constraint, not a bug; the report should note it so it is not filed as an anomaly.

## 6. Re-deriving pinned values

If the pinned identities in a run config drift, regenerate.

### OrderUid

```bash
cargo test -p stop-loss --lib cow_1064 -- --nocapture
```

Asserts against the constants in `module.toml`; fails loudly if the EIP-712 type-hash or domain separator shifts. Raw Python equivalent:

```python
from eth_utils import keccak

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

### ComposableCoW.create() calldata

Generate locally with `python3 scripts/_twap_calldata.py` (never paste a pinned blob). The helper backdates `t0` by 60 s per invocation so part 0 is Ready immediately; `t0` must never be 0 or every poll reverts `AFTER_TWAP_FINISHED`. Constants:

```python
import time
from eth_utils import keccak
from eth_abi import encode

selector = keccak(b"create((address,bytes32,bytes),bool)")[:4]
static = encode(
    ["(address,address,address,uint256,uint256,uint256,uint256,uint256,uint256,bytes32)"],
    [(
        "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14",   # sellToken
        "0x0625aFB445C3B6B7B929342a04A22599fd5dBB59",   # buyToken
        "0x14995a1118Caf95833e923faf8Dd155721cd53c2",   # receiver
        1_000_000_000_000_000, 500_000_000_000_000_000, # partSellAmount, minPartLimit
        int(time.time()) - 60, 2, 600, 0,               # t0 (never 0), n, t, span
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

## 7. References

- M2 + M3 runbooks (sister docs)
- Engine config: `engine.e2e.toml`
- Report template: `docs/operations/e2e-reports/e2e-report.template.md`
