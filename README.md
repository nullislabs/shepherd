# Shepherd

[![CI](https://github.com/nullislabs/shepherd/actions/workflows/ci.yml/badge.svg)](https://github.com/nullislabs/shepherd/actions/workflows/ci.yml) [![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

Shepherd is a CoW Protocol extension of the [Nexum Runtime](https://github.com/nullislabs): on-chain automation that runs as sandboxed WebAssembly.

The Nexum Runtime executes untrusted automation as WASM components against the `nexum:host` WIT contract. A module receives only the host capabilities it declares in its manifest: no ambient filesystem or network. Execution is metered by fuel and epoch, memory-capped, and transactional per event: state commits on success and rolls back on trap. Modules are content-addressed and verified by hash. There is no central service; you run the node.

Shepherd registers the `videre:venue` platform and bundles the `cow-venue` adapter, so a keeper module submits CoW Protocol orders through the venue registry rather than through engine-baked logic. A keeper watching `ComposableCoW` or `EthFlow` is an ordinary module.

A module built against `nexum:host` runs on any Nexum-compatible host. CoW order submission additionally needs a host that registers the venue platform, which the `shepherd` binary does.

> Pre-release, under active development. Testnets and lab environments only.

## Layout

| Path | Purpose |
| --- | --- |
| `nexum/crates/nexum-runtime/` | The engine: a wasmtime host implementing the `nexum:host` contract. |
| `nexum/crates/nexum-launch/` | Launcher library: shared CLI, config load, tracing, preset launch. |
| `nexum/crates/nexum-cli/` | The bare `nexum` binary: the core lattice, no extension payload. |
| `shepherd/crates/shepherd/` | The `shepherd` binary: the cow composition root registering the videre venue platform and the Prometheus add-on. |
| `nexum/crates/nexum-sdk/` | Guest SDK: host trait seam, bind macro, chain/config/address helpers, `wasi:http` fetch, tracing facade. |
| `videre/crates/videre-sdk/` | Venue-platform SDK: the `videre:venue` client and adapter contracts. |
| `shepherd/crates/cow-venue/` | The bundled CoW venue adapter component. |
| `wit/nexum-host/` | The `nexum:host` WIT package: the host/guest contract. |
| `wit/videre-venue/` | The `videre:venue` WIT package: the venue-adapter contract. |
| `wit/shepherd-cow/` | `cow-events.wit`: the CoW event ABIs of record. |
| `modules/` | Guest modules: TWAP and EthFlow keepers, examples, and test fixtures. |
| `docs/` | Architecture and design notes. Start with [`docs/00-overview.md`](docs/00-overview.md). |

An engine is a concrete implementation that runs WASM components (`nexum`, a wasmtime daemon). The `nexum:host` WIT package is the contract. Modules built against it run on any compliant engine.

## Build from source

Shepherd uses [Nix](https://nixos.org/) flakes to pin the toolchain and [just](https://github.com/casey/just) as the task runner.

```sh
nix develop        # dev shell (Rust, wasm-tools, just)
just build         # build the engine and the example module
just run           # run the engine against the example module
just test          # unit tests
```

Without Nix you need Rust (edition 2024), the `wasm32-wasip2` target, and `wasm-tools`.

## Running

Development, a single module against a synthetic event:

```sh
nexum <component.wasm> [<module.toml>]
```

Production, an `engine.toml` declaring chains, state directory, modules, and adapters:

```sh
shepherd --engine-config engine.toml
```

See [`docs/deployment.md`](docs/deployment.md) for the `engine.toml` reference and [`docs/production.md`](docs/production.md) for the production deploy.

## Contributing

Open an issue before non-trivial PRs. Conventional Commits. CI runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, and per-module `wasm32-wasip2` builds.

## Security

Capability sandboxing, key handling, and order signing are security-critical. Report vulnerabilities privately rather than in public issues.

## License

AGPL-3.0-or-later © Nullis Labs LLC and contributors. See [LICENSE](LICENSE).
