//! # clock-reader (test fixture)
//!
//! On every event reads `std::time::SystemTime::now()` and logs the wall
//! time as whole seconds since the Unix epoch. Under `wasm32-wasip2` that
//! read routes to `wasi:clocks/wall-clock`, which the supervisor
//! virtualizes per store, so a test that boots this fixture under a pinned
//! clock override can assert from the log line that the guest observed the
//! overridden time rather than the ambient host clock.
//!
//! Not a production module. Lives under `modules/fixtures/` so it is
//! obviously test-only and never gets loaded by the testnet configs.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use std::time::{SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    path: [
        "../../../wit/nexum-value-flow",
        "../../../wit/nexum-intent",
        "../../../wit/nexum-host",
    ],
    world: "nexum:host/event-module",
    generate_all,
});

use nexum::host::{logging, types};

struct ClockReader;

impl Guest for ClockReader {
    fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
        // Minimal SDK-free fixture: no tracing subscriber is installed,
        // so log through the raw host binding directly.
        logging::log(logging::Level::Info, "clock-reader init");
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), Fault> {
        // Whole seconds since the epoch is parseable and stable: the
        // override pins wall time to an exact instant, so the guest reads
        // that instant back rather than the ambient host clock.
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        logging::log(logging::Level::Info, &format!("clock wall {secs}"));
        Ok(())
    }
}

export!(ClockReader);
