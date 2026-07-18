//! # composable-cow
//!
//! ComposableCoW keeper machinery, kept out of the CoW venue: the
//! conditional-order body ([`ComposableBody`]) and the structured poll
//! seam ([`Verdict`]), with the deployed 1.x reverting wire quarantined
//! behind [`LegacyRevertAdapter`]. The `sweep` slice adds the shared
//! poll-loop composition ([`run`]) over the typed CoW venue client.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

pub mod body;
pub mod poll;
#[cfg(feature = "sweep")]
pub mod sweep;

pub use body::ComposableBody;
pub use poll::{IConditionalOrder, LegacyRevertAdapter, Verdict};
#[cfg(feature = "sweep")]
pub use sweep::run;
