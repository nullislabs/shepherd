# 7-Day Soak Runbook

How to run the **7-day unattended stability soak** — all 5 modules on
Sepolia, continuously, with hourly metrics snapshots.

## Purpose

Grant evidence. The milestones require:

- **M1 (24h):** snapshots from hour 0-24 proving sustained operation.
- **M2 (48h):** snapshots from hour 0-48 proving 48-hour stability.
- **M4 (7-day):** full soak run artifact set (log + snapshots + start/end metrics).

**Hard gate: clean run by Jul 21.**

The soak validates *stability* — it is the step after the E2E run
(`docs/operations/e2e-testnet-runbook.md`) which validates correctness.
Do not start the soak unless the E2E run has passed its acceptance bar.

---

## Pre-flight checklist

- [ ] `scripts/.env` populated:
  - `RPC_URL_SEPOLIA` — must be `wss://` (engine uses `eth_subscribe`)
  - `RPC_URL_SEPOLIA_HTTP` — must be `http(s)://` (used by e2e-onchain.sh,
    not the engine itself, but load_env validates it)
  - `OPERATOR_PRIVATE_KEY` — only required if you are also running onchain
    markers; not needed for the soak engine itself
- [ ] Paid RPC endpoint with WebSocket support. Public nodes (e.g.
  `wss://ethereum-sepolia-rpc.publicnode.com`) throttle `eth_subscribe`
  under sustained load (≥ 4 `eth_call`/12 s at minimum across 5 modules).
  Alchemy or Infura growth tier recommended for 7-day runs.
- [ ] ≥ 20 GB free disk on the machine running the soak:
  - Engine log at `info` level: ~1-2 GB/day (estimate; varies by chain activity)
  - Hourly snapshots: ~168 files × ~50 KB each = ~8 MB (negligible)
  - Total headroom needed: ~20 GB over 7 days
- [ ] Machine will not sleep:
  - **macOS:** System Settings → Battery → Prevent automatic sleep when power
    adapter is connected. Or run `caffeinate -i scripts/soak-run.sh`.
  - **Linux:** `systemd-inhibit --what=sleep scripts/soak-run.sh` or configure
    `systemd` to run soak as a service.
- [ ] Log rotation configured (optional but recommended):
  - **Linux (logrotate):** add a config entry for the engine log path printed
    by `soak-run.sh` with `copytruncate` so the process does not need to be
    restarted.
  - **macOS (newsyslog):** add an entry to `/etc/newsyslog.conf` for the log
    file. `soak-run.sh` will warn if neither tool is found.
- [ ] All 5 modules + engine built:
  ```bash
  just build-e2e
  # or if that target doesn't exist:
  just build
  # or directly:
  cargo build -p twap-monitor    --target wasm32-wasip2 --release
  cargo build -p ethflow-watcher --target wasm32-wasip2 --release
  cargo build -p price-alert     --target wasm32-wasip2 --release
  cargo build -p balance-tracker --target wasm32-wasip2 --release
  cargo build -p stop-loss       --target wasm32-wasip2 --release
  cargo build -p nexum-cli                               --release
  ```
  `soak-run.sh` runs these automatically, but pre-building avoids
  surprises on a slow connection.
- [ ] E2E run has passed its acceptance bar (see `e2e-testnet-runbook.md`).
  Do not soak an untested configuration.

---

## Starting the soak

```bash
scripts/soak-run.sh
```

The script will:

1. Validate `scripts/.env`.
2. Render `engine.soak.toml` → `engine.soak.local.toml` (your RPC key stays
   out of git history).
3. Build all 5 modules + engine.
4. Launch nexum via `nohup` with JSON logs going to
   `docs/operations/soak-reports/engine-<ts>.log`.
5. Wait up to 90 s for `supervisor ready modules=5 chains=1`.
6. Capture `metrics-start-<ts>.txt`.
7. Start a background hourly snapshot loop (PID saved to `.state.soak`).
8. Print the operator banner with the tail command.

The soak state dir (`data/soak`) is **not** wiped on launch — if you
restart after an interruption the engine resumes from its last checkpoint.

---

## Monitoring

**Tail live per-module markers:**

```bash
tail -F docs/operations/soak-reports/engine-<ts>.log \
  | jq -r 'select(.fields.message | test("watch:|submitted:|dropped:|backoff:|TRIGGERED")) | "\(.fields.module): \(.fields.message)"' 2>/dev/null
```

Replace `<ts>` with the timestamp printed in the banner, or use
`$(ls -t docs/operations/soak-reports/engine-*.log | head -1)`.

**Check snapshot count:**

```bash
ls docs/operations/soak-reports/metrics-snap-*.txt | wc -l
```

Expect one file per completed hour. At 24h you should see ≥ 23 files
(the first snapshot fires after the first full hour).

**Scrape live metrics:**

```bash
curl http://127.0.0.1:9100/metrics
```

**Check engine is still alive:**

```bash
# Read PID from state file
engine_pid="$(grep '^ENGINE_PID=' scripts/.state.soak | cut -d= -f2)"
kill -0 "$engine_pid" && echo "alive" || echo "DEAD — check log"
```

**Check snapshot loop is still alive:**

```bash
snap_pid="$(grep '^SNAPSHOT_PID=' scripts/.state.soak | cut -d= -f2)"
kill -0 "$snap_pid" && echo "alive" || echo "stopped"
```

---

## Stopping cleanly

```bash
scripts/soak-finish.sh
```

The script will:

1. Stop the snapshot loop.
2. Capture `metrics-end-<ts>.txt`.
3. Send SIGINT to the engine and wait up to 30 s for the graceful-shutdown
   log line (`graceful shutdown complete`).
4. Print a summary: uptime, snapshot count, artifact paths.
5. Clear `scripts/.state.soak`.

The engine log and all snapshot files are preserved — do not delete them
until after the grant review.

---

## Evidence artifacts

After `soak-finish.sh` completes, the following files in
`docs/operations/soak-reports/` constitute the grant evidence:

| Artifact | Pattern | Purpose |
|---|---|---|
| Engine log | `engine-<ts>.log` | Full operation history, graceful-shutdown line |
| Baseline metrics | `metrics-start-<ts>.txt` | Counter values at t=0 |
| Hourly snapshots | `metrics-snap-<ts>.txt` × N | Hourly Prometheus scrapes |
| Final metrics | `metrics-end-<ts>.txt` | Counter values at shutdown |

These satisfy:

- **M1 (24h):** `metrics-snap-*.txt` files timestamped within 0-24h of
  `metrics-start-*.txt`.
- **M2 (48h):** `metrics-snap-*.txt` files timestamped within 0-48h.
- **M4 (7-day):** The full artifact set above covering ≥ 7 days of uptime.

To extract the shutdown summary from the log:

```bash
grep "graceful shutdown complete" docs/operations/soak-reports/engine-*.log | tail -1
```

---

## Troubleshooting

**Engine died early:**

```bash
tail -100 docs/operations/soak-reports/engine-<ts>.log
```

Common causes:

- OOM kill: check `dmesg | grep -i oom` (Linux) or Console.app (macOS).
  Increase swap or reduce module count.
- RPC errors: look for `connection refused` or `rate limit` in the log.
  Switch to a paid endpoint with higher rate limits.
- WASM trap: look for `module trapped` or `module poisoned`. File a bug.

**Metrics scrape failed (snapshot file is empty):**

Verify `[engine.metrics]` is enabled in `engine.soak.toml` and the engine
is still alive:

```bash
curl -v http://127.0.0.1:9100/metrics
```

**Snapshot loop stopped but engine is still alive:**

The snapshot loop exits if `kill -0 $engine_pid` fails (engine died) or
if the subshell itself was killed. Restart the soak (run `soak-finish.sh`
then `soak-run.sh`) — the engine state in `data/soak` is preserved.

Check `.state.soak` for the recorded `SNAPSHOT_PID`:

```bash
cat scripts/.state.soak
```

**Machine slept mid-run:**

The engine process pauses when the machine sleeps. The log will show a
gap in block events. The uptime counter in `graceful shutdown complete`
reflects wall-clock uptime, so a sleep gap will reduce the reported
uptime. Verify the snapshot timestamps span the required duration even
if the machine had a brief sleep.

**Restarting an interrupted run:**

If the engine crashed mid-run and you want to continue counting toward
the grant milestone:

1. `scripts/soak-finish.sh` (clears stale state; engine already dead).
2. `scripts/soak-run.sh` (relaunches; `data/soak` state is preserved).

The new run generates a new `engine-<ts>.log` and a new `metrics-start`
file. Preserve all log and snapshot files from both runs — reviewers can
see the combined coverage.
