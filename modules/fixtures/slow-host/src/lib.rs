//! # slow-host (test fixture)
//!
//! On every event issues a single `chain::request` host call and returns
//! `Ok`. The handler does no guest-side work of note; the point is the
//! host call itself.
//!
//! Fuel meters only guest wasm instructions and epoch interruption fires
//! only at wasm instruction boundaries, so neither can see, let alone
//! bound, time the guest spends suspended inside a host call. This fixture
//! makes that gap observable: the integration test wires the `chain`
//! capability to a mock provider that parks the first `request` far past a
//! short `event_deadline_secs` override. The guest suspends inside the host
//! call, the per-dispatch wall-clock deadline fires, and the supervisor
//! must drop the suspended call, mark the module dead, and reinstantiate it
//! on a fresh store. On the next dispatch the mock answers promptly, so the
//! same guest recovers and returns `Ok`.
//!
//! The result of the call is deliberately ignored: whether the request
//! resolves, errors, or is cut off, the handler returns `Ok(())`, so the
//! only thing that can end a dispatch early is the deadline under test.
//!
//! Not a production module. Lives under `modules/fixtures/` so it is
//! obviously test-only and never gets loaded by the testnet configs.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: [
        "../../../wit/nexum-host",
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
