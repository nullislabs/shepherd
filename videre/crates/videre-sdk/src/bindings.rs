//! Guest bindings for the videre SDK, generated once from an import-only
//! inline world carrying every interface both personas speak: the videre
//! types, the host types and scoped transport, and the keeper-facing
//! `videre:venue/client` shims. The [`VenueAdapter`](crate::VenueAdapter)
//! trait, transport wrappers, and client core are all expressed over
//! them; the per-cdylib bindgens remap the shared interfaces onto these
//! modules with `with`, so a macro-built component shares type identity.
//!
//! Import-only on purpose: keeping the adapter export out of the SDK
//! world lets a keeper module link this crate without becoming a venue,
//! since an unused import prunes at componentization but an export is a
//! hard obligation.

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
        "../../wit/deps/nexum-host",
        "../../wit/videre-venue",
    ],
    world: "videre:sdk-shims/sdk-imports",
    generate_all,
    additional_derives: [PartialEq],
});
