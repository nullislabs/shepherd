# 7-day soak runbook

Runs all 5 modules on Sepolia continuously and unattended, with hourly metrics snapshots, to validate stability. The soak follows the E2E run (`docs/operations/e2e-testnet-runbook.md`), which validates correctness; do not start it until the E2E run has passed its acceptance bar.

## How it works

Two containers managed by `docker-compose.soak.yml`:

- **engine**: the shepherd binary with `restart: unless-stopped`. Docker handles log rotation (json-file driver, 500 MB x 14 files) and crash recovery.
- **snapshotter**: an Alpine container running `scripts/soak-snapshot.sh`: captures a baseline on start, then scrapes `/metrics` every hour to `metrics-snap-<ts>.txt` under `docs/operations/soak-reports/` (bind-mounted from the host).

## Pre-flight checklist

- [ ] Docker installed and running.
- [ ] Paid RPC endpoint with WebSocket support (public nodes throttle `eth_subscribe` under sustained load).
- [ ] `SEPOLIA_RPC_URL` set in the repo-root `.env`:
  ```bash
  echo "SEPOLIA_RPC_URL=wss://eth-sepolia.g.alchemy.com/v2/YOUR_KEY" >> .env
  ```
- [ ] >= 20 GB free disk (engine logs cap at ~7 GB via rotation).
- [ ] Machine will not sleep (macOS: prevent sleep on power adapter; Linux: `systemd-inhibit --what=sleep` or `sudo systemctl enable docker`).
- [ ] E2E run has passed its acceptance bar.
- [ ] `SHEPHERD_IMAGE` set to an image that exists. The compose default (`ghcr.io/nullislabs/shepherd:latest`) is only published on pushes to `main`; until then, `pull` fails with `denied`. Either build locally from the soak commit:
  ```bash
  docker build -t shepherd-soak:$(git rev-parse --short HEAD) .
  echo "SHEPHERD_IMAGE=shepherd-soak:$(git rev-parse --short HEAD)" >> .env
  ```
  or publish via CI (`gh workflow run docker.yml --ref develop`), then pin the printed tag and `docker compose -f docker-compose.soak.yml pull`. Record the resolved image:
  ```bash
  docker inspect --format '{{.Config.Image}} {{.Image}}' soak-engine \
    > docs/operations/soak-reports/image-pin.txt   # after `up`
  ```

## Starting the soak

```bash
docker compose -f docker-compose.soak.yml up -d
docker compose -f docker-compose.soak.yml ps
# Both services show "Up"; engine shows "(healthy)".
```

## Monitoring

Follow engine logs:

```bash
docker compose -f docker-compose.soak.yml logs -f engine
```

Filter per-module markers (`docker logs`, not `docker compose logs`, so jq is not fed the service-name prefix; the JSON formatter flattens fields, message at `.message`):

```bash
docker logs -f soak-engine \
  | jq -r 'select(.message // "" | test("watch:|submitted:|dropped:|backoff:|TRIGGERED")) | .message'
```

Snapshot count (expect one file per completed hour; >= 23 at 24 h):

```bash
ls docs/operations/soak-reports/metrics-snap-*.txt | wc -l
```

Memory evidence (start once, right after `up`). The exporter has no process collector, so `/metrics` carries no RSS; this host-side loop is the only memory record:

```bash
nohup sh -c 'while true; do
  echo "$(date -u +%Y%m%dT%H%M%SZ) $(docker stats --no-stream --format "{{.MemUsage}} {{.CPUPerc}}" soak-engine)" \
    >> docs/operations/soak-reports/memory.log
  sleep 3600
done' >/dev/null 2>&1 &
echo $! > docs/operations/soak-reports/.memory-loop.pid
```

## Stopping cleanly

```bash
# Final metrics before stopping.
curl -sf http://127.0.0.1:9100/metrics \
  > "docs/operations/soak-reports/metrics-end-$(date -u +%Y%m%dT%H%M%SZ).txt"

# Restart record: RestartCount > 0 means Docker recovered a crash; that must be in the evidence.
docker inspect soak-engine --format \
  'restarts={{.RestartCount}} started={{.State.StartedAt}} oom={{.State.OOMKilled}}' \
  > "docs/operations/soak-reports/engine-state-$(date -u +%Y%m%dT%H%M%SZ).txt"

# Stop the host-side memory loop.
kill "$(cat docs/operations/soak-reports/.memory-loop.pid)" 2>/dev/null || true

# Bring everything down (SIGINT -> graceful shutdown -> SIGKILL after 30 s).
docker compose -f docker-compose.soak.yml down
```

## Evidence artefacts

All files in `docs/operations/soak-reports/`:

| Artefact | Pattern | Purpose |
|---|---|---|
| Engine log | `engine.log.gz` | Full operation history |
| Image pin | `image-pin.txt` | Image + digest the run executed |
| Baseline metrics | `metrics-start-<ts>.txt` | Counter values at t=0 |
| Hourly snapshots | `metrics-snap-<ts>.txt` x N | Hourly Prometheus scrapes |
| Final metrics | `metrics-end-<ts>.txt` | Counter values at shutdown |
| Memory samples | `memory.log` | Hourly RSS/CPU |
| Restart record | `engine-state-<ts>.txt` | RestartCount / OOMKilled |

Save the engine log compressed (uncompressed it approaches the ~7 GB cap; attach the `.gz`, do not commit it):

```bash
docker logs soak-engine 2>&1 | gzip > docs/operations/soak-reports/engine.log.gz
```

Extract the shutdown summary:

```bash
docker compose -f docker-compose.soak.yml logs engine | grep "graceful shutdown complete" | tail -1
```

## Troubleshooting

Engine container exited early:

```bash
docker compose -f docker-compose.soak.yml logs --tail=100 engine
```

- OOM kill: `docker inspect soak-engine | jq '.[0].State'`, look for `OOMKilled: true`. Raise the `memory` limit in `docker-compose.soak.yml` or reduce module count.
- RPC errors: look for `connection refused` / `rate limit`. Switch to a paid endpoint.
- WASM trap: look for `module trapped` / `module poisoned`. File a bug.

Snapshotter not producing files: verify the engine is healthy and the metrics port is up (`curl -v http://127.0.0.1:9100/metrics`).

Restarting an interrupted run: both services carry `restart: unless-stopped`, so an engine crash recovers automatically. Manual intervention is only needed after an operator `down` or a host reboot without Docker auto-start:

```bash
docker compose -f docker-compose.soak.yml up -d
```

The local store lives in the `soak-state` Docker volume and persists across restarts. A new `metrics-start-<ts>.txt` appears only when the snapshotter container restarts, not on engine-only recoveries. Preserve files from every run; the `engine-state` snapshot explains any counter resets.
