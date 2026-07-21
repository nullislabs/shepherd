//! Shared CLI surface for engine binaries, derived via clap.

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};

/// Parsed CLI surface.
///
/// `<bin> [<wasm-path> [<manifest-path>]] [--engine-config <path>] [--pretty-logs]`
///
/// Positional `<wasm-path>` synthesises a one-module engine config.
/// Production deployments pass `--engine-config` and declare modules in
/// TOML.
///
/// `--pretty-logs` selects the human-readable tracing formatter; without
/// it the engine emits JSON log lines per the structured-logging contract.
#[derive(Parser, Debug, Default)]
#[command(
    about = "Run one or more Wasm Component modules under the engine supervisor",
    long_about = None,
    version,
)]
pub struct Cli {
    /// Optional positional path to a Wasm Component file. Synthesises
    /// a one-module engine config when no `--engine-config` is given.
    pub wasm: Option<PathBuf>,

    /// Optional positional path to the module's `module.toml` manifest.
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

    /// Override the chain-log poller's per-block `eth_getLogs`
    /// concurrency during backfill. Higher catches up faster at more
    /// node load. Overrides `[engine] log_backfill_concurrency` when
    /// set.
    #[arg(long = "log-backfill-concurrency")]
    pub log_backfill_concurrency: Option<usize>,
}

impl Cli {
    /// Parse the process arguments under the binary's `name`, exiting on
    /// `--help`/`--version` or a usage error.
    #[must_use]
    pub fn parse_as(name: &'static str) -> Self {
        let matches = Self::command().name(name).get_matches();
        Self::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flags land on the parsed surface under a caller-supplied name.
    #[test]
    fn flags_parse_under_a_supplied_name() {
        let matches = Cli::command()
            .name("nexum")
            .try_get_matches_from([
                "nexum",
                "--engine-config",
                "engine.toml",
                "--pretty-logs",
                "--log-backfill-concurrency",
                "8",
            ])
            .expect("valid arguments parse");
        let cli = Cli::from_arg_matches(&matches).expect("matches destructure");
        assert_eq!(cli.engine_config, Some(PathBuf::from("engine.toml")));
        assert!(cli.pretty_logs);
        assert_eq!(cli.log_backfill_concurrency, Some(8));
        assert!(cli.wasm.is_none());
    }
}
