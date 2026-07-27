# Production deployment

Operator handbook for running the `shepherd` binary in production: systemd unit, state backup, observability wiring. The hardening design (resource enforcement, restart policy, RPC resilience, error model) is in [`docs/06-production-hardening.md`](./06-production-hardening.md); the `engine.toml` reference is in [`docs/deployment.md`](./deployment.md); containers in [`docs/deployment/docker.md`](./deployment/docker.md).

## 1. Pre-flight

- Engine built in release: `cargo build -p shepherd-engine --release` gives `target/release/shepherd`.
- Module and adapter `.wasm` artefacts present under `target/wasm32-wasip2/release/`.
- `engine.toml` with `state_dir` on a persistent path (never `/tmp`), `log_level = "info"`, `[engine.metrics] enabled = true` and `bind_addr = "127.0.0.1:9100"`, one `[chains.<id>]` per subscribed chain with a paid RPC URL, one `[[modules]]` per module, and the `[[adapters]]` cow entry.
- The `state_dir` exists and is writable by the service user.
- A Prometheus instance scraping `/metrics` (§6) with the alert rules in §7.
- A log aggregator ingesting the engine's JSON stdout (§5).

## 2. systemd unit

`/etc/systemd/system/shepherd.service`:

```ini
[Unit]
Description=Shepherd CoW Protocol automation runtime
Documentation=https://github.com/nullislabs/shepherd
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=shepherd
Group=shepherd
WorkingDirectory=/opt/shepherd
ExecStart=/opt/shepherd/bin/shepherd --engine-config /etc/shepherd/engine.toml

# SIGINT/SIGTERM ends the event loop between dispatches: it drains the
# in-flight dispatch, commits the last_dispatched_block cursor, and exits 0.
# 30s covers in-flight RPC.
KillSignal=SIGINT
TimeoutStopSec=30s

# Hardening.
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ReadWritePaths=/var/lib/shepherd
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=true
MemoryDenyWriteExecute=false   # wasmtime JIT needs writable-executable pages

# The supervisor restarts poisoned modules itself; this restarts the host
# process on a non-zero exit. RestartSec avoids a fast loop on config errors.
Restart=on-failure
RestartSec=5s

# Defence in depth on top of the per-module wasmtime caps.
LimitNOFILE=65536
MemoryMax=2G
CPUQuota=200%

Environment=RUST_BACKTRACE=1
# RUST_LOG overrides engine.toml log_level; leave unset so the config is
# the single auditable source.

[Install]
WantedBy=multi-user.target
```

Bring up:

```bash
sudo useradd -r -s /usr/sbin/nologin -d /var/lib/shepherd shepherd
sudo install -d -o shepherd -g shepherd /var/lib/shepherd
sudo install -d -o shepherd -g shepherd /opt/shepherd/bin
sudo install -m 0755 -o shepherd -g shepherd target/release/shepherd /opt/shepherd/bin/
sudo install -d /etc/shepherd
sudo install -m 0644 engine.toml /etc/shepherd/
sudo systemctl daemon-reload
sudo systemctl enable --now shepherd
```

Tail the logs:

```bash
journalctl -u shepherd -f --output=json | jq '.MESSAGE | fromjson?'
```

For the container path (Docker Compose, image tags, `.env` wiring) see [`docs/deployment/docker.md`](./deployment/docker.md).

## 3. State backup (`redb`)

The local-store is a single redb file at `<state_dir>/local-store.redb`. It holds per-module keys (`watch:`, `submitted:`, `dropped:`, `backoff:`, `last_dispatched_block:{chain_id}`); losing it forces a from-scratch resync as modules re-discover state from chain logs.

Cold backup (recommended before upgrades). The engine writes to redb only during dispatch, and the graceful shutdown drains in-flight dispatches, so the stopped file is quiescent:

```bash
sudo systemctl stop shepherd
sudo cp /var/lib/shepherd/local-store.redb \
    /backup/shepherd-$(date -u +%Y%m%dT%H%M%SZ).redb
sudo systemctl start shepherd
```

Live copy. A plain `cp` under a live writer can capture an in-flight commit; pause the process first:

```bash
kill -STOP <pid>
cp /var/lib/shepherd/local-store.redb /backup/...
kill -CONT <pid>
```

The pause window is sub-second on a small store, and the WS connections survive it. Restore by stopping the engine, copying the snapshot back, and restarting. If a restored file does not open, roll forward from the previous snapshot or start with an empty `state_dir` and accept the resync.

## 4. Chain-log cursor

A `resume` subscription persists its progress under `last_dispatched_block:{chain_id}`, written after each successful dispatch, so a restart resumes from the last committed block. The engine backfills the gap on reconnect (see `docs/06-production-hardening.md`).

## 5. Logs

The engine emits JSON `tracing` events on stdout (`--pretty-logs` switches to the human format used in the runbooks). Every event carries `target` (crate + module path), `level`, `fields.message`, and `fields.module` on per-module events. Production should not see `ERROR` from `nexum_runtime::*`.

Aggregate stdout into your log stack (Loki, CloudWatch, Datadog). A Vector journald source parsing the JSON `message` field and routing by `level` is the typical pattern.

## 6. Metrics

`/metrics` binds when `[engine.metrics] enabled = true`. Always bind loopback, never `0.0.0.0`; Prometheus scrapes over the loopback or container network. The bare `nexum` binary does not register the exporter; run `shepherd`.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `shepherd_event_latency_seconds` | histogram | `module`, `event_kind` | Per-module dispatch latency. |
| `shepherd_dispatch_dropped_total` | counter | `module`, `event_kind` | Events dropped by the per-module dispatch rate limit (`[limits.dispatch]`, default `burst=256` / `refill_per_sec=128`). |
| `shepherd_module_errors_total` | counter | `module`, `error_kind` | Host faults and traps. `error_kind="trap"` is a wasmtime trap; other kinds are fault labels. |
| `shepherd_module_restarts_total` | counter | `module` | Per-module restart attempts. |
| `shepherd_module_poisoned` | gauge | `module` | `1` once a module crosses `[limits.poison]` (default 5 failures / 600 s). Stays `1` until process restart. |
| `shepherd_adapter_errors_total` | counter | `adapter`, `error_kind` | Venue-adapter faults and traps. |
| `shepherd_adapter_restarts_total` | counter | `adapter` | Venue-adapter restart attempts. |
| `shepherd_adapter_poisoned` | gauge | `adapter` | `1` once an adapter is quarantined. |
| `shepherd_chain_request_total` | counter | `chain_id`, `method`, `outcome` | Every `chain::request`. `outcome="err"` rate is the RPC-degraded signal. |
| `shepherd_chain_response_capped_total` | counter | `chain_id`, `method` | Responses rejected for exceeding `[limits.chain] response_body_max_bytes`. |
| `shepherd_stream_reconnects_total` | counter | `kind`, `chain_id`, `module?` | WS/poller reconnects. `kind="block"` is per-chain; `kind="chain-log"` also carries `module`. |

Prometheus scrape:

```yaml
scrape_configs:
  - job_name: shepherd
    scrape_interval: 15s
    static_configs:
      - targets: ["127.0.0.1:9100"]
```

## 7. Alerting

`prometheus-rules.yml`:

```yaml
groups:
  - name: shepherd
    interval: 30s
    rules:
      - alert: ShepherdModulePoisoned
        expr: shepherd_module_poisoned > 0 or shepherd_adapter_poisoned > 0
        for: 1m
        labels: { severity: page }
        annotations:
          summary: "Shepherd {{ $labels.module }}{{ $labels.adapter }} is poisoned"

      - alert: ShepherdModuleTraps
        expr: rate(shepherd_module_errors_total{error_kind="trap"}[5m]) > 0
        for: 5m
        labels: { severity: ticket }
        annotations:
          summary: "Shepherd module {{ $labels.module }} trapping"

      - alert: ShepherdRpcErrorRate
        expr: |
          sum by (chain_id) (rate(shepherd_chain_request_total{outcome="err"}[5m]))
            / sum by (chain_id) (rate(shepherd_chain_request_total[5m])) > 0.05
        for: 10m
        labels: { severity: ticket }
        annotations:
          summary: "Shepherd RPC error rate > 5% on chain {{ $labels.chain_id }}"

      - alert: ShepherdReconnectStorm
        expr: rate(shepherd_stream_reconnects_total[5m]) > 0.1
        for: 5m
        labels: { severity: ticket }
        annotations:
          summary: "Shepherd WS reconnecting frequently"

      - alert: ShepherdDispatchLatency
        expr: |
          histogram_quantile(0.95,
            sum by (module, le) (rate(shepherd_event_latency_seconds_bucket[10m]))) > 5
        for: 15m
        labels: { severity: ticket }
        annotations:
          summary: "Shepherd module {{ $labels.module }} p95 latency > 5s"

      - alert: ShepherdDown
        expr: up{job="shepherd"} == 0
        for: 2m
        labels: { severity: page }
        annotations:
          summary: "Shepherd is down (metrics scrape failing)"
```

`page` wakes on-call (poison, down); `ticket` routes during business hours.

## 8. RPC selection

The engine reaches chains through alloy providers configured at boot. Public nodes throttle `eth_subscribe` and `eth_call`, so production must use a paid endpoint (Alchemy, Infura, QuickNode). Prefer `wss://` where offered: a WebSocket pushes new blocks via `eth_subscribe(newHeads)`, an HTTP URL polls `eth_getBlockByNumber`; both work, push is lower-latency. `shepherd_chain_request_total{outcome="err"}` is the degradation signal.

Resource caps are engine defaults today; per-module overrides in `[limits]` apply uniformly. A module that consistently traps `OutOfFuel` is a bug, not a tuning miss.

## 9. Runbook

Tail one module:

```bash
journalctl -u shepherd -f --output=json \
  | jq 'select(.MESSAGE | fromjson? | .fields.module == "twap-monitor")'
```

Recover a poisoned module: fix the underlying bug, rebuild the artefact, then `sudo systemctl restart shepherd` (the failure ring is in-memory and clears at boot). The engine reads `[[modules]]` and `[[adapters]]` at boot only, so adding a module means editing `engine.toml` and restarting. Logging-level changes also require a restart.

## 10. Pre-upgrade

- Read the CHANGELOG for breaking config or manifest changes.
- Cold-backup the local-store (§3).
- Stage the new binary, run it once with the production `engine.toml`, and confirm the supervisor-ready line before Ctrl-C.
- Swap the binary and `sudo systemctl restart shepherd`.
- Watch `journalctl -u shepherd -f` for new ERROR/WARN lines for at least 5 minutes.

## References

- Hardening design: `docs/06-production-hardening.md`
- Module handbook: `docs/tutorial-first-module.md`
- ADR-0001, ADR-0002, ADR-0003 (`docs/adr/`)
