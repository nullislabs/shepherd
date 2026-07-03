#!/usr/bin/env bash
# scripts/soak-finish.sh — gracefully wind down the 7-day soak run.
#
# 1. Reads scripts/.state.soak to find engine PID, snapshot PID, log
#    file, and start-time.
# 2. Stops the hourly snapshot loop (SNAPSHOT_PID).
# 3. Captures /metrics → metrics-end-<ts>.txt before signalling.
# 4. Sends SIGINT to the engine. The graceful-shutdown path writes
#    `last_dispatched_block:{chain_id}` to the local-store + logs
#    `graceful shutdown complete dispatched_blocks=N dispatched_logs=M
#    uptime_secs=K`.
# 5. Waits up to 30 s for that log line to appear.
# 6. Prints a summary: snapshot count, total uptime, artifact paths.
# 7. Clears scripts/.state.soak (run is closed).

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/lib.sh"

require_cmd curl

load_env

[[ -f "$SOAK_STATE_FILE" ]] || die "scripts/.state.soak not found — was scripts/soak-run.sh ever invoked?"
engine_pid="$(soak_state_value ENGINE_PID)"    || die "ENGINE_PID missing from .state.soak"
log_file="$(soak_state_value LOG_FILE)"        || die "LOG_FILE missing from .state.soak"
start_ts="$(soak_state_value START_TS)"        || die "START_TS missing from .state.soak"
start_iso="$(soak_state_value START_ISO)"      || die "START_ISO missing from .state.soak"
snapshot_pid="$(soak_state_value SNAPSHOT_PID 2>/dev/null || true)"

ts="$(date -u +%Y%m%dT%H%M%SZ)"
metrics_end="$SOAK_REPORTS_DIR/metrics-end-$ts.txt"
end_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Stop the snapshot loop first so it does not fire after engine exits.
if [[ -n "${snapshot_pid:-}" ]] && kill -0 "$snapshot_pid" 2>/dev/null; then
    log "stopping snapshot loop (PID $snapshot_pid)"
    kill "$snapshot_pid" 2>/dev/null || true
    wait "$snapshot_pid" 2>/dev/null || true
else
    warn "snapshot loop PID not found or already stopped"
fi

if ! kill -0 "$engine_pid" 2>/dev/null; then
    warn "engine PID $engine_pid is not running anymore — skipping SIGINT, going straight to summary"
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

# Count snapshot files collected during the run.
snap_count=0
if compgen -G "$SOAK_REPORTS_DIR/metrics-snap-*.txt" >/dev/null 2>&1; then
    snap_count="$(find "$SOAK_REPORTS_DIR" -maxdepth 1 -name 'metrics-snap-*.txt' 2>/dev/null | wc -l | tr -d ' ')"
fi

# Compute uptime in hours (approximate, from START_TS to now).
# START_TS and ts are both in %Y%m%dT%H%M%SZ format.
uptime_secs=""
if command -v python3 >/dev/null 2>&1; then
    uptime_secs="$(python3 -c "
from datetime import datetime, timezone
fmt = '%Y%m%dT%H%M%SZ'
start = datetime.strptime('$start_ts', fmt).replace(tzinfo=timezone.utc)
end   = datetime.strptime('$ts',       fmt).replace(tzinfo=timezone.utc)
diff  = int((end - start).total_seconds())
h, rem = divmod(diff, 3600)
m = rem // 60
print(f'{h}h {m}m')
" 2>/dev/null || echo "unknown")"
fi

cat <<EOF

  ┌──────────────────────────────────────────────────────────────┐
  │ Soak run complete.                                           │
  │                                                              │
  │ Start:       $start_iso
  │ End:         $end_iso
  │ Uptime:      ${uptime_secs:-unknown}
  │ Snapshots:   $snap_count hourly files in
  │              $SOAK_REPORTS_DIR
  │                                                              │
  │ Evidence artifacts                                           │
  │   Engine log:   $log_file
  │   Metrics start: $SOAK_REPORTS_DIR/metrics-start-$start_ts.txt
  │   Metrics end:   $metrics_end
  │   Hourly snaps:  $SOAK_REPORTS_DIR/metrics-snap-*.txt        │
  │                                                              │
  │ To generate a report:                                        │
  │   ls $SOAK_REPORTS_DIR/metrics-snap-*.txt | wc -l           │
  │   grep "graceful shutdown" $log_file | tail -1              │
  └──────────────────────────────────────────────────────────────┘

Milestone evidence:
  M1 (24h): snapshots from hour 0-24 in $SOAK_REPORTS_DIR
  M2 (48h): snapshots from hour 0-48 in $SOAK_REPORTS_DIR
  M4 (7-day): full run artifacts above

EOF

log "clearing soak state file $SOAK_STATE_FILE"
clear_soak_state
