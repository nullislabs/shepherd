//! Small constructors and From conversions that build the WIT
//! `HostError` shape, used by every `Host` trait impl.

use crate::bindings::HostError;
use crate::bindings::nexum::host::types::HostErrorKind;
use crate::host::local_store_redb::StorageError;
use crate::host::provider_pool::ProviderError;

/// `Unsupported` (HTTP 501-style) error for capabilities the engine
/// reference build does not implement yet.
pub(crate) fn unimplemented(domain: &str, detail: impl Into<String>) -> HostError {
    HostError {
        domain: domain.into(),
        kind: HostErrorKind::Unsupported,
        code: 501,
        message: detail.into(),
        data: None,
    }
}

/// `Internal` (HTTP 500-style) error for unexpected backend failures.
pub(crate) fn internal_error(domain: &str, detail: impl Into<String>) -> HostError {
    HostError {
        domain: domain.into(),
        kind: HostErrorKind::Internal,
        code: 0,
        message: detail.into(),
        data: None,
    }
}

/// Project a [`ProviderError`] into the WIT-side `HostError`.
///
/// For an `Rpc` error the node-reported JSON-RPC `code` and structured
/// `data` payload are forwarded verbatim so the SDK revert classifier
/// can dispatch the ComposableCoW envelopes. Transport-side failures
/// carry no payload and fall back to `-32603` with `data = None`.
impl From<ProviderError> for HostError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::UnknownChain(id) => HostError {
                domain: "chain".into(),
                kind: HostErrorKind::Unsupported,
                code: 0,
                message: format!("chain {id} has no engine.toml RPC entry"),
                data: None,
            },
            ProviderError::InvalidParams { ref source, .. } => HostError {
                domain: "chain".into(),
                kind: HostErrorKind::InvalidInput,
                code: -32602,
                message: source.to_string(),
                data: None,
            },
            ProviderError::Rpc {
                ref source,
                code,
                ref data,
                ..
            } => HostError {
                domain: "chain".into(),
                kind: HostErrorKind::Internal,
                // Preserve the node-reported JSON-RPC code when the node
                // actually returned an `ErrorResp` (typically `-32000` for
                // `eth_call` reverts); fall back to `-32603` (Internal
                // error) for transport-side failures. Out-of-`i32` codes
                // saturate to `-32603` - real-world JSON-RPC codes fit
                // (range `-32768..-32000`).
                code: code.and_then(|c| i32::try_from(c).ok()).unwrap_or(-32603),
                message: source.to_string(),
                data: data.clone(),
            },
            other => internal_error("chain", other.to_string()),
        }
    }
}

/// Project a `cowprotocol::Error` from the orderbook into the WIT-side
/// `HostError`.
///
/// For an `OrderbookApi` reply the JSON `ApiError` envelope is forwarded
/// in `data` so the guest can dispatch on `errorType`. Other variants
/// carry no structured payload and leave `data` as `None`. Both branches
/// use `kind = Denied`.
impl From<cowprotocol::Error> for HostError {
    fn from(err: cowprotocol::Error) -> Self {
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
}

/// Project a [`StorageError`] into the WIT-side `HostError` as an
/// `internal_error("local-store", ..)`, keeping the `local-store` shape
/// consistent across every store endpoint.
impl From<StorageError> for HostError {
    fn from(err: StorageError) -> Self {
        internal_error("local-store", err.to_string())
    }
}
