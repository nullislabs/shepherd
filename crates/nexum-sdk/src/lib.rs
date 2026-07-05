//! # nexum-sdk
//!
//! Guest-side SDK for nexum runtime modules. The helpers here are
//! host-neutral and domain-free: any module targeting the runtime can
//! use them regardless of which world it exports. Domain layers such as
//! the CoW SDK depend on this crate and re-export it, so module authors
//! keep a single import surface.
//!
//! ## What lives here
//!
//! - [`http`] - outbound HTTP over wasi:http in the standard `http`
//!   crate's request/response vocabulary: a synchronous [`fetch`]
//!   helper (guest target only), the [`Fetch`] trait seam for host-free
//!   strategy tests, and a [`FetchError`] that distinguishes allowlist
//!   denials from transport failures.
//!
//! [`fetch`]: http::Fetch::fetch
//! [`Fetch`]: http::Fetch
//! [`FetchError`]: http::FetchError

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod http;
