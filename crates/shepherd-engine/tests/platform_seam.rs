//! Cow-on-the-seam E2E coverage: shepherd's keeper modules boot over the
//! generic runtime seam against the natively registered cow venue, and a
//! polled status transition reaches a keeper without trapping.
//!
//! These tests reach into shepherd's own L3 assets (the `ethflow-watcher`
//! and `twap-monitor` components, and the cow venue this binary links) and
//! drive them through `shepherd_engine::venues` and the runtime's boot
//! path. They live here, in the repo that OWNS those assets, rather than in
//! videre-host (L2, the generic venue host) which must not know about the
//! cow venue.
//!
//! The venue is no longer a wasm artifact, so there is nothing left to
//! assert about its component imports: the world-contract test the file
//! used to carry went with `[[adapters]]`.
//!
//! Build the keepers with `just build-modules`; a missing artifact fails
//! unless `NEXUM_ALLOW_MISSING_WASM` is set.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexum_runtime::bindings::nexum::host::types::{ExtensionTrigger, Trigger};
use nexum_runtime::config::EngineConfig;
use nexum_runtime::extension::{Extension, ExtensionDelivery};
use nexum_runtime::test_utils::{
    BootScenario, Booted, Entry, MockTypes, mock_components, module_wasm_or_skip,
};
use nexum_runtime::toml;
use videre_host::{IntentStatusUpdate, Videre};
use videre_status_body::{INTENT_STATUS_KIND, IntentStatus, StatusBody};

/// The `[extensions.videre]` table the tests register the cow venue from,
/// matching the mainnet chain the keepers pin.
const VENUE_CONFIG: &str = r#"
[venues.cow]
chain = 1
"#;

/// Path under the workspace root. Shepherd IS the L3 root, so assets sit
/// directly beneath it. This crate sits at `crates/<pkg>`, so the root is
/// exactly two levels up; an ancestor walk would answer the enclosing
/// checkout instead when the worktree is nested inside one.
fn workspace_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<pkg> sits two levels under the workspace root")
        .join(relative)
}

/// The venue platform with the cow venue registered, through the same
/// composition-root path the binary uses.
fn videre_platform() -> Arc<Videre> {
    let videre = videre_host::platform();
    let mut config = EngineConfig::default();
    config.extensions.insert(
        "videre".to_owned(),
        toml::from_str::<toml::Value>(VENUE_CONFIG).expect("the venue table parses"),
    );
    shepherd_engine::venues::register(videre.registry(), &config).expect("register the cow venue");
    Arc::new(videre)
}

/// Boot one keeper against the registered cow venue, or `None` when its
/// wasm is absent and the skip opt-out is set.
async fn boot(keeper: &str, manifest: &str) -> Option<Booted<MockTypes>> {
    let wasm = module_wasm_or_skip(keeper)?;
    let videre = videre_platform() as Arc<dyn Extension<MockTypes>>;
    let booted = BootScenario::<MockTypes>::over(mock_components())
        .extensions([videre])
        .module(Entry::new(workspace_path(manifest)).id(keeper).wasm(wasm))
        .boot()
        .await
        .expect("boot");
    Some(booted)
}

/// Wrap a polled transition as the delivery the platform's status source
/// emits.
fn status_delivery(update: &IntentStatusUpdate) -> ExtensionDelivery {
    ExtensionDelivery {
        extension_kind: INTENT_STATUS_KIND,
        attrs: vec![("venue", update.venue.clone())],
        trigger: Trigger::Extension(ExtensionTrigger {
            extension_kind: INTENT_STATUS_KIND.to_owned(),
            payload: update.encode().expect("encode intent-status envelope"),
        }),
    }
}

/// ethflow-watcher boots with its shipped manifest, which means the
/// body-version handshake admitted it against the registered cow venue,
/// and it handles a delivered cow status transition without trapping.
#[tokio::test]
async fn e2e_ethflow_watcher_boots_and_handles_intent_status() {
    let Some(mut booted) = boot("ethflow-watcher", "modules/ethflow-watcher/component.toml").await
    else {
        return;
    };
    assert_eq!(booted.supervisor.alive_count(), 1);

    let update = IntentStatusUpdate {
        venue: "cow".to_owned(),
        receipt: vec![0xAB; 56],
        status: StatusBody {
            status: IntentStatus::Open,
            proof: None,
            reason: None,
        }
        .encode()
        .expect("encode"),
    };
    assert_eq!(
        booted
            .supervisor
            .dispatch_extension_trigger(status_delivery(&update))
            .await,
        1,
    );
    assert_eq!(booted.supervisor.alive_count(), 1);
}

/// The shepherd bundle pair: twap-monitor boots against the registered cow
/// venue (the body-version handshake admits the pair) and a mainnet block
/// dispatch reaches it and keeps it alive.
#[tokio::test]
async fn e2e_twap_monitor_boots_against_the_cow_venue() {
    let Some(mut booted) = boot("twap-monitor", "modules/twap-monitor/component.toml").await else {
        return;
    };
    assert_eq!(booted.supervisor.alive_count(), 1, "twap-monitor is alive");

    // twap-monitor triggers on mainnet blocks (poll path); with no
    // commitments indexed the run is empty and the keeper stays alive.
    assert_eq!(booted.dispatch_block_on(1).await, 1);
    assert_eq!(booted.supervisor.alive_count(), 1);
}
