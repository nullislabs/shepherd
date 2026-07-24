//! The client-side `#[videre_sdk::venue(id = "...", body = Type)]` path:
//! fills a `Venue` marker impl and checks the id against `module.toml`
//! at expansion. No component world, so a keeper linking the client
//! slice never pulls adapter bindgen.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{ItemImpl, LitStr, Token, Type};

/// `id = "cow", body = CowIntentBody`: the venue id and the body schema
/// the marker binds.
struct Args {
    id: LitStr,
    body: Type,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut id = None;
        let mut body = None;
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "id" => id = Some(input.parse()?),
                "body" => body = Some(input.parse()?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown argument `{other}`, expected `id` or `body`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            id: id.ok_or_else(|| syn::Error::new(Span::call_site(), "missing `id = \"...\"`"))?,
            body: body
                .ok_or_else(|| syn::Error::new(Span::call_site(), "missing `body = Type`"))?,
        })
    }
}

/// Expand `#[videre_sdk::venue(id, body)]` on an `impl Venue for Marker
/// {}` block: inject `const ID`/`type Body` from the args and assert the
/// id equals the crate manifest's `[module] name`.
pub fn expand(attr: TokenStream, input: &ItemImpl) -> Result<TokenStream, syn::Error> {
    let args: Args = syn::parse2(attr)?;

    let Some((None, trait_path, _)) = &input.trait_ else {
        return Err(syn::Error::new_spanned(
            &input.self_ty,
            "#[videre_sdk::venue(id = ..)] must be applied to an `impl Venue for ...` block",
        ));
    };
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Venue")
    {
        return Err(syn::Error::new_spanned(
            trait_path,
            "#[videre_sdk::venue(id = ..)] must be applied to an impl of `videre_sdk::client::Venue`",
        ));
    }
    if !input.items.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.self_ty,
            "#[videre_sdk::venue(id = ..)] fills the impl body; leave it empty",
        ));
    }
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[videre_sdk::venue(id = ..)] must be applied to a non-generic impl",
        ));
    }

    let self_ty = &input.self_ty;
    let id = &args.id;
    let body = &args.body;

    // Read the crate manifest at expansion and hold the id to its
    // registered name, alloy-`sol!`-style. Any mismatch is a
    // compile_error, so the marker and the adapter it types cannot drift.
    let manifest_path = manifest_id_check(id)?;

    Ok(quote! {
        // Rebuild anchor: an edited `[module] name` re-runs the check.
        const _: &[u8] = ::core::include_bytes!(#manifest_path);

        impl #trait_path for #self_ty {
            const ID: ::videre_sdk::client::VenueId =
                ::videre_sdk::client::VenueId::from_static(#id);
            type Body = #body;
        }
    })
}

/// Assert `id` equals the crate manifest's `[module] name`, returning the
/// manifest path for the rebuild anchor.
fn manifest_id_check(id: &LitStr) -> Result<String, syn::Error> {
    let err = |msg: String| syn::Error::new(id.span(), msg);
    let manifest_path = nexum_world::manifest_dir()
        .map_err(&err)?
        .join("module.toml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| {
        err(format!(
            "could not read {} ({e}); #[videre_sdk::venue(id = ..)] holds the id to the \
             manifest's [module] name, so the manifest must sit next to Cargo.toml",
            manifest_path.display()
        ))
    })?;
    let name = nexum_world::manifest_name(&text)
        .map_err(|e| err(format!("{}: {e}", manifest_path.display())))?;
    if name != id.value() {
        return Err(err(format!(
            "{}: venue id {:?} disagrees with [module] name {name:?}",
            manifest_path.display(),
            id.value(),
        )));
    }
    Ok(manifest_path.to_string_lossy().into_owned())
}
