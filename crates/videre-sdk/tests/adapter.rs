//! Acceptance surface for the venue SDK: a hand-written adapter
//! compiles against [`VenueAdapter`] and round-trips a versioned body
//! through `#[derive(IntentBody)]` - including the typed
//! unknown-version failure and the typed [`VenueClient`] driving the
//! adapter through the [`VenueTransport`] seam. The world-export glue
//! is `#[videre_sdk::venue]`'s alone; echo-venue is its worked target.

use borsh::{BorshDeserialize, BorshSerialize};
use videre_sdk::value_flow::{Asset, AssetAmount};
use videre_sdk::{
    AuthScheme, BodyError, ClientError, Config, Fault, IntentBody, IntentHeader, IntentStatus,
    Quotation, Settlement, SubmitOutcome, Venue, VenueAdapter, VenueClient, VenueError, VenueFault,
    VenueId, VenueTransport,
};

/// Drive a client future on the test's synchronous boundary.
fn run<F: std::future::Future>(future: F) -> F::Output {
    match videre_sdk::client::poll_once(future) {
        std::task::Poll::Ready(output) => output,
        std::task::Poll::Pending => panic!("client futures complete in one poll"),
    }
}

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

    fn quote(body: Vec<u8>) -> Result<Quotation, VenueError> {
        let (amount_wei, valid_until_ms) = Self::decode(&body)?;
        let zero = AssetAmount {
            asset: Asset::Native,
            amount: Vec::new(),
        };
        Ok(Quotation {
            gives: AssetAmount {
                asset: Asset::Native,
                amount: amount_wei.to_be_bytes().to_vec(),
            },
            wants: zero.clone(),
            fee: zero,
            valid_until_ms: valid_until_ms.unwrap_or(u64::MAX),
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

/// The demo venue as a keeper types it.
struct DemoVenue;

impl Venue for DemoVenue {
    const ID: VenueId = VenueId::from_static("demo");
    type Body = QuoteBody;
}

/// A venue id no adapter answers for, over the same body schema.
struct NowhereVenue;

impl Venue for NowhereVenue {
    const ID: VenueId = VenueId::from_static("nowhere");
    type Body = QuoteBody;
}

/// In-process transport: routes the demo venue id straight into the
/// adapter, standing in for the host registry the keeper-side seam
/// binds.
struct InProcessClient;

impl videre_sdk::client::sealed::SealedTransport for InProcessClient {}

impl VenueTransport for InProcessClient {
    async fn quote(&self, venue: &VenueId, body: Vec<u8>) -> Result<Quotation, VenueFault> {
        if venue.as_str() != "demo" {
            return Err(VenueFault::UnknownVenue);
        }
        DemoAdapter::quote(body).map_err(Into::into)
    }

    async fn submit(&self, venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
        if venue.as_str() != "demo" {
            return Err(VenueFault::UnknownVenue);
        }
        DemoAdapter::submit(body).map_err(Into::into)
    }

    async fn status(&self, venue: &VenueId, receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
        if venue.as_str() != "demo" {
            return Err(VenueFault::UnknownVenue);
        }
        DemoAdapter::status(receipt.to_vec()).map_err(Into::into)
    }

    async fn cancel(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
        if venue.as_str() != "demo" {
            return Err(VenueFault::UnknownVenue);
        }
        DemoAdapter::cancel(receipt.to_vec()).map_err(Into::into)
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
fn typed_client_round_trips_through_the_transport_seam() {
    let client = VenueClient::<DemoVenue, _>::with_transport(InProcessClient);
    assert_eq!(client.venue(), DemoVenue::ID);

    let outcome = run(client.submit(&v2_body())).unwrap();
    let SubmitOutcome::Accepted(receipt) = outcome else {
        panic!("demo venue always accepts");
    };
    assert_eq!(receipt, RECEIPT.to_vec());

    assert_eq!(run(client.status(&receipt)).unwrap(), IntentStatus::Open);
    run(client.cancel(&receipt)).unwrap();

    assert!(matches!(
        run(client.status(&[0, 1])).unwrap_err(),
        ClientError::Venue(VenueFault::Denied(_))
    ));
}

#[test]
fn quote_typestate_prices_then_submits_the_quoted_body() {
    async fn drive(
        client: &VenueClient<DemoVenue, InProcessClient>,
    ) -> Result<SubmitOutcome, ClientError> {
        // The typestate chain under test: a quotation is the only path
        // from a priced body to its submission. Static dispatch end to
        // end: the transport is native AFIT, nothing boxes.
        client.quote(&v2_body()).await?.submit().await
    }

    let client = VenueClient::<DemoVenue, _>::with_transport(InProcessClient);

    let quoted = run(client.quote(&v2_body())).unwrap();
    assert_eq!(
        quoted.quotation().gives.amount,
        1_000_000u64.to_be_bytes().to_vec()
    );
    assert_eq!(quoted.quotation().valid_until_ms, 1_700_000_000_000);

    let outcome = run(drive(&client)).unwrap();
    assert!(matches!(outcome, SubmitOutcome::Accepted(r) if r == RECEIPT.to_vec()));
}

#[test]
fn empty_receipt_is_rejected_before_the_transport() {
    // The unbound venue would report unknown-venue, so invalid-body
    // proves the guard fires before the transport is consulted.
    let client = VenueClient::<NowhereVenue, _>::with_transport(InProcessClient);
    assert!(matches!(
        run(client.status(&[])).unwrap_err(),
        ClientError::Venue(VenueFault::InvalidBody(detail)) if detail == "empty receipt"
    ));
    assert!(matches!(
        run(client.cancel(&[])).unwrap_err(),
        ClientError::Venue(VenueFault::InvalidBody(detail)) if detail == "empty receipt"
    ));
}

#[test]
fn unbound_venue_is_unknown_at_the_client() {
    let client = VenueClient::<NowhereVenue, _>::with_transport(InProcessClient);
    assert!(matches!(
        run(client.submit(&v2_body())).unwrap_err(),
        ClientError::Venue(VenueFault::UnknownVenue)
    ));
}
