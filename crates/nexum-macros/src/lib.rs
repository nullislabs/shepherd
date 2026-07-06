//! Proc-macro glue for nexum runtime modules.
//!
//! [`module`] turns an `impl` block of named handlers into a complete
//! per-cdylib module: it emits the `wit_bindgen::generate!` call for the
//! blanket `shepherd:cow/shepherd` world, the host adapter (via
//! `nexum_sdk::bind_host_via_wit_bindgen!`), the `Guest` implementation
//! whose `on-event` dispatches to the handlers present, and `export!`.
//!
//! Consumers reach this through the `nexum_sdk::module` re-export rather
//! than depending on this crate directly.

use std::path::{Path, PathBuf};

use proc_macro::TokenStream;
use quote::quote;
use syn::{ImplItem, ItemImpl, Type};

/// The handler names recognised on a `#[module]` impl. Any method not in
/// this set is left untouched on the type; any handler in the set that is
/// absent is treated as a no-op in the generated `on-event` dispatch.
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
/// `CARGO_MANIFEST_DIR`.
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
