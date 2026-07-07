//! Proc-macro glue for nexum runtime modules.
//!
//! [`module`] turns an `impl` block of named handlers into a complete
//! per-cdylib module: it emits the `wit_bindgen::generate!` call for the
//! blanket `shepherd:cow/shepherd` world, the host adapter (via
//! `nexum_sdk::bind_host_via_wit_bindgen!`), the `Guest` implementation
//! whose `on-event` dispatches to the handlers present, and `export!`.
//!
//! [`derive@IntentBody`] implements the venue SDK's versioned body codec
//! over a per-venue version enum.
//!
//! Consumers reach these through the SDK re-exports (`nexum_sdk::module`,
//! `nexum_venue_sdk::IntentBody`) rather than depending on this crate
//! directly.

mod intent_body;

use std::path::{Path, PathBuf};

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
/// `nexum_venue_sdk::IntentBody` re-export with `nexum-venue-sdk` as a
/// direct dependency.
#[proc_macro_derive(IntentBody)]
pub fn derive_intent_body(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    intent_body::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The handler names recognised on a `#[module]` impl. Any method not in
/// this set is left untouched on the type, except that names starting
/// with `on_` are rejected at compile time (a typo'd handler would
/// otherwise silently never fire); any handler in the set that is absent
/// is treated as a no-op in the generated `on-event` dispatch.
const HANDLERS: [&str; 5] = ["init", "on_block", "on_chain_logs", "on_tick", "on_message"];

/// Generate the per-cdylib glue for a nexum module.
///
/// Apply to an `impl` block whose associated functions are the event
/// handlers (`init`, `on_block`, `on_chain_logs`, `on_tick`,
/// `on_message`). Each handler takes the wit-bindgen payload for its
/// event and returns `Result<(), Fault>`; `init` takes the config table.
/// Handlers left undefined are ignored (their events become no-ops). The
/// macro emits `wit_bindgen::generate!`, the host adapter, the `Guest`
/// impl, and `export!` around the untouched impl.
///
/// The one non-obvious invariant: the `wit`/wit-bindgen output
/// (`Guest`, `Fault`, the `nexum::host::*` modules) lands at the module
/// crate root, so the emitted glue and the handler bodies resolve those
/// names there; the WIT directory is located by walking up from
/// `CARGO_MANIFEST_DIR`. Two corollaries: the consuming crate must
/// declare `wit-bindgen` as a direct dependency (the emitted
/// `wit_bindgen::generate!` call resolves against the consumer's
/// namespace), and the crate root must not shadow std prelude names
/// such as `Result`, `Vec`, or `Ok` (wit-bindgen's generated `Guest`
/// trait refers to them unqualified).
#[proc_macro_attribute]
pub fn module(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[nexum_sdk::module] takes no arguments",
        )
        .to_compile_error()
        .into();
    }

    let input = syn::parse_macro_input!(item as ItemImpl);

    let self_ty = &input.self_ty;
    if !is_plain_type(self_ty) {
        return syn::Error::new_spanned(
            self_ty,
            "#[nexum_sdk::module] must be applied to an inherent impl of a named type",
        )
        .to_compile_error()
        .into();
    }
    if let Some((_, trait_path, _)) = &input.trait_ {
        return syn::Error::new_spanned(
            trait_path,
            "#[nexum_sdk::module] must be applied to an inherent impl, not a trait impl",
        )
        .to_compile_error()
        .into();
    }
    if !input.generics.params.is_empty() {
        return syn::Error::new_spanned(
            &input.generics,
            "#[nexum_sdk::module] must be applied to a non-generic impl",
        )
        .to_compile_error()
        .into();
    }

    // A typo'd handler (`on_blocks`, `on_chainlogs`, ...) would otherwise
    // compile as an ordinary helper while its event silently no-ops, so
    // reserve the `on_` prefix for the recognised handler set.
    for item in &input.items {
        if let ImplItem::Fn(f) = item {
            let name = f.sig.ident.to_string();
            if name.starts_with("on_") && !HANDLERS.contains(&name.as_str()) {
                return syn::Error::new_spanned(
                    &f.sig.ident,
                    format!(
                        "`{name}` is not a recognised #[nexum_sdk::module] handler; expected one \
                         of {HANDLERS:?} (rename helpers so they do not start with `on_`)"
                    ),
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let present: Vec<&str> = input
        .items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Fn(f) => {
                let name = f.sig.ident.to_string();
                HANDLERS.into_iter().find(|h| *h == name)
            }
            _ => None,
        })
        .collect();
    if present.is_empty() {
        return syn::Error::new_spanned(
            self_ty,
            "#[nexum_sdk::module] found no recognised handlers on this impl; define at least one \
             of `init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`",
        )
        .to_compile_error()
        .into();
    }
    let has = |name: &str| present.contains(&name);

    let (nexum_wit, shepherd_wit) = match locate_wit() {
        Ok(paths) => paths,
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };
    let nexum_wit = nexum_wit.to_string_lossy().into_owned();
    let shepherd_wit = shepherd_wit.to_string_lossy().into_owned();

    // `init` is a required export; when the handler is absent the config
    // is bound but unused, so drop it to keep the module warning-clean.
    let init_impl = if has("init") {
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

    let arm = |handler: &str, variant| -> proc_macro2::TokenStream {
        let variant = syn::Ident::new(variant, proc_macro2::Span::call_site());
        if has(handler) {
            let call = syn::Ident::new(handler, proc_macro2::Span::call_site());
            quote! { nexum::host::types::Event::#variant(payload) => <#self_ty>::#call(payload), }
        } else {
            quote! { nexum::host::types::Event::#variant(_) => ::core::result::Result::Ok(()), }
        }
    };
    let block_arm = arm("on_block", "Block");
    let logs_arm = arm("on_chain_logs", "ChainLogs");
    let tick_arm = arm("on_tick", "Tick");
    let message_arm = arm("on_message", "Message");

    quote! {
        wit_bindgen::generate!({
            path: [#nexum_wit, #shepherd_wit],
            world: "shepherd:cow/shepherd",
            generate_all,
        });

        ::nexum_sdk::bind_host_via_wit_bindgen!();

        #input

        #[doc(hidden)]
        struct __NexumModuleExport;

        impl Guest for __NexumModuleExport {
            #init_impl

            fn on_event(event: nexum::host::types::Event) -> ::core::result::Result<(), Fault> {
                match event {
                    #block_arm
                    #logs_arm
                    #tick_arm
                    #message_arm
                }
            }
        }

        export!(__NexumModuleExport);
    }
    .into()
}

/// Whether a type is a plain named path (`Foo`), the only shape a module
/// export type may take.
fn is_plain_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(tp) if tp.qself.is_none())
}

/// Locate the workspace `wit/nexum-host` and `wit/shepherd-cow`
/// directories by walking up from the consuming crate's manifest.
fn locate_wit() -> Result<(PathBuf, PathBuf), String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())?;
    let mut dir: Option<&Path> = Some(Path::new(&manifest));
    while let Some(cur) = dir {
        let wit = cur.join("wit");
        let nexum = wit.join("nexum-host");
        let shepherd = wit.join("shepherd-cow");
        if nexum.is_dir() && shepherd.is_dir() {
            return Ok((nexum, shepherd));
        }
        dir = cur.parent();
    }
    Err(format!(
        "could not find a `wit/` directory containing `nexum-host` and `shepherd-cow` \
         in any ancestor of {manifest}"
    ))
}
