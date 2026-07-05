//! # http-probe (example Shepherd module)
//!
//! On every matching block, fetches an allowlisted URL over wasi:http
//! and logs the response status, then fetches an off-list URL and
//! verifies the host denies it before any connection is made.
//! Demonstrates the guest-side HTTP patterns of a Shepherd module:
//!
//! - `nexum_sdk::http::fetch` (wasi:http via the SDK helper)
//! - the `[capabilities.http].allow` allowlist and its denial path
//! - `[config]` driven behaviour parsed once in `init`
//!
//! ## Module layout
//!
//! - `strategy.rs` holds the pure logic and tests against the SDK's
//!   `http::Fetch` + `host::LoggingHost` seams. It does not know
//!   `wit-bindgen` exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter, the `Guest` impl.
//!
//! ## Settings
//!
//! ```toml
//! [config]
//! # URL fetched on every matching block; host must be allowlisted.
//! probe_url = "https://api.cow.fi/mainnet/api/v1/version"
//! # URL whose host is deliberately off-list; the module expects the
//! # denied error and treats any other outcome as a failure.
//! denied_url = "https://example.com/"
//! # Optional throttle: probe every N blocks. Default 1.
//! every_n_blocks = "1"
//! ```

// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../../wit/nexum-host", "../../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

mod strategy;

use std::sync::OnceLock;

use nexum::host::{logging, types};

// `WitBindgenHost`, `convert_err`, `sdk_err_into_wit`, `convert_level`
// are generated below. Single source of truth in `shepherd-sdk`.
shepherd_sdk::bind_host_via_wit_bindgen!();

static SETTINGS: OnceLock<strategy::Settings> = OnceLock::new();

struct HttpProbe;

impl Guest for HttpProbe {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        let cfg = strategy::parse_config(&config).map_err(sdk_err_into_wit)?;
        logging::log(
            logging::Level::Info,
            &format!(
                "http-probe init: probe_url={} denied_url={} every_n_blocks={}",
                cfg.probe_url, cfg.denied_url, cfg.every_n_blocks,
            ),
        );
        let _ = SETTINGS.set(cfg);
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        let Some(cfg) = SETTINGS.get() else {
            return Ok(());
        };
        if let types::Event::Block(block) = event {
            strategy::on_block(
                &nexum_sdk::http::WasiFetch,
                &WitBindgenHost,
                cfg,
                block.number,
            )
            .map_err(sdk_err_into_wit)?;
        }
        Ok(())
    }
}

export!(HttpProbe);
