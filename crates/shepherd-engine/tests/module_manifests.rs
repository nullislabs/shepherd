//! Chain-log durability pins for shepherd's shipped keeper manifests.
//!
//! Both keepers build their whole state from logs, so an event trigger
//! that opens at head loses history no later block can restate. Two keys
//! prevent that, and neither is visible in any behavioural test: `resume`
//! carries the cursor across a restart, and `start_block` seeds the very
//! first boot from the contract's deployment block.
//!
//! Dropping either one still parses, still boots, and still passes every
//! other test in this repo. It just silently stops seeing orders.

use std::path::{Path, PathBuf};

use nexum_runtime::toml;

/// Path under the workspace root; this crate sits at `crates/<pkg>`.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<pkg> sits two levels under the workspace root")
        .join(relative)
}

/// Every `[[trigger]]` table of kind `event` in one manifest.
fn event_triggers(manifest: &str) -> Vec<toml::Table> {
    let path = workspace_path(manifest);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let parsed: toml::Table = raw
        .parse()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    parsed
        .get("trigger")
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("{manifest} declares no [[trigger]] tables"))
        .iter()
        .filter_map(toml::Value::as_table)
        .filter(|t| t.get("on").and_then(toml::Value::as_str) == Some("event"))
        .cloned()
        .collect()
}

/// An event trigger without `resume` re-opens at head, so every log mined
/// while the daemon was down is lost permanently.
#[test]
fn every_event_trigger_resumes_from_a_durable_cursor() {
    for manifest in [
        "modules/twap-monitor/component.toml",
        "modules/ethflow-watcher/component.toml",
    ] {
        let triggers = event_triggers(manifest);
        assert!(!triggers.is_empty(), "{manifest} declares no event trigger");
        for trigger in triggers {
            assert_eq!(
                trigger.get("resume").and_then(toml::Value::as_bool),
                Some(true),
                "{manifest}: an event trigger without `resume` loses every log \
                 mined during downtime: {trigger:?}",
            );
        }
    }
}

/// `resume` alone still leaves a FIRST boot at head. A ComposableCoW
/// conditional order can stay live for weeks, so a daemon that starts at
/// head never learns of an order registered before it first ran and never
/// polls it. `start_block` is the contract's deployment block, and it
/// seeds only while no cursor is stored.
#[test]
fn every_event_trigger_backfills_from_its_contract_deployment_block() {
    // Deployment blocks resolved from each contract's deploy transaction.
    // The ComposableCoW fork has its own address and deployment block,
    // distinct from upstream. EthFlow has had several per-network
    // deployments, so its block belongs to the specific `address` below.
    const EXPECTED: &[(&str, &str, u64)] = &[
        (
            "modules/twap-monitor/component.toml",
            "0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232",
            25_674_440,
        ),
        (
            "modules/ethflow-watcher/component.toml",
            "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC",
            7_541_028,
        ),
    ];

    for (manifest, address, block) in EXPECTED {
        let triggers = event_triggers(manifest);
        assert!(!triggers.is_empty(), "{manifest} declares no event trigger");
        for trigger in triggers {
            assert_eq!(
                trigger.get("address").and_then(toml::Value::as_str),
                Some(*address),
                "{manifest}: the pinned deployment block belongs to this \
                 address; a changed address needs a re-derived block",
            );
            assert_eq!(
                trigger.get("start_block").and_then(toml::Value::as_integer),
                Some(i64::try_from(*block).expect("a deployment block fits an i64")),
                "{manifest}: without the deployment block a first boot starts \
                 at head and never sees an order created before it ran: \
                 {trigger:?}",
            );
        }
    }
}

/// `start_block` is refused without `resume`, because the seed would then
/// re-apply on every open and rescan the whole range after each restart.
/// The pairing is what makes it a one-time backfill.
#[test]
fn no_trigger_seeds_a_start_block_without_a_durable_cursor() {
    for manifest in [
        "modules/twap-monitor/component.toml",
        "modules/ethflow-watcher/component.toml",
    ] {
        for trigger in event_triggers(manifest) {
            if trigger.contains_key("start_block") {
                assert_eq!(
                    trigger.get("resume").and_then(toml::Value::as_bool),
                    Some(true),
                    "{manifest}: `start_block` without `resume` rescans from \
                     the seed on every restart: {trigger:?}",
                );
            }
        }
    }
}
