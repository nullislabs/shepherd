//! CLI surface for the `nexum-engine` binary, derived via clap.
//!
//! The 0.2 binary accepts either a positional `<wasm-path> [<manifest-path>]`
//! shortcut that synthesises a one-module engine config, or a
//! `--engine-config <path>` flag that points at a TOML declaring
//! multiple modules. Production deployments use the second form; the
//! positional shortcut stays for parity with the M1 reference CLI and
//! for smoke tests.

use std::path::PathBuf;

use clap::Parser;

/// Parsed CLI surface.
///
/// `nexum-engine [<wasm-path> [<manifest-path>]] [--engine-config <path>] [--pretty-logs]`
///
/// Positional `<wasm-path>` is a backwards-compat shortcut that
/// synthesises a one-module engine config. Production deployments pass
/// `--engine-config` and declare modules in TOML.
///
/// `--pretty-logs` selects the human-readable tracing formatter (the
/// historical 0.1 default). Without the flag the engine emits JSON
/// log lines per the structured-logging contract: a single
/// `jq` / Loki / Grafana stream reconstructs the full timeline of
/// any dispatch, host call, or order submission.
#[derive(Parser, Debug, Default)]
#[command(
    name = "nexum-engine",
    about = "Run one or more Wasm Component modules under the Shepherd supervisor",
    long_about = None,
    version,
)]
pub struct Cli {
    /// Optional positional path to a Wasm Component file. Synthesises
    /// a one-module engine config when no `--engine-config` is given.
    pub wasm: Option<PathBuf>,

    /// Optional positional path to the module's `nexum.toml` manifest.
    /// Only consulted alongside the positional `wasm` shortcut.
    pub manifest: Option<PathBuf>,

    /// Optional explicit path to the engine-wide `engine.toml` config.
    /// When omitted, the engine resolves the default search path
    /// documented in `engine_config::load_or_default`.
    #[arg(long = "engine-config")]
    pub engine_config: Option<PathBuf>,

    /// Use the human-readable tracing formatter instead of the
    /// default JSON formatter (structured-logging contract).
    #[arg(long = "pretty-logs")]
    pub pretty_logs: bool,
}
