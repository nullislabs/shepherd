//! # composable-cow
//!
//! ComposableCoW keeper machinery, kept out of the CoW venue: the
//! conditional-order body ([`ComposableBody`]) and the structured poll
//! seam ([`Verdict`]), with the deployed 1.x reverting wire quarantined
//! behind [`LegacyRevertAdapter`]. The `run` slice adds the shared
//! poll-loop composition (`run`) over the typed CoW venue client.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

pub mod body;
pub mod fork;
pub mod poll;
#[cfg(feature = "run")]
pub mod run;

pub use body::ComposableBody;
pub use fork::{Mapped, PollResult, Suppressed, classify_revert, map_verdict};
pub use poll::{IConditionalOrder, LegacyRevertAdapter, NextPoll, Verdict};
#[cfg(feature = "run")]
pub use run::run;
