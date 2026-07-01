//! # flaky-bomb (test fixture)
//!
//! Traps deterministically on the first N events and succeeds on
//! every subsequent event. Drives the supervisor's exponential-
//! backoff restart policy through its full lifecycle:
//!
//! 1. Dispatch 1: trap (failure_count = 1, next_attempt = +1s).
//! 2. (engine waits the backoff window)
//! 3. Dispatch 2 (eligible after 1s): trap again, failure_count = 2.
//! 4. ...
//! 5. Dispatch N+1: succeeds, failure_count resets to 0.
//!
//! N is config-supplied via `[config].fail_first_n`. The fixture
//! reads the value once during `init` into a `OnceLock` and keeps
//! a static `AtomicU32` counter across calls.
//!
//! Not a production module. Lives under `modules/fixtures/` so it is
//! obviously test-only.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../../wit/nexum-host",
    world: "nexum:host/event-module",
});

use std::sync::OnceLock;

use nexum::host::{local_store, logging, types};

/// Number of consecutive events to trap on. Set from `[config].fail_first_n`
/// at init; defaults to `1` (trap once, recover on second event).
static FAIL_FIRST_N: OnceLock<u32> = OnceLock::new();

const ATTEMPTS_KEY: &str = "attempts";

struct FlakyBomb;

impl Guest for FlakyBomb {
    fn init(config: Vec<(String, String)>) -> Result<(), HostError> {
        let n: u32 = config
            .iter()
            .find(|(k, _)| k == "fail_first_n")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(1);
        FAIL_FIRST_N.set(n).ok();
        logging::log(
            logging::Level::Info,
            &format!("flaky-bomb init: will trap on the first {n} event(s)"),
        );
        Ok(())
    }

    fn on_event(_event: types::Event) -> Result<(), HostError> {
        // Read + increment the attempt counter from local-store.
        // Survives wasm-side state resets (the supervisor's restart
        // path tears down the Store; local-store is host-side and
        // persistent within the supervisor's lifetime, exactly the
        // store keeps across reinstantiations).
        let prior = local_store::get(ATTEMPTS_KEY)?
            .and_then(|b| <[u8; 4]>::try_from(b.as_slice()).ok())
            .map(u32::from_le_bytes)
            .unwrap_or(0);
        let attempt = prior + 1;
        local_store::set(ATTEMPTS_KEY, &attempt.to_le_bytes())?;

        let n = FAIL_FIRST_N.get().copied().unwrap_or(1);
        if attempt <= n {
            logging::log(
                logging::Level::Warn,
                &format!("flaky-bomb attempt {attempt}/{n}: burning fuel to trigger OutOfFuel"),
            );
            // Burn fuel until wasmtime traps with `OutOfFuel`. The
            // supervisor catches the trap + schedules a backoff
            // restart. After the backoff window the supervisor
            // re-instantiates the component (fresh wasm Store), but
            // local-store survives so the attempt counter keeps
            // climbing across restarts.
            let mut x: u64 = 0;
            loop {
                x = x.wrapping_add(1);
                std::hint::black_box(x);
            }
        }
        logging::log(
            logging::Level::Info,
            &format!("flaky-bomb attempt {attempt}: ok, recovered"),
        );
        Ok(())
    }
}

export!(FlakyBomb);
