//! Conversions between the three failure vocabularies an adapter
//! touches: the wire [`Fault`] its exports return, the SDK-neutral
//! [`host::Fault`] the transport seams speak, and the [`VenueError`] the
//! intent face reports.
//!
//! Every conversion here is lossy only downward (a structured case folds
//! to a payload-bearing string case, never the reverse), so `?` in an
//! adapter always preserves the most structured form the target
//! vocabulary can carry.

use nexum_sdk::host;

use crate::bindings::nexum::host::types::RateLimit as WireRateLimit;
use crate::{Fault, RateLimit, VenueError};

/// Lift the wire fault into the SDK-neutral vocabulary the transport
/// seams and `nexum-sdk` helpers speak. Exhaustive: the wire enum is
/// this crate's own bindgen, so a new WIT case fails here first.
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
/// `init` returns, so a helper's `host::Fault` propagates with `?`.
///
/// Carries a wildcard arm because `host::Fault` is `#[non_exhaustive]`:
/// a future SDK case lands as `internal` carrying its `Display` detail.
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
/// structurally; `unavailable` keeps its detail; the caller-shaped cases
/// (`invalid-input`, `internal`) fold to retryable `unavailable` because
/// inside an intent function the transport's caller is the adapter
/// itself, never the module.
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

/// Fold a wasi:http fetch failure into the venue error an intent
/// function returns: an allowlist refusal stays `denied`, a timeout is
/// `timeout`, and transport failures (including a request the adapter
/// itself malformed) are retryable `unavailable`.
impl From<nexum_sdk::http::FetchError> for VenueError {
    fn from(err: nexum_sdk::http::FetchError) -> Self {
        use nexum_sdk::http::FetchError;
        match err {
            FetchError::Denied => VenueError::Denied(err.to_string()),
            FetchError::Timeout(_) => VenueError::Timeout,
            FetchError::Transport(_) | FetchError::InvalidRequest(_) => {
                VenueError::Unavailable(err.to_string())
            }
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
