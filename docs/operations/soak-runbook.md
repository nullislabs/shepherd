# 7-day soak runbook

The soak runs the two shipped modules continuously for 7 days without an operator, and it records a metrics snapshot each hour.
Its purpose is to show stability: no crash, no quarantined module, and no memory growth.
It does not test correctness, so validate the modules on the target chains before you start it.

The engine config reference is in [Deploying shepherd](../deployment.md).
The chain layout is in [Multi-chain deployment](../deployment/multi-chain.md).

## How it works

`docker-compose.soak.yml` defines two containers.

| Container | Image | Function |
|---|---|---|
| `soak-engine` | `${SHEPHERD_IMAGE}` | The `shepherd` engine, with `restart: unless-stopped`. Docker rotates its log: 14 files of 500 MB, approximately 7 GB in total. |
| `soak-snapshotter` | `alpine:3` | Runs `scripts/soak-snapshot.sh`. It writes one baseline file at its start, and then one snapshot of `/metrics` each hour. |

The snapshotter writes into `docs/operations/soak-reports/`, which Compose mounts from the host.
The engine state is on the named volume `soak-state`.
The engine metrics are on `127.0.0.1:9100` of the host.

`scripts/soak-snapshot.sh` is for the snapshotter container only.
It reads `http://engine:9100/metrics` and writes to `/evidence`, and both exist only in that container.
Do not run it, or `just soak-snapshot`, on the host.

## What the soak runs

The image contains `ccow-monitor` and `ethflow-watcher`, and `engine.soak.docker.toml` loads both.

- `ccow-monitor` reads its registry on Ethereum Mainnet (chain 1).
  The venue is on chain 1, so the module submits each ready order to the production mainnet orderbook.
  The soak is therefore not a testnet-only run.
- `ethflow-watcher` reads Sepolia (chain 11155111).
  The venue is on chain 1, so the orderbook cannot follow a Sepolia order.
  The module still receives and decodes each placement, which exercises the log stream.

The soak targets Ethereum Mainnet.
A follow-up change moves `ethflow-watcher` to mainnet and repairs the committed harness.
Until it lands, this runbook describes the files as they are committed.

## Prerequisites

The harness does not boot as committed.
Items 1 to 3 below must be true before the engine can start.

1. The engine config declares each chain that a module trigger names.
   `engine.soak.docker.toml` declares `[chains.1]` only, and `ethflow-watcher` has a trigger on chain 11155111.
   The engine refuses that boot with `UnconfiguredChain`.
2. Both `MAINNET_RPC_URL` and `SEPOLIA_RPC_URL` reach the engine container.
   `engine.soak.docker.toml` reads `MAINNET_RPC_URL`, but `docker-compose.soak.yml` forwards only `SEPOLIA_RPC_URL`.
   The engine refuses that boot with `Substitute(Missing { name: "MAINNET_RPC_URL" })`, and Docker restarts it in a loop.
3. `SHEPHERD_IMAGE` names an image that the host has or can pull.
   `docker-compose.soak.yml` has no `build:` section, so Compose does not build the image for you.

Until the committed files change, make items 1 and 2 true with two local files.
Do not commit them.

Make a local engine config that adds the Sepolia chain:

```bash
cp engine.soak.docker.toml engine.soak.local.toml
cat >> engine.soak.local.toml <<'EOF'

[chains.11155111]
rpc_url = "${SEPOLIA_RPC_URL}"
EOF
```

Make a Compose override file, `docker-compose.soak.override.yml`, that mounts the local config and forwards the second variable:

```yaml
services:
  engine:
    volumes:
      - ./engine.soak.local.toml:/etc/shepherd/engine.toml:ro
    environment:
      MAINNET_RPC_URL: ${MAINNET_RPC_URL:?MAINNET_RPC_URL must be set in .env}
```

Put the endpoints and the image in the `.env` file at the repository root:

```bash
cat >> .env <<'EOF'
MAINNET_RPC_URL=wss://<your mainnet endpoint>
SEPOLIA_RPC_URL=wss://<your sepolia endpoint>
SHEPHERD_IMAGE=ghcr.io/nullislabs/shepherd:sha-<short>
EOF
```

Pin `SHEPHERD_IMAGE` to a `sha-<short>` tag, so that the run uses one known build.
To test a commit that has no published image, build it and use the local tag:

```bash
docker build -t "shepherd-soak:$(git rev-parse --short HEAD)" .
echo "SHEPHERD_IMAGE=shepherd-soak:$(git rev-parse --short HEAD)" >> .env
```

Select both Compose files for the shell in which you run the soak:

```bash
export COMPOSE_FILE=docker-compose.soak.yml:docker-compose.soak.override.yml
```

Each `docker compose` command in this runbook uses that variable.
Set it again in each new shell.

Also make sure that these conditions are true:

- Docker Engine with the Compose plugin is installed, and Docker starts when the host starts (`sudo systemctl enable docker`).
- Both RPC endpoints are paid endpoints.
  Public endpoints limit `eth_subscribe` and `eth_getLogs` under continuous load.
- The host has 20 GB or more of free disk space.
- The host does not go to sleep during the run.
- Port 9100 on the host loopback is free.
  The main stack in `docker-compose.yml` uses the same port, so stop it first.
- `jq` is installed on the host, for the log commands below.

## Start

```bash
docker compose up -d
docker compose ps
```

Compose starts the snapshotter only after the engine is healthy.
`docker compose ps` must show `soak-engine` as `healthy` and `soak-snapshotter` as `Up`.

Record the image that the run uses:

```bash
docker inspect --format '{{.Config.Image}} {{.Image}}' soak-engine \
  > docs/operations/soak-reports/image-pin.txt
```

Start the memory record once.
The `/metrics` output has no process memory value, so this loop on the host is the only record of it:

```bash
nohup sh -c 'while true; do
  echo "$(date -u +%Y%m%dT%H%M%SZ) $(docker stats --no-stream --format "{{.MemUsage}} {{.CPUPerc}}" soak-engine)" \
    >> docs/operations/soak-reports/memory.log
  sleep 3600
done' >/dev/null 2>&1 &
echo $! > docs/operations/soak-reports/.memory-loop.pid
```

On the first boot, each event trigger reads the logs from its `start_block` to the chain head.
The engine logs the progress of that backfill.

## Monitor

Follow the engine log:

```bash
docker compose logs -f engine
```

The engine writes one JSON object for each log line.
Show the lines of one module:

```bash
docker logs -f soak-engine 2>&1 \
  | jq -R -r 'fromjson? | select(.module == "ccow-monitor") | "\(.timestamp) \(.level) \(.message)"'
```

Count the warnings and the errors by message:

```bash
docker logs soak-engine 2>&1 \
  | jq -R -r 'fromjson? | select(.level == "WARN" or .level == "ERROR") | .message' \
  | sort | uniq -c | sort -rn
```

Use `docker logs`, not `docker compose logs`, with `jq`: Compose puts the service name in front of each line.

Look at the module states:

```bash
curl -s http://127.0.0.1:9100/readyz
```

The answer must be `ready: true`, `ccow-monitor: alive`, and `ethflow-watcher: alive`.

Count the snapshots.
There is one file for each completed hour, so 23 or more after 24 hours:

```bash
ls docs/operations/soak-reports/metrics-snap-*.txt | wc -l
```

## Stop

Do these steps in this order.
`docker compose down` removes the engine container and its log, so save the log first.

```bash
# 1. Final metrics, while the engine runs.
curl -sf http://127.0.0.1:9100/metrics \
  > "docs/operations/soak-reports/metrics-end-$(date -u +%Y%m%dT%H%M%SZ).txt"

# 2. Stop both containers. The engine receives SIGINT and shuts down.
docker compose stop

# 3. The restart record. A restart count above 0 means that Docker restarted the engine.
docker inspect soak-engine --format \
  'restarts={{.RestartCount}} started={{.State.StartedAt}} oom={{.State.OOMKilled}}' \
  > "docs/operations/soak-reports/engine-state-$(date -u +%Y%m%dT%H%M%SZ).txt"

# 4. The engine log, compressed.
docker logs soak-engine 2>&1 | gzip > docs/operations/soak-reports/engine.log.gz

# 5. The shutdown summary line.
docker logs soak-engine 2>&1 | grep "graceful shutdown complete" | tail -1

# 6. Stop the memory loop on the host.
kill "$(cat docs/operations/soak-reports/.memory-loop.pid)" 2>/dev/null || true

# 7. Remove the containers. The soak-state volume stays.
docker compose down
```

The summary line of step 5 gives `dispatched_blocks`, `dispatched_events`, `dispatched_extension_triggers`, and `uptime_secs`.
`uptime_secs` counts from the last engine start, so it is less than 7 days if Docker restarted the engine.

`docker-compose.soak.yml` sets `stop_grace_period: 30s`.
Docker sends SIGKILL 30 seconds after SIGINT, and the engine can use up to 150 seconds to drain by default.
If step 5 prints nothing, Docker killed the engine before the shutdown was complete.

## Evidence

All files are in `docs/operations/soak-reports/`.

| File | Written by | Content |
|---|---|---|
| `image-pin.txt` | The operator, at the start | The image name and the image id of the run. |
| `metrics-start-<ts>.txt` | The snapshotter | The `/metrics` output at the start of the snapshotter. |
| `metrics-snap-<ts>.txt` | The snapshotter, each hour | The `/metrics` output. |
| `metrics-end-<ts>.txt` | The operator, at the stop | The `/metrics` output before the shutdown. |
| `memory.log` | The loop on the host, each hour | The memory and the CPU of the engine container. |
| `engine-state-<ts>.txt` | The operator, at the stop | The restart count, the start time, and the out-of-memory flag. |
| `engine.log.gz` | The operator, at the stop | The engine log, with the RPC URLs in it. Do not publish it. |
| `engine.public.log.gz` | The operator, before the report | The engine log without the RPC endpoints. |

The snapshotter runs as root, so its files belong to root on the host.
Git does not ignore the files in this directory.
Do not commit them: attach them to the soak report.
The log can be near 7 GB before compression.

### Remove the RPC endpoints from the log

The engine does not hide the RPC URL in its log.
It prints the full URL in the `opening chain RPC provider` line at each start, and an RPC error line can contain the URL again.
The URL can hold a provider key or the address of a private node.
`engine.log.gz` is for the operator only.
Before you attach the log to a report, make a copy that has no endpoint:

```bash
# The host of each RPC URL, without the scheme, the port, or the path.
RPC_HOSTS="<mainnet rpc host> <sepolia rpc host>"

script=""
for host in $RPC_HOSTS; do
  script="${script}s#[a-z]+://[^\" )]*${host}[^\" )]*#[rpc-endpoint]#g; s#${host}#[rpc-host]#g; "
done
zcat docs/operations/soak-reports/engine.log.gz \
  | sed -E "$script" \
  | gzip > docs/operations/soak-reports/engine.public.log.gz

# Each line must print 0.
for host in $RPC_HOSTS; do
  zcat docs/operations/soak-reports/engine.public.log.gz | grep -c -F "$host"
done
```

If a URL has a provider key, search the copy for the key also.
Attach `engine.public.log.gz`, not `engine.log.gz`.

Read these values in the metrics files.
Every metric name starts with `nexum_runtime_`.

| Metric | What it shows |
|---|---|
| `nexum_runtime_module_state` | The state of each module. The `alive` series must be 1. |
| `nexum_runtime_module_poisoned` | A value of 1 means that the engine quarantined the module. The series is absent while no module is quarantined. |
| `nexum_runtime_module_restarts_total` | The restarts of each module. |
| `nexum_runtime_module_errors_total` | The faults and the traps of each module, by `error_kind`. |
| `nexum_runtime_module_memory_bytes` | The linear memory of each module. |
| `nexum_runtime_source_reconnects_total` | The reconnects of each block stream and each log stream. |
| `nexum_runtime_chain_request_total` | The RPC requests by chain and by `outcome`. |
| `nexum_runtime_dispatch_latency_seconds` | The dispatch time of each module. |

The counters start at 0 when the engine process starts.
A counter that goes down between two snapshots shows an engine restart, and `engine-state-<ts>.txt` confirms it.
The nexum-runtime operator handbook, section 6, defines each metric: <https://github.com/nullislabs/nexum-runtime/blob/main/docs/production.md>.

## Troubleshooting

The engine restarts in a loop immediately after the start.
Read the last log lines:

```bash
docker logs --tail 20 soak-engine
```

| Last line | Cause | Fix |
|---|---|---|
| `Substitute(Missing { name: "MAINNET_RPC_URL" })` | The override file is not in use, or the variable is not in `.env`. | Set `COMPOSE_FILE` as in "Prerequisites", and set the variable. |
| `UnconfiguredChain { name: "ethflow-watcher", chain_id: 11155111, ... }` | The engine config has no `[chains.11155111]` table. | Use `engine.soak.local.toml` from "Prerequisites". |
| `required variable SEPOLIA_RPC_URL is missing a value`, from Compose | The variable is not in `.env`. | Set it. |

The engine stopped during the run.

- Out of memory: `docker inspect soak-engine --format '{{.State.OOMKilled}}'` prints `true`.
  The memory limit is 2 GB in `docker-compose.soak.yml`.
  Record the event, then set a larger limit in the override file.
- RPC errors: the log has `block source open failed` or `event source head fetch failed - retrying after backoff`.
  The engine retries without help.
  If the messages continue, the endpoint is the cause.
- A quarantined module: the log has `module poisoned`, and `/readyz` shows the module as `poisoned`.
  Record the event as a soak failure.
  The handbook, section 9, explains the two causes and the recovery.

The snapshotter writes no files.

- Make sure that the engine is healthy: `docker compose ps`.
- Make sure that the metrics endpoint answers: `curl -v http://127.0.0.1:9100/metrics`.
- Read the snapshotter log: `docker logs soak-snapshotter`.

The run was interrupted.

- Both containers have `restart: unless-stopped`, so Docker restarts them after an engine crash or a host restart.
- After an operator stop, start them again with `docker compose up -d`.
- The local store is on the `soak-state` volume, so the cursors continue.
- A new `metrics-start-<ts>.txt` file appears each time the snapshotter container starts.
  Keep the files of each start.

## A run without Docker

`engine.soak.toml` is a soak config for a binary that runs directly on the host.
This runbook does not cover that path.
As committed, the file names example modules that this repository does not build, it declares `[chains.1]` only, and its comments contain two `${NAME}` tokens that the engine tries to replace.
Correct those points in a local copy before you use it.
