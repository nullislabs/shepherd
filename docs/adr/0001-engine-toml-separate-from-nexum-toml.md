---
status: proposed
implemented-in: nullislabs/shepherd#8, nullislabs/shepherd#9
---

# Operator config (`engine.toml`) is separate from module manifest (`module.toml`)

## Context

The engine needs two distinct kinds of configuration: what the **operator** decides at deployment time (which chains to connect to, where the local-store database lives, which modules to boot) and what the **module developer** declares at build time (required and optional capabilities, HTTP allowlist, module-specific config keys). These have different reviewers, different threat models, and change on different cadences.

The filenames need to signal who owns each file directly. An operator opening a config file should know without prior context whether the file is their concern or the module developer's. A name like `nexum.toml` requires the reader to know that "nexum" refers to the runtime that hosts the module, which is one indirection too many; `module.toml` reads as "the module's manifest" with no prior context.

## Decision

Two distinct files, distinct schemas, distinct loaders:

- **`engine.toml`** - operator-owned, lives next to the engine binary or pointed to by `--engine-config`. Defines `[engine]` (state_dir, log_level), `[chains.<id>]` (rpc_url), and `[[modules]]` (path, manifest). Loaded by `engine_config::EngineConfig::load`.
- **`module.toml`** - module-developer-owned, ships in the module's bundle alongside its `.wasm` component. Defines `[module]`, `[capabilities]` (required, optional, http allowlist), `[config]`. Loaded by `manifest::load`.

The engine config carries the path to each module's manifest; the two never collapse into one file. The names `engine.toml` and `module.toml` map directly onto the two distinct roles, so a reader reaching either file knows whose concerns it covers.

## Considered options

- **Single `shepherd.toml` with `[engine]`, `[chains]`, `[[modules]]` *and* nested `[modules.<n>.capabilities]` per module.** Rejected: conflates operator and developer concerns. A module's capability declaration is a property of the build, not the deployment - it belongs in the artifact, not in the operator's local file. Auditing a module's capabilities also becomes a per-deployment exercise instead of a property visible in the published bundle.
- **Keep the `nexum.toml` filename for the module manifest.** Rejected: the name does not signal who owns the file (engine vs module). `module.toml` reads as "the module's manifest" without prior context.
- **`module.toml` inside the engine config (module entries embed it inline).** Rejected for the same reason as the single-file proposal; also bloats `engine.toml`.
- **Drop `engine.toml` entirely; pass everything as CLI flags or env vars.** Rejected: per-chain RPC URLs and module lists are awkward as flags, and `RUST_LOG` already covers the only thing that env vars naturally express.

## Consequences

- A deployment needs both files. A missing `engine.toml` falls back to "no chains, default state_dir" - the example logging module still runs; cow-api / chain backends report `unsupported`.
- A missing `module.toml` triggers the 0.1-compat deprecation warning in `manifest::fallback_manifest()` (defined in `crates/nexum-engine/src/manifest.rs`) and treats every linked capability as required. This fallback is scheduled for removal in 0.3 per `docs/migration/0.1-to-0.2.md`.
- Module-bundle redistribution carries `module.toml` with the artifact; engines do not need to ship templates.
- Future content-addressed module distribution (0.3) embeds `module.toml` in the bundle hash; `engine.toml` references modules by content address rather than filesystem path. The split survives that migration unchanged.
- Implementation impact: `crates/nexum-engine/src/manifest.rs` and `engine_config.rs` need to update the filename lookup from `nexum.toml` to `module.toml`. The 0.1-compat fallback in `manifest::fallback_manifest()` should accept both names during the transition; after 0.3 only `module.toml` is recognised.

_Errata: `crates/nexum-engine` was renamed to `crates/nexum-runtime` + `crates/nexum-cli` in the 0.2 refactor._
