//! Published-fixture conformance: the codec vectors and header goldens
//! under `tests/vectors/` replayed through the shipped body codec and
//! the adapter's own derivation. The files are the contract a non-Rust
//! adapter author reads.

use cow_venue::{CowAdapter, CowIntentBody};
use videre_sdk::VenueAdapter;
use videre_test::{CodecVectors, HeaderGoldens};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors")
        .join(name)
}

#[test]
fn codec_conforms_to_the_published_vectors() {
    let vectors = CodecVectors::load(fixture("cow-intent-body.json")).expect("vectors parse");
    vectors.assert_conforms::<CowIntentBody>();
}

#[test]
fn derive_header_conforms_to_the_published_goldens() {
    // The goldens pin mainnet; init drives the same configured path
    // the host boots the component through.
    CowAdapter::init(vec![("chain".to_owned(), "1".to_owned())]).expect("config parses");
    let goldens = HeaderGoldens::load(fixture("cow-header-goldens.json")).expect("goldens parse");
    goldens.assert_conforms(CowAdapter::derive_header);
}
