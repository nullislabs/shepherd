# 7-Day Soak Runbook

How to run the **7-day unattended stability soak** — all 5 modules on
Sepolia, continuously, with hourly metrics snapshots.

## Purpose

Grant evidence. The milestones require:

- **M1 (24h):** snapshots from hour 0-24 proving sustained operation.
- **M2 (48h):** snapshots from hour 0-48 proving 48-hour stability.
- **M4 (7-day):** full run artifact set (logs + snapshots + start/end metrics).

The soak validates *stability* — it is the step after the E2E run
(`docs/operations/e2e-testnet-runbook.md`) which validates correctness.
Do not start the soak unless the E2E run has passed its acceptance bar.

---

## How it works

Two Docker containers managed by `docker-compose.soak.yml`:

- **engine** — the nexum binary with `restart: unless-stopped`. Docker
  handles log rotation (json-file driver, 500 MB × 14 files ≈ 7 GB cap)
  and automatic crash recovery.
- **snapshotter** — an Alpine container that runs `scripts/soak-snapshot.sh`:
  captures a baseline on start, then scrapes `/metrics` every hour and
  writes `metrics-snap-<ts>.txt` to `docs/operations/soak-reports/`
  (bind-mounted from the host so files are immediately accessible).

---

## Pre-flight checklist

- [ ] Docker installed and running.
- [ ] Paid RPC endpoint with WebSocket support. Public nodes (e.g.
  `wss://ethereum-sepolia-rpc.publicnode.com`) throttle `eth_subscribe`
  under sustained load. Alchemy or Infura growth tier recommended.
- [ ] `SEPOLIA_RPC_URL` set in the repo-root `.env`:
  ```bash
  echo "SEPOLIA_RPC_URL=wss://eth-sepolia.g.alchemy.com/v2/YOUR_KEY" >> .env
  ```
  Docker Compose reads `.env` automatically and forwards the variable
  into the engine container.
- [ ] ≥ 20 GB free disk (engine logs capped at ~7 GB by Docker log
  rotation; snapshots are negligible).
- [ ] Machine will not sleep:
  - **macOS:** System Settings → Battery → Prevent automatic sleep when power
    adapter is connected.
  - **Linux:** `systemd-inhibit --what=sleep` or configure Docker to start
    on boot (`sudo systemctl enable docker`).
- [ ] E2E run has passed its acceptance bar (see `e2e-testnet-runbook.md`).
- [ ] Pull the image that will run the soak:
  ```bash
  docker compose -f docker-compose.soak.yml pull
  # or pin to a specific build:
  SHEPHERD_IMAGE=ghcr.io/nullislabs/shepherd:sha-abc1234 \
    docker compose -f docker-compose.soak.yml pull
  ```

---

## Starting the soak

```bash
docker compose -f docker-compose.soak.yml up -d
```

Docker starts the engine, waits for it to become healthy (up to 90 s), then
starts the snapshotter which immediately captures `metrics-start-<ts>.txt`.

Check it started cleanly:

```bash
docker compose -f docker-compose.soak.yml ps
# Both services should show "Up" and engine shows "(healthy)"
```

---

## Monitoring

**Follow engine logs live:**

```bash
docker compose -f docker-compose.soak.yml logs -f engine
```

**Filter per-module activity markers:**

```bash
docker compose -f docker-compose.soak.yml logs -f engine \
  | jq -r 'select(.fields.message | test("watch:|submitted:|dropped:|backoff:|TRIGGERED")) | "\(.fields.module): \(.fields.message)"' 2>/dev/null
```

**Check snapshot count:**

```bash
ls docs/operations/soak-reports/metrics-snap-*.txt | wc -l
```

Expect one file per completed hour. At 24 h you should see ≥ 23 files.

**Scrape live metrics:**

```bash
curl http://127.0.0.1:9100/metrics
```

**Check both containers are alive:**

```bash
docker compose -f docker-compose.soak.yml ps
```

---

## Stopping cleanly

```bash
# Capture final metrics before stopping.
curl -sf http://127.0.0.1:9100/metrics \
  > "docs/operations/soak-reports/metrics-end-$(date -u +%Y%m%dT%H%M%SZ).txt"

# Bring everything down (SIGINT → graceful shutdown → SIGKILL after 30 s).
docker compose -f docker-compose.soak.yml down
```

---

## Evidence artifacts

All files in `docs/operations/soak-reports/` constitute the grant evidence:

| Artifact | Pattern | Purpose |
|---|---|---|
| Engine log | `docker compose logs engine` | Full operation history |
| Baseline metrics | `metrics-start-<ts>.txt` | Counter values at t=0 |
| Hourly snapshots | `metrics-snap-<ts>.txt` × N | Hourly Prometheus scrapes |
| Final metrics | `metrics-end-<ts>.txt` | Counter values at shutdown |

```bash
# Save engine log to a file for submission.
docker compose -f docker-compose.soak.yml logs --no-color engine \
  > docs/operations/soak-reports/engine.log
```

These satisfy:

- **M1 (24h):** `metrics-snap-*.txt` files timestamped within 0-24 h of `metrics-start-*.txt`.
- **M2 (48h):** `metrics-snap-*.txt` files timestamped within 0-48 h.
- **M4 (7-day):** The full artifact set above covering ≥ 7 days of uptime.

To extract the shutdown summary from the log:

```bash
docker compose -f docker-compose.soak.yml logs engine \
  | grep "graceful shutdown complete" | tail -1
```

---

## Troubleshooting

**Engine container exited early:**

```bash
docker compose -f docker-compose.soak.yml logs --tail=100 engine
```

Common causes:

- OOM kill: `docker inspect soak-engine | jq '.[0].State'` — look for
  `OOMKilled: true`. Increase the `memory` limit in `docker-compose.soak.yml`
  or reduce module count.
- RPC errors: look for `connection refused` or `rate limit` in the log.
  Switch to a paid endpoint with higher rate limits.
- WASM trap: look for `module trapped` or `module poisoned`. File a bug.

**Snapshotter not producing files:**

Verify the engine is healthy and the metrics port is up:

```bash
curl -v http://127.0.0.1:9100/metrics
docker compose -f docker-compose.soak.yml ps
```

**Restarting an interrupted run:**

The engine's local store is in the `soak-state` Docker volume and persists
across container restarts — no state is lost.

```bash
# Engine already dead — snapshotter will have exited too.
docker compose -f docker-compose.soak.yml down
docker compose -f docker-compose.soak.yml up -d
```

A new `metrics-start-<ts>.txt` is written when the snapshotter restarts.
Preserve all files from both runs — reviewers can see the combined coverage.
