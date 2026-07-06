//! Typed `shepherd:cow/cow-api` error surface and orderbook rejection
//! classification.
//!
//! [`CowApiError`] mirrors the WIT `cow-api-error` variant: a shared
//! host [`Fault`], a raw [`HttpFailure`], or a typed [`OrderRejection`]
//! the host parsed once from the orderbook's `{errorType, description}`
//! envelope. The guest dispatches on the variant directly, so no
//! second JSON decode of a failure body happens strategy-side.
//!
//! [`classify_api_error`] maps a decoded [`OrderRejection`] into a
//! [`RetryAction`] the lifecycle layer dispatches on.

use nexum_sdk::host::{Fault, HostFault};
use strum::IntoStaticStr;

/// A non-2xx orderbook reply with no typed rejection envelope. `body`
/// is the raw response text, foreign orderbook JSON kept verbatim: a
/// caller matches on `status` and reads `body` only for diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpFailure {
    /// HTTP status code.
    pub status: u16,
    /// Raw response body, when the host captured one.
    pub body: Option<String>,
}

/// A typed orderbook rejection of a submitted order, parsed once
/// host-side from the `{errorType, description, data}` envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderRejection {
    /// HTTP status returned with the rejection.
    pub status: u16,
    /// Machine-readable `errorType` (e.g. `"InsufficientFee"`).
    pub error_type: String,
    /// Human-readable description.
    pub description: String,
    /// The envelope's optional structured payload (e.g. a minimum-fee
    /// quote), re-encoded by the host as a JSON string.
    pub data: Option<String>,
}

/// Mirror of `shepherd:cow/cow-api.cow-api-error`. The domain-side
/// counterpart the [`bind_cow_host_via_wit_bindgen`](crate::bind_cow_host_via_wit_bindgen)
/// macro converts the per-cdylib wit-bindgen error into, so strategy
/// logic dispatches on one host-neutral type.
///
/// `IntoStaticStr` exposes the variant name as a snake_case `&'static
/// str`; [`HostFault::label`] refines the [`Fault`] case to the
/// embedded fault's own label so metric and log labels stay granular.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum CowApiError {
    /// A shared host fault (unsupported, timeout, transport down, ...).
    #[error(transparent)]
    Fault(Fault),
    /// A raw non-2xx HTTP reply without a typed rejection envelope.
    #[error("orderbook http {}", .0.status)]
    Http(HttpFailure),
    /// A typed orderbook rejection of a submitted order.
    #[error("orderbook rejected ({} {}): {}", .0.status, .0.error_type, .0.description)]
    Rejected(OrderRejection),
}

impl HostFault for CowApiError {
    fn fault(&self) -> Option<&Fault> {
        match self {
            CowApiError::Fault(f) => Some(f),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            CowApiError::Fault(f) => f.label(),
            other => other.into(),
        }
    }
}

/// What the lifecycle layer should do after a failed submission.
///
/// Mirrors the retry contract: `TryNextBlock` /
/// `BackoffSeconds(s)` / `Drop`. The `Backoff` arm has no producer
/// today because the retry classifier is bool-only; the
/// variant is kept so dispatch can grow into it once a server
/// `Retry-After` hint shows up.
///
/// `IntoStaticStr` exposes each variant as a snake_case `&'static
/// str` so the dispatch layer can record
/// `shepherd_cow_api_retry_total{action=...}` and surface the action
/// in `tracing::info!(retry_action = ...)` without an ad-hoc match
/// ladder.
#[derive(Debug, Eq, PartialEq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum RetryAction {
    /// Leave the watch / placement in place; the next event will
    /// re-attempt.
    TryNextBlock,
    /// Persist `next_attempt = now + seconds`. Reserved - no producer
    /// today (kept so the dispatch contract is stable).
    #[allow(dead_code)]
    Backoff {
        /// Seconds to wait before retrying.
        seconds: u64,
    },
    /// Remove the watch / mark as terminally rejected. The orderbook
    /// will not accept this body on a retry.
    Drop,
}

/// Classify a decoded orderbook [`OrderRejection`] into a
/// [`RetryAction`].
///
/// - Retriable `error_type`s (`InsufficientFee`, `TooManyLimitOrders`,
///   `PriceExceedsMarketPrice`) -> `TryNextBlock`.
/// - Every other (including unrecognised) kind -> `Drop`.
///
/// Non-`Rejected` failures (transport faults, raw HTTP errors) carry
/// no `error_type` and are not classified here; the caller treats them
/// as transient (leave the watch in place) so a flaky orderbook does
/// not poison a still-valid order.
///
/// # Example
///
/// ```
/// use shepherd_sdk::cow::{classify_api_error, OrderRejection, RetryAction};
///
/// // Transient: orderbook rejects with InsufficientFee -> retry next block.
/// let transient = OrderRejection {
///     status: 400,
///     error_type: "InsufficientFee".to_string(),
///     description: "fee too low".to_string(),
///     data: None,
/// };
/// assert_eq!(classify_api_error(&transient), RetryAction::TryNextBlock);
///
/// // Permanent: InvalidSignature -> drop the watch / placement.
/// let permanent = OrderRejection {
///     status: 400,
///     error_type: "InvalidSignature".to_string(),
///     description: "bad sig".to_string(),
///     data: None,
/// };
/// assert_eq!(classify_api_error(&permanent), RetryAction::Drop);
/// ```
pub fn classify_api_error(rejection: &OrderRejection) -> RetryAction {
    if is_retriable(&rejection.error_type) {
        RetryAction::TryNextBlock
    } else {
        RetryAction::Drop
    }
}

/// Orderbook `errorType` values the protocol treats as transient: a
/// fresh submission on a later block may succeed. Everything else
/// (including unrecognised types) is permanent. Mirrors the upstream
/// order-post retry classifier.
fn is_retriable(error_type: &str) -> bool {
    matches!(
        error_type,
        "InsufficientFee" | "TooManyLimitOrders" | "PriceExceedsMarketPrice"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexum_sdk::host::RateLimit;

    fn rejection(error_type: &str) -> OrderRejection {
        OrderRejection {
            status: 400,
            error_type: error_type.to_string(),
            description: "test".to_string(),
            data: None,
        }
    }

    #[test]
    fn retriable_kinds_yield_try_next_block() {
        for kind in [
            "InsufficientFee",
            "TooManyLimitOrders",
            "PriceExceedsMarketPrice",
        ] {
            assert_eq!(
                classify_api_error(&rejection(kind)),
                RetryAction::TryNextBlock,
                "{kind}",
            );
        }
    }

    #[test]
    fn permanent_kinds_yield_drop() {
        for kind in [
            "InvalidSignature",
            "WrongOwner",
            "DuplicateOrder",
            "UnsupportedToken",
            "InvalidAppData",
            "InvalidErc1271Signature",
        ] {
            assert_eq!(
                classify_api_error(&rejection(kind)),
                RetryAction::Drop,
                "{kind}",
            );
        }
    }

    #[test]
    fn unknown_kind_yields_drop() {
        assert_eq!(
            classify_api_error(&rejection("NewlyMintedErrorType")),
            RetryAction::Drop,
        );
    }

    #[test]
    fn fault_case_recovers_embedded_fault_and_label() {
        let err = CowApiError::Fault(Fault::Timeout);
        assert_eq!(err.fault(), Some(&Fault::Timeout));
        // Fault case refines the label to the embedded fault's own.
        assert_eq!(err.label(), "timeout");

        let rl = CowApiError::Fault(Fault::RateLimited(RateLimit {
            retry_after_ms: Some(250),
        }));
        assert_eq!(rl.label(), "rate_limited");
    }

    #[test]
    fn non_fault_cases_expose_variant_label_and_no_fault() {
        let http = CowApiError::Http(HttpFailure {
            status: 404,
            body: None,
        });
        assert_eq!(http.fault(), None);
        assert_eq!(http.label(), "http");

        let rejected = CowApiError::Rejected(rejection("InvalidSignature"));
        assert_eq!(rejected.fault(), None);
        assert_eq!(rejected.label(), "rejected");
    }
}
