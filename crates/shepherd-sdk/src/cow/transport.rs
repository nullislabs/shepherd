//! Transitional venue transport over the legacy `shepherd:cow/cow-api`
//! seam.
//!
//! [`CowApiTransport`] implements the videre [`VenueTransport`]
//! contract by assembling the orderbook `OrderCreation` from the
//! decoded [`CowIntentBody`] and driving
//! [`CowApiHost::submit_order`], so the keeper [`run`](super::run())
//! submits through the typed [`CowClient`](super::CowClient) while
//! module worlds still import the legacy host extension. Deleted when
//! the worlds flip to `videre:venue/client`.

use alloy_primitives::{Address, hex};
use cow_venue::assembly;
use cow_venue::body::{CowIntent, CowIntentBody};
use cowprotocol::Chain;
use nexum_sdk::host::Fault;
use nexum_sdk::keeper::RetryAction;
use videre_sdk::client::sealed::SealedTransport;
use videre_sdk::{
    IntentBody as _, IntentStatus, Quotation, SubmitOutcome, VenueFault, VenueId, VenueTransport,
};

use super::{CowApiError, CowApiHost, classify_api_error, is_already_submitted};

/// The `videre:venue/client` verbs carried over the legacy
/// `shepherd:cow/cow-api` import: submit only, pre-bound to one chain's
/// orderbook. Quote, status, and cancel have no legacy submission-path
/// counterpart and refuse as `unsupported`.
pub struct CowApiTransport<'h, H> {
    host: &'h H,
    chain_id: u64,
}

impl<'h, H: CowApiHost> CowApiTransport<'h, H> {
    /// Bind the legacy seam to one chain's orderbook.
    #[must_use]
    pub const fn new(host: &'h H, chain_id: u64) -> Self {
        Self { host, chain_id }
    }
}

impl<H: CowApiHost> SealedTransport for CowApiTransport<'_, H> {}

impl<H: CowApiHost> VenueTransport for CowApiTransport<'_, H> {
    async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
        Err(VenueFault::Unsupported)
    }

    async fn submit(&self, _venue: &VenueId, body: Vec<u8>) -> Result<SubmitOutcome, VenueFault> {
        let CowIntentBody::V1(intent) =
            CowIntentBody::from_bytes(&body).map_err(|e| VenueFault::InvalidBody(e.to_string()))?;
        // The legacy seam posts EIP-1271 only; the pre-sign flow needs
        // the adapter.
        let CowIntent::Signed(signed) = intent else {
            return Err(VenueFault::Unsupported);
        };
        let order = assembly::body_to_order_data(&signed.order);
        let owner = Address::from(signed.owner);
        let creation = assembly::build_order_creation(&order, &signed.signature, owner)
            .map_err(|e| VenueFault::InvalidBody(e.to_string()))?;
        let json = serde_json::to_vec(&creation)
            .map_err(|e| VenueFault::Unavailable(format!("order encode failed: {e}")))?;
        match self.host.submit_order(self.chain_id, &json) {
            Ok(uid) => Ok(SubmitOutcome::Accepted(receipt_bytes(&uid))),
            // Already-held is success wearing an error status; the
            // receipt is the client-derived UID (empty on a chain the
            // SDK cannot derive for).
            Err(CowApiError::Rejected(r)) if is_already_submitted(&r) => {
                let receipt = Chain::try_from(self.chain_id)
                    .map(|chain| {
                        assembly::order_uid(chain, &order, owner)
                            .as_slice()
                            .to_vec()
                    })
                    .unwrap_or_default();
                Ok(SubmitOutcome::Accepted(receipt))
            }
            Err(err) => Err(venue_fault(&err)),
        }
    }

    async fn status(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<IntentStatus, VenueFault> {
        Err(VenueFault::Unsupported)
    }

    async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
        Err(VenueFault::Unsupported)
    }
}

/// The server UID at its wire spelling; a non-hex receipt rides through
/// as raw bytes rather than failing an accepted submit.
fn receipt_bytes(uid: &str) -> Vec<u8> {
    hex::decode(uid).unwrap_or_else(|_| uid.as_bytes().to_vec())
}

/// Project a legacy submission failure onto the venue fault the typed
/// client reports, mirroring the adapter: throttles keep their hint,
/// host and server failures stay retryable, and only a structured
/// rejection, folded through the shipped classification table, carries
/// a permanent venue verdict.
fn venue_fault(err: &CowApiError) -> VenueFault {
    match err {
        CowApiError::Fault(Fault::RateLimited(limit)) => VenueFault::RateLimited {
            retry_after_ms: limit.retry_after_ms,
        },
        CowApiError::Fault(Fault::Timeout) => VenueFault::Timeout,
        // Any other host fault is infrastructure, not a venue verdict:
        // it stays retryable so an unprovisioned capability or unknown
        // chain never drops a still-valid order.
        CowApiError::Fault(fault) => VenueFault::Unavailable(fault.to_string()),
        CowApiError::Http(http) if http.status == 429 => VenueFault::RateLimited {
            retry_after_ms: None,
        },
        CowApiError::Http(http) => {
            VenueFault::Unavailable(format!("orderbook http {}", http.status))
        }
        CowApiError::Rejected(rejection) => {
            let detail = format!("{}: {}", rejection.error_type, rejection.description);
            match classify_api_error(rejection) {
                RetryAction::TryNextBlock => VenueFault::Unavailable(detail),
                RetryAction::Backoff { seconds } => VenueFault::RateLimited {
                    retry_after_ms: Some(seconds.saturating_mul(1000)),
                },
                _ => VenueFault::Denied(detail),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use nexum_sdk::host::RateLimit;
    use videre_sdk::keeper::retry_action;

    use super::super::{HttpFailure, OrderRejection};
    use super::*;

    fn rejected(error_type: &str) -> CowApiError {
        CowApiError::Rejected(OrderRejection {
            status: 400,
            error_type: error_type.into(),
            description: "d".into(),
            data: None,
        })
    }

    #[test]
    fn legacy_failures_project_onto_the_venue_fault_by_shape() {
        assert!(matches!(
            venue_fault(&rejected("InsufficientFee")),
            VenueFault::Unavailable(detail) if detail.contains("InsufficientFee")
        ));
        assert!(matches!(
            venue_fault(&rejected("TooManyLimitOrders")),
            VenueFault::RateLimited {
                retry_after_ms: Some(30_000)
            }
        ));
        assert!(matches!(
            venue_fault(&rejected("InvalidSignature")),
            VenueFault::Denied(detail) if detail.contains("InvalidSignature")
        ));
        assert!(matches!(
            venue_fault(&CowApiError::Fault(Fault::RateLimited(RateLimit {
                retry_after_ms: Some(2_500),
            }))),
            VenueFault::RateLimited {
                retry_after_ms: Some(2_500)
            }
        ));
        assert!(matches!(
            venue_fault(&CowApiError::Fault(Fault::Timeout)),
            VenueFault::Timeout
        ));
        assert!(matches!(
            venue_fault(&CowApiError::Http(HttpFailure {
                status: 429,
                body: None,
            })),
            VenueFault::RateLimited {
                retry_after_ms: None
            }
        ));
        assert!(matches!(
            venue_fault(&CowApiError::Http(HttpFailure {
                status: 502,
                body: None,
            })),
            VenueFault::Unavailable(_)
        ));
    }

    #[test]
    fn host_faults_stay_retryable_and_never_drop_the_watch() {
        for fault in [
            Fault::Unsupported("cow-api not provisioned".into()),
            Fault::Denied("allowlist".into()),
            Fault::Unavailable("rpc down".into()),
            Fault::Internal("host bug".into()),
            Fault::InvalidInput("mangled".into()),
        ] {
            let projected = venue_fault(&CowApiError::Fault(fault));
            assert!(matches!(projected, VenueFault::Unavailable(_)));
            assert_eq!(retry_action(&projected), RetryAction::TryNextBlock);
        }
        assert_eq!(
            retry_action(&venue_fault(&CowApiError::Fault(Fault::Timeout))),
            RetryAction::TryNextBlock
        );
        assert_eq!(
            retry_action(&venue_fault(&CowApiError::Fault(Fault::RateLimited(
                RateLimit {
                    retry_after_ms: Some(2_500),
                }
            )))),
            RetryAction::Backoff { seconds: 3 }
        );
    }

    #[test]
    fn server_uid_decodes_to_wire_bytes_with_a_raw_fallback() {
        assert_eq!(receipt_bytes("0xc0ffee"), vec![0xC0, 0xFF, 0xEE]);
        assert_eq!(receipt_bytes("not-hex"), b"not-hex".to_vec());
    }
}
