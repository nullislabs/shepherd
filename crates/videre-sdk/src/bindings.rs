//! Guest bindings for the `videre:venue/venue-adapter` world.
//!
//! Unlike event modules, which run `wit_bindgen::generate!` per cdylib,
//! the venue SDK generates the adapter world's bindings once, here: the
//! [`VenueAdapter`](crate::VenueAdapter) trait, the typed transport
//! wrappers, and the intent client core are all expressed over these
//! types, and [`export_venue_adapter!`](crate::export_venue_adapter)
//! emits the component export glue into the adapter's own cdylib via the
//! generated (hidden) export macro. Downstream bindgens wanting type
//! identity with this crate remap `videre:types/types` and
//! `videre:value-flow/types` onto these modules with `with`.

wit_bindgen::generate!({
    path: [
        "../../wit/videre-value-flow",
        "../../wit/videre-types",
        "../../wit/nexum-host",
        "../../wit/videre-venue",
    ],
    world: "videre:venue/venue-adapter",
    generate_all,
    pub_export_macro: true,
    export_macro_name: "__export_venue_adapter_world",
    default_bindings_module: "videre_sdk::bindings",
    additional_derives: [PartialEq],
});
