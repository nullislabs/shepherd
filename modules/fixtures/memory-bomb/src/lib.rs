//! # memory-bomb (test fixture)
//!
//! Deliberately allocates past the default 64 MiB per-module memory
//! cap on every `on_event`. The wasmtime `StoreLimits` reject the
//! linear-memory grow, the host traps the module, the supervisor
//! marks it dead, and other modules keep dispatching.
//!
//! Not a production module. Lives under `modules/fixtures/` so it is
//! obviously test-only.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../wit/nexum-host",
    world: "nexum:host/event-module",
});

use nexum::host::{logging, types};

struct MemoryBomb;

impl Guest for MemoryBomb {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(
            logging::Level::Info,
            "memory-bomb init (will exhaust memory)",
        );
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), HostError> {
        // The default per-module cap is 64 MiB (see
        // `crates/nexum-engine/src/runtime/limits.rs::DEFAULT_MEMORY_LIMIT`).
        // Asking for 128 MiB forces a wasmtime `memory.grow` trap.
        // `black_box` keeps the allocation live so the optimiser
        // cannot eliminate the request.
        let size = 128 * 1024 * 1024;
        let mut buf: Vec<u8> = Vec::with_capacity(size);
        buf.resize(size, 0xab);
        std::hint::black_box(&buf);
        Ok(())
    }
}

export!(MemoryBomb);
