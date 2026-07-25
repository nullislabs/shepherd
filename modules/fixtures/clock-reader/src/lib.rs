//! # clock-reader (test fixture)
//!
//! Logs `SystemTime::now()` as whole seconds since the epoch. Under
//! `wasm32-wasip2` the read routes to `wasi:clocks/wall-clock`, which
//! the supervisor virtualizes per store, so a test under a pinned clock
//! override can assert the guest observed the overridden time.
//! Test-only.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

use std::time::{SystemTime, UNIX_EPOCH};

wit_bindgen::generate!({
    path: [
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
