#!/usr/bin/env bash
# scripts/load-bootstrap.sh - bring up the supporting processes for
# the load test:
#
#   1. anvil --fork-url $RPC_URL_SEPOLIA_HTTP        (port 8545)
#   2. tools/orderbook-mock                           (port 9999)
#
# Both run in the background; their PIDs land in /tmp/shepherd-load.pids
# so scripts/load-run.sh and an ad-hoc Ctrl-C cleanup can reach them.
#
# Designed to be sourced OR executed. When sourced, the helpers
# `load_bootstrap`, `load_teardown` become available in the caller's
# shell.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PID_FILE="/tmp/shepherd-load.pids"
LOG_DIR="${LOG_DIR:-/tmp/shepherd-load}"
mkdir -p "$LOG_DIR"

# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd anvil
require_cmd cast
require_cmd curl

load_bootstrap() {
    load_env
    [[ -n "${RPC_URL_SEPOLIA_HTTP:-}" ]] \
        || die "RPC_URL_SEPOLIA_HTTP unset; required to fork Sepolia under Anvil"

    : >"$PID_FILE"

    local block_time="${LOAD_BLOCK_TIME:-1}"
    log "starting anvil fork of Sepolia (port 8545, --block-time ${block_time})"
    anvil \
        --fork-url      "$RPC_URL_SEPOLIA_HTTP" \
        --port          8545 \
        --block-time    "$block_time" \
        --silent \
        >"$LOG_DIR/anvil.log" 2>&1 &
    local anvil_pid=$!
    echo "ANVIL_PID=$anvil_pid" >>"$PID_FILE"
    log "  anvil pid=$anvil_pid log=$LOG_DIR/anvil.log"

    log "waiting for anvil RPC to accept eth_blockNumber"
    local tries=0
    until cast block-number --rpc-url http://localhost:8545 >/dev/null 2>&1; do
        tries=$((tries+1))
        [[ $tries -lt 30 ]] || die "anvil did not become ready within 30s"
        sleep 1
    done

    log "starting tools/orderbook-mock (port 9999)"
    cargo run --release --quiet -p orderbook-mock -- --port 9999 \
        >"$LOG_DIR/orderbook-mock.log" 2>&1 &
    local mock_pid=$!
    echo "ORDERBOOK_MOCK_PID=$mock_pid" >>"$PID_FILE"
    log "  orderbook-mock pid=$mock_pid log=$LOG_DIR/orderbook-mock.log"

    log "waiting for orderbook-mock /healthz"
    tries=0
    until curl -fsS http://localhost:9999/healthz >/dev/null 2>&1; do
        tries=$((tries+1))
        [[ $tries -lt 60 ]] || die "orderbook-mock did not become ready within 60s"
        sleep 1
    done

    log "bootstrap complete: anvil ($anvil_pid) + orderbook-mock ($mock_pid)"
    log "  to stop: scripts/load-teardown.sh"
}

load_teardown() {
    [[ -f "$PID_FILE" ]] || { log "no pidfile, nothing to tear down"; return 0; }
    # shellcheck disable=SC1090
    source "$PID_FILE"
    for var in ENGINE_PID LOAD_GEN_PID ORDERBOOK_MOCK_PID ANVIL_PID; do
        local pid="${!var:-}"
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            log "stopping $var=$pid"
            kill "$pid" 2>/dev/null || true
            sleep 1
            kill -9 "$pid" 2>/dev/null || true
        fi
    done
    rm -f "$PID_FILE"
    log "teardown complete"
}

# When executed directly (not sourced), just run bootstrap.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    load_bootstrap
fi
