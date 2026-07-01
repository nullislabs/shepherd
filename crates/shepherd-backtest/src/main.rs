//! # shepherd-backtest
//!
//! Offline replay harness for Shepherd modules. Loads a fixtures
//! JSON produced by `tools/backtest-collect/backtest_collect.py`,
//! drives each on-chain event through the production strategy code
//! via `shepherd_sdk_test::MockHost`, classifies the result, and
//! emits a Markdown report at
//! `docs/operations/backtest-reports/backtest-7d-YYYY-MM-DD.md`.
//!
//! ## Scope
//!
//! v1 covers the EthFlow lane end-to-end. The TWAP lane requires
//! per-part eth_call walking against an archive RPC which the
//! current public-tier endpoints refuse (see the
//! `tools/baseline-latency` finding). TWAP fixtures are
//! still loaded and counted in the report so the gap is visible,
//! but the replay is gated on a paid endpoint (Phase 2B).

#![cfg_attr(not(test), warn(unused_crate_dependencies))]

use std::path::PathBuf;

use clap::Parser;

mod fixtures;
mod replay;
mod report;

use fixtures::Fixtures;
use replay::{Classification, replay_ethflow};

#[derive(Parser, Debug)]
#[command(
    name = "shepherd-backtest",
    about = "Replay collected Sepolia events through production strategies"
)]
struct Args {
    /// Fixtures JSON produced by `tools/backtest-collect/backtest_collect.py`.
    #[arg(long)]
    fixtures: PathBuf,

    /// Markdown report output. The default path follows the
    /// `backtest-{window}d-{date}.md` convention the
    /// `docs/operations/backtest-reports/` directory expects.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Acceptance threshold for the report's sign-off line. The
    /// acceptance criterion is ≥ 95% of replayed events
    /// land in `Submitted` or `RejectedExpected`; the threshold is
    /// surfaced as a CLI flag so a soak-team override is possible
    /// without re-editing the binary.
    #[arg(long, default_value_t = 0.95)]
    accept_threshold: f64,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    eprintln!(
        "=== shepherd-backtest - loading {} ===",
        args.fixtures.display()
    );
    let raw = std::fs::read_to_string(&args.fixtures)?;
    let fx: Fixtures = serde_json::from_str(&raw)?;
    eprintln!(
        "  chain: {} (id={})  window: {}d  blocks {}..{}",
        fx.metadata.chain_name,
        fx.metadata.chain_id,
        fx.metadata.window_days,
        fx.metadata.from_block,
        fx.metadata.to_block,
    );
    eprintln!("  ethflow fixtures: {}", fx.ethflow_orders.len());
    eprintln!("  twap fixtures: {}", fx.twap_conditionals.len());

    // ---- replay EthFlow ----
    let mut outcomes = Vec::with_capacity(fx.ethflow_orders.len());
    for (idx, order) in fx.ethflow_orders.iter().enumerate() {
        let outcome = replay_ethflow(order, fx.metadata.chain_id);
        if idx < 3 || idx == fx.ethflow_orders.len() - 1 {
            eprintln!(
                "  [{}/{}] {} {}",
                idx + 1,
                fx.ethflow_orders.len(),
                outcome.class.label(),
                outcome.uid,
            );
        }
        outcomes.push(outcome);
    }

    let report_md = report::render(&fx, &outcomes, args.accept_threshold);
    let out_path = args.out.unwrap_or_else(|| {
        let date = fx
            .metadata
            .collected_at
            .split('T')
            .next()
            .unwrap_or("unknown");
        PathBuf::from(format!(
            "docs/operations/backtest-reports/backtest-{}d-{}.md",
            fx.metadata.window_days, date
        ))
    });
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&out_path, &report_md)?;
    eprintln!("\nreport written: {}", out_path.display());

    // ---- summary + exit code ----
    let total = outcomes.len();
    let accepted = outcomes
        .iter()
        .filter(|o| {
            matches!(
                o.class,
                Classification::Submitted | Classification::RejectedExpected(_)
            )
        })
        .count();
    let ratio = if total == 0 {
        0.0
    } else {
        accepted as f64 / total as f64
    };
    eprintln!(
        "summary: {}/{} ({:.1}%) Accepted+RejectedExpected (threshold {:.1}%)",
        accepted,
        total,
        ratio * 100.0,
        args.accept_threshold * 100.0,
    );
    if total > 0 && ratio < args.accept_threshold {
        eprintln!("FAIL: below threshold");
        std::process::exit(1);
    }
    Ok(())
}
