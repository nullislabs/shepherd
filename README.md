# Shepherd

[![CI](https://github.com/nullislabs/shepherd/actions/workflows/ci.yml/badge.svg)](https://github.com/nullislabs/shepherd/actions/workflows/ci.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

**Shepherd is a CoW Protocol-extended [Nexum Runtime](https://github.com/nullislabs): on-chain automation that runs as sandboxed WebAssembly, not scripts.**

The Nexum Runtime executes untrusted automation as WASM components against the `nexum:host` WIT contract. Every module receives exactly the host capabilities it declares in its manifest and nothing more - no ambient filesystem, network, clock, or entropy. Execution is metered by fuel and epoch, memory-capped, and transactional per event: state commits on success and rolls back on trap. Modules are distributed content-addressed and verified by hash. There is no central service to depend on; you run the node.

Shepherd extends that runtime with `shepherd:cow` - CoW Protocol order APIs and submission - so a TWAP, EthFlow, or ComposableCoW watch-tower is an ordinary module, not a special case baked into the engine. Write the strategy once as a component; the runtime supervises, restarts, meters, and sandboxes it.

A module built against the universal `nexum:host` world runs on any Nexum-compatible host. A module built against `shepherd:cow` additionally gains CoW Protocol access and requires a Shepherd host.

> **Pre-release** and under active development. Testnets and lab environments only.

Looking for the org? See **[github.com/nullislabs](https://github.com/nullislabs)**.

---

## Why

- **WASM Component Model, not a plugin API** - a WIT-typed host/guest contract with structural isolation and multi-language guests (Rust today; anything that compiles to a component next).
- **Capability-scoped by construction** - a module sees only the host primitives it declares. No ambient authority: no filesystem, network, clock, or randomness unless granted.
- **Metered and transactional** - per-event fuel and epoch limits, a memory cap, and all-or-nothing state. A runaway module cannot starve its neighbours or corrupt its store.
- **Declarative subscriptions** - modules declare their block, log, and cron events in a manifest; the runtime wires and multiplexes the sources.
- **Content-addressed distribution** - modules are fetched by hash (Swarm, IPFS, OCI, HTTPS) and integrity-checked before they load.
- **Self-hosted** - one binary, your keys, your RPC. No centralised dependency.

---

## Layout

| Path | Purpose |
| --- | --- |
| `crates/nexum-runtime/` | The **engine** - the Nexum Runtime's reference host: a wasmtime implementation of the `nexum:host` contract. |
| `crates/shepherd-sdk/` | Guest SDK - typed helpers over the host contract plus the CoW client. |
| `wit/nexum-host/` | The **`nexum:host`** WIT package - the host/guest contract every engine implements and every module imports. |
| `wit/shepherd-cow/` | The `shepherd:cow` WIT package - CoW Protocol extensions on top of `nexum:host`. |
| `modules/` | Guest modules - TWAP and EthFlow watch-towers, examples, and test fixtures. |
| `docs/` | Architecture and design notes. Start with [`docs/00-overview.md`](docs/00-overview.md). |

> **Engine vs. host.** An *engine* is a concrete implementation that runs WASM components (today `nexum-engine`, a wasmtime daemon). The `nexum:host` WIT package is the *contract* - the host imports a guest sees. Other engines (mobile, browser) can implement the same contract, and modules built against it run on any compliant engine.

---

## Build from source

Shepherd uses [Nix](https://nixos.org/) flakes to pin the toolchain and [just](https://github.com/casey/just) as the task runner.

```sh
nix develop        # enter the dev shell (Rust, wasm-tools, just, ...)
just build         # build the engine and the example module
just run           # run the engine against the example module
just test          # unit tests
```

Without Nix you need Rust (edition 2024), the `wasm32-wasip2` target, and `wasm-tools`.

---

## Running

Single module (development):

```sh
nexum-engine <component.wasm> [<module.toml>]
```

Multi-module (production) - `engine.toml` declares RPC endpoints, the state directory, and a `[[modules]]` list:

```sh
nexum-engine --engine-config engine.toml
```

A module's own `module.toml` declares its capabilities and event subscriptions:

```toml
[module]
name    = "twap-monitor"
version = "0.1.0"

[capabilities]
required = ["chain", "local-store", "cow-api"]
optional = ["http"]

[[subscription]]
kind     = "log"
chain_id = 1
address  = "0xfdaFc9d1902f4e0b84f65F49f244b32b31013b74"  # ComposableCoW
```

See [`docs/`](docs) for the full schema and the design corpus - start with [`docs/00-overview.md`](docs/00-overview.md).

---

## Contributing

Open an issue before non-trivial PRs - this is a pre-release codebase under active churn. Conventional Commits. CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and per-module `wasm32-wasip2` builds.

## Security

Capability sandboxing, key handling, and order signing are security-critical. Please report vulnerabilities privately rather than in public issues.

## License

AGPL-3.0-or-later © Nullis Labs LLC and contributors. See [LICENSE](LICENSE).

```
●  AGPL-3.0  ·  pre-release  ·  Nexum Runtime
```
