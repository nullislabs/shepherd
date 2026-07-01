#!/usr/bin/env bash
# scripts/e2e-finish.sh — gracefully wind down the E2E run.
#
# 1. Reads scripts/.state to find the engine PID + log file.
# 2. Captures /metrics → metrics-end-<ts>.txt before signalling.
# 3. Sends SIGINT to the engine. The graceful-shutdown path
#    writes `last_dispatched_block:{chain_id}` to the
#    local-store + logs `graceful shutdown complete dispatched_
#    blocks=N dispatched_logs=M uptime_secs=K`.
# 4. Waits up to 30 s for that log line to appear.
# 5. Hands off to scripts/e2e-report-gen.sh which writes
#    docs/operations/e2e-reports/e2e-report-YYYY-MM-DD.md.
# 6. Clears scripts/.state (run is closed).

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd curl

load_env

[[ -f "$STATE_FILE" ]] || die "scripts/.state not found — was scripts/e2e-run.sh ever invoked?"
engine_pid="$(state_value ENGINE_PID)" || die "ENGINE_PID missing from .state"
log_file="$(state_value LOG_FILE)"     || die "LOG_FILE missing from .state"
start_ts="$(state_value START_TS)"     || die "START_TS missing from .state"

ts="$(date -u +%Y%m%dT%H%M%SZ)"
metrics_end="$REPORTS_DIR/metrics-end-$ts.txt"
end_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if ! kill -0 "$engine_pid" 2>/dev/null; then
    warn "engine PID $engine_pid is not running anymore — skipping SIGINT, going straight to report"
else
    log "capturing end-state metrics → $metrics_end"
    if ! curl -sf http://127.0.0.1:9100/metrics > "$metrics_end"; then
        warn "/metrics scrape failed before SIGINT — metrics-end will be empty"
        : > "$metrics_end"
    fi

    log "sending SIGINT to engine PID $engine_pid"
    kill -INT "$engine_pid"

    log "waiting up to 30 s for graceful-shutdown log line"
    shutdown_ok=0
    for _ in $(seq 1 30); do
        if grep -q "graceful shutdown complete" "$log_file" 2>/dev/null; then
            shutdown_ok=1
            break
        fi
        if ! kill -0 "$engine_pid" 2>/dev/null; then
            break
        fi
        sleep 1
    done
    if [[ $shutdown_ok -eq 0 ]]; then
        warn "graceful-shutdown line never appeared; engine may have exited ungracefully"
    fi

    # Final cleanup in case the process is still alive after 30s.
    if kill -0 "$engine_pid" 2>/dev/null; then
        warn "engine still alive after 30s — escalating to SIGKILL"
        kill -KILL "$engine_pid" 2>/dev/null || true
    fi
fi

write_state "METRICS_END=$metrics_end"
write_state "END_TS=$ts"
write_state "END_ISO=$end_iso"

log "generating report"
"$SCRIPT_DIR/e2e-report-gen.sh"

log "report ready at $REPORTS_DIR/e2e-report-$(date -u +%Y-%m-%d).md"
log "run state file preserved at $STATE_FILE for reference (clear with: rm $STATE_FILE)"
