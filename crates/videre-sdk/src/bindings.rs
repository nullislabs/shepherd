//! Guest bindings for the `videre:venue/venue-adapter` world.
//!
//! Unlike event modules, which run `wit_bindgen::generate!` per cdylib,
//! the venue SDK generates the adapter world's bindings once, here: the
//! [`VenueAdapter`](crate::VenueAdapter) trait, the typed transport
//! wrappers, and the intent client core are all expressed over these
//! types. The `#[videre_sdk::venue]` attribute's per-cdylib bindgen
//! remaps `videre:types/types`, `videre:value-flow/types`, and
//! `nexum:host/types` onto these modules with `with`, so a macro-built
//! adapter shares type identity with the SDK and the conformance kit.

wit_bindgen::generate!({
    path: [
        "../../wit/videre-value-flow",
        "../../wit/videre-types",
        "../../wit/nexum-host",
        "../../wit/videre-venue",
    ],
    world: "videre:venue/venue-adapter",
    generate_all,
    additional_derives: [PartialEq],
});
