//! Acceptance surface for the venue SDK: a hand-written adapter
//! compiles against [`VenueAdapter`], exports through
//! `export_venue_adapter!`, and round-trips a versioned body through
//! `#[derive(IntentBody)]` - including the typed unknown-version
//! failure and the typed client core driving the adapter through the
//! [`VenueClient`] seam.

use borsh::{BorshDeserialize, BorshSerialize};
use nexum_venue_sdk::value_flow::{Asset, AssetAmount};
use nexum_venue_sdk::{
    AuthScheme, BodyError, ClientError, Config, Fault, IntentBody, IntentClient, IntentHeader,
    IntentStatus, Settlement, SubmitOutcome, VenueAdapter, VenueClient, VenueError,
};

/// First published body version: a fixed-price quote.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
struct QuoteV1 {
    amount_wei: u64,
    memo: String,
}

/// Second published version: v1 plus an expiry.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
struct QuoteV2 {
    amount_wei: u64,
    memo: String,
    valid_until_ms: Option<u64>,
}

/// The outer per-venue version enum: the schema the demo venue
/// publishes. Tag order is the schema; versions append.
#[derive(IntentBody, Clone, Debug, PartialEq, Eq)]
enum QuoteBody {
    V1(QuoteV1),
    V2(QuoteV2),
}

/// The hand-written adapter: enough venue to exercise every trait
/// function without a live transport.
struct DemoAdapter;

/// The receipt the demo venue issues for every accepted intent.
const RECEIPT: [u8; 4] = [0xA5, 0x5A, 0xC3, 0x3C];

impl DemoAdapter {
    fn decode(body: &[u8]) -> Result<(u64, Option<u64>), VenueError> {
        // `BodyError` converts through `?`: malformed and
        // unknown-version bodies surface as `invalid-body`.
        let body = QuoteBody::from_bytes(body)?;
        Ok(match body {
            QuoteBody::V1(quote) => (quote.amount_wei, None),
            QuoteBody::V2(quote) => (quote.amount_wei, quote.valid_until_ms),
        })
    }
}

impl VenueAdapter for DemoAdapter {
    fn init(_config: Config) -> Result<(), Fault> {
        Ok(())
    }

    fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
        let (amount_wei, _valid_until_ms) = Self::decode(&body)?;
        Ok(IntentHeader {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: amount_wei.to_be_bytes().to_vec(),
            },
            wants: AssetAmount {
                asset: Asset::Native,
                amount: Vec::new(),
            },
            settlement: Settlement { chain: 1 },
            authorisation: AuthScheme::Eip712,
        })
    }

    fn submit(body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        Self::decode(&body)?;
        Ok(SubmitOutcome::Accepted(RECEIPT.to_vec()))
    }

    fn status(receipt: Vec<u8>) -> Result<IntentStatus, VenueError> {
        if receipt == RECEIPT {
            Ok(IntentStatus::Open)
        } else {
            // A receipt this venue never issued can never succeed, so the
            // refusal is the non-retryable case.
            Err(VenueError::Denied(
                "receipt not issued by this venue".into(),
            ))
        }
    }

    fn cancel(receipt: Vec<u8>) -> Result<(), VenueError> {
        Self::status(receipt).map(|_| ())
    }
}

// The acceptance gate proper: the hand-written adapter exports as the
// venue-adapter world.
nexum_venue_sdk::export_venue_adapter!(DemoAdapter);

/// In-process client: routes the demo venue id straight into the adapter,
/// standing in for the host registry the keeper-side seam will bind.
struct InProcessClient;

impl VenueClient for InProcessClient {
    fn submit(&self, venue: &str, body: Vec<u8>) -> Result<SubmitOutcome, VenueError> {
        if venue != "demo" {
            return Err(VenueError::UnknownVenue);
        }
        DemoAdapter::submit(body)
    }

    fn status(&self, venue: &str, receipt: &[u8]) -> Result<IntentStatus, VenueError> {
        if venue != "demo" {
            return Err(VenueError::UnknownVenue);
        }
        DemoAdapter::status(receipt.to_vec())
    }

    fn cancel(&self, venue: &str, receipt: &[u8]) -> Result<(), VenueError> {
        if venue != "demo" {
            return Err(VenueError::UnknownVenue);
        }
        DemoAdapter::cancel(receipt.to_vec())
    }
}

fn v2_body() -> QuoteBody {
    QuoteBody::V2(QuoteV2 {
        amount_wei: 1_000_000,
        memo: "two coffees".to_owned(),
        valid_until_ms: Some(1_700_000_000_000),
    })
}

#[test]
fn versioned_body_round_trips_through_the_derive() {
    for body in [
        QuoteBody::V1(QuoteV1 {
            amount_wei: 42,
            memo: "one".to_owned(),
        }),
        v2_body(),
    ] {
        let bytes = body.to_bytes().expect("derived payloads encode");
        assert_eq!(QuoteBody::from_bytes(&bytes).unwrap(), body);
    }
}

#[test]
fn wire_tag_is_the_declaration_index() {
    let v1 = QuoteBody::V1(QuoteV1 {
        amount_wei: 1,
        memo: String::new(),
    })
    .to_bytes()
    .unwrap();
    let v2 = v2_body().to_bytes().unwrap();
    assert_eq!(v1[0], 0);
    assert_eq!(v2[0], 1);
}

#[test]
fn unknown_version_fails_typedly() {
    let mut bytes = v2_body().to_bytes().unwrap();
    bytes[0] = 9;
    assert_eq!(
        QuoteBody::from_bytes(&bytes),
        Err(BodyError::UnknownVersion { version: 9 })
    );
}

#[test]
fn empty_and_malformed_bodies_fail_typedly() {
    assert_eq!(QuoteBody::from_bytes(&[]), Err(BodyError::Empty));

    // A known tag with a truncated payload.
    let mut bytes = v2_body().to_bytes().unwrap();
    bytes.truncate(bytes.len() - 1);
    assert!(matches!(
        QuoteBody::from_bytes(&bytes),
        Err(BodyError::Malformed { version: 1, .. })
    ));

    // A known tag with trailing bytes: borsh requires full consumption.
    let mut bytes = v2_body().to_bytes().unwrap();
    bytes.push(0);
    assert!(matches!(
        QuoteBody::from_bytes(&bytes),
        Err(BodyError::Malformed { version: 1, .. })
    ));
}

#[test]
fn adapter_projects_the_header_from_a_versioned_body() {
    let bytes = v2_body().to_bytes().unwrap();
    let header = DemoAdapter::derive_header(bytes).unwrap();
    assert_eq!(header.gives.asset, Asset::Native);
    assert_eq!(header.gives.amount, 1_000_000u64.to_be_bytes().to_vec());
    assert_eq!(header.settlement, Settlement { chain: 1 });
    assert_eq!(header.authorisation, AuthScheme::Eip712);
}

#[test]
fn adapter_reports_an_unknown_version_as_invalid_body() {
    let mut bytes = v2_body().to_bytes().unwrap();
    bytes[0] = 7;
    let err = DemoAdapter::derive_header(bytes).unwrap_err();
    match err {
        VenueError::InvalidBody(detail) => assert!(detail.contains("unknown body version 7")),
        other => panic!("expected invalid-body, got {other:?}"),
    }
}

#[test]
fn typed_client_round_trips_through_the_client_seam() {
    let client = IntentClient::new(InProcessClient, "demo");

    let outcome = client.submit(&v2_body()).unwrap();
    let SubmitOutcome::Accepted(receipt) = outcome else {
        panic!("demo venue always accepts");
    };
    assert_eq!(receipt, RECEIPT.to_vec());

    assert_eq!(client.status(&receipt).unwrap(), IntentStatus::Open);
    client.cancel(&receipt).unwrap();

    assert!(matches!(
        client.status(&[0, 1]).unwrap_err(),
        ClientError::Venue(VenueError::Denied(_))
    ));
}

#[test]
fn unbound_venue_is_unknown_at_the_client() {
    let client = IntentClient::new(InProcessClient, "nowhere");
    assert!(matches!(
        client.submit(&v2_body()).unwrap_err(),
        ClientError::Venue(VenueError::UnknownVenue)
    ));
}
