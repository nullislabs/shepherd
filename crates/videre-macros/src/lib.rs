//! Proc-macro glue for videre venue adapters.
//!
//! [`venue`] is the single blessed venue authoring path: applied to an
//! `impl VenueAdapter` block it emits the per-cdylib wit-bindgen for a
//! manifest-derived world exporting `videre:venue/adapter`, asserts the
//! manifest kind, and expands to the SDK's internal export codegen.
//!
//! [`derive@IntentBody`] implements the venue SDK's versioned body codec
//! over a per-venue version enum.
//!
//! The module-side macro (`#[module]`) lives in `nexum-module-macros`.
//!
//! Consumers reach these through the SDK re-exports
//! (`videre_sdk::venue`, `videre_sdk::IntentBody`) rather than
//! depending on this crate directly.

mod intent_body;
mod world;

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, ItemImpl, Type};

/// Derive the venue SDK's `IntentBody` codec on the outer per-venue
/// version enum: one newtype variant per published body version, each
/// payload a borsh type.
///
/// The wire form is the borsh enum layout (a one-byte tag, the variant's
/// declaration index, then the borsh payload), so the tag order is the
/// schema: append new versions, never reorder. Decoding an unknown tag
/// fails typedly as `BodyError::UnknownVersion`.
///
/// Generated code resolves the SDK by crate path, so use the
/// `videre_sdk::IntentBody` re-export with `videre-sdk` as a
/// direct dependency.
#[proc_macro_derive(IntentBody)]
pub fn derive_intent_body(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    intent_body::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The manifest `kind` a venue adapter must declare. Mirrors the
/// venue-adapter provider kind's manifest spelling.
const VENUE_KIND: &str = "venue-adapter";

/// Generate the per-cdylib glue for a venue adapter.
///
/// Apply to the adapter's `impl VenueAdapter for MyVenue` block: the
/// macro reads the crate's `module.toml`, asserts its `[module] kind`
/// is `venue-adapter`, synthesizes a per-component world exporting the
/// `videre:venue/adapter` face and importing exactly the manifest's
/// declared scoped transport, then emits `wit_bindgen::generate!`, the
/// untouched trait impl, and the SDK's internal export codegen wiring
/// the world's `Guest` faces through the trait. So the built component
/// imports what the manifest declares and nothing else, by construction
/// of the emitted world.
///
/// The generated world remaps `videre:types/types`,
/// `videre:value-flow/types`, and `nexum:host/types` onto the SDK's
/// bindings, so the impl speaks `videre_sdk` types directly and shares
/// type identity with the conformance kit and the client core.
///
/// A venue's capabilities are scoped transport only: an undeclared
/// capability's bindings do not exist (using one is a compile error),
/// and a capability outside the venue-permitted set (`chain`,
/// `messaging`, `http`) is rejected at expansion.
///
/// The same crate-root resolution invariants as `#[module]` apply: the
/// wit-bindgen output lands at the module crate root (so the export
/// codegen resolves `Guest`, `exports`, and `export!` there), and the
/// consuming crate must declare `wit-bindgen` and `videre-sdk` as
/// direct dependencies.
#[proc_macro_attribute]
pub fn venue(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[videre_sdk::venue] takes no arguments",
        )
        .to_compile_error()
        .into();
    }

    let input = syn::parse_macro_input!(item as ItemImpl);

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
    if !is_plain_type(self_ty) {
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
    let wit_paths = match resolve_wit_packages(&venue_world.packages) {
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

/// Whether a type is a plain named path (`Foo`), the only shape a module
/// export type may take.
fn is_plain_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(tp) if tp.qself.is_none())
}

/// The consuming crate's manifest directory, the root every crate-local
/// lookup starts from.
fn manifest_dir() -> Result<std::path::PathBuf, String> {
    std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())
}

/// Read the consuming crate's `module.toml`, assert it declares the
/// venue-adapter kind, and synthesize the per-component venue-adapter
/// world from its `[capabilities]` declarations. Returns the manifest
/// path (for the rebuild anchor) alongside the world.
fn derive_venue_world() -> Result<(String, nexum_world::ModuleWorld), String> {
    let manifest_path = manifest_dir()?.join("module.toml");
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

/// Resolve each needed WIT package directory crate-locally (vendored
/// `wit/deps/<package>`, then own `wit/<package>`), falling back through
/// ancestors for the transitional monorepo layout.
fn resolve_wit_packages(packages: &[String]) -> Result<Vec<String>, String> {
    Ok(
        nexum_world::resolve_wit_packages(&manifest_dir()?, packages)?
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    )
}
