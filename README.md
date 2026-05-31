# Shepherd

[![CI](https://github.com/nullisLabs/shepherd/actions/workflows/ci.yml/badge.svg)](https://github.com/nullisLabs/shepherd/actions/workflows/ci.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL_3.0-blue.svg)](LICENSE)

Shepherd is the [CoW Protocol](https://cow.fi) distribution of **Nexum**, a WebAssembly Component Model runtime for secure, sandboxed execution of capability-scoped modules.

A module compiled against the universal `nexum:host/event-module` world runs on any Nexum-compatible host. A module compiled against `shepherd:cow/shepherd` additionally gains access to CoW Protocol APIs and order submission — and requires a Shepherd host.

> **Upgrading from 0.1?** See the [Migration Guide](docs/migration/0.1-to-0.2.md) for the full rename table, the new `host-error` model, and the manifest-driven capability negotiation introduced in 0.2.

## Why

- **Component Model from day 1** — WIT-defined API contract; structural sandboxing (no WASI, no FS, no network); multi-language guests.
- **Capability-scoped** — modules see only the host primitives they declare; nothing implicit.
- **Declarative subscriptions** — modules declare events in their manifest; the runtime wires sources.
- **Transactional state** — per-event all-or-nothing semantics; commit on success, rollback on trap.
- **Content-addressed distribution** — modules are fetched by hash (Swarm, IPFS, OCI, HTTPS); integrity always verified.
- **Self-hosted** — no centralised dependency; operator runs their own node.

## Layout

| Path | Purpose |
| --- | --- |
| `crates/nexum-engine/` | The **engine** — a wasmtime-based host *implementation* of the `nexum:host` contract. The reference server runtime. |
| `wit/nexum-host/` | The **`nexum:host` WIT package** — the host/guest *contract* (interfaces, types, worlds) that every engine implements and every module imports. |
| `wit/shepherd-cow/` | `shepherd:cow` WIT package — CoW Protocol-specific extensions on top of `nexum:host`. |
| `modules/example/` | Reference guest module demonstrating the module ABI. |
| `docs/` | Architecture, design notes, and the universal primitive taxonomy. Start with [`docs/00-overview.md`](docs/00-overview.md). |

> **Engine vs. host.** "Engine" is a concrete implementation that runs WASM components (today: `nexum-engine`, a wasmtime-based daemon). The `nexum:host` WIT package is the *contract* — the host-imports surface a guest sees. Other engines (mobile, browser) can implement the same `nexum:host` contract; modules built against the contract run on any compliant engine.

## Building

Shepherd uses [Nix](https://nixos.org/) flakes to pin the toolchain and [just](https://github.com/casey/just) as a task runner.

```sh
# Enter the dev shell (pulls Rust, wasm-tools, just, etc.)
nix develop

# Or with direnv:
direnv allow

# Build everything
just build

# Run the runtime against the example module
just run
```

Without Nix, you need: Rust (edition 2024, see `rust-toolchain.toml` if present), the `wasm32-wasip2` target, and `wasm-tools`.

## Documentation

The `docs/` directory contains the design corpus:

- [`00-overview.md`](docs/00-overview.md) — architecture, primitives, WIT worlds
- [`01-runtime-environment.md`](docs/01-runtime-environment.md) — engine internals (wasmtime, fuel, epoch, ResourceLimiter)
- [`02-modules-events-packaging.md`](docs/02-modules-events-packaging.md) — module ABI, events, packaging
- [`03-module-discovery.md`](docs/03-module-discovery.md) — static / ENS / on-chain registry
- [`04-state-store.md`](docs/04-state-store.md) — local + remote state
- [`05-sdk-design.md`](docs/05-sdk-design.md) — guest SDK
- [`06-production-hardening.md`](docs/06-production-hardening.md) — operational concerns
- [`07-rpc-namespace-design.md`](docs/07-rpc-namespace-design.md) — `chain` namespace
- [`08-platform-generalisation.md`](docs/08-platform-generalisation.md) — beyond CoW
- [`migration/0.1-to-0.2.md`](docs/migration/0.1-to-0.2.md) — upgrading from Nexum 0.1

## Contributing

Pull requests are welcome. Please open an issue first for substantial changes. CI runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` against the workspace.

## License

[AGPL-3.0](LICENSE) © Nullis Labs LLC and contributors.
