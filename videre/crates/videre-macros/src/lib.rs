//! Proc-macro glue for the videre personas, reached through the SDK
//! re-exports (`videre_sdk::venue`/`keeper`/`IntentBody`), not directly.
//!
//! - [`venue`]: on an `impl VenueAdapter` block, emits the per-cdylib
//!   wit-bindgen for a manifest-derived venue-adapter world plus the SDK
//!   export codegen.
//! - [`keeper`]: the worker mirror, wiring the `videre:venue/client` import
//!   onto the SDK shims and dispatching events to the handler impl.
//! - [`derive@IntentBody`]: the versioned body codec over a per-venue enum.

mod intent_body;
mod keeper;
mod venue_marker;
mod world;

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, ItemImpl};

/// Derive the `IntentBody` codec on a per-venue version enum: one newtype
/// variant per body version. The wire form is the borsh enum layout (a
/// one-byte tag at the variant's declaration index), so append versions,
/// never reorder; an unknown tag fails as `BodyError::UnknownVersion`. Use
/// the `videre_sdk::IntentBody` re-export.
#[proc_macro_derive(IntentBody)]
pub fn derive_intent_body(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    intent_body::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The manifest `kind` a venue adapter must declare.
const VENUE_KIND: &str = "venue-adapter";

/// Generate the per-cdylib glue for a venue adapter.
///
/// Apply to an `impl VenueAdapter for MyVenue` block: reads `module.toml`,
/// asserts `[module] kind = venue-adapter`, synthesizes a world exporting
/// `videre:venue/adapter` and importing exactly the manifest's declared
/// scoped transport, then emits `wit_bindgen::generate!`, the trait impl,
/// and the SDK export codegen. The world remaps the `videre` and
/// `nexum:host` types onto the SDK bindings, so the impl speaks `videre_sdk`
/// types directly. A capability outside the venue-permitted set (`chain`,
/// `messaging`, `http`) is rejected at expansion. The consuming crate must
/// declare `wit-bindgen` and `videre-sdk` as direct dependencies.
///
/// # Client marker
///
/// With arguments (`#[videre_sdk::venue(id = "cow", body = CowBody)]`) it
/// instead fills a client-side `impl Venue for Marker {}`, emitting the
/// `const ID`/`type Body` and asserting the id equals `[module] name`. No
/// component world, so a keeper linking the client slice never pulls adapter
/// bindgen.
#[proc_macro_attribute]
pub fn venue(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as ItemImpl);

    if !attr.is_empty() {
        return venue_marker::expand(attr.into(), &input)
            .unwrap_or_else(syn::Error::into_compile_error)
            .into();
    }

    let Some((None, trait_path, _)) = &input.trait_ else {
        return syn::Error::new_spanned(
            &input.self_ty,
            "#[videre_sdk::venue] must be applied to an `impl VenueAdapter for ...` block",
        )
        .to_compile_error()
        .into();
    };
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "VenueAdapter")
    {
        return syn::Error::new_spanned(
            trait_path,
            "#[videre_sdk::venue] must be applied to an impl of `videre_sdk::VenueAdapter`",
        )
        .to_compile_error()
        .into();
    }
    let self_ty = &input.self_ty;
    if !nexum_world::is_plain_type(self_ty) {
        return syn::Error::new_spanned(
            self_ty,
            "#[videre_sdk::venue] must be applied to an impl on a named type",
        )
        .to_compile_error()
        .into();
    }
    if !input.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &input.generics,
            "#[videre_sdk::venue] must be applied to a non-generic impl",
        )
        .to_compile_error()
        .into();
    }

    let (manifest_path, venue_world) = match derive_venue_world() {
        Ok(parts) => parts,
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };
    let wit_paths = match nexum_world::manifest_wit_packages(&venue_world.packages) {
        Ok(paths) => paths,
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };
    let inline_world = &venue_world.wit;

    quote! {
        // Anchor a rebuild on the manifest: the emitted world is derived
        // from it, so an edited [capabilities] must recompile the adapter.
        const _: &[u8] = ::core::include_bytes!(#manifest_path);

        wit_bindgen::generate!({
            inline: #inline_world,
            path: [#(#wit_paths),*],
            world: "nexum:venue-world/venue-adapter",
            generate_all,
            with: {
                "nexum:host/types@0.1.0": ::videre_sdk::bindings::nexum::host::types,
                "videre:types/types@0.1.0": ::videre_sdk::bindings::videre::types::types,
                "videre:value-flow/types@0.1.0":
                    ::videre_sdk::bindings::videre::value_flow::types,
            },
        });

        #input

        ::videre_sdk::__export_venue_adapter!(#self_ty);
    }
    .into()
}

/// Generate the per-cdylib glue for a keeper: a worker driving venues
/// through the typed client.
///
/// Apply to an `impl` block whose functions are the event handlers (`init`,
/// `on_block`, `on_chain_logs`, `on_tick`, `on_message`,
/// `on_intent_status`); handlers may be `async`, completed on the guest
/// boundary, so one can await the typed `VenueClient` directly. Reads
/// `module.toml`, requires the `client` capability, synthesizes the module
/// world as `#[module]` does, and remaps the videre interfaces onto the SDK
/// bindings so the client and wire share one type set. Emits a
/// `From<ClientError>` onto the wire fault, so `?` works in handlers. The
/// consuming crate must declare `wit-bindgen`, `videre-sdk`, and `nexum-sdk`
/// as direct dependencies.
#[proc_macro_attribute]
pub fn keeper(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[videre_sdk::keeper] takes no arguments",
        )
        .to_compile_error()
        .into();
    }
    let input = syn::parse_macro_input!(item as ItemImpl);
    keeper::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Read `module.toml`, assert the venue-adapter kind, and synthesize the
/// venue-adapter world from its `[capabilities]`. Returns the manifest path
/// (rebuild anchor) and the world.
fn derive_venue_world() -> Result<(String, nexum_world::ModuleWorld), String> {
    let manifest_path = nexum_world::manifest_dir()?.join("module.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "could not read {} ({e}); #[videre_sdk::venue] derives the component's WIT world \
             from the manifest's [capabilities] section, so the manifest must sit next to \
             Cargo.toml",
            manifest_path.display()
        )
    })?;
    let kind = nexum_world::manifest_kind(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    if kind.as_deref() != Some(VENUE_KIND) {
        return Err(format!(
            "{}: [module] kind must be \"{VENUE_KIND}\" for a #[videre_sdk::venue] adapter, \
             found {}",
            manifest_path.display(),
            kind.map_or_else(|| "none".to_owned(), |kind| format!("\"{kind}\"")),
        ));
    }
    let declared = nexum_world::manifest_capabilities(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest_path = manifest_path.to_string_lossy().into_owned();
    let venue_world =
        world::synthesize_venue(&declared).map_err(|e| format!("{manifest_path}: {e}"))?;
    Ok((manifest_path, venue_world))
}
