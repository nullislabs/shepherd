//! Chain-log durability pins for shepherd's shipped keeper manifests.
//!
//! An event trigger that opens at head loses the logs it did not see. Two
//! keys prevent that, and neither is visible in any behavioural test:
//! `resume` carries the cursor across a restart, and `start_block` seeds
//! the very first boot from the contract's deployment block.
//!
//! Both keepers need `resume`. Only `ccow-monitor` needs `start_block`: it
//! builds its order set from logs, and a conditional order stays live for
//! weeks. `ethflow-watcher` only observes placements, so its first boot
//! starts at head.
//!
//! Dropping `resume`, or the `start_block` of `ccow-monitor`, still parses,
//! still boots, and still passes every other test in this repo. It just
//! silently stops seeing orders.

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
        "modules/ccow-monitor/component.toml",
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
/// polls it. `start_block` is the registry's deployment block, and it
/// seeds only while no cursor is stored. The registry is a fork deployment
/// with its own address and its own block, distinct from upstream.
#[test]
fn the_registry_triggers_backfill_from_the_registry_deployment_block() {
    const MANIFEST: &str = "modules/ccow-monitor/component.toml";
    const REGISTRY: &str = "0xf9ba6F64c9b41Df1cEe76A50e2039D3847064232";
    const DEPLOYED_AT: i64 = 25_674_440;

    let triggers = event_triggers(MANIFEST);
    assert!(!triggers.is_empty(), "{MANIFEST} declares no event trigger");
    for trigger in triggers {
        assert_eq!(
            trigger.get("address").and_then(toml::Value::as_str),
            Some(REGISTRY),
            "{MANIFEST}: the pinned deployment block belongs to this \
             address; a changed address needs a re-derived block",
        );
        assert_eq!(
            trigger.get("start_block").and_then(toml::Value::as_integer),
            Some(DEPLOYED_AT),
            "{MANIFEST}: without the deployment block a first boot starts \
             at head and never sees an order created before it ran: \
             {trigger:?}",
        );
    }
}

/// The watcher only observes: it journals a placement and asks the venue
/// to watch its status. A first boot that read the whole history of the
/// mainnet EthFlow contract would put every past order under watch, all of
/// them long settled or expired. So the trigger carries no `start_block`
/// and a first boot opens at head; `resume` covers every later restart.
#[test]
fn the_ethflow_trigger_opens_at_head_on_the_mainnet_production_contract() {
    const MANIFEST: &str = "modules/ethflow-watcher/component.toml";
    // `ETH_FLOW_PRODUCTION` in the pinned cowprotocol crate.
    const ETH_FLOW: &str = "0xbA3cB449bD2B4ADddBc894D8697F5170800EAdeC";

    let triggers = event_triggers(MANIFEST);
    assert_eq!(triggers.len(), 1, "{MANIFEST}: one event trigger");
    let trigger = &triggers[0];
    assert_eq!(
        trigger.get("chain_id").and_then(toml::Value::as_integer),
        Some(1),
        "{MANIFEST}: the shipped configs serve chain 1 only: {trigger:?}",
    );
    assert_eq!(
        trigger.get("address").and_then(toml::Value::as_str),
        Some(ETH_FLOW),
        "{MANIFEST}: {trigger:?}",
    );
    assert!(
        !trigger.contains_key("start_block"),
        "{MANIFEST}: a `start_block` makes the first boot read and watch \
         the contract's whole history: {trigger:?}",
    );
}

/// `start_block` is refused without `resume`, because the seed would then
/// re-apply on every open and rescan the whole range after each restart.
/// The pairing is what makes it a one-time backfill.
#[test]
fn no_trigger_seeds_a_start_block_without_a_durable_cursor() {
    for manifest in [
        "modules/ccow-monitor/component.toml",
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
