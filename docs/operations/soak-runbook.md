# Soak runbook

A soak runs the engine continuously for several days, and Prometheus records its metrics.
Its purpose is to show stability: no engine restart, no quarantined module, and no growth in memory or latency.
It does not test correctness.

The soak uses the Compose deployment of [Docker deployment](../deployment/docker.md) with its `observability` profile.
The queries below also work on any other Prometheus that scrapes the engine.

## What the soak runs

`engine.docker.toml` loads both shipped modules on Ethereum Mainnet.

- `ccow-monitor` submits each ready order of its registry to the production mainnet orderbook.
  The soak is therefore a production run, not a test-network run.
- `ethflow-watcher` observes each EthFlow placement and watches its status.
  It submits nothing.

## Before the start

- Use a paid RPC endpoint, or your own node.
  The endpoint must keep the logs of the blocks that the engine reads after a restart.
- Pin the image: build it from one commit, or set `SHEPHERD_IMAGE` to a `sha-<short>` tag.
- Make sure that the host does not go to sleep and that Docker starts when the host starts.
- Keep 5 GB of free disk space: Compose keeps up to 1 GB of engine log, and Prometheus keeps 30 days of data.

## Start

```bash
docker compose --profile observability up -d
docker compose ps
```

`docker compose ps` must show `shepherd` as `healthy`.
Record the image and the start time:

```bash
mkdir -p soak-evidence
docker inspect --format '{{.Config.Image}} {{.Image}} {{.State.StartedAt}}' shepherd \
  > soak-evidence/start.txt
```

Git ignores the `soak-evidence/` directory.

## During the run

Look at the module states:

```bash
curl -s http://127.0.0.1:9100/readyz
```

The answer must be `ready: true`, `ccow-monitor: alive`, and `ethflow-watcher: alive`.

Count the warnings and the errors by message.
This command needs `jq`:

```bash
docker logs shepherd 2>&1 \
  | jq -R -r 'fromjson? | select(.level == "WARN" or .level == "ERROR") | .message' \
  | sort | uniq -c | sort -rn
```

The engine has no metric for the memory of its process.
Read it from Docker:

```bash
docker stats --no-stream --format '{{.MemUsage}} {{.CPUPerc}}' shepherd
```

## What to read in Prometheus

Run these queries in the Prometheus UI at `http://127.0.0.1:9090`, or with `curl` as "Evidence" shows.
Every metric name starts with `nexum_runtime_`, and the handbook, section 6, defines each one: <https://github.com/nxm-rs/nexum-runtime/blob/main/docs/production.md>.

| Question | Query | Expected |
|---|---|---|
| How long has the engine run? | `time() - nexum_runtime_start_time_seconds` | The time since the start, in seconds. |
| Did the engine restart? | `changes(nexum_runtime_start_time_seconds[7d])` | `0` |
| Is each module alive? | `nexum_runtime_module_state{state="alive"}` | `1` for each module. |
| Was a module quarantined? | `max_over_time(nexum_runtime_module_poisoned[7d])` | No result, or `0`. |
| Did a module restart? | `increase(nexum_runtime_module_restarts_total[7d])` | No result, or `0`. |
| Did a module fault or trap? | `sum by (module, error_kind) (increase(nexum_runtime_module_errors_total[7d]))` | Read each row. |
| How long does the engine hold a trigger? | `histogram_quantile(0.95, sum by (module, le) (rate(nexum_runtime_delivery_latency_seconds_bucket[1h])))` | The p95 time from the receipt of a trigger to the start of the handler. |
| How long does a handler run? | `histogram_quantile(0.95, sum by (module, le) (rate(nexum_runtime_dispatch_latency_seconds_bucket[1h])))` | The p95 handler time. |
| Does module memory grow? | `nexum_runtime_module_memory_bytes` | A flat graph over the run. |
| Did a stream reconnect? | `increase(nexum_runtime_source_reconnects_total[7d])` | Read each row. |
| Did RPC requests fail? | `sum by (chain_id, outcome) (increase(nexum_runtime_chain_request_total[7d]))` | A low share of `outcome="err"`. |

A counter starts at 0 when the engine starts, and a series is absent until its first event.
"No result" for a failure counter is therefore the good answer.

## Stop

Do these steps in this order.
`docker compose down` removes the container and its log, so save the log first.

```bash
# 1. The final metrics, while the engine runs.
curl -sf http://127.0.0.1:9100/metrics > soak-evidence/metrics-end.txt

# 2. Stop the engine. Leave Prometheus up for the queries of "Evidence".
docker compose stop shepherd

# 3. The restart record of Docker.
docker inspect --format \
  'restarts={{.RestartCount}} started={{.State.StartedAt}} finished={{.State.FinishedAt}} oom={{.State.OOMKilled}}' \
  shepherd > soak-evidence/engine-state.txt

# 4. The engine log. Keep this file private: see "Remove the RPC endpoint from the log".
docker logs shepherd 2>&1 | gzip > soak-evidence/engine.log.gz

# 5. The shutdown summary line.
docker logs shepherd 2>&1 | grep "graceful shutdown complete" | tail -1
```

The line of step 5 gives the number of dispatched blocks and events, and `uptime_secs`.
`uptime_secs` counts from the last engine start, so it is less than the length of the run if the engine restarted.

## Evidence

Save the answer of each query of the table above.
For example:

```bash
q() { curl -sf --data-urlencode "query=$1" http://127.0.0.1:9090/api/v1/query; }
q 'changes(nexum_runtime_start_time_seconds[7d])'      > soak-evidence/engine-restarts.json
q 'max_over_time(nexum_runtime_module_poisoned[7d])'   > soak-evidence/poisoned.json
q 'increase(nexum_runtime_module_restarts_total[7d])'  > soak-evidence/module-restarts.json
```

Write the length of the run in place of `7d`.

| File | Content |
|---|---|
| `start.txt` | The image name, the image id, and the start time. |
| `metrics-end.txt` | The `/metrics` output before the shutdown. |
| `engine-state.txt` | The restart count of Docker, the start time, the stop time, and the out-of-memory flag. |
| `*.json` | The answer of one query. |
| `engine.log.gz` | The engine log, with the RPC URL in it. Do not publish it. |
| `engine.public.log.gz` | The engine log without the RPC endpoint. |

### Remove the RPC endpoint from the log

The engine does not hide the RPC URL in its log.
It prints the full URL in the `opening chain RPC provider` line at each start, and an RPC error line can contain the URL again.
The URL can hold a provider key or the address of a private node.
Before you give the log to another person, make a copy that has no endpoint:

```bash
# The host of the RPC URL, without the scheme, the port, or the path.
RPC_HOST="<rpc host>"

zcat soak-evidence/engine.log.gz \
  | sed -E "s#[a-z]+://[^\" )]*${RPC_HOST}[^\" )]*#[rpc-endpoint]#g; s#${RPC_HOST}#[rpc-host]#g" \
  | gzip > soak-evidence/engine.public.log.gz

# This command must print 0.
zcat soak-evidence/engine.public.log.gz | grep -c -F "$RPC_HOST"
```

If the URL has a provider key, search the copy for the key also.
Publish `engine.public.log.gz`, not `engine.log.gz`.

## If something goes wrong

- The container restarts in a loop after the start.
  Read `docker logs --tail 20 shepherd`, then the troubleshooting table of [Deploying shepherd](../deployment.md).
- The engine stopped during the run.
  `docker inspect shepherd --format '{{.State.OOMKilled}}'` prints `true` after an out-of-memory kill.
  The memory limit is 2 GB in `docker-compose.yml`.
- A module is quarantined: `/readyz` shows it as `poisoned`.
  Record the event as a soak failure.
  The handbook, section 9, explains the causes and the recovery.
- The run was interrupted.
  The container has `restart: unless-stopped`, so Docker starts it again after a crash or a host restart.
  The cursors are on the `shepherd-state` volume, so each event trigger continues from its cursor.
