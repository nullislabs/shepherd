//! Re-exports the per-module fuel and memory limits; canonical source
//! [`crate::engine_config::ModuleLimits`].
//!
//! Fuel meters only guest instructions; host-call time is unmetered, so the
//! per-dispatch wall-clock deadline in [`crate::supervisor`] is the backstop.
