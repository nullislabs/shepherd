//! # composable-cow
//!
//! ComposableCoW keeper machinery, kept out of the CoW venue: the
//! structured poll seam ([`Verdict`]) and the fork wire that produces
//! it. The `run` slice adds the poll-loop composition (`run`) over the
//! typed CoW venue client, driven by the `due` index so a tick reads
//! the commitments that are due rather than every one held.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

#[cfg(feature = "run")]
pub mod due;
pub mod fork;
pub mod poll;
#[cfg(feature = "run")]
pub mod run;

pub use fork::{Mapped, PollResult, Suppressed, classify_revert, map_verdict, to_verdict};
pub use poll::{NextPoll, ParkReason, Verdict};
#[cfg(feature = "run")]
pub use run::run;
