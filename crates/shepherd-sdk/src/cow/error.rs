//! Typed `shepherd:cow/cow-api` error surface and orderbook rejection
//! classification.
//!
//! [`CowApiError`] mirrors the WIT `cow-api-error` variant: a shared
//! host [`Fault`], a raw [`HttpFailure`], or a typed [`OrderRejection`]
//! the host parsed once from the orderbook's `{errorType, description}`
//! envelope. The guest dispatches on the variant directly, so no
//! second JSON decode of a failure body happens strategy-side.
//!
//! [`classify_api_error`] maps a decoded [`OrderRejection`] into the
//! keeper [`RetryAction`] the retry ledger dispatches on;
//! [`classify_submit_error`] widens the table to the whole
//! [`CowApiError`] surface.

use nexum_sdk::host::{Fault, HostFault};
use strum::IntoStaticStr;

pub use nexum_sdk::keeper::RetryAction;

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
    /// quote), serialised to a JSON string by the host via
    /// `serde_json::Value::to_string`.
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

/// Classify a decoded orderbook [`OrderRejection`] into the keeper
/// [`RetryAction`] via the shipped CoW classification table
/// ([`cow_venue::classify`]): the `errorType` drives the action -
/// transient types retry next block, throttle types back off, permanent
/// types drop. The one invariant the table enforces: an `errorType`
/// absent from the data is permanent, never retried every block forever.
///
/// Non-`Rejected` failures carry no `error_type`; classify those with
/// [`classify_submit_error`].
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
    cow_venue::classify(&rejection.error_type)
}

/// Whether the rejection says the orderbook already holds this exact
/// order, per the classification table's `already-submitted` flag
/// (`DuplicatedOrder`, plus the `DuplicateOrder` spelling older
/// deployments emit). Already-submitted is success wearing an error
/// status - dropping the watch on it would kill every future tranche of
/// a TWAP - so the caller records the `submitted:` receipt and keeps the
/// watch.
pub fn is_already_submitted(rejection: &OrderRejection) -> bool {
    cow_venue::is_already_submitted(&rejection.error_type)
}

/// Classify a whole [`CowApiError`] from a submission into the keeper
/// [`RetryAction`].
///
/// A typed rejection dispatches through [`classify_api_error`]; a
/// rate-limit fault with server guidance becomes `Backoff` (hint
/// rounded up to whole seconds, minimum one). Everything else
/// (transport faults, raw HTTP errors, unguided rate limits) is
/// transient -> `TryNextBlock`, so a flaky orderbook never poisons a
/// still-valid order.
pub fn classify_submit_error(err: &CowApiError) -> RetryAction {
    match err {
        CowApiError::Rejected(rejection) => classify_api_error(rejection),
        CowApiError::Fault(Fault::RateLimited(limit)) => match limit.retry_after_ms {
            Some(ms) => RetryAction::Backoff {
                seconds: ms.div_ceil(1000).max(1),
            },
            None => RetryAction::TryNextBlock,
        },
        _ => RetryAction::TryNextBlock,
    }
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
        for kind in ["InsufficientFee", "PriceExceedsMarketPrice"] {
            assert_eq!(
                classify_api_error(&rejection(kind)),
                RetryAction::TryNextBlock,
                "{kind}",
            );
        }
    }

    /// A throttle errorType backs off rather than retrying next block,
    /// so the table reaches every retry arm - the `Backoff` producer the
    /// hand-coded classifier lacked.
    #[test]
    fn throttle_kind_yields_backoff() {
        assert_eq!(
            classify_api_error(&rejection("TooManyLimitOrders")),
            RetryAction::Backoff { seconds: 30 },
        );
    }

    #[test]
    fn permanent_kinds_yield_drop() {
        for kind in [
            "InvalidSignature",
            "WrongOwner",
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

    /// Both spellings pin: the orderbook emits `DuplicatedOrder`, the
    /// older `DuplicateOrder` form must classify identically. Neither
    /// may drop the watch - that would kill every future tranche.
    #[test]
    fn duplicated_order_is_already_submitted_and_never_drops() {
        for kind in ["DuplicatedOrder", "DuplicateOrder"] {
            assert!(is_already_submitted(&rejection(kind)), "{kind}");
            assert_eq!(
                classify_api_error(&rejection(kind)),
                RetryAction::TryNextBlock,
                "{kind}",
            );
        }
        assert!(!is_already_submitted(&rejection("InsufficientFee")));
        assert!(!is_already_submitted(&rejection("InvalidSignature")));
    }

    #[test]
    fn submit_error_rejection_routes_through_the_table() {
        assert_eq!(
            classify_submit_error(&CowApiError::Rejected(rejection("InvalidSignature"))),
            RetryAction::Drop,
        );
        assert_eq!(
            classify_submit_error(&CowApiError::Rejected(rejection("InsufficientFee"))),
            RetryAction::TryNextBlock,
        );
    }

    #[test]
    fn submit_error_rate_limit_hint_becomes_backoff_in_whole_seconds() {
        let limited = |ms| CowApiError::Fault(Fault::RateLimited(RateLimit { retry_after_ms: ms }));
        assert_eq!(
            classify_submit_error(&limited(Some(2_500))),
            RetryAction::Backoff { seconds: 3 },
        );
        // Sub-second hints round up to a full second, never to zero.
        assert_eq!(
            classify_submit_error(&limited(Some(1))),
            RetryAction::Backoff { seconds: 1 },
        );
        // No guidance -> plain next-block retry.
        assert_eq!(
            classify_submit_error(&limited(None)),
            RetryAction::TryNextBlock
        );
    }

    #[test]
    fn submit_error_transient_shapes_stay_try_next_block() {
        assert_eq!(
            classify_submit_error(&CowApiError::Fault(Fault::Timeout)),
            RetryAction::TryNextBlock,
        );
        assert_eq!(
            classify_submit_error(&CowApiError::Http(HttpFailure {
                status: 502,
                body: None,
            })),
            RetryAction::TryNextBlock,
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
