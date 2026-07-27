# Production hardening

The design facts behind the runtime's resource enforcement, restart policy, RPC resilience, and error model. Deploy procedures live in [`docs/production.md`](./production.md); the metric surface and alert rules in [`docs/production.md`](./production.md) §6-7; containers in [`docs/deployment/docker.md`](./deployment/docker.md).

## Resource enforcement

Four dimensions are capped per module. Caps come from `[limits]` in `engine.toml`, resolving to built-in defaults (`nexum/crates/nexum-runtime/src/engine_config.rs`). They apply uniformly to every module; per-module overrides land in 0.3.

### Fuel

Each `on_event` call is granted `fuel_per_event` fuel (default 1_000_000_000, ~1s of compute). Exhaustion traps the call and rolls back its state (doc 04). Fuel is deterministic: the same WASM consumes the same fuel regardless of host speed. It meters only guest instructions.

### Wall-clock deadline

Fuel does not meter time spent in host calls (chain RPC, HTTP, redb). A per-dispatch wall-clock deadline (`event_deadline_secs`, default 120, floor 1) is the backstop: the supervisor runs each dispatch under a `tokio::time::timeout`, and a dispatch that outlives it, guest plus every awaited host call, is cancelled and the module marked dead. Fuel and the deadline are complementary, not redundant.

### Memory

The supervisor attaches a `wasmtime::StoreLimitsBuilder` built from `memory_bytes` (default 64 MiB) to each module store; `memory.grow` past the cap is denied.

### Storage

The local-store byte quota (`state_bytes`, default 50 MiB) is enforced on the `local-store::set` host path. Overrun is rejected with a `fault.invalid-input`, not a trap, so the module can handle it.

| Resource | Mechanism | Failure mode |
|----------|-----------|-------------|
| CPU (guest instructions) | Fuel | Trap, rollback, restart |
| Wall-clock | Per-dispatch tokio timeout | Cancel, module marked dead |
| Memory | `StoreLimits` | `memory.grow` denied |
| Storage | Host-side byte tracking | `local-store::set` returns `fault.invalid-input` |

## Restart policy

When a module's `init` or `on_event` traps or returns `Err`, the supervisor restarts it after an exponential backoff: 1s, 2s, 4s, 8s, doubling to a 300s (5 min) cap (`runtime/restart_policy.rs`). A restart creates a fresh `Store` (clean WASM memory) but reuses the compiled `InstancePre`; `init` runs again and local-store data persists (doc 04). A successful dispatch resets the counter.

A module that keeps trapping is a poison pill. After `max_failures` traps within a sliding `window_secs` (`[limits.poison]`, default 5 / 600s in `runtime/poison_policy.rs`), the module is quarantined: dispatch to it stops, the `shepherd_module_poisoned` gauge goes to `1`, and a WARN is logged. Quarantine clears only on an engine restart. Venue adapters follow the same policy under the `shepherd_adapter_poisoned` gauge.

## RPC resilience

RPC I/O flows through one alloy provider per chain, opened from `engine.toml` at boot (`nexum/crates/nexum-runtime/src/host/provider_pool.rs`). Each chain has a single `rpc_url`; there is no secondary-endpoint failover. The `chain::request` host function forwards the typed method to the provider.

Two layers harden it:

- A transport `RetryBackoffLayer` (10 retries, 300ms base backoff, 100 CU/s pacing) heals transient node blips below the poller, so a momentary hiccup does not force a stream re-open.
- A per-request timeout (`request_timeout_secs`, default 30) bounds each `chain::request`; it does not apply to the long-lived subscription and log-poller streams.

Block following uses `eth_subscribe(newHeads)` on a WebSocket URL and polls `eth_getBlockByNumber` on HTTP; logs poll `eth_getLogs` on either transport. On a dropped WebSocket the event loop reconnects with backoff, then backfills the gap between the last dispatched block and head so modules do not silently miss events. A `resume` chain-log subscription persists its cursor under `last_dispatched_block:{chain_id}` and resumes from it across restarts.

## Error model

Host interfaces surface a common `fault` variant (`wit/nexum-host/types.wit`): `unsupported`, `unavailable`, `denied`, `rate-limited` (carrying `retry-after-ms` guidance), `timeout`, `invalid-input`, `internal`. A fault is a typed, recoverable return the guest can handle; a trap (fuel, memory, panic) is not, and drives the restart path above.

## Structured logging

All runtime logging uses `tracing` with structured fields, emitted as JSON in production (or the pretty format under `--pretty-logs`). Every per-module event carries the module name and, on chain events, the `chain_id` and `block_number`. When a guest calls `logging::log`, the host writes a `tracing` event tagged with the module's context. Log operations (retention, aggregation) are in [`docs/production.md`](./production.md) §5.

## Metrics

The runtime records through the `metrics` crate facade. The `shepherd` binary installs a `metrics-exporter-prometheus` exporter (the Prometheus add-on) that binds `/metrics` on `[engine.metrics] bind_addr` when `enabled = true`; the bare `nexum` binary installs the recorder but binds no listener. The runtime emits no alerts itself. The metric surface and recommended alert rules are in [`docs/production.md`](./production.md) §6-7.
