//! Published-fixture conformance: the codec vectors and header goldens
//! under `tests/vectors/` replayed through the shipped body codec and
//! the venue's own derivation. The files are the contract a non-Rust
//! venue author reads.

use alloy_primitives::{Address, U256};
use cow_venue::{
    BuyToken, Chain, CowAdapter, CowConfig, CowIntent, CowIntentBody, OrderBody, OrderbookHttp,
    SellToken, SignedOrder,
};
use videre_sdk::{IntentBody as _, IntentHeader, VenueError};
use videre_test::{CodecVectors, Expectation, HeaderGoldens};

/// The goldens pin mainnet, so the venue derives against that chain.
fn mainnet() -> CowAdapter<OrderbookHttp> {
    CowAdapter::new(CowConfig::new(Chain::Mainnet)).expect("the http client builds")
}

/// Derivation is pure, so the goldens replay it without a transport call.
fn derive_header(body: Vec<u8>) -> Result<IntentHeader, VenueError> {
    mainnet().derive_header(&body)
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors")
        .join(name)
}

/// The one order every published fixture carries.
fn order_body() -> OrderBody {
    OrderBody::sell(
        SellToken(Address::repeat_byte(0x11)),
        U256::from(42u64),
        BuyToken(Address::repeat_byte(0x22)),
        U256::from(41u64),
        1_700_000_000,
    )
    .app_data([0x44; 32])
    .build()
}

fn signed_order() -> SignedOrder {
    SignedOrder {
        order: order_body(),
        owner: Address::repeat_byte(0x55),
        signature: vec![0xC0, 0xFF, 0xEE],
    }
}

fn encoded(intent: CowIntent) -> Vec<u8> {
    CowIntentBody::V1(intent).to_bytes().expect("body encodes")
}

/// Rebuild the published codec vectors from the shipped codec.
fn build_codec_vectors() -> CodecVectors {
    let mut vectors = CodecVectors::new("cow-venue/cow-intent-body");
    vectors
        .push_round_trip(
            "v1-order",
            &CowIntentBody::V1(CowIntent::Order(order_body())),
        )
        .expect("order body encodes");
    vectors
        .push_round_trip(
            "v1-signed",
            &CowIntentBody::V1(CowIntent::Signed(signed_order())),
        )
        .expect("signed order encodes");
    let mut unknown = encoded(CowIntent::Order(order_body()));
    unknown[0] = 9;
    vectors.push_failure(
        "unknown-version",
        unknown,
        Expectation::UnknownVersion { version: 9 },
    );
    vectors.push_failure("empty", Vec::new(), Expectation::Empty);
    let mut truncated = encoded(CowIntent::Order(order_body()));
    truncated.truncate(truncated.len() - 1);
    vectors.push_failure(
        "truncated-payload",
        truncated,
        Expectation::Malformed { version: 0 },
    );
    let mut trailing = encoded(CowIntent::Order(order_body()));
    trailing.push(0);
    vectors.push_failure(
        "trailing-bytes",
        trailing,
        Expectation::Malformed { version: 0 },
    );
    vectors
}

/// Rebuild the published header goldens through the venue's own
/// mainnet-configured derivation.
fn build_header_goldens() -> HeaderGoldens {
    let mut goldens = HeaderGoldens::new("cow");
    goldens
        .record(
            "v1-order-presign",
            encoded(CowIntent::Order(order_body())),
            derive_header,
        )
        .expect("header derives")
        .notes = Some("unsigned order: authorised by host-held keys (pre-sign)".to_owned());
    goldens
        .record(
            "v1-signed",
            encoded(CowIntent::Signed(signed_order())),
            derive_header,
        )
        .expect("header derives")
        .notes = Some("owner-signed order: EIP-1271".to_owned());
    goldens
}

#[test]
fn codec_conforms_to_the_published_vectors() {
    let vectors = CodecVectors::load(fixture("cow-intent-body.json")).expect("vectors parse");
    vectors.assert_conforms::<CowIntentBody>();
}

#[test]
fn derive_header_conforms_to_the_published_goldens() {
    let goldens = HeaderGoldens::load(fixture("cow-header-goldens.json")).expect("goldens parse");
    goldens.assert_conforms(derive_header);
}

#[test]
fn published_vectors_match_regeneration() {
    assert_eq!(
        CodecVectors::load(fixture("cow-intent-body.json"))
            .expect("vectors parse")
            .to_json(),
        build_codec_vectors().to_json(),
        "cow-intent-body.json has drifted; run the ignored \
         regenerate_published_fixtures test and commit the result",
    );
}

#[test]
fn published_goldens_match_regeneration() {
    assert_eq!(
        HeaderGoldens::load(fixture("cow-header-goldens.json"))
            .expect("goldens parse")
            .to_json(),
        build_header_goldens().to_json(),
        "cow-header-goldens.json has drifted; run the ignored \
         regenerate_published_fixtures test and commit the result",
    );
}

/// Rewrite the published files after a deliberate wire change, then
/// commit the diff.
#[test]
#[ignore = "writes the published fixture files in place"]
fn regenerate_published_fixtures() {
    build_codec_vectors()
        .write(fixture("cow-intent-body.json"))
        .unwrap();
    build_header_goldens()
        .write(fixture("cow-header-goldens.json"))
        .unwrap();
}
