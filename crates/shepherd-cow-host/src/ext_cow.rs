//! The cow-api extension: `shepherd:cow/cow-api` wired through the
//! extension seam rather than hard-linked into the core host.
//!
//! Shape: a local `bindgen!` for the extension world, a `Host` impl for
//! the foreign `HostState<T>` reached through [`ExtState`], a payload
//! trait ([`CowBackend`]) the lattice `Ext` member satisfies, and an
//! [`Extension`] bundling the linker hook with the capability namespace.
//!
//! The bindgen shares `nexum:host/types` with the core bindings via
//! `with`, so the extension's `HostError` is the same type the core host
//! constructs.

use std::time::Instant;

use alloy_chains::Chain;
use nexum_runtime::bindings::HostError;
use nexum_runtime::bindings::nexum::host::types::HostErrorKind;
use nexum_runtime::host::component::{BuilderContext, ComponentBuilder, RuntimeTypes};
use nexum_runtime::host::error::{internal_error, unimplemented};
use nexum_runtime::host::extension::Extension;
use nexum_runtime::host::state::{ExtState, HostState};
use nexum_runtime::manifest::NamespaceCaps;
use wasmtime::component::HasSelf;

use crate::cow::CowApi;
use crate::cow_orderbook::{CowApiError, OrderBookPool};

mod bindings {
    wasmtime::component::bindgen!({
        path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
        world: "shepherd:cow/cow-ext",
        imports: { default: async },
        with: { "nexum:host/types": nexum_runtime::bindings::nexum::host::types },
    });
}

/// Capability namespace this extension owns. Merged into capability
/// enforcement so a module importing `shepherd:cow/cow-api` validates.
pub const COW_CAPABILITIES: NamespaceCaps = NamespaceCaps {
    prefix: "shepherd:cow/",
    ifaces: &["cow-api"],
};

/// Extension payload providing a cow-api backend. The lattice `Ext` member
/// implements this so the `Host` impl can extract the backend generically.
pub trait CowBackend {
    /// The cow orderbook backend type.
    type Cow: CowApi;
    /// Borrow the cow backend.
    fn cow(&self) -> &Self::Cow;
}

/// The cow-api payload the reference engine ships in its `Ext` slot.
#[derive(Clone)]
pub struct ReferenceExt {
    /// `cow-api` backend - per-chain `OrderBookApi` clients + reqwest.
    pub cow: OrderBookPool,
}

impl CowBackend for ReferenceExt {
    type Cow = OrderBookPool;
    fn cow(&self) -> &OrderBookPool {
        &self.cow
    }
}

/// Builds the reference `Ext` payload: the cow orderbook pool from
/// `[extensions.cow]`. Lives here because the cow cone (and so the
/// [`OrderBookPool`] it opens) belongs to this extension crate, not the
/// core runtime.
pub struct ReferenceExtBuilder;

impl ComponentBuilder for ReferenceExtBuilder {
    type Output = ReferenceExt;

    async fn build(self, ctx: &BuilderContext<'_>) -> anyhow::Result<ReferenceExt> {
        let cow = OrderBookPool::from_config(ctx.config)?;
        Ok(ReferenceExt { cow })
    }
}

/// Build the cow extension for a lattice whose `Ext` payload carries a cow
/// backend. Wired at the composition root into `build_linker` and
/// capability enforcement.
pub fn extension<T>() -> Extension<T>
where
    T: RuntimeTypes,
    T::Ext: CowBackend,
{
    Extension {
        link: std::sync::Arc::new(|linker| {
            // Link only the cow-api interface. The whole-world
            // `CowExt::add_to_linker` would also re-add the shared
            // `nexum:host/types` instance, which the core event-module
            // linker already provides, tripping a "defined twice" error.
            bindings::shepherd::cow::cow_api::add_to_linker::<HostState<T>, HasSelf<HostState<T>>>(
                linker,
                |s| s,
            )?;
            Ok(())
        }),
        capabilities: COW_CAPABILITIES,
    }
}

/// Project a `cowprotocol::Error` from the orderbook into the WIT-side
/// `HostError`.
///
/// For an `OrderbookApi` reply the JSON `ApiError` envelope is forwarded
/// in `data` so the guest can dispatch on `errorType`. Other variants
/// carry no structured payload and leave `data` as `None`. Both branches
/// use `kind = Denied`. Kept a free function rather than a `From` impl:
/// `HostError` and `cowprotocol::Error` are both foreign to this crate.
fn orderbook_error_to_host(err: cowprotocol::Error) -> HostError {
    let message = err.to_string();
    if let cowprotocol::Error::OrderbookApi { status, api } = err {
        let data = serde_json::to_string(&api).ok();
        return HostError {
            domain: "cow-api".into(),
            kind: HostErrorKind::Denied,
            code: i32::from(status),
            message,
            data,
        };
    }
    HostError {
        domain: "cow-api".into(),
        kind: HostErrorKind::Denied,
        code: 0,
        message,
        data: None,
    }
}

impl<T> bindings::shepherd::cow::cow_api::Host for HostState<T>
where
    T: RuntimeTypes,
    T::Ext: CowBackend,
{
    async fn request(
        &mut self,
        chain_id: u64,
        method: String,
        path: String,
        body: Option<String>,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        let chain = Chain::from_id(chain_id);
        tracing::debug!(chain_id, %method, %path, "cow-api::request");
        // The guest hands us a free-form method string; normalise to
        // uppercase so `get` and `GET` both resolve, then type it. The
        // allowlist itself lives behind the seam.
        let method = match http::Method::from_bytes(method.to_ascii_uppercase().as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                return Err(HostError {
                    domain: "cow-api".into(),
                    kind: HostErrorKind::InvalidInput,
                    code: 0,
                    message: format!("unsupported HTTP method: {method}"),
                    data: None,
                });
            }
        };
        let result = match self
            .ext()
            .cow()
            .request(chain, method, &path, body.as_deref())
            .await
        {
            Ok(body) => Ok(body),
            Err(CowApiError::UnknownChain(id)) => Err(unimplemented(
                "cow-api",
                format!("chain {id} not in cowprotocol"),
            )),
            Err(CowApiError::BadMethod(m)) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: format!("unsupported HTTP method: {m}"),
                data: None,
            }),
            Err(CowApiError::BadPath(msg)) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: msg,
                data: None,
            }),
            Err(CowApiError::HttpError { status, body }) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::Internal,
                code: status as i32,
                message: format!("HTTP {status}"),
                data: Some(body),
            }),
            Err(err) => Err(internal_error("cow-api", err.to_string())),
        };
        tracing::trace!(elapsed_ms = ?start.elapsed(), "cow-api::request done");
        result
    }

    async fn submit_order(
        &mut self,
        chain_id: u64,
        order_data: Vec<u8>,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        let chain = Chain::from_id(chain_id);
        tracing::debug!(chain_id, bytes = order_data.len(), "cow-api::submit-order");
        let result = match self.ext().cow().submit_order_json(chain, &order_data).await {
            Ok(uid) => Ok(alloy_primitives::hex::encode_prefixed(uid.as_slice())),
            Err(CowApiError::UnknownChain(id)) => Err(unimplemented(
                "cow-api",
                format!("chain {id} not in cowprotocol"),
            )),
            Err(CowApiError::Decode(err)) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: format!("invalid OrderCreation JSON: {err}"),
                data: None,
            }),
            Err(CowApiError::Orderbook(err)) => Err(orderbook_error_to_host(err)),
            Err(err) => Err(internal_error("cow-api", err.to_string())),
        };
        tracing::trace!(elapsed_ms = ?start.elapsed(), "cow-api::submit-order done");
        let outcome = if result.is_ok() { "ok" } else { "err" };
        metrics::counter!(
            "shepherd_cow_api_submit_total",
            "chain_id" => chain_id.to_string(),
            "outcome" => outcome,
        )
        .increment(1);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowprotocol::error::ApiError;

    #[test]
    fn orderbook_api_error_is_forwarded_in_data() {
        // The orderbook rejects with a typed envelope. The mapping
        // must serialise it into HostError.data so the guest can
        // dispatch on `errorType`.
        let api = ApiError {
            error_type: "DuplicatedOrder".to_owned(),
            description: "order already exists".to_owned(),
            data: None,
        };
        let err = cowprotocol::Error::OrderbookApi { status: 400, api };

        let host_err = orderbook_error_to_host(err);

        assert!(matches!(host_err.kind, HostErrorKind::Denied));
        assert_eq!(host_err.code, 400);
        let data = host_err.data.expect("orderbook envelope forwarded");
        let parsed: ApiError = serde_json::from_str(&data).expect("data is ApiError JSON");
        assert_eq!(parsed.error_type, "DuplicatedOrder");
        assert_eq!(parsed.description, "order already exists");
    }

    #[test]
    fn orderbook_api_error_preserves_optional_data_field() {
        // ApiError carries an optional `data` field of its own. The
        // forward must round-trip it so the guest sees what the
        // orderbook actually returned.
        let api = ApiError {
            error_type: "InsufficientFee".to_owned(),
            description: "fee too low".to_owned(),
            data: Some(serde_json::json!({"min_fee": "1234"})),
        };
        let err = cowprotocol::Error::OrderbookApi { status: 400, api };

        let host_err = orderbook_error_to_host(err);

        let data = host_err.data.expect("envelope forwarded");
        let parsed: ApiError = serde_json::from_str(&data).expect("round-trip");
        assert_eq!(
            parsed.data.expect("inner data preserved")["min_fee"],
            "1234"
        );
    }

    #[test]
    fn non_envelope_cowprotocol_error_leaves_data_none() {
        // Transport / serde / unexpected-status errors don't carry a
        // structured ApiError; the guest classifier handles the
        // None-data case via its TryNextBlock safe default.
        let err = cowprotocol::Error::UnexpectedStatus {
            status: 502,
            body: "<html>upstream</html>".to_owned(),
        };

        let host_err = orderbook_error_to_host(err);

        assert!(host_err.data.is_none());
        assert_eq!(host_err.code, 0);
        assert!(matches!(host_err.kind, HostErrorKind::Denied));
    }
}
