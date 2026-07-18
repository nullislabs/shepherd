//! Guest bindings for the videre SDK, generated once from an
//! import-only inline world carrying every interface both personas
//! speak: the videre type vocabulary, the host types and scoped
//! transport, and the keeper-facing `videre:venue/client` shims.
//!
//! Unlike event modules, which run `wit_bindgen::generate!` per cdylib,
//! the SDK generates these bindings once: the
//! [`VenueAdapter`](crate::VenueAdapter) trait, the typed transport
//! wrappers, and the client core are all expressed over them. The
//! per-cdylib bindgens (`#[videre_sdk::venue]`, `#[videre_sdk::keeper]`)
//! remap the shared interfaces onto these modules with `with`, so a
//! macro-built component shares type identity (and, for the keeper's
//! client, one shim set) with the SDK and the conformance kit.
//!
//! The world is import-only on purpose: an embedded world section is
//! unioned into every linking module at componentization, where an
//! unused import prunes but an export is a hard obligation. Keeping the
//! adapter export out of the SDK world is what lets a keeper module
//! link this crate without being asked to be a venue; the export face
//! is emitted per-cdylib by `#[videre_sdk::venue]` alone.

wit_bindgen::generate!({
    inline: "package videre:sdk-shims;

world sdk-imports {
    import videre:types/types@0.1.0;
    import videre:value-flow/types@0.1.0;
    import nexum:host/types@0.1.0;
    import nexum:host/chain@0.1.0;
    import nexum:host/messaging@0.1.0;
    import videre:venue/client@0.1.0;
}
",
    path: [
        "../../wit/videre-value-flow",
        "../../wit/videre-types",
        "../../wit/nexum-host",
        "../../wit/videre-venue",
    ],
    world: "videre:sdk-shims/sdk-imports",
    generate_all,
    additional_derives: [PartialEq],
});
