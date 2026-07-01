# Production deployment guide

Operator handbook for running `nexum-engine` (Shepherd) in
production. Focused on **concrete artefacts** — unit files,
backup recipes, alert rules — not the design rationale, which
lives in `docs/06-production-hardening.md` (resource enforcement,
restart policy, RPC resilience, logging + metrics design).

Audience: someone deploying Shepherd onto a Linux host or a
container orchestrator for the first time, with the assumption
that the runtime, modules, and module manifests are already
known-good (M3 + M4 milestones complete; module developer's
handbook is `docs/tutorial-first-module.md`).

---

## 1. Pre-flight checklist

Before launching:

- [ ] **Engine binary built in `--release`** mode.
  `cargo build -p nexum-engine --release` → `target/release/nexum-engine`.
- [ ] **All module artefacts present** under
  `target/wasm32-wasip2/release/` and content-addressable
  (the operator pins the sha256 in each module's manifest
  `[module] component = "sha256:..."` once 0.3 verification
  lands; for 0.2 the field exists but is not enforced).
- [ ] **`engine.toml`** (the production-shape config) exists with:
  - `[engine] state_dir = "/var/lib/shepherd"` (or equivalent
    persistent path; never `/tmp`).
  - `[engine] log_level = "info"` (NOT debug — see §5).
  - `[engine.metrics] enabled = true` and `bind_addr` on
    `127.0.0.1:9100` (NOT `0.0.0.0` — see §7).
  - One `[chains.<id>]` entry per chain you intend to
    subscribe to, with a **paid** WS URL (Alchemy / Infura /
    QuickNode — public nodes will throttle under sustained
    load, see §6).
  - One `[[modules]]` entry per module to load.
- [ ] **`/var/lib/shepherd`** exists, writable by the engine's
  service user, and on a volume large enough for the local-store
  growth budget (§4).
- [ ] **A Prometheus instance** scraping the engine's `/metrics`
  endpoint (§7) and an alert pipeline pointed at the rules in §9.
- [ ] **A log aggregator** ingesting the engine's JSON stdout
  (§5) — stdout, not a file written by the engine.
- [ ] **An on-call runbook reference** — link to this document
  and to `docs/operations/m3-testnet-runbook.md` (testnet
  validation, useful for staging deploys).

---

## 2. Process-level deploy: systemd unit

`/etc/systemd/system/shepherd.service`:

```ini
[Unit]
Description=Shepherd (nexum-engine) — CoW Protocol off-chain automation runtime
Documentation=https://github.com/bleu/nullis-shepherd
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=shepherd
Group=shepherd

# Working directory + binary.
WorkingDirectory=/opt/shepherd
ExecStart=/opt/shepherd/bin/nexum-engine \
    --engine-config /etc/shepherd/engine.toml

# Graceful shutdown — engine handles SIGINT/SIGTERM by:
#   1. closing chain subscription tasks,
#   2. finishing the in-flight dispatch,
#   3. writing `last_dispatched_block:{chain_id}` to local-store,
#   4. logging `graceful shutdown complete ...` and exiting 0.
# Give it 30 s — production runs can have ~5 s of in-flight RPC.
KillSignal=SIGINT
TimeoutStopSec=30s

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ReadWritePaths=/var/lib/shepherd
# Engine binds 127.0.0.1:9100 for metrics. No other listeners.
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=true
MemoryDenyWriteExecute=false   # wasmtime JIT requires writable+executable memory pages

# Restart policy — supervisor handles per-module poison/restart
# itself, but if the host process exits non-zero (panic, OOM,
# etc.) restart after 5 s. RestartSec=0 would loop fast on
# config errors.
Restart=on-failure
RestartSec=5s

# Resource caps (defence in depth — wasmtime is already capping
# per-module memory at 64 MiB and fuel at ~1B inst/event).
LimitNOFILE=65536
MemoryMax=2G
CPUQuota=200%

# Environment
Environment=RUST_BACKTRACE=1
# RUST_LOG overrides engine.toml::log_level if set. Leave unset
# in production; tune via the config file so the change is
# auditable.
# Environment=RUST_LOG=info,nexum_engine=debug

[Install]
WantedBy=multi-user.target
```

Bring up:

```bash
sudo useradd -r -s /usr/sbin/nologin -d /var/lib/shepherd shepherd
sudo install -d -o shepherd -g shepherd /var/lib/shepherd
sudo install -d -o shepherd -g shepherd /opt/shepherd/bin
sudo install -m 0755 -o shepherd -g shepherd \
    target/release/nexum-engine /opt/shepherd/bin/
sudo install -d /etc/shepherd
sudo install -m 0644 -o root -g root engine.toml /etc/shepherd/
sudo systemctl daemon-reload
sudo systemctl enable --now shepherd
sudo systemctl status shepherd
```

Tail the logs:

```bash
journalctl -u shepherd -f --output=json | jq '.MESSAGE | fromjson?'
```

---

## 3. Container deploy: Docker Compose

> **Status note:** the official Dockerfile is tracked as a
> separate issue. Until it lands, build the image locally with
> the multi-stage recipe below; the Compose file is forward-
> compatible with the eventual published image.

### 3.1 Dockerfile (interim)

```dockerfile
# syntax=docker/dockerfile:1.6
FROM rust:1.86-slim-bookworm AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev cmake clang \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-wasip2
COPY . .
RUN cargo build -p nexum-engine --release
# Build all 5 modules. Add yours here.
RUN cargo build -p twap-monitor     --target wasm32-wasip2 --release \
 && cargo build -p ethflow-watcher  --target wasm32-wasip2 --release \
 && cargo build -p price-alert      --target wasm32-wasip2 --release \
 && cargo build -p balance-tracker  --target wasm32-wasip2 --release \
 && cargo build -p stop-loss        --target wasm32-wasip2 --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -s /usr/sbin/nologin -d /var/lib/shepherd shepherd \
    && install -d -o shepherd -g shepherd /var/lib/shepherd
COPY --from=build /src/target/release/nexum-engine /usr/local/bin/
COPY --from=build /src/target/wasm32-wasip2/release/*.wasm /opt/shepherd/modules/
COPY --from=build /src/modules /opt/shepherd/manifests
USER shepherd
WORKDIR /var/lib/shepherd
EXPOSE 9100
ENTRYPOINT ["/usr/bin/tini", "--", "nexum-engine"]
CMD ["--engine-config", "/etc/shepherd/engine.toml"]
```

### 3.2 docker-compose.yml

```yaml
version: "3.9"
services:
  shepherd:
    build: .
    image: shepherd:latest
    restart: unless-stopped
    volumes:
      - shepherd-state:/var/lib/shepherd
      - ./engine.toml:/etc/shepherd/engine.toml:ro
    ports:
      # Bind metrics endpoint to the host loopback only —
      # Prometheus scrapes it via docker network, no public
      # exposure.
      - "127.0.0.1:9100:9100"
    stop_signal: SIGINT
    stop_grace_period: 30s
    healthcheck:
      # Metrics endpoint serves a Prometheus exposition page;
      # treating a successful GET as liveness is good enough
      # until a dedicated /health endpoint lands.
      test: ["CMD-SHELL", "wget -qO- http://127.0.0.1:9100/metrics > /dev/null"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 15s
    deploy:
      resources:
        limits:
          memory: 2G
          cpus: "2.0"

  prometheus:
    image: prom/prometheus:latest
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - ./prometheus-rules.yml:/etc/prometheus/rules.yml:ro
      - prometheus-data:/prometheus
    ports:
      - "127.0.0.1:9090:9090"

volumes:
  shepherd-state:
  prometheus-data:
```

`prometheus.yml`:

```yaml
scrape_configs:
  - job_name: shepherd
    scrape_interval: 15s
    static_configs:
      - targets: ["shepherd:9100"]
rule_files:
  - /etc/prometheus/rules.yml
```

---

## 4. State store backup (`redb`)

The local-store is a single redb file at
`<state_dir>/ls.redb`. It accumulates per-module
`watch:`, `submitted:`, `dropped:`, `backoff:`, `last:`, and
`last_dispatched_block:{chain_id}` keys; losing it on a
production module forces a from-scratch resync (twap-monitor
re-discovers `watch:` from the next `ConditionalOrderCreated`
log; stop-loss re-issues a `submitted:` write if the trigger
fires again).

### 4.1 Cold backup (recommended for first deploy + before upgrades)

The engine writes to redb only during dispatch. On a SIGINT the
graceful shutdown path drains in-flight dispatches and the file
becomes quiescent within ≤ 5 s.

```bash
sudo systemctl stop shepherd          # or: docker compose stop shepherd
sudo cp /var/lib/shepherd/ls.redb /backup/shepherd-ls-$(date -u +%Y%m%dT%H%M%SZ).redb
sudo systemctl start shepherd
```

Cold copies are byte-identical to a fresh database and need no
verification.

### 4.2 Hot backup (live process)

redb 2.x is single-file MVCC + a commit-on-disk log; an `cp`
under a live writer can capture an in-flight commit and produce
a database that fails `Database::check_integrity` on restore.
For the M4 release the supported path is:

1. Send SIGSTOP to the engine PID (`kill -STOP <pid>`).
2. `cp` the file (redb's on-disk format is consistent at any
   commit boundary, and SIGSTOP guarantees no writer is mid-
   commit).
3. Send SIGCONT (`kill -CONT <pid>`).

The pause-and-copy window is ≤ 1 s on a ~100 MiB local-store
(typical 30-day production size). Subscribers won't drop because
the alloy WS connection survives a brief process stop.

A `redb::Database::backup`-style API (snapshot from within a
read transaction) is on the roadmap — track in upstream redb
releases > 2.6.

### 4.3 Restore + integrity check

```bash
sudo systemctl stop shepherd
sudo cp /backup/shepherd-ls-<timestamp>.redb /var/lib/shepherd/ls.redb
sudo -u shepherd /opt/shepherd/bin/nexum-engine \
    --engine-config /etc/shepherd/engine.toml \
    --check-integrity-only      # planned 0.3 flag; manual call today:
# rust: redb::Database::open(path)?.check_integrity()? -> bool
sudo systemctl start shepherd
```

If the integrity check returns `false`, do **not** start the
engine on the restored file. Roll forward from the previous
known-good snapshot; in the worst case start with an empty
state directory and accept the resync cost above.

### 4.4 Retention policy (suggested)

- 7 daily cold backups.
- 4 weekly cold backups (rotated every Sunday).
- 12 monthly cold backups.

Total cost on a 100 MiB store ≈ 23 × 100 MiB = 2.3 GiB.

---

## 5. Logs

### 5.1 Format

The engine emits JSON-formatted `tracing` events on stdout
(unless `--pretty-logs` is passed; only the runbook docs use
that flag). Sample event:

```json
{
  "timestamp": "2026-06-18T15:30:00.000Z",
  "level": "INFO",
  "target": "nexum_engine::supervisor",
  "fields": {
    "message": "init succeeded",
    "module": "twap-monitor"
  }
}
```

Important fields on every event:

| Field | Meaning |
|---|---|
| `target` | Crate + module path. Useful filters: `nexum_engine`, `nexum_engine::supervisor`, `nexum_engine::host::impls::cow_api`. |
| `level` | `TRACE` < `DEBUG` < `INFO` < `WARN` < `ERROR`. **Production should never see `ERROR`** from `nexum_engine::*` (only from third-party crates the supervisor wraps as warnings). |
| `fields.message` | Human-readable summary. Greppable. |
| `fields.module` | Set on every per-module event — supervisor, host calls, guest log emissions. Use this for per-module dashboards. |

### 5.2 Retention + aggregation

Two-tier model:

1. **Hot (last 7 days)** — full INFO + DEBUG. Lives in your
   log aggregator (Loki / CloudWatch Logs / Datadog). Used for
   incident investigation.
2. **Cold (90 days)** — INFO only, drop DEBUG at ingest time.
   S3 / GCS with lifecycle rule to Glacier at 90 days. Used for
   audit + post-mortem.

INFO-level retention sizing: each dispatch produces ~1 KB of
INFO/DEBUG output combined. 5 modules × 1 block / 12 s × 7
days ≈ 200 MiB/week. DEBUG roughly doubles this; the cold tier
dropping DEBUG keeps the long-term cost trivial.

### 5.3 Aggregation pattern: Vector → Loki

`vector.toml`:

```toml
[sources.shepherd]
type = "journald"
include_units = ["shepherd.service"]

[transforms.parse_json]
type = "remap"
inputs = ["shepherd"]
source = '''
  . = parse_json!(.message)
'''

[transforms.drop_debug_cold]
type = "filter"
inputs = ["parse_json"]
condition = '.level != "DEBUG"'

[sinks.loki_hot]
type = "loki"
inputs = ["parse_json"]
endpoint = "http://loki:3100"
labels = { app = "shepherd", level = "{{ .level }}", module = "{{ .fields.module }}" }

[sinks.s3_cold]
type = "aws_s3"
inputs = ["drop_debug_cold"]
bucket = "shepherd-logs-cold"
key_prefix = "year=%Y/month=%m/day=%d/"
compression = "gzip"
```

---

## 6. RPC selection

The engine talks to chains exclusively through alloy providers
configured at boot. Public nodes throttle `eth_subscribe` and
`eth_call` aggressively; production deployments **must** use a
paid endpoint.

> **Use `wss://`, not `https://`.** `eth_subscribe` (the engine's
> block + log event source) is WebSocket-only in the JSON-RPC spec;
> HTTP transports return `"subscriptions are not available on this
> provider"` and the supervisor's reconnect backoff will
> loop forever waiting for a subscription that can never open.
> Every paid provider exposes both schemes per endpoint — pick the
> WS form. The engine surfaces a boot-time ERROR log line for any
> `http(s)://` `rpc_url`, with the exact `wss://` swap suggested.
> Set `[chains.<id>] require_ws = false` to opt out (for poll-only
> deployments that never subscribe).

| Provider | Plan recommendation | Notes |
|---|---|---|
| Alchemy | Growth tier (≥ 660M CU/mo) | First-class WS pubsub; SLA-backed. |
| Infura | Developer Plus (≥ 6M req/day) | Solid WS; rate-limits per project key. |
| QuickNode | Discover tier (≥ 25 req/s) | Dedicated endpoints; recommended for multi-chain swarms. |

`engine.toml`:

```toml
[chains.11155111]
rpc_url = "wss://eth-sepolia.g.alchemy.com/v2/<KEY>"

[chains.42161]
rpc_url = "wss://arb-mainnet.g.alchemy.com/v2/<KEY>"
```

Capacity sizing (per chain):

- `1` block subscription, always-on. WS.
- `N` log subscriptions, where `N` = number of modules with
  `[[subscription]] kind = "log"`.
- `M` `eth_call` per block, where `M` ≈ sum of polling modules'
  active orders. The TWAP module's load grows linearly with the
  number of registered orders; budget accordingly.

`shepherd_chain_request_total{outcome="err"}` rate is the
canonical "the RPC is degraded" signal — see §9 alerts.

---

## 7. Metrics + scraping

`/metrics` is exposed when `[engine.metrics] enabled = true` in
`engine.toml`. **Always** bind to a loopback address; never
`0.0.0.0`. Prometheus scrapes via the loopback / container
network.

### 7.1 Metric surface

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `shepherd_event_latency_seconds` | histogram | `module`, `event_kind` | Per-module dispatch latency. p95 > 1 s on a non-RPC-heavy module is suspicious. |
| `shepherd_module_errors_total` | counter | `module`, `error_kind` | All host errors + traps. `error_kind="trap"` = wasmtime trap (fuel / memory / panic); other kinds map to `HostErrorKind` variants. |
| `shepherd_module_restarts_total` | counter | `module` | Increments on every `reinstantiate_one` attempt (per-module restart backoff). |
| `shepherd_module_poisoned` | gauge | `module` | `1` if the module has been quarantined per `POISON_MAX_FAILURES=5` / `POISON_WINDOW=10m`. Stays `1` until process restart. |
| `shepherd_chain_request_total` | counter | `chain_id`, `method`, `outcome` | Every `chain::request` host call. `outcome="err"` rate > 5% = RPC degraded. |
| `shepherd_cow_api_submit_total` | counter | `chain_id`, `outcome` | Every orderbook submit. `outcome="err"` covers both retriable and dropped — drill into supervisor logs to discriminate. |
| `shepherd_stream_reconnects_total` | counter | `kind`, `chain_id`, `module?` | WS reconnect attempts. `kind="block"` is per-chain; `kind="log"` carries the `module` label too. |

### 7.2 Prometheus config snippet

```yaml
scrape_configs:
  - job_name: shepherd
    scrape_interval: 15s
    static_configs:
      - targets: ["127.0.0.1:9100"]
```

15 s is conservative; the metrics cardinality is bounded by
modules × chains, which on a 5-module / 2-chain deploy is ~15
series for the gauges + ~30 for the counters.

---

## 8. Workload-class tuning

Resource limits today are compile-time constants. Per-module
overrides via `[engine.limits]` are tracked as a 0.3 follow-up
(referenced from `crates/nexum-engine/src/runtime/limits.rs`).
The tuning advice below is therefore advisory — adjust by
changing the constants in `runtime/limits.rs` and rebuilding,
or by ensuring per-module loads fit within the current
defaults.

| Class | Modules typical | Fuel/event | Memory cap | Notes |
|---|---|---|---|---|
| **Light indexer** | price-alert, balance-tracker | 200M | 16 MiB | Block-tick poll + 1-2 RPC reads. Defaults are 5× headroom. |
| **TWAP-style polling** | twap-monitor, stop-loss | 1B (default) | 64 MiB (default) | Per-block `getTradeableOrderWithSignature` calls per registered order; long ABI decode + signature work. Defaults sized for this case. |
| **Multi-chain swarm** | 5+ modules × 2+ chains | 2B | 128 MiB | More headroom for parallel dispatch overhead; modules don't share state, but the per-store wasmtime overhead is per-(module, chain). |

A module that consistently traps `OutOfFuel` is a bug, not a
tuning miss -- open an issue with the supervisor log
snippet rather than raising the fuel budget. The defaults are
already 5-10× the largest observed real-world dispatch.

---

## 9. Alerting

Prometheus alert rules (`prometheus-rules.yml`):

```yaml
groups:
  - name: shepherd
    interval: 30s
    rules:
      # P0: a production module is permanently quarantined.
      # Recovery requires operator action (process restart +
      # module triage).
      - alert: ShepherdModulePoisoned
        expr: shepherd_module_poisoned > 0
        for: 1m
        labels:
          severity: page
        annotations:
          summary: "Shepherd module {{ $labels.module }} is poisoned"
          description: |
            Module has crossed POISON_MAX_FAILURES traps within
            POISON_WINDOW. Engine has stopped dispatching to it.
            Investigate: journalctl -u shepherd | jq 'select(.fields.module=="{{ $labels.module }}")'

      # P1: trap rate climbing. Pre-poison signal — gives 5 min
      # of warning before ShepherdModulePoisoned fires.
      - alert: ShepherdModuleTraps
        expr: rate(shepherd_module_errors_total{error_kind="trap"}[5m]) > 0
        for: 5m
        labels:
          severity: ticket
        annotations:
          summary: "Shepherd module {{ $labels.module }} trapping"
          description: |
            Module is restart-looping. Investigate before
            POISON_MAX_FAILURES (5 traps / 10 min) trips.

      # P1: RPC layer degraded. Engine keeps running but
      # dispatches will degrade; operator should switch
      # endpoints or escalate to provider.
      - alert: ShepherdRpcErrorRate
        expr: |
          sum by (chain_id) (rate(shepherd_chain_request_total{outcome="err"}[5m]))
            /
          sum by (chain_id) (rate(shepherd_chain_request_total[5m]))
            > 0.05
        for: 10m
        labels:
          severity: ticket
        annotations:
          summary: "Shepherd RPC error rate > 5% on chain {{ $labels.chain_id }}"

      # P1: WS reconnect storm. A flapping endpoint is worse
      # than a hard-down one (subscriptions keep partially
      # working but events get dropped during reconnect windows).
      - alert: ShepherdReconnectStorm
        expr: rate(shepherd_stream_reconnects_total[5m]) > 0.1
        for: 5m
        labels:
          severity: ticket
        annotations:
          summary: "Shepherd WS reconnecting frequently"

      # P2: orderbook degraded. Modules will retry per the SDK's
      # `classify_api_error` taxonomy; this alert fires only on
      # sustained errs and is a CoW-side signal more than a
      # Shepherd signal.
      - alert: ShepherdCowApiErrorRate
        expr: |
          sum by (chain_id) (rate(shepherd_cow_api_submit_total{outcome="err"}[10m]))
            /
          sum by (chain_id) (rate(shepherd_cow_api_submit_total[10m]))
            > 0.20
        for: 15m
        labels:
          severity: ticket
        annotations:
          summary: "Shepherd cow-api submit error rate > 20% on chain {{ $labels.chain_id }}"

      # P2: dispatch latency. Modules with sustained p95 > 5 s
      # are usually doing more on-chain reads than budgeted; not
      # an outage but worth tuning.
      - alert: ShepherdDispatchLatency
        expr: |
          histogram_quantile(0.95,
            sum by (module, le) (rate(shepherd_event_latency_seconds_bucket[10m]))
          ) > 5
        for: 15m
        labels:
          severity: ticket
        annotations:
          summary: "Shepherd module {{ $labels.module }} p95 latency > 5 s"

      # P3: engine absent. Either crashed and systemd hasn't
      # restarted yet, or metrics binding failed.
      - alert: ShepherdDown
        expr: up{job="shepherd"} == 0
        for: 2m
        labels:
          severity: page
        annotations:
          summary: "Shepherd is down (metrics scrape failing)"
```

Severity convention:

| Label | Action |
|---|---|
| `page` | On-call wakes up. ShepherdModulePoisoned + ShepherdDown only. |
| `ticket` | Routed to the Shepherd team during business hours. |

---

## 10. Operational runbook (common tasks)

### 10.1 Tail a single module's events

```bash
journalctl -u shepherd -f --output=json \
  | jq 'select(.MESSAGE | fromjson? | .fields.module == "twap-monitor")'
```

### 10.2 Reset a poisoned module

A poisoned module stays poisoned until process restart (M4
design — no live un-poison API yet). The recovery flow:

1. Triage the failure: `journalctl -u shepherd | jq 'select(.MESSAGE | fromjson? | .level == "ERROR" or (.fields.message | test("trapped|poisoned")))'`.
2. Fix the underlying bug (in the module's Rust code, or the
   manifest config, or the on-chain target). Rebuild the module.
3. Restart the engine: `sudo systemctl restart shepherd`. The
   `failure_count` + `failure_timestamps` ring is in-memory and
   resets at boot.

### 10.3 Add a module to a running deploy

The engine reads `[[modules]]` at boot only. To add a module:

1. Build the module's wasm artefact + drop it in the artefacts
   directory.
2. Append a `[[modules]]` entry to `engine.toml`.
3. `sudo systemctl restart shepherd`. The graceful shutdown
   writes `last_dispatched_block:{chain_id}` so new modules
   know which block to start from (if they care).

A live `engine::reload` API is not in scope for 0.2; tracked as
a 0.3+ follow-up.

### 10.4 Inspect the local-store contents

There is no `ls-dump` CLI today. Workarounds:

- Boot a one-shot Rust script with `redb::Database::open` (read-
  only) against the live file. Safe — redb supports concurrent
  readers + a single writer.
- Stop the engine + use any redb inspector tool against the
  copy.

### 10.5 Bump the log level live

Logging-level changes today require an engine restart (the
filter is wired at boot). On 0.3, a SIGHUP handler will re-read
`engine.toml::log_level`. Until then:

```bash
sudo sed -i 's/log_level = "info"/log_level = "info,nexum_engine=debug"/' \
    /etc/shepherd/engine.toml
sudo systemctl restart shepherd
# revert when the investigation is done
```

---

## 11. Pre-upgrade checklist

Before bumping `nexum-engine` between minor versions:

- [ ] Read the CHANGELOG for breaking config / manifest
  changes.
- [ ] Cold-backup the local-store per §4.1.
- [ ] Stage the new binary in `/opt/shepherd/bin/nexum-engine.new`
  + run it once with `--engine-config /etc/shepherd/engine.toml`
  + Ctrl-C after `supervisor ready modules=N chains=M` to
  validate the config still parses. Roll forward only if the
  ready line appears.
- [ ] `mv /opt/shepherd/bin/nexum-engine.new /opt/shepherd/bin/nexum-engine`.
- [ ] `sudo systemctl restart shepherd`.
- [ ] Watch `journalctl -u shepherd -f` for ≥ 5 min after
  restart. Look for any new ERROR / WARN lines that weren't
  present pre-upgrade.

---

## 12. References

- Architectural rationale: `docs/06-production-hardening.md`
- Per-module developer handbook: `docs/tutorial-first-module.md`
- Testnet runbooks (staging validation):
  - `docs/operations/m2-testnet-runbook.md`
  - `docs/operations/m3-testnet-runbook.md`
  - `docs/operations/e2e-testnet-runbook.md` (full 5-module run)
- ADRs touching production posture:
  - `docs/adr/0001-engine-toml-separate-from-nexum-toml.md`
  - `docs/adr/0002-provider-pool-transport-by-scheme.md`
  - `docs/adr/0003-local-store-namespacing.md`
