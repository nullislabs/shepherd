//! Declarative macro that generates the `WitBindgenHost` adapter for
//! CoW modules: the generic adapter plus the `CowApiHost` impl.
//!
//! Layers on `nexum_sdk::bind_host_via_wit_bindgen!`, which emits the
//! core adapter (`WitBindgenHost`, the `ChainHost` / `LocalStoreHost`
//! / `LoggingHost` impls, the error and level converters, and the
//! tracing wiring). This macro invokes it and adds the
//! [`CowApiHost`](crate::cow::CowApiHost) impl over the
//! `shepherd:cow/cow-api` import shims.
//!
//! The macro assumes the module compiles against `shepherd:cow/shepherd`
//! with `wit_bindgen::generate!({ ..., generate_all })`, so both the
//! `nexum::host::*` and `shepherd::cow::cow_api` output paths are in
//! scope at the call site, and that the module crate also depends on
//! `nexum-sdk` (the expansion names `::nexum_sdk` directly).
//!
//! Usage in a module's `lib.rs`:
//!
//! ```ignore
//! wit_bindgen::generate!({ /* ... */ });
//! shepherd_sdk::bind_cow_host_via_wit_bindgen!();
//! // Everything the generic macro emits is in scope, plus the
//! // `CowApiHost` impl for `WitBindgenHost`.
//! ```

/// Generate the generic `WitBindgenHost` adapter plus the `CowApiHost`
/// impl. See module docs.
#[macro_export]
macro_rules! bind_cow_host_via_wit_bindgen {
    () => {
        ::nexum_sdk::bind_host_via_wit_bindgen!();

        impl $crate::cow::CowApiHost for WitBindgenHost {
            fn submit_order(
                &self,
                chain_id: u64,
                body: &[u8],
            ) -> ::core::result::Result<::std::string::String, ::nexum_sdk::host::HostError> {
                shepherd::cow::cow_api::submit_order(chain_id, body).map_err(convert_err)
            }

            fn cow_api_request(
                &self,
                chain_id: u64,
                method: &str,
                path: &str,
                body: ::core::option::Option<&str>,
            ) -> ::core::result::Result<::std::string::String, ::nexum_sdk::host::HostError> {
                shepherd::cow::cow_api::request(chain_id, method, path, body).map_err(convert_err)
            }
        }
    };
}
