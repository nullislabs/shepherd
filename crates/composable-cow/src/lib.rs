//! # composable-cow
//!
//! ComposableCoW keeper machinery, kept out of the CoW venue: the
//! conditional-order body ([`ComposableBody`]), the structured poll
//! seam ([`Verdict`]), and the fork wire that produces it. The `run`
//! slice adds the shared poll-loop composition (`run`) over the typed
//! CoW venue client.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

pub mod body;
pub mod fork;
pub mod poll;
#[cfg(feature = "run")]
pub mod run;

pub use body::ComposableBody;
pub use fork::{Mapped, PollResult, Suppressed, classify_revert, map_verdict, to_verdict};
pub use poll::{NextPoll, Verdict};
#[cfg(feature = "run")]
pub use run::run;
