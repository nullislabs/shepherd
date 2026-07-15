//! The cow-api extension: `shepherd:cow/cow-api` wired through the
//! extension seam rather than hard-linked into the core host.
//!
//! Shape: a local `bindgen!` for the extension world, a `Host` impl for
//! the foreign `HostState<T>` reached through [`ExtState`], a payload
//! trait ([`CowBackend`]) the lattice `Ext` member satisfies, and an
//! [`Extension`] bundling the linker hook with the capability namespace.
//!
//! The bindgen shares `nexum:host/types` with the core bindings via
//! `with`, so the `fault` the extension's `cow-api-error` embeds is the
//! same type the core host constructs.

use std::time::Instant;

use alloy_chains::Chain;
use nexum_runtime::bindings::nexum::host::types::{Fault, RateLimit};
use nexum_runtime::host::component::{BuilderContext, ComponentBuilder, RuntimeTypes};
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

use bindings::shepherd::cow::cow_api::{
    CowApiError as WitCowApiError, HttpFailure, OrderRejection,
};

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

/// Project the backend [`CowApiError`] into the WIT `cow-api-error`.
///
/// Local-shape failures (unknown chain, bad method/path, decode) become
/// a shared [`Fault`]; a transport-layer HTTP failure becomes an
/// [`Http`](bindings::shepherd::cow::cow_api::CowApiError::Http) case,
/// except 429 and a `reqwest` timeout, which carry a more precise
/// [`Fault::RateLimited`] / [`Fault::Timeout`] instead - mirroring the
/// chain-host's `transport_fault` classification; an orderbook
/// rejection envelope is parsed once here into a
/// [`Rejected`](bindings::shepherd::cow::cow_api::CowApiError::Rejected)
/// case so the guest never re-decodes the failure body.
fn cow_error_to_wit(err: CowApiError) -> WitCowApiError {
    match err {
        CowApiError::UnknownChain(chain) => WitCowApiError::Fault(Fault::Unsupported(format!(
            "chain {chain} not in cowprotocol"
        ))),
        CowApiError::BadMethod(m) => {
            WitCowApiError::Fault(Fault::InvalidInput(format!("unsupported HTTP method: {m}")))
        }
        CowApiError::BadPath(msg) => WitCowApiError::Fault(Fault::InvalidInput(msg)),
        CowApiError::HttpError { status: 429, .. } => {
            WitCowApiError::Fault(Fault::RateLimited(RateLimit {
                retry_after_ms: None,
            }))
        }
        CowApiError::HttpError { status, body } => WitCowApiError::Http(HttpFailure {
            status,
            body: Some(body),
        }),
        CowApiError::Network(e) if e.is_timeout() => WitCowApiError::Fault(Fault::Timeout),
        CowApiError::Network(e) => WitCowApiError::Fault(Fault::Unavailable(e.to_string())),
        CowApiError::Decode(e) => WitCowApiError::Fault(Fault::InvalidInput(format!(
            "invalid OrderCreation JSON: {e}"
        ))),
        CowApiError::Orderbook(e) => orderbook_error_to_wit(e),
    }
}

/// Map a `cowprotocol::Error` to WIT form.
///
/// An `OrderbookApi` reply is parsed once into a typed
/// [`OrderRejection`] carrying the orderbook's `errorType` /
/// `description` plus its optional structured `data` payload,
/// re-encoded as a JSON string. A non-2xx reply with an unparseable
/// body becomes an [`HttpFailure`]. Everything else is a host-side
/// [`Fault::Internal`].
fn orderbook_error_to_wit(err: cowprotocol::Error) -> WitCowApiError {
    match err {
        cowprotocol::Error::OrderbookApi { status, api } => {
            WitCowApiError::Rejected(OrderRejection {
                status,
                error_type: api.error_type,
                description: api.description,
                data: api.data.map(|d| d.to_string()),
            })
        }
        cowprotocol::Error::UnexpectedStatus { status, body } => {
            WitCowApiError::Http(HttpFailure {
                status,
                body: Some(body),
            })
        }
        other => WitCowApiError::Fault(Fault::Internal(other.to_string())),
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
    ) -> Result<String, WitCowApiError> {
        let start = Instant::now();
        let chain = Chain::from_id(chain_id);
        tracing::debug!(chain_id, %method, %path, "cow-api::request");
        // The guest hands us a free-form method string; normalise to
        // uppercase so `get` and `GET` both resolve, then type it. The
        // allowlist itself lives behind the seam.
        let method = match http::Method::from_bytes(method.to_ascii_uppercase().as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                return Err(WitCowApiError::Fault(Fault::InvalidInput(format!(
                    "unsupported HTTP method: {method}"
                ))));
            }
        };
        let result = self
            .ext()
            .cow()
            .request(chain, method, &path, body.as_deref())
            .await
            .map_err(cow_error_to_wit);
        tracing::trace!(elapsed_ms = ?start.elapsed(), "cow-api::request done");
        result
    }

    async fn submit_order(
        &mut self,
        chain_id: u64,
        order_data: Vec<u8>,
    ) -> Result<String, WitCowApiError> {
        let start = Instant::now();
        let chain = Chain::from_id(chain_id);
        tracing::debug!(chain_id, bytes = order_data.len(), "cow-api::submit-order");
        let result = self
            .ext()
            .cow()
            .submit_order_json(chain, &order_data)
            .await
            .map(|uid| alloy_primitives::hex::encode_prefixed(uid.as_slice()))
            .map_err(cow_error_to_wit);
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
    fn orderbook_api_error_becomes_typed_rejection() {
        // The orderbook rejects with a typed envelope. The mapping
        // parses it once, host-side, into an `order-rejection` so the
        // guest dispatches on `error-type` without a second decode.
        let api = ApiError {
            error_type: "DuplicatedOrder".to_owned(),
            description: "order already exists".to_owned(),
            data: Some(serde_json::json!({"min_fee": "1234"})),
        };
        let err = cowprotocol::Error::OrderbookApi { status: 400, api };

        let WitCowApiError::Rejected(rejection) = orderbook_error_to_wit(err) else {
            panic!("orderbook envelope must project to a typed rejection");
        };
        assert_eq!(rejection.status, 400);
        assert_eq!(rejection.error_type, "DuplicatedOrder");
        assert_eq!(rejection.description, "order already exists");
        // The envelope's structured payload survives as a JSON string.
        assert_eq!(rejection.data.as_deref(), Some(r#"{"min_fee":"1234"}"#));
    }

    #[test]
    fn unexpected_status_becomes_http_failure() {
        // A non-2xx reply with an unparseable body carries no typed
        // rejection; it surfaces as a raw http-failure with the body
        // preserved for diagnostics.
        let err = cowprotocol::Error::UnexpectedStatus {
            status: 502,
            body: "<html>upstream</html>".to_owned(),
        };

        let WitCowApiError::Http(http) = orderbook_error_to_wit(err) else {
            panic!("unexpected-status must project to an http-failure");
        };
        assert_eq!(http.status, 502);
        assert_eq!(http.body.as_deref(), Some("<html>upstream</html>"));
    }

    #[test]
    fn backend_http_error_projects_to_http_failure() {
        // The passthrough backend surfaces a non-2xx as `HttpError`;
        // it must reach the guest as an http-failure so a 404 is
        // matchable on `status`.
        let err = CowApiError::HttpError {
            status: 404,
            body: "not found".to_owned(),
        };

        let WitCowApiError::Http(http) = cow_error_to_wit(err) else {
            panic!("backend HttpError must project to an http-failure");
        };
        assert_eq!(http.status, 404);
        assert_eq!(http.body.as_deref(), Some("not found"));
    }

    #[test]
    fn unknown_chain_projects_to_unsupported_fault() {
        let err = CowApiError::UnknownChain(Chain::from_id(9999));
        assert!(matches!(
            cow_error_to_wit(err),
            WitCowApiError::Fault(Fault::Unsupported(_)),
        ));
    }

    #[test]
    fn backend_http_429_projects_to_rate_limited_fault() {
        // 429 is backpressure, not a rejection body the guest needs to
        // inspect - it must reach the shared rate-limited vocabulary
        // instead of a raw http-failure, unlike every other status.
        let err = CowApiError::HttpError {
            status: 429,
            body: "slow down".to_owned(),
        };

        assert!(matches!(
            cow_error_to_wit(err),
            WitCowApiError::Fault(Fault::RateLimited(RateLimit {
                retry_after_ms: None
            })),
        ));
    }

    #[tokio::test]
    async fn backend_network_timeout_projects_to_timeout_fault() {
        // A `reqwest` timeout (any phase - connect included) must
        // become `Fault::Timeout`, not the blanket `Fault::Unavailable`
        // every other transport failure gets. 10.255.255.1 is a
        // standard non-routable test address; the 1ms deadline fires
        // before a connection can complete.
        let client = reqwest::Client::new();
        let send_err = client
            .get("http://10.255.255.1/")
            .timeout(std::time::Duration::from_millis(1))
            .send()
            .await
            .expect_err("request against a non-routable address must fail");
        assert!(send_err.is_timeout(), "expected a timeout error");

        assert!(matches!(
            cow_error_to_wit(CowApiError::Network(send_err)),
            WitCowApiError::Fault(Fault::Timeout),
        ));
    }
}
