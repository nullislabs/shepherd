//! # panic-bomb (test fixture)
//!
//! Installs the nexum-sdk tracing facade (subscriber + panic hook) in
//! `init` and panics on every `on_event`. The hook forwards the panic
//! to stderr and the host logging call before the trap reaches the
//! supervisor, so one death leaves Stderr, HostInterface, and Panic
//! records. Test-only.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: [
        "../../../../wit/nexum-host",
    ],
    world: "nexum:host/event-module",
    generate_all,
});

use nexum::host::{logging, types};

/// Routes facade lines to the bound host logging import.
struct HostLogSink;

impl nexum_sdk::tracing::LogSink for HostLogSink {
    fn log(&self, level: nexum_sdk::Level, message: &str) {
        use nexum_sdk::Level;
        // `Level` is a set of associated consts, so compare rather than
        // match; the five tiers are total, hence the final `Trace` arm.
        let level = if level == Level::ERROR {
            logging::Level::Error
        } else if level == Level::WARN {
            logging::Level::Warn
        } else if level == Level::INFO {
            logging::Level::Info
        } else if level == Level::DEBUG {
            logging::Level::Debug
        } else {
            logging::Level::Trace
        };
        logging::log(level, message);
    }
}

struct PanicBomb;

impl Guest for PanicBomb {
    fn init(_config: Vec<(String, String)>) -> Result<(), Fault> {
        nexum_sdk::tracing::init(HostLogSink);
        tracing::info!("panic-bomb init (will panic)");
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), Fault> {
        panic!("panic-bomb detonated");
    }
}

export!(PanicBomb);
