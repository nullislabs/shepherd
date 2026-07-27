//! # slow-host (test fixture)
//!
//! Issues one `chain::request` per event and returns `Ok` regardless of
//! its result. Fuel and epoch interruption only meter wasm instructions,
//! not time suspended inside a host call, so the test parks the first
//! `request` past a short `event_deadline_secs`: the wall-clock deadline
//! must fire, the supervisor drop the suspended call, mark the module
//! dead, and reinstantiate it. The next dispatch answers promptly and
//! recovers. Test-only.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: [
        "../../../../wit/nexum-host",
    ],
    world: "nexum:host/event-module",
    generate_all,
});

use nexum::host::{chain, logging, types};

struct SlowHost;

impl Guest for SlowHost {
    fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
        // Minimal SDK-free fixture: no tracing subscriber is installed,
        // so log through the raw host binding directly.
        logging::log(logging::Level::Info, "slow-host init");
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), Fault> {
        // A single read-only RPC. The test's mock provider decides how long
        // it takes to answer; the guest just awaits it. `eth_blockNumber`
        // with empty params is the cheapest well-formed request in the
        // permitted read surface.
        let _ = chain::request(1, "eth_blockNumber", "[]");
        logging::log(logging::Level::Info, "slow-host on_event returned");
        Ok(())
    }
}

export!(SlowHost);
