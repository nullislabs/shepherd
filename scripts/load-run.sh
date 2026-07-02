#!/usr/bin/env bash
# scripts/load-run.sh - orchestrate one load scenario.
#
# Pipeline:
#   1. bootstrap (anvil fork + orderbook-mock)
#   2. wipe ./data/load and start the engine with engine.load.toml
#   3. snapshot prometheus /metrics
#   4. run tools/load-gen for --duration-min
#   5. snapshot prometheus /metrics again
#   6. tear everything down
#   7. emit a one-page summary to docs/operations/load-reports/
#
# Args (any subset; defaults shown):
#   --twap-per-block 5
#   --ethflow-per-block 5
#   --duration-min 1
#   --scenario baseline
#
# Requires:
#   - scripts/.env with RPC_URL_SEPOLIA_HTTP
#   - anvil + cast + curl on PATH
#   - cargo build --release (this script kicks off the builds)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="${LOG_DIR:-/tmp/shepherd-load}"
PID_FILE="/tmp/shepherd-load.pids"

# shellcheck disable=SC1091
source "$SCRIPT_DIR/load-bootstrap.sh"
# lib.sh (sourced transitively above) sets REPORTS_DIR to the
# e2e-reports/ directory; the load reports live under their own dir so
# they do not collide with the live-Sepolia run reports.
REPORTS_DIR="$REPO_ROOT/docs/operations/load-reports"
mkdir -p "$LOG_DIR" "$REPORTS_DIR"

# Defaults
TWAP=5
ETHFLOW=5
DURATION_MIN=1
SCENARIO="baseline"
PARALLEL=1
BLOCK_TIME=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --twap-per-block)    TWAP="$2";          shift 2 ;;
        --ethflow-per-block) ETHFLOW="$2";       shift 2 ;;
        --duration-min)      DURATION_MIN="$2";  shift 2 ;;
        --scenario)          SCENARIO="$2";      shift 2 ;;
        --parallel)          PARALLEL="$2";      shift 2 ;;
        --block-time)        BLOCK_TIME="$2";    shift 2 ;;
        -h|--help)
            cat <<EOF
usage: scripts/load-run.sh [--twap-per-block N] [--ethflow-per-block M]
                           [--duration-min K] [--scenario LABEL]
                           [--parallel W] [--block-time S]
EOF
            exit 0 ;;
        *) die "unknown arg: $1" ;;
    esac
done

# Exported so load-bootstrap.sh's anvil invocation picks it up.
export LOAD_BLOCK_TIME="$BLOCK_TIME"

trap load_teardown EXIT INT TERM

log "scenario=$SCENARIO  TWAP/block=$TWAP  EthFlow/block=$ETHFLOW  parallel=$PARALLEL  block-time=${BLOCK_TIME}s  duration=${DURATION_MIN}min"

load_bootstrap

log "wiping ./data/load to start clean"
rm -rf "$REPO_ROOT/data/load"
mkdir -p "$REPO_ROOT/data/load"

log "building modules + engine + load-gen (release)"
( cd "$REPO_ROOT" && cargo build --release --quiet \
    --target wasm32-wasip2 \
    -p twap-monitor -p ethflow-watcher )
( cd "$REPO_ROOT" && cargo build --release --quiet -p nexum-cli -p load-gen )

log "starting nexum (engine.load.toml)"
( cd "$REPO_ROOT" && ./target/release/nexum --engine-config engine.load.toml ) \
    >"$LOG_DIR/engine.log" 2>&1 &
ENGINE_PID=$!
echo "ENGINE_PID=$ENGINE_PID" >>"$PID_FILE"
log "  engine pid=$ENGINE_PID log=$LOG_DIR/engine.log"

log "waiting for /metrics on 9100"
tries=0
until curl -fsS http://localhost:9100/metrics >/dev/null 2>&1; do
    tries=$((tries+1))
    [[ $tries -lt 60 ]] || die "engine /metrics did not come up within 60s"
    sleep 1
done

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
metrics_start="$LOG_DIR/metrics-start-$stamp.txt"
metrics_end="$LOG_DIR/metrics-end-$stamp.txt"
curl -fsS http://localhost:9100/metrics >"$metrics_start"
log "metrics snapshot (t=0) -> $metrics_start"

log "running tools/load-gen (release)"
( cd "$REPO_ROOT" && ./target/release/load-gen \
    --anvil ws://localhost:8545 \
    --twap-per-block "$TWAP" \
    --ethflow-per-block "$ETHFLOW" \
    --duration-min "$DURATION_MIN" \
    --parallel "$PARALLEL" ) \
    >"$LOG_DIR/load-gen.log" 2>&1 &
LOAD_GEN_PID=$!
echo "LOAD_GEN_PID=$LOAD_GEN_PID" >>"$PID_FILE"
log "  load-gen pid=$LOAD_GEN_PID log=$LOG_DIR/load-gen.log"

wait $LOAD_GEN_PID || true
log "load-gen exited"

# Give the engine a moment to flush any in-flight dispatches before
# snapshotting the metrics tail.
sleep 3
curl -fsS http://localhost:9100/metrics >"$metrics_end"
log "metrics snapshot (t=end) -> $metrics_end"

mock_stats="$(curl -fsS http://localhost:9999/_stats 2>/dev/null || echo '{}')"

report="$REPORTS_DIR/load-${TWAP}x${ETHFLOW}-${SCENARIO}-$(date -u +%Y-%m-%d).md"
{
    echo "# Load test report - scenario=$SCENARIO"
    echo ""
    echo "| Field | Value |"
    echo "|---|---|"
    echo "| Stamp (UTC) | $stamp |"
    echo "| Duration | ${DURATION_MIN} minute(s) |"
    echo "| TWAP / block | $TWAP |"
    echo "| EthFlow / block | $ETHFLOW |"
    echo ""
    echo "## Mock orderbook stats"
    echo ""
    echo '```json'
    echo "$mock_stats"
    echo '```'
    echo ""
    echo "## load-gen tail"
    echo ""
    echo '```'
    tail -n 40 "$LOG_DIR/load-gen.log" 2>/dev/null || echo '(no load-gen log)'
    echo '```'
    echo ""
    echo "## Engine log tail"
    echo ""
    echo '```'
    tail -n 60 "$LOG_DIR/engine.log" 2>/dev/null || echo '(no engine log)'
    echo '```'
    echo ""
    echo "## Metrics delta"
    echo ""
    echo "Inputs: $(basename "$metrics_start") -> $(basename "$metrics_end")"
    echo ""
    echo "Operator: pipe through scripts/e2e-report-gen.sh delta logic or compute by hand. (Auto-delta lands in a follow-up.)"
} >"$report"
log "report -> $report"
log "done."
