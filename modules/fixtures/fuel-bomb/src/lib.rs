//! # fuel-bomb (test fixture)
//!
//! Deliberately exhausts the wasmtime fuel budget on every `on_event`
//! by running an unbounded counter loop. The wasmtime engine must
//! trap with `OutOfFuel`; the supervisor must catch the trap, mark
//! the module dead, and continue dispatching to other modules.
//!
//! Not a production module. Lives under `modules/fixtures/` so it is
//! obviously test-only and never gets loaded by the M2 / M3 testnet
//! configs.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../wit/nexum-host",
    world: "nexum:host/event-module",
});

use nexum::host::{logging, types};

struct FuelBomb;

impl Guest for FuelBomb {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "fuel-bomb init (will exhaust fuel)");
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), HostError> {
        // Unbounded loop. `std::hint::black_box` prevents the
        // optimiser from constant-folding this away, so the loop
        // genuinely burns wasmtime fuel one branch + add at a time.
        // 1 billion default fuel / ~10 fuel-per-iteration -> trap
        // within ~100M iterations, well under a second of wall
        // clock on real hardware.
        let mut x: u64 = 0;
        loop {
            x = x.wrapping_add(1);
            std::hint::black_box(x);
        }
    }
}

export!(FuelBomb);
