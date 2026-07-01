//! Orderbook submission error classification.
//!
//! Maps `cow_api::submit_order` failures into a typed [`RetryAction`]
//! the lifecycle layer dispatches on. The orderbook returns a typed
//! [`ApiError`] JSON body on permanent / transient failures; the host
//! forwards that JSON in `host-error.data` (once the chain backend
//! supports it - see the ADR follow-up). Until then,
//! [`classify_api_error`] falls back to `TryNextBlock` so a flaky
//! orderbook does not poison still-valid orders.
//!
//! [`ApiError`]: cowprotocol::error::ApiError

use cowprotocol::error::ApiError;
use strum::IntoStaticStr;

/// What the lifecycle layer should do after a failed submission.
///
/// Mirrors the retry contract: `TryNextBlock` /
/// `BackoffSeconds(s)` / `Drop`. The `Backoff` arm has no producer
/// today because cowprotocol's `retry_hint()` is bool-only; the
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

/// Best-effort decode of the orderbook's typed [`ApiError`] body from
/// the `host-error.data` field a guest receives on a failed
/// `cow_api::submit_order` call. Returns `None` when the host did not
/// forward a payload, or when the payload does not parse as
/// `ApiError`.
pub fn try_decode_api_error(host_error_data: Option<&str>) -> Option<ApiError> {
    serde_json::from_str::<ApiError>(host_error_data?).ok()
}

/// Classify the host's failure-side payload (the JSON the orderbook
/// returned) into a [`RetryAction`].
///
/// - Retriable kinds per `OrderPostErrorKind::is_retriable` (today:
///   `InsufficientFee`, `TooManyLimitOrders`, `PriceExceedsMarketPrice`)
///   → `TryNextBlock`.
/// - Recognised non-retriable kinds → `Drop`.
/// - Payload absent or unparseable → `TryNextBlock` (safe default; a
///   flaky orderbook should not be treated as a permanent rejection).
///
/// # Example
///
/// ```
/// use shepherd_sdk::cow::{classify_api_error, RetryAction};
///
/// // Transient: orderbook rejects with InsufficientFee -> retry next block.
/// let transient = serde_json::json!({
///     "errorType": "InsufficientFee",
///     "description": "fee too low",
/// })
/// .to_string();
/// assert_eq!(classify_api_error(Some(&transient)), RetryAction::TryNextBlock);
///
/// // Permanent: InvalidSignature -> drop the watch / placement.
/// let permanent = serde_json::json!({
///     "errorType": "InvalidSignature",
///     "description": "bad sig",
/// })
/// .to_string();
/// assert_eq!(classify_api_error(Some(&permanent)), RetryAction::Drop);
///
/// // No payload (e.g. host-error.data is None) -> safe default.
/// assert_eq!(classify_api_error(None), RetryAction::TryNextBlock);
/// ```
pub fn classify_api_error(host_error_data: Option<&str>) -> RetryAction {
    match try_decode_api_error(host_error_data) {
        Some(api) if api.retry_hint() => RetryAction::TryNextBlock,
        Some(_) => RetryAction::Drop,
        None => RetryAction::TryNextBlock,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_for(error_type: &str) -> String {
        serde_json::json!({
            "errorType": error_type,
            "description": "test",
        })
        .to_string()
    }

    #[test]
    fn retriable_kinds_yield_try_next_block() {
        for kind in [
            "InsufficientFee",
            "TooManyLimitOrders",
            "PriceExceedsMarketPrice",
        ] {
            assert_eq!(
                classify_api_error(Some(&body_for(kind))),
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
                classify_api_error(Some(&body_for(kind))),
                RetryAction::Drop,
                "{kind}",
            );
        }
    }

    #[test]
    fn unknown_kind_yields_drop() {
        // `Unknown(_)` is non-retriable per cowprotocol's classifier.
        assert_eq!(
            classify_api_error(Some(&body_for("NewlyMintedErrorType"))),
            RetryAction::Drop,
        );
    }

    #[test]
    fn missing_data_yields_try_next_block() {
        assert_eq!(classify_api_error(None), RetryAction::TryNextBlock);
    }

    #[test]
    fn malformed_data_yields_try_next_block() {
        assert_eq!(
            classify_api_error(Some("<html>upstream</html>")),
            RetryAction::TryNextBlock,
        );
    }

    #[test]
    fn try_decode_round_trips() {
        let body = body_for("InsufficientFee");
        let api = try_decode_api_error(Some(&body)).expect("decode");
        assert_eq!(api.error_type, "InsufficientFee");
    }
}
