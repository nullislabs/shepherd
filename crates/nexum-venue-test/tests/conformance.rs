//! Acceptance surface for the conformance kit: an adapter written
//! against `nexum-venue-sdk` is held to the published vector and
//! golden files, and a deliberately divergent adapter is caught by
//! them.

use nexum_venue_sdk::value_flow::{Asset, AssetAmount};
use nexum_venue_sdk::{
    AuthScheme, Config, Fault, IntentHeader, IntentStatus, SubmitOutcome, VenueAdapter, VenueError,
};
use nexum_venue_test::reference::{
    CODEC_VECTORS_JSON, HEADER_GOLDENS_JSON, ReferenceBody, derive_reference_header,
};
use nexum_venue_test::{CodecVectors, HeaderGoldens, MessagingHost, MockTransport};

/// An adapter under test: the reference venue implemented through the
/// SDK trait, transport injected through the seams so the kit's mocks
/// drive it.
struct ReferenceAdapter;

impl VenueAdapter for ReferenceAdapter {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        derive_reference_header(body)
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        Ok(SubmitOutcome::Accepted(body))
    }

    fn status(_receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        Ok(IntentStatus::Open)
    }

    fn cancel(_receipt: Vec<u8>) -> Result<(), VenueError> {
        Ok(())
    }
}

#[test]
fn adapter_codec_conforms_to_the_published_vectors() {
    CodecVectors::from_json(CODEC_VECTORS_JSON)
        .expect("the published vector file parses")
        .assert_conforms::<ReferenceBody>();
}

#[test]
fn adapter_derive_header_conforms_to_the_published_goldens() {
    HeaderGoldens::from_json(HEADER_GOLDENS_JSON)
        .expect("the published golden file parses")
        .assert_conforms(ReferenceAdapter::derive_header);
}

#[test]
fn divergent_derivation_is_caught_by_the_published_goldens() {
    // The classic byte-order bug: little-endian amounts.
    let derive = |body: Vec<u8>| -> Result<IntentHeader, VenueError> {
        let mut header = derive_reference_header(body)?;
        header.gives.amount.reverse();
        Ok(header)
    };
    let report = HeaderGoldens::from_json(HEADER_GOLDENS_JSON)
        .unwrap()
        .check(derive)
        .unwrap_err();
    assert!(!report.violations.is_empty());
    assert!(report.violations[0].detail.contains("diverges"));
}

#[test]
fn mock_transport_drives_seam_shaped_adapter_logic() {
    // A slice of adapter logic written against the seams: announce a
    // submission over messaging, confirm via the venue's HTTP API.
    fn announce<M: MessagingHost>(messaging: &M, receipt: &[u8]) -> Result<(), VenueError> {
        messaging
            .publish("/reference/1/receipts/proto", receipt)
            .map_err(VenueError::from)
    }

    let transport = MockTransport::new();
    transport
        .messaging
        .scope_topics(["/reference/1/receipts/proto"]);

    let SubmitOutcome::Accepted(receipt) = ReferenceAdapter::submit(vec![1, 2, 3]).unwrap() else {
        panic!("the reference venue accepts directly");
    };
    announce(&transport, &receipt).unwrap();
    assert_eq!(
        transport.messaging.last_published().unwrap().payload,
        receipt,
    );

    // An off-scope topic surfaces as the typed policy refusal.
    let denied = transport
        .messaging
        .publish("/elsewhere", &receipt)
        .map_err(VenueError::from)
        .unwrap_err();
    assert!(matches!(denied, VenueError::Denied(_)));
}

#[test]
fn published_files_document_the_wire_format_in_hex() {
    // Non-Rust authors consume the files directly: every byte field is
    // lowercase hex, and the first round-trip vector carries prose.
    let vectors = CodecVectors::from_json(CODEC_VECTORS_JSON).unwrap();
    assert!(vectors.vectors.iter().any(|vector| vector.notes.is_some()));

    let goldens = HeaderGoldens::from_json(HEADER_GOLDENS_JSON).unwrap();
    let golden = &goldens.goldens[0];
    // The golden's body is a codec vector's bytes: the two files pin
    // the same wire form from both sides.
    assert!(
        vectors
            .vectors
            .iter()
            .any(|vector| vector.bytes == golden.body),
        "header goldens reuse published codec bodies",
    );
    // And the expected header speaks the value-flow vocabulary.
    let derived = derive_reference_header(golden.body.clone()).unwrap();
    assert_eq!(derived.gives.asset, Asset::Native);
    assert_eq!(derived.authorisation, AuthScheme::Eip712);
    let _: &AssetAmount = &derived.gives;
}
