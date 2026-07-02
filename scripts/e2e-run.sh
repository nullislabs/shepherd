#!/usr/bin/env bash
# scripts/e2e-run.sh — boot the E2E run.
#
# 1. Loads scripts/.env (RPC URLs, optional flags).
# 2. Renders engine.e2e.toml -> engine.e2e.local.toml with the
#    operator's RPC URL (with key) substituted in. Local file is
#    gitignored.
# 3. Cleans data/e2e for a fresh local-store.
# 4. Builds all 5 modules + the engine.
# 5. Launches nexum-engine via nohup, redirecting stdout/stderr to
#    docs/operations/e2e-reports/engine-<timestamp>.log. JSON logs
#    (no --pretty-logs) so e2e-report-gen.sh can mine them with jq.
# 6. Waits up to 60 s for the `supervisor ready modules=5 chains=1`
#    line, exiting non-zero if it never appears.
# 7. Captures metrics-start.txt.
# 8. Persists engine PID, log path, and start-time to scripts/.state
#    so e2e-onchain.sh + e2e-finish.sh can find them.
# 9. Prints the next-steps banner.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd curl
require_cmd cargo
require_cmd python3
require_cmd jq

load_env

if [[ -f "$STATE_FILE" ]]; then
    if existing_pid="$(state_value ENGINE_PID || true)"; [[ -n "${existing_pid:-}" ]] && kill -0 "$existing_pid" 2>/dev/null; then
        die "engine already running (PID $existing_pid). Run scripts/e2e-finish.sh first, or kill -INT $existing_pid manually."
    fi
    warn "stale state file $STATE_FILE — removing"
    clear_state
fi

mkdir -p "$REPORTS_DIR"

render_engine_config

log "cleaning local-store at $REPO_ROOT/data/e2e"
rm -rf "$REPO_ROOT/data/e2e"

log "building 5 modules + engine (this can take a minute on first run)"
(
    cd "$REPO_ROOT"
    cargo build -p twap-monitor     --target wasm32-wasip2 --release >/dev/null
    cargo build -p ethflow-watcher  --target wasm32-wasip2 --release >/dev/null
    cargo build -p price-alert      --target wasm32-wasip2 --release >/dev/null
    cargo build -p balance-tracker  --target wasm32-wasip2 --release >/dev/null
    cargo build -p stop-loss        --target wasm32-wasip2 --release >/dev/null
    cargo build -p nexum-runtime                             --release >/dev/null
)

ts="$(date -u +%Y%m%dT%H%M%SZ)"
log_file="$REPORTS_DIR/engine-$ts.log"
metrics_start="$REPORTS_DIR/metrics-start-$ts.txt"
start_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

log "launching engine — log: $log_file"
(
    cd "$REPO_ROOT"
    nohup "$REPO_ROOT/target/release/nexum-engine" \
        --engine-config "$REPO_ROOT/engine.e2e.local.toml" \
        >"$log_file" 2>&1 &
    echo $! > "$STATE_FILE.pid.tmp"
)
engine_pid="$(cat "$STATE_FILE.pid.tmp")"
rm "$STATE_FILE.pid.tmp"

log "waiting for supervisor-ready (PID $engine_pid)"
# The engine emits JSON to stdout (no --pretty-logs), so look for
# the message + modules + chains fields in the JSON shape rather
# than the pretty-printed `modules=5 chains=1` flat string.
ready=0
for _ in $(seq 1 90); do
    if grep -qE '"message":"supervisor ready"[^}]*"modules":5[^}]*"chains":1' "$log_file" 2>/dev/null \
        || grep -qE '"message":"supervisor ready"[^}]*"chains":1[^}]*"modules":5' "$log_file" 2>/dev/null; then
        ready=1
        break
    fi
    if ! kill -0 "$engine_pid" 2>/dev/null; then
        die "engine PID $engine_pid died before supervisor-ready. Tail: $(tail -20 "$log_file")"
    fi
    sleep 1
done
[[ $ready -eq 1 ]] || die "engine did not reach supervisor-ready in 90s. Tail: $(tail -20 "$log_file")"

log "capturing baseline metrics → $metrics_start"
curl -sf http://127.0.0.1:9100/metrics > "$metrics_start" \
    || die "/metrics scrape failed — is the metrics exporter bound?"

{
    echo "ENGINE_PID=$engine_pid"
    echo "LOG_FILE=$log_file"
    echo "METRICS_START=$metrics_start"
    echo "START_TS=$ts"
    echo "START_ISO=$start_iso"
} > "$STATE_FILE"

cat <<EOF

  ┌──────────────────────────────────────────────────────────────┐
  │ Engine running. PID $engine_pid                              │
  │ Log: $log_file
  │ Metrics: http://127.0.0.1:9100/metrics                       │
  └──────────────────────────────────────────────────────────────┘

Next:

  1. (auto) submit the 2 remaining on-chain markers (TWAP + EthFlow):
       scripts/e2e-onchain.sh

  2. Leave the engine running for ~5h to hit the
     acceptance bar (≥ 1500 Sepolia blocks).

  3. When ready to wrap:
       scripts/e2e-finish.sh
     This snapshots metrics-end.txt, sends SIGINT for graceful
     shutdown, and auto-generates docs/operations/e2e-reports/
     e2e-report-$(date -u +%Y-%m-%d).md from the log + metrics
     deltas.

Tail per-module markers in real time:
  tail -F "$log_file" | jq -r 'select(.fields.message | test("watch:|submitted:|dropped:|backoff:|TRIGGERED")) | "\(.fields.module): \(.fields.message)"' 2>/dev/null

EOF
