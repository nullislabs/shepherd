//! # composable-cow
//!
//! ComposableCoW keeper machinery, kept out of the CoW venue: the
//! conditional-order body ([`ComposableBody`]) and the structured poll
//! seam ([`Verdict`]), with the deployed 1.x reverting wire quarantined
//! behind [`LegacyRevertAdapter`].

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

pub mod body;
pub mod poll;

pub use body::ComposableBody;
pub use poll::{IConditionalOrder, LegacyRevertAdapter, Verdict};
