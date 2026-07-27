---
status: accepted
---

# Operator config (`engine.toml`) is separate from module manifest (`module.toml`)

## Context

The runtime carries two kinds of configuration with different owners, reviewers, and change cadences: what the operator decides at deployment time (chains, local-store location, which modules to boot) and what the module developer declares at build time (required and optional capabilities, HTTP allowlist, module config keys). A module's capability declaration is a property of the build, so it belongs in the published bundle, not in the operator's local file.

## Decision

Two files, two schemas, two loaders:

- **`engine.toml`** operator-owned, next to the engine binary or pointed to by `--engine-config`. Defines `[engine]` (`state_dir`, `log_level`), `[chains.<id>]` (`rpc_url`), and `[[modules]]` (path, manifest). Loaded by `engine_config::EngineConfig::load`.
- **`module.toml`** module-developer-owned, ships in the module bundle alongside its `.wasm` component. Defines `[module]`, `[capabilities]` (required, optional, http allowlist), `[config]`. Loaded by `manifest::load`.

The engine config carries each module's manifest path; the two files never collapse into one.

## Consequences

- A deployment needs both files. A missing `engine.toml` falls back to no chains and the default `state_dir` (`./data`); the example logging module still runs, chain-backed capabilities report `unsupported`.
- A `module.toml` without a `[capabilities]` block triggers the 0.1-compat deprecation warning in `manifest::fallback_manifest` (`nexum/crates/nexum-runtime/src/manifest/load.rs`) and treats every linked capability as required.
- Module-bundle redistribution carries `module.toml` with the artifact; engines ship no templates.
