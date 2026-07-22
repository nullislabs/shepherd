//! Proc-macro glue for videre venue adapters.
//!
//! [`venue`] emits the per-cdylib wit-bindgen and `export!` for a
//! per-component venue-adapter world exporting the
//! `videre:venue/adapter` face and importing only the manifest's
//! declared scoped transport.
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
use syn::{DeriveInput, ImplItem, ItemImpl, Type};

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

/// The associated functions the `videre:venue/adapter` face mandates. A
/// venue adapter must define all five; `init` is separate (a no-op when
/// absent, exactly as in a module).
const VENUE_EXPORTS: [&str; 5] = ["derive_header", "quote", "submit", "status", "cancel"];

/// Generate the per-cdylib glue for a venue adapter.
///
/// Apply to an inherent `impl` block whose associated functions are the
/// adapter face: `derive_header`, `quote`, `submit`, `status`, `cancel`
/// (all required, from `videre:venue/adapter`), plus an optional `init`
/// (absent means a no-op) and an optional `body_versions` (absent
/// declares none). Each takes and returns the per-cdylib
/// wit-bindgen payloads for its signature. The macro reads the crate's
/// `module.toml`, synthesizes a per-component world exporting the
/// adapter face and importing exactly the manifest's declared scoped
/// transport, then emits `wit_bindgen::generate!`, the `Guest` impls
/// wiring the world to the adapter's functions, and `export!` around the
/// untouched impl. So the built component imports what the manifest
/// declares and nothing else, retiring the toolchain-elision dependency
/// on the venue side.
///
/// A venue's capabilities are scoped transport only: an undeclared
/// capability's bindings do not exist (using one is a compile error),
/// and a capability outside the venue-permitted set (`chain`,
/// `messaging`, `http`) is rejected at expansion.
///
/// The same crate-root resolution invariants as `#[module]` apply: the
/// wit-bindgen output lands at the module crate root (so the emitted
/// glue resolves `Guest`, `Fault`, and the `nexum::*`/`videre::*` type modules
/// there), the consuming crate must declare `wit-bindgen` as a direct
/// dependency, and the crate root must not shadow std prelude names.
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

    let self_ty = &input.self_ty;
    if !is_plain_type(self_ty) {
        return syn::Error::new_spanned(
            self_ty,
            "#[videre_sdk::venue] must be applied to an inherent impl of a named type",
        )
        .to_compile_error()
        .into();
    }
    if let Some((_, trait_path, _)) = &input.trait_ {
        return syn::Error::new_spanned(
            trait_path,
            "#[videre_sdk::venue] must be applied to an inherent impl, not a trait impl",
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

    let defines = |name: &str| {
        input
            .items
            .iter()
            .any(|item| matches!(item, ImplItem::Fn(f) if f.sig.ident == name))
    };
    let missing: Vec<&str> = VENUE_EXPORTS
        .into_iter()
        .filter(|name| !defines(name))
        .collect();
    if !missing.is_empty() {
        return syn::Error::new_spanned(
            self_ty,
            format!(
                "#[videre_sdk::venue] requires the adapter face; this impl is missing {:?}. \
                 Define all of `derive_header`, `quote`, `submit`, `status`, `cancel` (plus an \
                 optional `init`)",
                missing
            ),
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

    // `body-versions` is a required adapter export; when the adapter
    // omits it, declare none. Install asserts the export equals the
    // manifest `[venue] body_versions` set.
    let body_versions_impl = if defines("body_versions") {
        quote! {
            fn body_versions() -> ::std::vec::Vec<u32> {
                <#self_ty>::body_versions()
            }
        }
    } else {
        quote! {
            fn body_versions() -> ::std::vec::Vec<u32> {
                ::std::vec::Vec::new()
            }
        }
    };

    // `init` is a required world export; when the adapter omits it the
    // config is bound but unused, so drop it to stay warning-clean.
    let init_impl = if defines("init") {
        quote! {
            fn init(
                config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
            ) -> ::core::result::Result<(), Fault> {
                <#self_ty>::init(config)
            }
        }
    } else {
        quote! {
            fn init(
                _config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
            ) -> ::core::result::Result<(), Fault> {
                ::core::result::Result::Ok(())
            }
        }
    };

    quote! {
        // Anchor a rebuild on the manifest: the emitted world is derived
        // from it, so an edited [capabilities] must recompile the adapter.
        const _: &[u8] = ::core::include_bytes!(#manifest_path);

        wit_bindgen::generate!({
            inline: #inline_world,
            path: [#(#wit_paths),*],
            world: "nexum:venue-world/venue-adapter",
            generate_all,
        });

        #input

        #[doc(hidden)]
        struct __NexumVenueAdapterExport;

        impl Guest for __NexumVenueAdapterExport {
            #init_impl
        }

        impl exports::videre::venue::adapter::Guest for __NexumVenueAdapterExport {
            #body_versions_impl

            fn derive_header(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<
                videre::types::types::IntentHeader,
                videre::types::types::VenueError,
            > {
                <#self_ty>::derive_header(body)
            }

            fn quote(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<
                videre::types::types::Quotation,
                videre::types::types::VenueError,
            > {
                <#self_ty>::quote(body)
            }

            fn submit(
                body: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<
                videre::types::types::SubmitOutcome,
                videre::types::types::VenueError,
            > {
                <#self_ty>::submit(body)
            }

            fn status(
                receipt: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<
                videre::types::types::IntentStatus,
                videre::types::types::VenueError,
            > {
                <#self_ty>::status(receipt)
            }

            fn cancel(
                receipt: ::std::vec::Vec<u8>,
            ) -> ::core::result::Result<(), videre::types::types::VenueError> {
                <#self_ty>::cancel(receipt)
            }
        }

        export!(__NexumVenueAdapterExport);
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

/// Read the consuming crate's `module.toml` and synthesize the
/// per-component venue-adapter world from its `[capabilities]`
/// declarations. Returns the manifest path (for the rebuild anchor)
/// alongside the world.
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
