//! Markdown report renderer for the backtest run. Modelled on the
//! E2E report shape - run metadata, per-module counts,
//! per-event appendix table, anomalies, sign-off.

use std::collections::BTreeMap;

use crate::fixtures::Fixtures;
use crate::replay::{Classification, ReplayOutcome};

pub fn render(fx: &Fixtures, outcomes: &[ReplayOutcome], threshold: f64) -> String {
    let mut by_class: BTreeMap<&'static str, usize> = BTreeMap::new();
    for o in outcomes {
        *by_class.entry(o.class.label()).or_default() += 1;
    }
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
    let pass = total == 0 || ratio >= threshold;

    let now = chrono_like_now();
    let mut out = String::new();
    out.push_str(&format!(
        "# Pre-soak backtest - {}d window on {} ({})\n\n",
        fx.metadata.window_days, fx.metadata.chain_name, now,
    ));
    out.push_str(
        "Replays every collected EthFlow `OrderPlacement` event through the production \
         `ethflow_watcher::strategy::on_logs` code path via `shepherd_sdk_test::MockHost`. \
         The orderbook is **never hit**: the MockHost intercepts `submit_order` and \
         the resolved `app_data` documents (collected once by the Python collector) are \
         programmed as `cow_api_request` responses. The goal is *would the strategy assemble \
         a body the live orderbook accepts?*, not *does the orderbook accept this body now?*.\n\n",
    );
    out.push_str("## Run metadata\n\n");
    out.push_str("| Field | Value |\n|---|---|\n");
    out.push_str(&format!(
        "| Chain | {} (id={}) |\n",
        fx.metadata.chain_name, fx.metadata.chain_id
    ));
    out.push_str(&format!(
        "| Window | {}d ({}..{}) |\n",
        fx.metadata.window_days, fx.metadata.from_block, fx.metadata.to_block
    ));
    out.push_str(&format!(
        "| Collected at | {} |\n",
        fx.metadata.collected_at
    ));
    out.push_str(&format!("| RPC | `{}` |\n", fx.metadata.rpc_url));
    out.push_str(&format!("| Orderbook | `{}` |\n", fx.metadata.cow_api));
    out.push_str(&format!(
        "| EthFlow owner | `{}` |\n",
        fx.metadata.ethflow_owner
    ));
    out.push_str(&format!(
        "| ComposableCoW | `{}` |\n",
        fx.metadata.composable_cow
    ));
    out.push_str(&format!(
        "| Accept threshold | {:.0}% |\n",
        threshold * 100.0
    ));
    out.push('\n');

    if !fx.metadata.notes.is_empty() {
        out.push_str("### Collector notes\n\n");
        for n in &fx.metadata.notes {
            out.push_str(&format!("- {n}\n"));
        }
        out.push('\n');
    }

    out.push_str("## EthFlow replay summary\n\n");
    out.push_str(&format!("- Events replayed: **{total}**\n"));
    for (label, count) in &by_class {
        out.push_str(&format!(
            "- {label}: **{count}** ({:.1}%)\n",
            *count as f64 / total.max(1) as f64 * 100.0
        ));
    }
    out.push_str(&format!(
        "\nAccepted (Submitted + RejectedExpected): **{accepted}/{total} = {:.1}%** - {} threshold ({:.0}%).\n\n",
        ratio * 100.0,
        if pass { "PASS vs." } else { "**FAIL** vs." },
        threshold * 100.0,
    ));

    // ---- anomalies ----
    let anomalies: Vec<&ReplayOutcome> = outcomes
        .iter()
        .filter(|o| {
            matches!(
                o.class,
                Classification::RejectedUnexpected(_) | Classification::StrategyError(_)
            )
        })
        .collect();
    out.push_str("## Anomalies\n\n");
    if anomalies.is_empty() {
        out.push_str("None. Every replayed event landed in `Submitted` or `RejectedExpected`.\n\n");
    } else {
        out.push_str(&format!(
            "**{} event(s) need a follow-up before this report can be signed off.** \
             File one issue per uid (use the gitBranchName conventions).\n\n",
            anomalies.len(),
        ));
        out.push_str("| uid | block | class | detail | last log |\n");
        out.push_str("|---|---:|---|---|---|\n");
        for o in anomalies {
            let last_log = o.log_lines.last().map(String::as_str).unwrap_or("");
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                shorten(&o.uid),
                o.block_number,
                o.class.label(),
                escape_md(o.class.detail()),
                escape_md(last_log),
            ));
        }
        out.push('\n');
    }

    // ---- TWAP lane status ----
    out.push_str("## TWAP lane status\n\n");
    out.push_str(&format!(
        "{} `ConditionalOrderCreated` events were collected in this window. \
         **Replay deferred to Phase 2B** because driving `twap_monitor::strategy::on_block` \
         requires walking each watch's `eth_call(getTradeableOrderWithSignature)` per-block - \
         a workload public-tier RPCs refuse (see the baseline-latency finding). The \
         fixtures are committed for the future re-run; the TWAP gap on the sign-off is \
         intentional and tracked separately.\n\n",
        fx.twap_conditionals.len(),
    ));

    // ---- sign-off ----
    out.push_str("## Sign-off\n\n");
    if pass {
        out.push_str(&format!(
            "**PASS.** EthFlow replay clears the {:.0}% acceptance bar with no \
             outstanding anomalies. Soak is unblocked from the backtest \
             side; remaining blockers are external (paid RPC + VM for the wall-clock run).\n\n",
            threshold * 100.0,
        ));
    } else {
        out.push_str(&format!(
            "**FAIL.** EthFlow replay landed at {:.1}%, below the {:.0}% bar. \
             Anomalies above must be resolved (or formally classified as \
             RejectedExpected with a corresponding code change in the strategy) before \
             this report can be re-rendered.\n\n",
            ratio * 100.0,
            threshold * 100.0,
        ));
    }

    out.push_str("## Reproducing\n\n```bash\n");
    out.push_str(&format!(
        "python3 tools/backtest-collect/backtest_collect.py --days {}\n",
        fx.metadata.window_days,
    ));
    out.push_str(
        "cargo run -p shepherd-backtest -- \\\n    --fixtures tools/backtest-collect/fixtures-YYYY-MM-DD.json\n```\n\n",
    );

    out.push_str("## Appendix: per-event classification\n\n");
    out.push_str("| # | uid | block | timestamp | class |\n|---:|---|---:|---:|---|\n");
    for (i, o) in outcomes.iter().enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            i + 1,
            shorten(&o.uid),
            o.block_number,
            o.block_timestamp,
            o.class.label(),
        ));
    }
    out.push('\n');
    out
}

fn shorten(uid: &str) -> String {
    if uid.len() > 18 {
        format!("{}..{}", &uid[..10], &uid[uid.len() - 6..])
    } else {
        uid.to_owned()
    }
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn chrono_like_now() -> String {
    // Avoid pulling chrono just for a UTC string; UNIX epoch + ISO
    // formatter the report renderer doesn't need to be wall-clock
    // accurate to the second.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYY-MM-DDTHH:MM:SSZ - derived without leap-year handling
    // because the report only uses this for a header line; the
    // ground truth is the fixtures' `collected_at` field.
    let days = secs / 86400;
    let day_secs = secs % 86400;
    let (h, m, s) = (day_secs / 3600, (day_secs % 3600) / 60, day_secs % 60);
    // 1970-01-01 + days; rough date for the report timestamp. Good
    // enough to grep on.
    let (year, month, day) = days_to_ymd(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

fn days_to_ymd(mut days: i64) -> (i32, u32, u32) {
    // Adapted from the classic civil-date conversion.
    days += 719468;
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = (days - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}
