//! Expansion for `#[keeper]`: the worker mirror of `#[module]`. Same world
//! synthesis and event dispatch, with the keeper deltas: the `client`
//! capability is required, the videre interfaces remap onto the SDK
//! bindings, async handlers complete via `videre_sdk::client::poll_once`,
//! and `ClientError` folds into the wire fault so `?` works in handlers.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ItemImpl};

/// The handler names recognised on a `#[keeper]` impl.
const HANDLERS: [&str; 6] = [
    "init",
    "on_block",
    "on_chain_logs",
    "on_tick",
    "on_message",
    "on_intent_status",
];

/// The manifest capability granting the client import.
const CLIENT_CAPABILITY: &str = "client";

/// The import the `client` capability must map to.
const CLIENT_IMPORT: &str = "videre:venue/client@0.1.0";

/// WIT packages the client import needs on the resolve path, in
/// dependency order.
const CLIENT_PACKAGES: [&str; 3] = ["videre-value-flow", "videre-types", "videre-venue"];

/// The fault detail for a handler future that suspended.
const SUSPENDED: &str = "keeper handler suspended: guest futures complete in one poll";

/// Expand the handler impl into the keeper module glue.
pub(crate) fn expand(input: &ItemImpl) -> syn::Result<TokenStream> {
    let self_ty = &input.self_ty;
    if !nexum_world::is_plain_type(self_ty) {
        return Err(syn::Error::new_spanned(
            self_ty,
            "#[videre_sdk::keeper] must be applied to an inherent impl of a named type",
        ));
    }
    if let Some((_, trait_path, _)) = &input.trait_ {
        return Err(syn::Error::new_spanned(
            trait_path,
            "#[videre_sdk::keeper] must be applied to an inherent impl, not a trait impl",
        ));
    }
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[videre_sdk::keeper] must be applied to a non-generic impl",
        ));
    }

    // Reserve the `on_` prefix for the recognised handler set, exactly
    // as `#[module]` does: a typo'd handler must not silently no-op.
    for item in &input.items {
        if let ImplItem::Fn(f) = item {
            let name = f.sig.ident.to_string();
            if name.starts_with("on_") && !HANDLERS.contains(&name.as_str()) {
                return Err(syn::Error::new_spanned(
                    &f.sig.ident,
                    format!(
                        "`{name}` is not a recognised #[videre_sdk::keeper] handler; expected one \
                         of {HANDLERS:?} (rename helpers so they do not start with `on_`)"
                    ),
                ));
            }
        }
    }

    // Present handlers with their asyncness: async ones are completed
    // on the synchronous guest boundary by the emitted dispatch.
    let present: Vec<(&str, bool)> = input
        .items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Fn(f) => {
                let name = f.sig.ident.to_string();
                HANDLERS
                    .into_iter()
                    .find(|h| *h == name)
                    .map(|h| (h, f.sig.asyncness.is_some()))
            }
            _ => None,
        })
        .collect();
    if present.is_empty() {
        return Err(syn::Error::new_spanned(
            self_ty,
            "#[videre_sdk::keeper] found no recognised handlers on this impl; define at least one \
             of `init`, `on_block`, `on_chain_logs`, `on_tick`, `on_message`, `on_intent_status`",
        ));
    }
    let handler = |name: &str| present.iter().find(|(h, _)| *h == name).copied();

    let (anchors, module_world) = derive_keeper_world()
        .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let wit_paths = nexum_world::manifest_wit_packages(&module_world.packages)
        .map_err(|msg| syn::Error::new(proc_macro2::Span::call_site(), msg))?;
    let inline_world = &module_world.wit;
    let adapter_caps: Vec<syn::Ident> = module_world
        .adapters
        .iter()
        .map(|cap| syn::Ident::new(cap, proc_macro2::Span::call_site()))
        .collect();

    // Complete an async handler's future in one poll; a suspension is a
    // typed internal fault, never a hang.
    let drive = |call: TokenStream| {
        quote! {
            match ::videre_sdk::client::poll_once(#call) {
                ::core::task::Poll::Ready(result) => result,
                ::core::task::Poll::Pending => ::core::result::Result::Err(
                    nexum::host::types::Fault::Internal(
                        ::std::string::String::from(#SUSPENDED),
                    ),
                ),
            }
        }
    };

    let init_impl = match handler("init") {
        Some((_, is_async)) => {
            let call = quote! { <#self_ty>::init(config) };
            let body = if is_async { drive(call) } else { call };
            quote! {
                fn init(
                    config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
                ) -> ::core::result::Result<(), Fault> {
                    #body
                }
            }
        }
        None => quote! {
            fn init(
                _config: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
            ) -> ::core::result::Result<(), Fault> {
                ::core::result::Result::Ok(())
            }
        },
    };

    let arm = |name: &str, variant: &str| -> TokenStream {
        let variant = syn::Ident::new(variant, proc_macro2::Span::call_site());
        match handler(name) {
            Some((_, is_async)) => {
                let call = syn::Ident::new(name, proc_macro2::Span::call_site());
                let call = quote! { <#self_ty>::#call(payload) };
                let body = if is_async { drive(call) } else { call };
                quote! { nexum::host::types::Event::#variant(payload) => #body, }
            }
            None => quote! {
                nexum::host::types::Event::#variant(_) => ::core::result::Result::Ok(()),
            },
        }
    };
    let block_arm = arm("on_block", "Block");
    let logs_arm = arm("on_chain_logs", "ChainLogs");
    let tick_arm = arm("on_tick", "Tick");
    let message_arm = arm("on_message", "Message");
    // The intent-status transition rides the generic `custom` channel;
    // recover it typed through `videre_sdk::event` and dispatch to the
    // keeper's `on_intent_status` when the kind matches. A malformed
    // payload is the caller's `invalid-input`; a foreign kind is another
    // extension's event and no-ops.
    let custom_arm = match handler("on_intent_status") {
        Some((_, is_async)) => {
            let call = quote! { <#self_ty>::on_intent_status(update) };
            let body = if is_async { drive(call) } else { call };
            quote! {
                nexum::host::types::Event::Custom(payload) => {
                    match ::videre_sdk::event::intent_status_update(
                        &payload.kind,
                        &payload.payload,
                    ) {
                        ::core::option::Option::Some(::core::result::Result::Ok(update)) => #body,
                        ::core::option::Option::Some(::core::result::Result::Err(err)) => {
                            ::core::result::Result::Err(nexum::host::types::Fault::InvalidInput(
                                ::std::string::ToString::to_string(&err),
                            ))
                        }
                        ::core::option::Option::None => ::core::result::Result::Ok(()),
                    }
                }
            }
        }
        None => quote! {
            nexum::host::types::Event::Custom(_) => ::core::result::Result::Ok(()),
        },
    };

    Ok(quote! {
        // Anchor a rebuild on the manifest and the extension registry:
        // the emitted world is derived from them.
        #(const _: &[u8] = ::core::include_bytes!(#anchors);)*

        wit_bindgen::generate!({
            inline: #inline_world,
            path: [#(#wit_paths),*],
            world: "nexum:module-world/module",
            generate_all,
            with: {
                "videre:types/types@0.1.0": ::videre_sdk::bindings::videre::types::types,
                "videre:value-flow/types@0.1.0":
                    ::videre_sdk::bindings::videre::value_flow::types,
                "videre:venue/client@0.1.0":
                    ::videre_sdk::bindings::videre::venue::client,
            },
        });

        ::nexum_sdk::bind_host_via_wit_bindgen!(caps: [#(#adapter_caps),*]);

        #input

        // Folds a typed client failure into the wire fault, so `?`
        // applies to client calls inside handlers.
        impl ::core::convert::From<::videre_sdk::ClientError> for nexum::host::types::Fault {
            fn from(err: ::videre_sdk::ClientError) -> Self {
                ::core::convert::Into::into(::nexum_sdk::host::Fault::from(err))
            }
        }

        #[doc(hidden)]
        struct __VidereKeeperExport;

        impl Guest for __VidereKeeperExport {
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

        export!(__VidereKeeperExport);
    })
}

/// The canonical `client` extension row, injected when the composition
/// root's registry does not carry one.
fn client_row() -> nexum_world::ExtensionRow {
    nexum_world::ExtensionRow {
        name: CLIENT_CAPABILITY.to_owned(),
        import: CLIENT_IMPORT.to_owned(),
        packages: CLIENT_PACKAGES.map(str::to_owned).into(),
    }
}

/// Read `module.toml`, require the worker shape (no `[module] kind`) and
/// the `client` capability, and synthesize the module world with the client
/// extension row. Returns the rebuild anchor paths and the world.
fn derive_keeper_world() -> Result<(Vec<String>, nexum_world::ModuleWorld), String> {
    let crate_dir = nexum_world::manifest_dir()?;
    let manifest_path = crate_dir.join("module.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        format!(
            "could not read {} ({e}); #[videre_sdk::keeper] derives the component's WIT world \
             from the manifest's [capabilities] section, so the manifest must sit next to \
             Cargo.toml",
            manifest_path.display()
        )
    })?;
    if let Some(kind) = nexum_world::manifest_kind(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?
    {
        return Err(format!(
            "{}: a #[videre_sdk::keeper] module is a plain worker; drop `[module] kind = \
             \"{kind}\"`",
            manifest_path.display()
        ));
    }
    let declared = nexum_world::manifest_capabilities(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    if !declared.iter().any(|cap| cap == CLIENT_CAPABILITY) {
        return Err(format!(
            "{}: a keeper drives venues through `{CLIENT_IMPORT}`; declare the \
             `{CLIENT_CAPABILITY}` capability under [capabilities]",
            manifest_path.display()
        ));
    }
    let manifest_path = manifest_path.to_string_lossy().into_owned();

    let mut anchors = vec![manifest_path.clone()];
    let mut extensions = match nexum_world::find_extensions_manifest(&crate_dir) {
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
    match extensions.iter().find(|row| row.name == CLIENT_CAPABILITY) {
        None => extensions.push(client_row()),
        Some(row) if row.import == CLIENT_IMPORT => {}
        Some(row) => {
            return Err(format!(
                "the registered `{CLIENT_CAPABILITY}` extension imports `{}`; \
                 #[videre_sdk::keeper] requires `{CLIENT_IMPORT}`",
                row.import
            ));
        }
    }
    let module_world = nexum_world::synthesize(&declared, &extensions)
        .map_err(|e| format!("{manifest_path}: {e}"))?;
    Ok((anchors, module_world))
}
