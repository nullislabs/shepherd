//! Proc-macro glue for nexum runtime modules.
//!
//! [`module`] turns an `impl` block of named handlers into a complete
//! per-cdylib module: it emits the `wit_bindgen::generate!` call for a
//! per-module world derived from the crate's `module.toml`
//! `[capabilities]` declarations, the host adapter (via
//! `nexum_sdk::bind_host_via_wit_bindgen!`), the `Guest` implementation
//! whose `on-event` dispatches to the handlers present, and `export!`.
//!
//! The venue-side macros (`#[venue]`, `derive(IntentBody)`) live in
//! `videre-macros`.
//!
//! Consumers reach this through the SDK re-export (`nexum_sdk::module`)
//! rather than depending on this crate directly.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ImplItem, ItemImpl};

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
    "on_custom",
];

/// Generate the per-cdylib glue for a nexum module.
///
/// Apply to an `impl` block whose associated functions are the event
/// handlers (`init`, `on_block`, `on_chain_logs`, `on_tick`,
/// `on_message`, `on_custom`). Each handler takes the wit-bindgen
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
/// there; the WIT package directories resolve against the crate's own
/// `wit/` and `wit/deps/`, then the nearest ancestor carrying the
/// package. Two corollaries: the consuming crate must
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
    if !nexum_world::is_plain_type(self_ty) {
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
             of `init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`, `on_custom`",
        )
        .to_compile_error()
        .into();
    }
    let has = |name: &str| present.contains(&name);

    let (anchors, module_world) = match derive_module_world() {
        Ok(parts) => parts,
        Err(msg) => {
            return syn::Error::new(proc_macro2::Span::call_site(), msg)
                .to_compile_error()
                .into();
        }
    };
    let wit_paths = match nexum_world::manifest_wit_packages(&module_world.packages) {
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
    let custom_arm = arm("on_custom", "Custom");

    quote! {
        // Anchor a rebuild on the manifest and the extension registry:
        // the emitted world is derived from them, so an edit to either
        // must recompile the module.
        #(const _: &[u8] = ::core::include_bytes!(#anchors);)*

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
                    #custom_arm
                }
            }
        }

        export!(__NexumModuleExport);
    }
    .into()
}

/// Read the consuming crate's `module.toml` and synthesize the
/// per-module world from its `[capabilities]` declarations plus the
/// extension rows registered in the nearest ancestor `extensions.toml`.
/// Returns the rebuild anchor paths (the manifest, then the registry
/// when one exists) alongside the world.
fn derive_module_world() -> Result<(Vec<String>, nexum_world::ModuleWorld), String> {
    let crate_dir = nexum_world::manifest_dir()?;
    let manifest_path = crate_dir.join("module.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "could not read {} ({e}); #[nexum_sdk::module] derives the component's WIT world \
             from the manifest's [capabilities] section, so the manifest must sit next to \
             Cargo.toml",
            manifest_path.display()
        )
    })?;
    let declared = nexum_world::manifest_capabilities(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest_path = manifest_path.to_string_lossy().into_owned();

    let mut anchors = vec![manifest_path.clone()];
    let extensions = match nexum_world::find_extensions_manifest(&crate_dir) {
        None => Vec::new(),
        Some(registry) => {
            let text = std::fs::read_to_string(&registry)
                .map_err(|e| format!("could not read {}: {e}", registry.display()))?;
            let rows = nexum_world::manifest_extensions(&text)
                .map_err(|e| format!("{}: {e}", registry.display()))?;
            anchors.push(registry.to_string_lossy().into_owned());
            rows
        }
    };
    let module_world = nexum_world::synthesize(&declared, &extensions)
        .map_err(|e| format!("{manifest_path}: {e}"))?;
    Ok((anchors, module_world))
}
