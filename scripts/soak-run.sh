#!/usr/bin/env bash
# scripts/soak-run.sh — boot the 7-day unattended soak run.
#
# 1. Loads scripts/.env (RPC URLs, optional flags).
# 2. Renders engine.soak.toml -> engine.soak.local.toml with the
#    operator's RPC URL (with key) substituted in. Local file is
#    gitignored.
# 3. Builds all 5 modules + the engine (skipped if already built —
#    soak does NOT clean data/soak on launch; state persists).
# 4. Warns if logrotate / newsyslog is not available (7 days of
#    engine logs should be rotated to avoid disk exhaustion).
# 5. Launches nexum via nohup, redirecting stdout/stderr to
#    docs/operations/soak-reports/engine-<timestamp>.log. JSON logs
#    (no --pretty-logs) so snapshot diffs are mineable with jq.
# 6. Waits up to 90 s for the `supervisor ready modules=5 chains=1`
#    line, exiting non-zero if it never appears.
# 7. Captures metrics-start-<ts>.txt.
# 8. Starts a background snapshot loop that scrapes /metrics every
#    3600 s and writes metrics-snap-<ts>.txt. The loop PID is
#    persisted to scripts/.state.soak so soak-finish.sh can stop it.
# 9. Persists engine PID, log path, snapshot PID, and start-time to
#    scripts/.state.soak so soak-finish.sh can find them.
# 10. Prints the operator banner.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd curl
require_cmd cargo
require_cmd python3
require_cmd jq

load_env

if [[ -f "$SOAK_STATE_FILE" ]]; then
    if existing_pid="$(soak_state_value ENGINE_PID || true)"; [[ -n "${existing_pid:-}" ]] && kill -0 "$existing_pid" 2>/dev/null; then
        die "engine already running (PID $existing_pid). Run scripts/soak-finish.sh first, or kill -INT $existing_pid manually."
    fi
    warn "stale state file $SOAK_STATE_FILE — removing"
    clear_soak_state
fi

mkdir -p "$SOAK_REPORTS_DIR"

render_soak_config

# NOTE: unlike e2e-run.sh we do NOT wipe data/soak. The soak state
# dir persists across restarts so the engine can resume from its last
# checkpoint if the run is interrupted and relaunched.
log "soak state_dir at $REPO_ROOT/data/soak (not wiped — persists across restarts)"
mkdir -p "$REPO_ROOT/data/soak"

log "building 5 modules + engine (this can take a minute on first run)"
(
    cd "$REPO_ROOT"
    cargo build -p twap-monitor     --target wasm32-wasip2 --release >/dev/null
    cargo build -p ethflow-watcher  --target wasm32-wasip2 --release >/dev/null
    cargo build -p price-alert      --target wasm32-wasip2 --release >/dev/null
    cargo build -p balance-tracker  --target wasm32-wasip2 --release >/dev/null
    cargo build -p stop-loss        --target wasm32-wasip2 --release >/dev/null
    cargo build -p nexum-cli                                 --release >/dev/null
)

# Log rotation advisory — 7 days of engine output at info level can
# reach multiple GB. Set up logrotate (Linux) or newsyslog (macOS)
# to rotate the engine log file if this machine will run unattended.
# Example logrotate snippet:
#   /path/to/engine-<ts>.log {
#       daily
#       rotate 7
#       compress
#       missingok
#       copytruncate
#   }
if ! command -v logrotate >/dev/null 2>&1 && ! command -v newsyslog >/dev/null 2>&1; then
    warn "neither logrotate nor newsyslog found — engine log will not be rotated."
    warn "For a 7-day run this can consume several GB. Configure log rotation before starting."
fi

ts="$(date -u +%Y%m%dT%H%M%SZ)"
log_file="$SOAK_REPORTS_DIR/engine-$ts.log"
metrics_start="$SOAK_REPORTS_DIR/metrics-start-$ts.txt"
start_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

log "launching engine — log: $log_file"
(
    cd "$REPO_ROOT"
    nohup "$REPO_ROOT/target/release/nexum" \
        --engine-config "$REPO_ROOT/engine.soak.local.toml" \
        >"$log_file" 2>&1 &
    echo $! > "$SOAK_STATE_FILE.pid.tmp"
)
engine_pid="$(cat "$SOAK_STATE_FILE.pid.tmp")"
rm "$SOAK_STATE_FILE.pid.tmp"

log "waiting for supervisor-ready (PID $engine_pid)"
# The engine emits JSON to stdout (no --pretty-logs), so look for
# the message + modules + chains fields in the JSON shape rather
# than the pretty-printed `modules=5 chains=1` flat string.
ready=0
for _ in $(seq 1 90); do
    if grep -qE '"message":"supervisor ready".*"modules":5[^0-9].*"chains":1[^0-9]' "$log_file" 2>/dev/null \
        || grep -qE '"message":"supervisor ready".*"chains":1[^0-9].*"modules":5[^0-9]' "$log_file" 2>/dev/null; then
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

# Write initial state before starting snapshot loop so SOAK_STATE_FILE
# exists when the loop appends SNAP_ entries.
{
    echo "ENGINE_PID=$engine_pid"
    echo "LOG_FILE=$log_file"
    echo "METRICS_START=$metrics_start"
    echo "START_TS=$ts"
    echo "START_ISO=$start_iso"
} > "$SOAK_STATE_FILE"

# Start hourly snapshot loop in a background subshell. Each iteration
# scrapes /metrics into a metrics-snap-<ts>.txt file. soak-finish.sh
# counts snapshots by globbing that pattern in SOAK_REPORTS_DIR.
(
    while kill -0 "$engine_pid" 2>/dev/null; do
        sleep 3600  # snapshot every hour
        snap_ts="$(date -u +%Y%m%dT%H%M%SZ)"
        snap_file="$SOAK_REPORTS_DIR/metrics-snap-$snap_ts.txt"
        curl -sf http://127.0.0.1:9100/metrics > "$snap_file" 2>/dev/null || true
    done
) &
snapshot_loop_pid=$!
echo "SNAPSHOT_PID=$snapshot_loop_pid" >> "$SOAK_STATE_FILE"
# Detach the loop from this shell's job table so it survives SSH disconnect.
disown "$snapshot_loop_pid"

cat <<EOF

  ┌──────────────────────────────────────────────────────────────┐
  │ Engine running. PID $engine_pid                              │
  │ Log: $log_file
  │ Metrics: http://127.0.0.1:9100/metrics                       │
  │                                                              │
  │ Soak will run for 7 days.                                    │
  │ Snapshots every hour in $SOAK_REPORTS_DIR
  │                                                              │
  │ When ready to wrap: scripts/soak-finish.sh                   │
  └──────────────────────────────────────────────────────────────┘

Tail per-module markers in real time:
  tail -F "$log_file" | jq -r 'select(.fields.message | test("watch:|submitted:|dropped:|backoff:|TRIGGERED")) | "\(.fields.module): \(.fields.message)"' 2>/dev/null

EOF
