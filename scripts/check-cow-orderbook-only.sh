#!/usr/bin/env bash
# Orderbook-only check for the CoW venue crate: crates/cow-venue carries
# no composable symbol (Composable*, getTradeableOrder*, the
# IConditionalOrder revert selectors, LegacyRevertAdapter) and no
# dependency edge to the composable-cow keeper crate - the Cargo.toml
# scan covers the edge, since the dep line names the crate. Blocking in
# CI; run locally via `just check-cow-orderbook-only`.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.." || exit 2

pass() { printf '\033[1;32m[cow PASS]\033[0m %s\n' "$*" >&2; }
fail() { printf '\033[1;31m[cow FAIL]\033[0m %s\n' "$*" >&2; status=1; }

command -v rg >/dev/null || { echo "ripgrep (rg) is required" >&2; exit 2; }

status=0

symbols='composable|getTradeableOrder|IConditionalOrder|LegacyRevertAdapter|\bVerdict\b|OrderNotValid|PollTryNextBlock|PollTryAtBlock|PollTryAtEpoch|PollNever'
rg -in --no-heading -e "$symbols" crates/cow-venue
case $? in
    0) fail "composable symbols leak into crates/cow-venue" ;;
    1) pass "cow-venue symbol scan empty" ;;
    *) fail "symbol scan errored (crates/cow-venue missing?)" ;;
esac

exit "$status"
