//! Expansion for `#[derive(IntentBody)]`: the borsh codec over a
//! per-venue version enum.
//!
//! The derive enforces the outer-enum shape at compile time (an enum of
//! newtype variants, one published body version per variant) and emits
//! `to_bytes` / `from_bytes` whose wire form is the borsh enum layout: a
//! one-byte version tag (the variant's declaration index) followed by the
//! borsh-encoded payload. Decoding matches the tag itself, so an unknown
//! version surfaces as the typed `BodyError::UnknownVersion` rather than
//! a stringly borsh error, and a known version delegates the payload to
//! its type's `BorshDeserialize`.
//!
//! Generated code names the venue SDK by its crate path
//! (`::nexum_venue_sdk`), so the derive is only usable through that
//! crate's re-export. The expansion names only `::core` and the SDK's
//! `__private` re-exports (borsh, `alloc`), so a `#![no_std]` consumer
//! needs no `extern crate alloc`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

/// Expand the derive input into the `IntentBody` impl, or a compile
/// error naming the shape rule the input broke.
pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(IntentBody)] does not support generic version enums: a wire schema has \
             exactly one shape",
        ));
    }

    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            name,
            "#[derive(IntentBody)] applies to the outer per-venue version enum: an enum with one \
             newtype variant per published body version",
        ));
    };

    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "#[derive(IntentBody)] needs at least one version variant",
        ));
    }
    if data.variants.len() > usize::from(u8::MAX) + 1 {
        return Err(syn::Error::new_spanned(
            name,
            "#[derive(IntentBody)] supports at most 256 versions: the wire tag is one byte",
        ));
    }

    let mut encode_arms = Vec::with_capacity(data.variants.len());
    let mut decode_arms = Vec::with_capacity(data.variants.len());
    for (index, variant) in data.variants.iter().enumerate() {
        if let Some((eq, _)) = &variant.discriminant {
            return Err(syn::Error::new_spanned(
                eq,
                "#[derive(IntentBody)] does not support explicit discriminants: the version tag \
                 is the variant's declaration index, so append new versions at the end",
            ));
        }
        let payload_ty = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => &fields.unnamed[0].ty,
            _ => {
                return Err(syn::Error::new_spanned(
                    &variant.ident,
                    "#[derive(IntentBody)] version variants carry exactly one unnamed payload \
                     field, e.g. `V1(BodyV1)`",
                ));
            }
        };

        let ident = &variant.ident;
        let tag = proc_macro2::Literal::u8_suffixed(
            u8::try_from(index).expect("variant count checked above"),
        );

        encode_arms.push(quote! {
            Self::#ident(payload) => {
                let mut out = ::nexum_venue_sdk::body::__private::alloc::vec::Vec::new();
                out.push(#tag);
                ::nexum_venue_sdk::body::__private::borsh::to_writer(&mut out, payload).map_err(
                    |err| ::nexum_venue_sdk::body::BodyError::Encode {
                        version: #tag,
                        detail: ::nexum_venue_sdk::body::__private::alloc::string::ToString::to_string(&err),
                    },
                )?;
                ::core::result::Result::Ok(out)
            }
        });
        decode_arms.push(quote! {
            #tag => ::core::result::Result::Ok(Self::#ident(
                ::nexum_venue_sdk::body::__private::borsh::from_slice::<#payload_ty>(payload)
                    .map_err(|err| ::nexum_venue_sdk::body::BodyError::Malformed {
                        version: #tag,
                        detail: ::nexum_venue_sdk::body::__private::alloc::string::ToString::to_string(
                            &err,
                        ),
                    })?,
            )),
        });
    }

    Ok(quote! {
        #[automatically_derived]
        impl ::nexum_venue_sdk::body::IntentBody for #name {
            fn to_bytes(
                &self,
            ) -> ::core::result::Result<
                ::nexum_venue_sdk::body::__private::alloc::vec::Vec<u8>,
                ::nexum_venue_sdk::body::BodyError,
            > {
                match self {
                    #(#encode_arms)*
                }
            }

            fn from_bytes(
                bytes: &[u8],
            ) -> ::core::result::Result<Self, ::nexum_venue_sdk::body::BodyError> {
                let (version, payload) = bytes
                    .split_first()
                    .ok_or(::nexum_venue_sdk::body::BodyError::Empty)?;
                match *version {
                    #(#decode_arms)*
                    version => ::core::result::Result::Err(
                        ::nexum_venue_sdk::body::BodyError::UnknownVersion { version },
                    ),
                }
            }
        }
    })
}
