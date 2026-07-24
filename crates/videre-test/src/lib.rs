//! # videre-test
//!
//! Conformance kit for venue adapters: file-published codec vectors,
//! header-derivation goldens, and an in-memory transport mock, so an
//! adapter proves its wire behaviour against fixtures any
//! implementation language can read.
//!
//! ## The three pieces
//!
//! - [`CodecVectors`] - the venue's `IntentBody` wire bytes as a JSON
//!   file (bytes as lowercase hex). A Rust adapter checks its derived
//!   enum with [`CodecVectors::assert_conforms`]; a non-Rust author
//!   reads the same file and proves byte-exactness without linking
//!   Rust.
//! - [`HeaderGoldens`] - published bodies paired with the header a
//!   conforming `derive-header` projects from them, spelled in the
//!   [`GoldenHeader`] mirror types.
//! - [`MockTransport`] - the three transports an adapter is granted
//!   (chain, messaging, outbound HTTP) as programmable in-memory mocks
//!   behind the SDK's own seams.
//!
//! ## Usage
//!
//! Add as a dev-dep on the adapter crate:
//!
//! ```toml
//! [dev-dependencies]
//! videre-test = { path = "../../crates/videre-test" }
//! ```
//!
//! Hold the adapter to its published fixtures:
//!
//! ```rust
//! use videre_test::reference::{
//!     CODEC_VECTORS_JSON, HEADER_GOLDENS_JSON, ReferenceBody, derive_reference_header,
//! };
//! use videre_test::{CodecVectors, HeaderGoldens};
//!
//! // In a real adapter test these load the venue's own published
//! // files; the kit's reference venue stands in here.
//! let vectors = CodecVectors::from_json(CODEC_VECTORS_JSON).unwrap();
//! vectors.assert_conforms::<ReferenceBody>();
//!
//! let goldens = HeaderGoldens::from_json(HEADER_GOLDENS_JSON).unwrap();
//! goldens.assert_conforms(derive_reference_header);
//! ```
//!
//! Publishing works through the same types: build the fixtures with
//! [`CodecVectors::push_round_trip`] / [`HeaderGoldens::record`] and
//! [`write`](CodecVectors::write) them next to the venue's schema
//! documentation.
//!
//! ## Macro-built adapters
//!
//! `#[videre_sdk::venue]` adapters speak the SDK's own types (the
//! macro remaps the type interfaces onto `videre_sdk::bindings`), so
//! both checks apply directly: pass `MyAdapter::derive_header` to
//! [`HeaderGoldens::assert_conforms`] and the derived enum to
//! [`CodecVectors::assert_conforms`]. No bridge types.

#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(missing_docs)]

pub mod codec;
pub mod fixture;
pub mod header;
pub mod reconcile;
pub mod reference;
pub mod report;
pub mod transport;

pub use codec::{CodecVector, CodecVectors, Expectation};
pub use fixture::{FixtureError, FormatVersion};
pub use header::{
    GoldenAsset, GoldenAssetAmount, GoldenAuthScheme, GoldenHeader, GoldenSettlement, HeaderGolden,
    HeaderGoldens,
};
pub use reconcile::ReconcileFixture;
pub use report::{ConformanceReport, Violation};
pub use transport::{
    ChainCall, Message, MessagingHost, MockChain, MockFetch, MockMessaging, MockTransport,
    PublishRecord, RecordedRequest,
};
