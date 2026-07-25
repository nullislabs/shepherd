//! Conformance kit for venue adapters: file-published codec vectors,
//! header-derivation goldens, and an in-memory transport mock.
//!
//! - [`CodecVectors`]: the venue's `IntentBody` wire bytes as JSON
//!   (lowercase hex); [`CodecVectors::assert_conforms`] checks a derived
//!   enum against them.
//! - [`HeaderGoldens`]: published bodies paired with the header a
//!   conforming `derive-header` projects, in the [`GoldenHeader`] mirror
//!   types.
//! - [`MockTransport`]: the chain, messaging, and outbound-HTTP
//!   transports as programmable in-memory mocks behind the SDK seams.
//!
//! ```rust
//! use videre_test::reference::{
//!     CODEC_VECTORS_JSON, HEADER_GOLDENS_JSON, ReferenceBody, derive_reference_header,
//! };
//! use videre_test::{CodecVectors, HeaderGoldens};
//!
//! let vectors = CodecVectors::from_json(CODEC_VECTORS_JSON).unwrap();
//! vectors.assert_conforms::<ReferenceBody>();
//! let goldens = HeaderGoldens::from_json(HEADER_GOLDENS_JSON).unwrap();
//! goldens.assert_conforms(derive_reference_header);
//! ```

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
