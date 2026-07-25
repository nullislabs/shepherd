//! Conversions between the failure vocabularies an adapter touches: the
//! wire [`Fault`] its exports return, the SDK-neutral [`host::Fault`] the
//! transport seams speak, and the [`VenueError`] the intent face reports;
//! plus [`VenueFault`], the owned client-side mirror of the wire error.
//!
//! Conversions are lossy only downward (a structured case folds to a
//! string case, never the reverse), so `?` preserves the most structured
//! form the target vocabulary can carry.

use nexum_sdk::host;
use strum::IntoStaticStr;

use crate::bindings::nexum::host::types::RateLimit as WireRateLimit;
use crate::client::ClientError;
use crate::{Fault, RateLimit, VenueError};

/// Owned mirror of the wire `venue-error`: what typed client code reports
/// when the registry or a venue refuses. `IntoStaticStr` yields a
/// snake_case label per case.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum VenueFault {
    /// No adapter is registered under the named venue id.
    #[error("unknown venue")]
    UnknownVenue,
    /// The venue rejected the body as malformed.
    #[error("invalid body: {0}")]
    InvalidBody(String),
    /// The venue does not support the operation.
    #[error("unsupported")]
    Unsupported,
    /// The venue or a policy refused the call.
    #[error("denied: {0}")]
    Denied(String),
    /// The venue throttled the call.
    #[error("rate limited{}", retry_after_ms.map_or_else(String::new, |ms| format!(", retry after {ms} ms")))]
    RateLimited {
        /// Venue-suggested wait before retrying, in milliseconds.
        retry_after_ms: Option<u64>,
    },
    /// The venue is temporarily unreachable or failing.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// The call timed out.
    #[error("timeout")]
    Timeout,
    /// The receipt is empty or structurally invalid.
    #[error("invalid receipt")]
    InvalidReceipt,
    /// The venue-returned identifier disagrees with the locally derived
    /// one.
    #[error("receipt mismatch")]
    ReceiptMismatch,
}

/// Lift the wire error into the owned mirror; exhaustive, so a new WIT
/// case fails to compile here.
impl From<VenueError> for VenueFault {
    fn from(err: VenueError) -> Self {
        match err {
            VenueError::UnknownVenue => Self::UnknownVenue,
            VenueError::InvalidBody(s) => Self::InvalidBody(s),
            VenueError::Unsupported => Self::Unsupported,
            VenueError::Denied(s) => Self::Denied(s),
            VenueError::RateLimited(rl) => Self::RateLimited {
                retry_after_ms: rl.retry_after_ms,
            },
            VenueError::Unavailable(s) => Self::Unavailable(s),
            VenueError::Timeout => Self::Timeout,
            VenueError::InvalidReceipt => Self::InvalidReceipt,
            VenueError::ReceiptMismatch => Self::ReceiptMismatch,
        }
    }
}

/// Lift the wire fault into the SDK-neutral vocabulary the transport
/// seams speak; exhaustive, so a new WIT case fails to compile here.
pub fn fault_into_sdk(fault: Fault) -> host::Fault {
    match fault {
        Fault::Unsupported(s) => host::Fault::Unsupported(s),
        Fault::Unavailable(s) => host::Fault::Unavailable(s),
        Fault::Denied(s) => host::Fault::Denied(s),
        Fault::RateLimited(rl) => host::Fault::RateLimited(host::RateLimit {
            retry_after_ms: rl.retry_after_ms,
        }),
        Fault::Timeout => host::Fault::Timeout,
        Fault::InvalidInput(s) => host::Fault::InvalidInput(s),
        Fault::Internal(s) => host::Fault::Internal(s),
    }
}

/// Lower the SDK-neutral fault back into the wire fault an adapter's
/// `init` returns. `host::Fault` is `#[non_exhaustive]`, so a future case
/// lands as `internal` carrying its `Display` detail.
impl From<host::Fault> for Fault {
    fn from(fault: host::Fault) -> Self {
        match fault {
            host::Fault::Unsupported(s) => Fault::Unsupported(s),
            host::Fault::Unavailable(s) => Fault::Unavailable(s),
            host::Fault::Denied(s) => Fault::Denied(s),
            host::Fault::RateLimited(rl) => Fault::RateLimited(WireRateLimit {
                retry_after_ms: rl.retry_after_ms,
            }),
            host::Fault::Timeout => Fault::Timeout,
            host::Fault::InvalidInput(s) => Fault::InvalidInput(s),
            host::Fault::Internal(s) => Fault::Internal(s),
            other => Fault::Internal(other.to_string()),
        }
    }
}

/// Fold a transport fault into the venue error an intent function
/// returns: `denied`, `rate-limited`, `timeout`, and `unsupported` map
/// structurally; the caller-shaped cases (`invalid-input`, `internal`)
/// fold to retryable `unavailable`, since inside an intent function the
/// caller is the adapter itself.
impl From<host::Fault> for VenueError {
    fn from(fault: host::Fault) -> Self {
        match fault {
            host::Fault::Denied(s) => VenueError::Denied(s),
            host::Fault::Unsupported(_) => VenueError::Unsupported,
            host::Fault::RateLimited(rl) => VenueError::RateLimited(RateLimit {
                retry_after_ms: rl.retry_after_ms,
            }),
            host::Fault::Timeout => VenueError::Timeout,
            host::Fault::Unavailable(s) => VenueError::Unavailable(s),
            other => VenueError::Unavailable(other.to_string()),
        }
    }
}

/// Fold a typed client failure into the SDK-neutral fault a keeper
/// handler returns: an encode failure, a misnamed venue, and an invalid
/// receipt are the caller's `invalid-input`; a receipt mismatch is a
/// venue integrity `internal`; other refusals map structurally.
impl From<ClientError> for host::Fault {
    fn from(err: ClientError) -> Self {
        match err {
            ClientError::Body(body) => host::Fault::InvalidInput(body.to_string()),
            ClientError::Venue(fault) => match fault {
                VenueFault::UnknownVenue => host::Fault::InvalidInput(fault.to_string()),
                VenueFault::InvalidBody(s) => host::Fault::InvalidInput(s),
                VenueFault::Unsupported => host::Fault::Unsupported(fault.to_string()),
                VenueFault::Denied(s) => host::Fault::Denied(s),
                VenueFault::RateLimited { retry_after_ms } => {
                    host::Fault::RateLimited(host::RateLimit { retry_after_ms })
                }
                VenueFault::Unavailable(s) => host::Fault::Unavailable(s),
                VenueFault::Timeout => host::Fault::Timeout,
                VenueFault::InvalidReceipt => host::Fault::InvalidInput(fault.to_string()),
                VenueFault::ReceiptMismatch => host::Fault::Internal(fault.to_string()),
            },
        }
    }
}

/// Fold a wasi:http fetch failure into the venue error an intent
/// function returns: an allowlist refusal stays `denied`, a timeout is
/// `timeout`, and transport failures are retryable `unavailable`.
impl From<nexum_sdk::http::FetchError> for VenueError {
    fn from(err: nexum_sdk::http::FetchError) -> Self {
        use nexum_sdk::http::FetchError;
        match err {
            FetchError::Denied => VenueError::Denied(err.to_string()),
            FetchError::Timeout(_) => VenueError::Timeout,
            // `FetchError` is `#[non_exhaustive]`: a future transport
            // case folds to retryable `unavailable` with its detail.
            _ => VenueError::Unavailable(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use nexum_sdk::host;

    use crate::{Fault, VenueError};

    #[test]
    fn wire_fault_round_trips_through_sdk() {
        let cases = [
            Fault::Unsupported("u".into()),
            Fault::Unavailable("u".into()),
            Fault::Denied("d".into()),
            Fault::RateLimited(crate::bindings::nexum::host::types::RateLimit {
                retry_after_ms: Some(250),
            }),
            Fault::Timeout,
            Fault::InvalidInput("i".into()),
            Fault::Internal("i".into()),
        ];
        for case in cases {
            let there = super::fault_into_sdk(case.clone());
            assert_eq!(Fault::from(there), case);
        }
    }

    #[test]
    fn transport_fault_folds_to_venue_error_by_shape() {
        assert_eq!(
            VenueError::from(host::Fault::Denied("nope".into())),
            VenueError::Denied("nope".into()),
        );
        assert_eq!(VenueError::from(host::Fault::Timeout), VenueError::Timeout);
        assert!(matches!(
            VenueError::from(host::Fault::RateLimited(host::RateLimit {
                retry_after_ms: Some(250),
            })),
            VenueError::RateLimited(rl) if rl.retry_after_ms == Some(250)
        ));
        assert!(matches!(
            VenueError::from(host::Fault::InvalidInput("bug".into())),
            VenueError::Unavailable(_)
        ));
    }

    #[test]
    fn fetch_error_folds_to_venue_error_by_shape() {
        use nexum_sdk::http::FetchError;
        assert!(matches!(
            VenueError::from(FetchError::Denied),
            VenueError::Denied(_)
        ));
        assert!(matches!(
            VenueError::from(FetchError::Transport("reset".into())),
            VenueError::Unavailable(_)
        ));
        assert!(matches!(
            VenueError::from(FetchError::InvalidRequest("bad url".into())),
            VenueError::Unavailable(_)
        ));
    }
}
