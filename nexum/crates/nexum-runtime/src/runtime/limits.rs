//! Re-exports for the configurable per-module wasmtime fuel + memory
//! limits. The canonical source is [`crate::engine_config::ModuleLimits`].
//!
//! Fuel meters only guest instructions; host-call time is unmetered, so a
//! per-dispatch wall-clock deadline in [`crate::supervisor`] is the backstop.
//!
