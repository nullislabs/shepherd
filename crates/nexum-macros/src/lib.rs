//! Proc-macro glue for nexum runtime modules.
//!
//! [`module`] turns an `impl` block of named handlers into a complete
//! per-cdylib module: it emits the `wit_bindgen::generate!` call for a
//! per-module world derived from the crate's `module.toml`
//! `[capabilities]` declarations, the host adapter (via
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
mod world;

use std::path::Path;

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
const HANDLERS: [&str; 6] = [
    "init",
    "on_block",
    "on_chain_logs",
    "on_tick",
    "on_message",
    "on_intent_status",
];

/// Generate the per-cdylib glue for a nexum module.
///
/// Apply to an `impl` block whose associated functions are the event
/// handlers (`init`, `on_block`, `on_chain_logs`, `on_tick`,
/// `on_message`, `on_intent_status`). Each handler takes the wit-bindgen
/// payload for its event and returns `Result<(), Fault>`; `init` takes
/// the config table.
/// Handlers left undefined are ignored (their events become no-ops). The
/// macro emits `wit_bindgen::generate!`, the host adapter, the `Guest`
/// impl, and `export!` around the untouched impl.
///
/// The world is per module, not shared: the macro reads the crate's
/// `module.toml` and synthesizes a world whose imports are exactly the
/// `[capabilities].required` and `optional` declarations, so the built
/// component imports what the manifest declares and nothing else - the
/// runtime's load-time capability check passes by construction instead
/// of relying on the toolchain eliding unused imports. Corollaries: the
/// manifest must sit at the crate root and carry a `[capabilities]`
/// section, an undeclared capability's bindings simply do not exist
/// (using one is a compile error, the cue to declare it), and only the
/// host-adapter pieces for declared capabilities are emitted.
///
/// The other non-obvious invariant: the wit-bindgen output (`Guest`,
/// `Fault`, the `nexum::host::*` modules) lands at the module crate
/// root, so the emitted glue and the handler bodies resolve those names
/// there; the WIT package directories are located by walking up from
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
             of `init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`, `on_intent_status`",
        )
        .to_compile_error()
        .into();
    }
    let has = |name: &str| present.contains(&name);

    let (manifest_path, module_world) = match derive_module_world() {
        Ok(parts) => parts,
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };
    let wit_paths = match resolve_wit_packages(&module_world.packages) {
        Ok(paths) => paths,
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };
    let inline_world = &module_world.wit;
    let adapter_caps: Vec<syn::Ident> = module_world
        .adapters
        .iter()
        .map(|cap| syn::Ident::new(cap, proc_macro2::Span::call_site()))
        .collect();

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
    let intent_status_arm = arm("on_intent_status", "IntentStatus");

    quote! {
        // Anchor a rebuild on the manifest: the emitted world is derived
        // from it, so an edited [capabilities] must recompile the module.
        const _: &[u8] = ::core::include_bytes!(#manifest_path);

        wit_bindgen::generate!({
            inline: #inline_world,
            path: [#(#wit_paths),*],
            world: "nexum:module-world/module",
            generate_all,
        });

        ::nexum_sdk::bind_host_via_wit_bindgen!(caps: [#(#adapter_caps),*]);

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
                    #intent_status_arm
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

/// Read the consuming crate's `module.toml` and synthesize the
/// per-module world from its `[capabilities]` declarations. Returns the
/// manifest path (for the rebuild anchor) alongside the world.
fn derive_module_world() -> Result<(String, world::ModuleWorld), String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())?;
    let manifest_path = Path::new(&manifest_dir).join("module.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "could not read {} ({e}); #[nexum_sdk::module] derives the module's WIT world \
             from the manifest's [capabilities] section, so the manifest must sit next to \
             Cargo.toml",
            manifest_path.display()
        )
    })?;
    let declared = world::manifest_capabilities(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let module_world =
        world::synthesize(&declared).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    Ok((manifest_path.to_string_lossy().into_owned(), module_world))
}

/// Locate the workspace `wit/` root (the ancestor directory whose `wit/`
/// contains the `nexum-host` package) and resolve each needed package
/// directory under it.
fn resolve_wit_packages(packages: &[&str]) -> Result<Vec<String>, String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| "CARGO_MANIFEST_DIR is not set".to_string())?;
    let mut dir: Option<&Path> = Some(Path::new(&manifest));
    let root = loop {
        let Some(cur) = dir else {
            return Err(format!(
                "could not find a `wit/` directory containing `nexum-host` in any ancestor \
                 of {manifest}"
            ));
        };
        let wit = cur.join("wit");
        if wit.join("nexum-host").is_dir() {
            break wit;
        }
        dir = cur.parent();
    };
    packages
        .iter()
        .map(|package| {
            let path = root.join(package);
            if path.is_dir() {
                Ok(path.to_string_lossy().into_owned())
            } else {
                Err(format!(
                    "declared capabilities need the `{package}` WIT package, but {} is not \
                     a directory",
                    path.display()
                ))
            }
        })
        .collect()
}
