#!/usr/bin/env bash
# scripts/e2e-onchain.sh — execute the on-chain side of the E2E run.
#
# Pre-flight:
#   - derive the EOA address from $OPERATOR_PRIVATE_KEY
#     and assert it matches the pinned $TEST_EOA;
#   - assert balance ≥ 0.02 ETH (covers 2 tx + slippage).
#
# Required actions (cover twap-monitor + ethflow-watcher markers):
#   1. ComposableCoW.create(...) — fires ConditionalOrderCreated;
#      uses the 516-byte calldata pinned in lib.sh /
#      e2e-prep.md so the TWAP order shape is reproducible.
#   2. EthFlow.createOrder(EthFlowOrder.Data) — fires OrderPlacement;
#      tuple built dynamically from the cow.fi /quote response (the
#      `quoteId` + `feeAmount` only exist after a quote, so this part
#      is not pinned to a constant).
#
# Optional path (only if $RUN_OPTIONAL_PRESIGN=1 in scripts/.env):
#   3. WETH9.deposit() — wrap 0.01 ETH so stop-loss has a sell-side
#      balance.
#   4. setPreSignature($EXPECTED_ORDER_UID, true) — enables the
#      already-submitted stop-loss order for settlement.
#   5. WETH9.approve(GPv2VaultRelayer, 0.005 ETH) — sell-side
#      allowance.
#
# Output: each tx hash is appended to scripts/.state under
# TX_<KIND>=0x<hash> so the report generator can link them.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd cast
require_cmd curl
require_cmd python3

python3 -c 'import eth_abi, eth_utils, eth_hash.auto' 2>/dev/null \
    || die "missing Python deps. Run: pip3 install eth-abi eth-utils \"eth-hash[pycryptodome]\""

load_env
[[ -n "${OPERATOR_PRIVATE_KEY:-}" ]] || die "OPERATOR_PRIVATE_KEY unset in scripts/.env"

derived="$(cast wallet address --private-key "$OPERATOR_PRIVATE_KEY" 2>/dev/null)" \
    || die "cast wallet address failed — is OPERATOR_PRIVATE_KEY a valid 0x-prefixed 32-byte hex?"
# macOS still ships bash 3.2; ${var,,} (lowercase) is bash 4+ only,
# so we route through `tr` for case-insensitive comparison.
lower_derived="$(printf '%s' "$derived"  | tr '[:upper:]' '[:lower:]')"
lower_expected="$(printf '%s' "$TEST_EOA" | tr '[:upper:]' '[:lower:]')"
if [[ "$lower_derived" != "$lower_expected" ]]; then
    die "private key derives to $derived, expected $TEST_EOA — wrong EOA loaded"
fi
log "EOA: $derived"

balance="$(cast balance "$TEST_EOA" --rpc-url "$RPC_URL_SEPOLIA_HTTP")"
log "EOA balance: $(python3 -c "print(f'{int(\"$balance\")/1e18:.6f} ETH')") ($balance wei)"
if (( balance < 20000000000000000 )); then  # 0.02 ETH
    die "EOA balance < 0.02 ETH — top up from a Sepolia faucet first"
fi

# ── Action 1: ComposableCoW.create() ─────────────────────────────────

# Derive the calldata fresh on every invocation so the
# TWAP `t0` field tracks wall-clock. Hardcoding `t0 = 0` in the
# static-input tuple (the prior bug) makes `calculateValidTo` overflow
# `n`, producing an `AFTER_TWAP_FINISHED` revert on every poll. The
# helper backdates `t0` by 60 s so part 0 is Ready immediately.
log "deriving TWAP calldata via _twap_calldata.py (t0 = now-60)"
twap_calldata="$(python3 "$SCRIPT_DIR/_twap_calldata.py")" \
    || die "_twap_calldata.py failed - check the python3 deps"
[[ "$twap_calldata" =~ ^0x[a-fA-F0-9]+$ ]] || die "twap calldata malformed"

# Idempotency: if a prior invocation already wrote a TX_TWAP hash
# into .state, skip re-submitting (the ConditionalOrderCreated event
# already fired; re-running would either drop a tx with the same
# salt as a no-op, or — worse — bump the EOA's nonce for nothing).
if existing_twap="$(state_value TX_TWAP 2>/dev/null)" && [[ -n "${existing_twap:-}" ]]; then
    log "TWAP already submitted in a prior invocation — skipping (tx: $existing_twap)"
    tx_twap="$existing_twap"
else
    log "submitting TWAP ComposableCoW.create() → $COMPOSABLE_COW"
    tx_twap="$(cast send \
        --rpc-url    "$RPC_URL_SEPOLIA_HTTP" \
        --private-key "$OPERATOR_PRIVATE_KEY" \
        --json \
        "$COMPOSABLE_COW" \
        "$twap_calldata" \
        | jq -r '.transactionHash')"
    [[ "$tx_twap" =~ ^0x[a-fA-F0-9]{64}$ ]] || die "TWAP tx hash malformed: $tx_twap"
    log "  TWAP tx: $tx_twap"
    log "  Etherscan: https://sepolia.etherscan.io/tx/$tx_twap"
    write_state "TX_TWAP=$tx_twap"
fi

# ── Action 2: EthFlow.createOrder() ──────────────────────────────────

if existing_ethflow="$(state_value TX_ETHFLOW 2>/dev/null)" && [[ -n "${existing_ethflow:-}" ]]; then
    log "EthFlow already submitted in a prior invocation — skipping (tx: $existing_ethflow)"
else
    log "fetching cow.fi /quote for EthFlow swap (0.005 ETH → COW)"
    quote_out="$(python3 "$SCRIPT_DIR/_ethflow_quote.py" "$TEST_EOA" 5000000000000000)" \
        || die "EthFlow quote helper failed"
    ethflow_calldata="$(echo "$quote_out" | grep '^CALLDATA=' | cut -d= -f2-)"
    ethflow_value="$(echo "$quote_out" | grep '^VALUE_WEI=' | cut -d= -f2)"
    [[ "$ethflow_calldata" =~ ^0x[a-fA-F0-9]+$ ]] || die "EthFlow calldata malformed"
    [[ "$ethflow_value" =~ ^[0-9]+$ ]] || die "EthFlow value malformed: $ethflow_value"
    log "  msg.value = $ethflow_value wei ($(python3 -c "print(f'{$ethflow_value/1e18:.6f} ETH')"))"

    log "submitting EthFlow.createOrder() → $ETHFLOW"
    tx_ethflow="$(cast send \
        --rpc-url    "$RPC_URL_SEPOLIA_HTTP" \
        --private-key "$OPERATOR_PRIVATE_KEY" \
        --value      "$ethflow_value" \
        --json \
        "$ETHFLOW" \
        "$ethflow_calldata" \
        | jq -r '.transactionHash')"
    [[ "$tx_ethflow" =~ ^0x[a-fA-F0-9]{64}$ ]] || die "EthFlow tx hash malformed: $tx_ethflow"
    log "  EthFlow tx: $tx_ethflow"
    log "  Etherscan: https://sepolia.etherscan.io/tx/$tx_ethflow"
    write_state "TX_ETHFLOW=$tx_ethflow"
fi

# ── Optional actions ─────────────────────────────────────────────────

if [[ "${RUN_OPTIONAL_PRESIGN:-0}" -eq 1 ]]; then
    log "RUN_OPTIONAL_PRESIGN=1 → wrap WETH + setPreSignature + approve"

    log "  WETH9.deposit() — wrapping 0.01 ETH"
    tx_wrap="$(cast send --rpc-url "$RPC_URL_SEPOLIA_HTTP" --private-key "$OPERATOR_PRIVATE_KEY" --value 10000000000000000 --json "$WETH_SEPOLIA" "deposit()" | jq -r '.transactionHash')"
    log "    tx: $tx_wrap"
    write_state "TX_WRAP=$tx_wrap"

    log "  GPv2Settlement.setPreSignature($EXPECTED_ORDER_UID, true)"
    tx_presign="$(cast send --rpc-url "$RPC_URL_SEPOLIA_HTTP" --private-key "$OPERATOR_PRIVATE_KEY" --json "$GPV2_SETTLEMENT" "setPreSignature(bytes,bool)" "$EXPECTED_ORDER_UID" true | jq -r '.transactionHash')"
    log "    tx: $tx_presign"
    write_state "TX_PRESIGN=$tx_presign"

    log "  WETH9.approve(GPv2VaultRelayer, 0.005 ETH)"
    tx_approve="$(cast send --rpc-url "$RPC_URL_SEPOLIA_HTTP" --private-key "$OPERATOR_PRIVATE_KEY" --json "$WETH_SEPOLIA" "approve(address,uint256)" "$GPV2_VAULT_RELAYER" 5000000000000000 | jq -r '.transactionHash')"
    log "    tx: $tx_approve"
    write_state "TX_APPROVE=$tx_approve"
else
    log "RUN_OPTIONAL_PRESIGN=0 → skipping wrap/setPreSignature/approve"
    log "  (stop-loss still produces submitted:{uid} via the CoW orderbook"
    log "   pre-sign acceptance path; flip to 1 in .env to also enable on-chain settlement.)"
fi

log "done. tail the engine log to watch markers land:"
log "  tail -F $(state_value LOG_FILE)"
