//! Constructors and `From` conversions building the WIT error shapes
//! (`chain-error`, `Fault`); `fault_label` / `fault_message` project a
//! `Fault` into metric and log fields.

use crate::bindings::nexum::host::chain::{ChainError, RpcError};
use crate::bindings::nexum::host::types::{Fault, RateLimit};
use crate::host::local_store_redb::StorageError;
use crate::host::provider_pool::ProviderError;

/// `Denied` chain fault for a request the host policy refused.
pub(crate) fn chain_denied(detail: impl Into<String>) -> ChainError {
    ChainError::Fault(Fault::Denied(detail.into()))
}

/// Stable snake_case label for a [`Fault`], for metric and log `kind` fields.
pub fn fault_label(fault: &Fault) -> &'static str {
    use nexum_world::FaultLabel as Label;
    match fault {
        Fault::Unsupported(_) => Label::Unsupported,
        Fault::Unavailable(_) => Label::Unavailable,
        Fault::Denied(_) => Label::Denied,
        Fault::RateLimited(_) => Label::RateLimited,
        Fault::Timeout => Label::Timeout,
        Fault::InvalidInput(_) => Label::InvalidInput,
        Fault::Internal(_) => Label::Internal,
    }
    .into()
}

/// Human-readable detail carried by a [`Fault`], for the log `message` field.
pub fn fault_message(fault: &Fault) -> std::borrow::Cow<'_, str> {
    match fault {
        Fault::Unsupported(m)
        | Fault::Unavailable(m)
        | Fault::Denied(m)
        | Fault::InvalidInput(m)
        | Fault::Internal(m) => std::borrow::Cow::Borrowed(m),
        Fault::RateLimited(rl) => match rl.retry_after_ms {
            Some(ms) => std::borrow::Cow::Owned(format!("rate limited, retry after {ms} ms")),
            None => std::borrow::Cow::Borrowed("rate limited"),
        },
        Fault::Timeout => std::borrow::Cow::Borrowed("timeout"),
    }
}

/// Project a [`ProviderError`] into `chain-error`: a structured JSON-RPC
/// `ErrorResp` becomes [`ChainError::Rpc`] with its code and revert bytes,
/// everything else a shared [`Fault`].
impl From<ProviderError> for ChainError {
    fn from(err: ProviderError) -> Self {
        match err {
            ProviderError::UnknownChain(id) => ChainError::Fault(Fault::Unsupported(format!(
                "chain {id} has no engine.toml RPC entry"
            ))),
            ProviderError::Connect { chain, source } => ChainError::Fault(Fault::Unavailable(
                format!("connect chain {chain}: {source}"),
            )),
            ProviderError::ConnectUrl { chain, source } => ChainError::Fault(Fault::Unavailable(
                format!("connect chain {chain}: invalid URL: {source}"),
            )),
            ProviderError::InvalidParams { source, .. } => {
                ChainError::Fault(Fault::InvalidInput(source.to_string()))
            }
            // The configured per-request timeout elapsed. The dedicated
            // timeout fault lets a guest tell a slow node apart from a
            // revert or an unreachable endpoint.
            ProviderError::Timeout { .. } => ChainError::Fault(Fault::Timeout),
            // Boot-time misconfiguration: never reaches a guest (the
            // engine aborts at startup), but the match must stay total.
            ProviderError::ZeroTimeout { .. } => {
                ChainError::Fault(Fault::Internal("request_timeout_secs must not be 0".into()))
            }
            // A structured JSON-RPC error response: `code` is `Some`.
            ProviderError::Rpc {
                code: Some(code),
                data,
                ref source,
                ..
            } => ChainError::Rpc(RpcError {
                // Preserve the node-reported JSON-RPC code. A code outside
                // `i32` is a JSON-RPC spec violation, clamped to `-32603`
                // Internal error.
                code: i32::try_from(code).unwrap_or(-32603),
                message: source.to_string(),
                data,
            }),
            // Lets a guest tell "the node reverted" apart from "the node
            // was unreachable / timed out".
            ProviderError::Rpc { source, .. } => ChainError::Fault(transport_fault(&source)),
        }
    }
}

/// Classify a transport RPC failure: 429 to `rate-limited`, 503 or a dropped
/// backend to `unavailable`, a timeout to `timeout`, else `unavailable`.
fn transport_fault(source: &alloy_transport::TransportError) -> Fault {
    use alloy_transport::TransportErrorKind;
    if let Some(kind) = source.as_transport_err() {
        match kind {
            TransportErrorKind::HttpError(http) if http.status == 429 => {
                return Fault::RateLimited(RateLimit {
                    retry_after_ms: None,
                });
            }
            TransportErrorKind::HttpError(http) if http.status == 503 => {
                return Fault::Unavailable(source.to_string());
            }
            TransportErrorKind::BackendGone | TransportErrorKind::PubsubUnavailable => {
                return Fault::Unavailable(source.to_string());
            }
            _ => {}
        }
    }
    let msg = source.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        Fault::Timeout
    } else {
        Fault::Unavailable(msg)
    }
}

/// Project a [`StorageError`]: quota breach to `denied`, a per-batch cap to
/// `invalid-input`, else `internal`.
impl From<StorageError> for Fault {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::QuotaExceeded { .. } => Fault::Denied(err.to_string()),
            StorageError::ApplyOpsExceeded { .. } | StorageError::ApplyBytesExceeded { .. } => {
                Fault::InvalidInput(err.to_string())
            }
            _ => Fault::Internal(err.to_string()),
        }
    }
}
