//! `shepherd:cow/cow-api`: REST passthrough + typed `submit_order`.
//! Backend logic lives in [`crate::host::cow_orderbook`]; this is the
//! WIT-side error mapping.

use std::time::Instant;

use crate::bindings::nexum::host::types::HostErrorKind;
use crate::bindings::{HostError, shepherd};
use crate::host::cow_orderbook::CowApiError;
use crate::host::error::{internal_error, unimplemented};
use crate::host::state::HostState;

impl shepherd::cow::cow_api::Host for HostState {
    async fn request(
        &mut self,
        chain_id: u64,
        method: String,
        path: String,
        body: Option<String>,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        tracing::debug!(chain_id, %method, %path, "cow-api::request");
        let result = match self
            .cow
            .request(chain_id, &method, &path, body.as_deref())
            .await
        {
            Ok(body) => Ok(body),
            Err(CowApiError::UnknownChain(id)) => Err(unimplemented(
                "cow-api",
                format!("chain {id} not in cowprotocol"),
            )),
            Err(CowApiError::BadMethod(m)) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: format!("unsupported HTTP method: {m}"),
                data: None,
            }),
            Err(CowApiError::BadPath(msg)) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: msg,
                data: None,
            }),
            Err(CowApiError::HttpError { status, body }) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::Internal,
                code: status as i32,
                message: format!("HTTP {status}"),
                data: Some(body),
            }),
            Err(err) => Err(internal_error("cow-api", err.to_string())),
        };
        tracing::trace!(elapsed_ms = ?start.elapsed(), "cow-api::request done");
        result
    }

    async fn submit_order(
        &mut self,
        chain_id: u64,
        order_data: Vec<u8>,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        tracing::debug!(chain_id, bytes = order_data.len(), "cow-api::submit-order");
        let result = match self.cow.submit_order_json(chain_id, &order_data).await {
            Ok(uid) => Ok(alloy_primitives::hex::encode_prefixed(uid.as_slice())),
            Err(CowApiError::UnknownChain(id)) => Err(unimplemented(
                "cow-api",
                format!("chain {id} not in cowprotocol"),
            )),
            Err(CowApiError::Decode(err)) => Err(HostError {
                domain: "cow-api".into(),
                kind: HostErrorKind::InvalidInput,
                code: 0,
                message: format!("invalid OrderCreation JSON: {err}"),
                data: None,
            }),
            Err(CowApiError::Orderbook(err)) => Err(orderbook_to_host_error(err)),
            Err(err) => Err(internal_error("cow-api", err.to_string())),
        };
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

/// Project a `cowprotocol::Error` from `OrderBookApi::post_order` into
/// the WIT-side `HostError`.
///
/// For [`cowprotocol::Error::OrderbookApi`] (the orderbook returned a
/// typed `{"errorType": "...", ...}` envelope), the JSON-encoded
/// `ApiError` is forwarded verbatim in `HostError.data` so the guest's
/// `shepherd_sdk::cow::classify_api_error` can dispatch on `errorType`.
/// Without this projection the classifier is fed `None` and falls back
/// to `TryNextBlock`, producing infinite retry loops on permanent
/// rejections like `DuplicatedOrder` or `InvalidSignature`.
///
/// Other `cowprotocol::Error` variants (transport, serde, etc.) carry
/// no structured payload; `data` is left as `None` and the guest's
/// classifier applies its safe-default `TryNextBlock` branch.
fn orderbook_to_host_error(err: cowprotocol::Error) -> HostError {
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

#[cfg(test)]
mod tests {
    use super::*;
    use cowprotocol::error::ApiError;

    #[test]
    fn orderbook_api_error_is_forwarded_in_data() {
        // The orderbook rejects with a typed envelope. The mapping
        // must serialise it into HostError.data so the guest can
        // dispatch on `errorType`.
        let api = ApiError {
            error_type: "DuplicatedOrder".to_owned(),
            description: "order already exists".to_owned(),
            data: None,
        };
        let err = cowprotocol::Error::OrderbookApi { status: 400, api };

        let host_err = orderbook_to_host_error(err);

        assert!(matches!(host_err.kind, HostErrorKind::Denied));
        assert_eq!(host_err.code, 400);
        let data = host_err.data.expect("orderbook envelope forwarded");
        let parsed: ApiError = serde_json::from_str(&data).expect("data is ApiError JSON");
        assert_eq!(parsed.error_type, "DuplicatedOrder");
        assert_eq!(parsed.description, "order already exists");
    }

    #[test]
    fn orderbook_api_error_preserves_optional_data_field() {
        // ApiError carries an optional `data` field of its own. The
        // forward must round-trip it so the guest sees what the
        // orderbook actually returned.
        let api = ApiError {
            error_type: "InsufficientFee".to_owned(),
            description: "fee too low".to_owned(),
            data: Some(serde_json::json!({"min_fee": "1234"})),
        };
        let err = cowprotocol::Error::OrderbookApi { status: 400, api };

        let host_err = orderbook_to_host_error(err);

        let data = host_err.data.expect("envelope forwarded");
        let parsed: ApiError = serde_json::from_str(&data).expect("round-trip");
        assert_eq!(
            parsed.data.expect("inner data preserved")["min_fee"],
            "1234"
        );
    }

    #[test]
    fn non_envelope_cowprotocol_error_leaves_data_none() {
        // Transport / serde / unexpected-status errors don't carry a
        // structured ApiError; the guest classifier handles the
        // None-data case via its TryNextBlock safe default.
        let err = cowprotocol::Error::UnexpectedStatus {
            status: 502,
            body: "<html>upstream</html>".to_owned(),
        };

        let host_err = orderbook_to_host_error(err);

        assert!(host_err.data.is_none());
        assert_eq!(host_err.code, 0);
        assert!(matches!(host_err.kind, HostErrorKind::Denied));
    }
}
