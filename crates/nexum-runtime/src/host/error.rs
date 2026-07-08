//! Small constructors and From conversions that build the WIT error
//! shapes: the chain interface's `chain-error` and the per-interface
//! `Fault` the store interfaces report. `fault_label` / `fault_message`
//! project a reported `Fault` into stable metric and log fields.

use crate::bindings::nexum::host::chain::{ChainError, RpcError};
use crate::bindings::nexum::host::types::{Fault, RateLimit};
use crate::host::local_store_redb::StorageError;
use crate::host::provider_pool::ProviderError;

/// `Denied` chain fault for a request the host policy refused to
/// forward, such as a method outside the permitted read surface.
pub(crate) fn chain_denied(detail: impl Into<String>) -> ChainError {
    ChainError::Fault(Fault::Denied(detail.into()))
}

/// Stable snake_case label for a [`Fault`], used as a metric label and
/// structured-log `kind` field. Mirrors the SDK `HostFault::label`
/// vocabulary.
pub(crate) fn fault_label(fault: &Fault) -> &'static str {
    match fault {
        Fault::Unsupported(_) => "unsupported",
        Fault::Unavailable(_) => "unavailable",
        Fault::Denied(_) => "denied",
        Fault::RateLimited(_) => "rate_limited",
        Fault::Timeout => "timeout",
        Fault::InvalidInput(_) => "invalid_input",
        Fault::Internal(_) => "internal",
    }
}

/// Human-readable detail carried by a [`Fault`], for the log `message`
/// field. The payload-bearing cases carry their own detail; the two
/// payload-free cases render a fixed phrase.
pub(crate) fn fault_message(fault: &Fault) -> &str {
    match fault {
        Fault::Unsupported(m)
        | Fault::Unavailable(m)
        | Fault::Denied(m)
        | Fault::InvalidInput(m)
        | Fault::Internal(m) => m,
        Fault::RateLimited(_) => "rate limited",
        Fault::Timeout => "timeout",
    }
}

/// Project a [`ProviderError`] into the chain `chain-error`.
///
/// A structured JSON-RPC `ErrorResp` (the node returned a `code`,
/// typically `-32000` for an `eth_call` revert) becomes a
/// [`ChainError::Rpc`] carrying that code and any decoded revert bytes,
/// so the SDK revert classifier can dispatch the ComposableCoW
/// envelopes. Everything else - transport failures, an unknown chain,
/// bad params - becomes a shared [`Fault`].
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

/// Classify a transport-level RPC failure into a [`Fault`]. HTTP 429
/// maps to `rate-limited`, 503 / a dropped backend to `unavailable`,
/// and a timed-out request to `timeout`; anything else defaults to
/// `unavailable`.
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

/// The `local-store` interface is the failure domain, so the fault omits
/// the redundant subsystem tag.
impl From<StorageError> for Fault {
    fn from(err: StorageError) -> Self {
        Fault::Internal(err.to_string())
    }
}
