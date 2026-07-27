# Module Discovery

Doc 02 defines how modules are packaged (bundle = `module.toml` + `module.wasm`) and how content is fetched by hash. This document defines how the runtime **discovers which modules to load**.

## 0.2: static (local path)

The 0.2 engine loads modules from local filesystem paths listed in `engine.toml`. Each `[[modules]]` entry names the compiled component and, optionally, its manifest (defaulting to a sibling `module.toml`):

```toml
[[modules]]
path = "/var/nexum/twap-monitor/twap_monitor.wasm"
manifest = "/var/nexum/twap-monitor/module.toml"
```

This is the whole of discovery in 0.2. Content-addressed resolution (Swarm / IPFS / OCI) and `[[content.sources]]` are not wired: `EngineConfig::modules` resolves a `(component.wasm, module.toml)` pair on disk, nothing more (see `nexum/crates/nexum-runtime/src/engine_config.rs`).

## 0.3 direction: ENS and on-chain registry

The remainder of this document is design contract for a future minor release, not shipped behaviour. It is retained because the WIT and manifest shapes are stable enough to build against.

### ENS name resolution

A module author publishes a bundle to Swarm (or IPFS) and points an ENS name at it. The runtime would resolve the name to a content reference, fetch the bundle, and load it.

- **`contenthash`** (ENSIP-7 / EIP-1577): binary field encoding a protocol code plus content hash (Swarm `0xe4`, IPFS `0xe3`). The primary pointer to the bundle.
- **Text records** (ENSIP-5 / EIP-634): lightweight metadata (version, chains, name) readable without fetching the bundle.

Resolution flow: resolve ENS name -> resolver `contenthash(namehash)` -> decode protocol + hash -> fetch bundle from the content store -> verify `sha256(module.wasm)` against the manifest -> load via the standard lifecycle (doc 02). On a `contenthash` change the runtime re-fetches and hot-reloads.

### On-chain registry

For autonomous discovery the runtime would watch a contract for registration events and enter the ENS flow. Three shapes are viable:

- **Dedicated registry contract** emitting `ModuleRegistered` / `ModuleRemoved`.
- **ENS self-declaration**: a contract sets a `shepherd.module` text record on its own ENS name pointing at the module; the runtime watches `TextChanged` filtered to that key.
- **Wildcard subdomain registry** (ENSIP-10): `*.modules.shepherd.eth` resolved by a registry contract that anyone can register a subdomain against.

### Layered trust

Discovery is permissionless; execution requires operator consent. A `[discovery]` config would gate what auto-loads (`mode = "allowlist"` with `allowed_ens_names` / `allowed_registries`, versus `mode = "auto"` for public nodes) and apply resource caps to discovered modules.

All methods converge on the same flow: resolve a content reference -> fetch via content store -> verify hash -> load.
