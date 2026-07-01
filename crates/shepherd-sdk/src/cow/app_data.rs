//! Resolve a 32-byte `appData` hash to its canonical JSON document.
//!
//! CoW Protocol orders carry an `appData` field as `bytes32 =
//! keccak256(appDataJSON)`. The orderbook validates submissions by
//! re-hashing the JSON body and comparing to the signed hash, so any
//! caller that doesn't already know the document text needs to look
//! it up - either via IPFS or via the orderbook's mirror at
//! `GET /api/v1/app_data/{hex}`.
//!
//! This module hides that lookup behind a single
//! [`resolve_app_data`] helper. Strategies (notably twap-monitor)
//! call it before assembling an `OrderCreation` so cow-swap UI's
//! richer appData docs (partner-id, slippage settings,
//! quote-id, etc.) round-trip cleanly through the submit path.
//!
//! ## Behaviour
//!
//! - `hash == EMPTY_APP_DATA_HASH` (`keccak256("{}")`) → short-circuit
//!   to [`cowprotocol::EMPTY_APP_DATA_JSON`] (`"{}"`), no host call.
//! - Otherwise → `GET /api/v1/app_data/{hex}` on the chain's
//!   orderbook. The 200 response is `{"fullAppData": "<JSON>"}`; we
//!   pull `fullAppData` out and return it verbatim.
//! - On 404 (`HostError.code == 404`) → return the same error so the
//!   caller can drop the submit gracefully (the orderbook doesn't
//!   have the document mirrored; the caller has no path to recover
//!   without operator intervention).
//!
//! ## Why not a typed CoW endpoint
//!
//! `cow-api::request` is the generic REST passthrough already in the
//! WIT surface (since 0.2.0); we use it rather than adding a typed
//! `cow-api::get-app-data` host method to keep this PR scoped to the
//! SDK + module layers (no WIT bump → no breaking module recompile).
//! Should the lookup become hot enough to merit a typed host
//! endpoint (e.g. for cache control), follow-up issue.
//!
//! ## Why not IPFS
//!
//! The orderbook already mirrors IPFS app_data docs and serves them
//! over a single HTTPS endpoint. Going to IPFS directly would
//! require a fresh capability (`ipfs`), bigger module footprint,
//! and worse latency than a single GET against an already-trusted
//! upstream. If the orderbook 404s, IPFS would too - the doc isn't
//! pinned anywhere we can see from inside the engine.

use alloy_primitives::B256;
use cowprotocol::EMPTY_APP_DATA_HASH;

use crate::host::{CowApiHost, HostError, HostErrorKind};

/// Look up the JSON document corresponding to a signed `appData`
/// hash. See module-level docs for behaviour.
///
/// The hash is a 32-byte EVM word; the SDK takes [`B256`] across the
/// public surface rather than a raw `&[u8; 32]` per the rubric's
/// protocol-ID newtype rule. Callers holding a raw byte array
/// convert via `B256::from_slice(&bytes[..])` at the WIT boundary.
///
/// ```no_run
/// use shepherd_sdk::cow::resolve_app_data;
/// use shepherd_sdk::host::{CowApiHost, HostError};
/// use shepherd_sdk::prelude::B256;
///
/// fn pin_doc<H: CowApiHost>(host: &H, chain_id: u64, hash: &B256) -> Result<String, HostError> {
///     resolve_app_data(host, chain_id, hash)
/// }
/// ```
pub fn resolve_app_data<H: CowApiHost + ?Sized>(
    host: &H,
    chain_id: u64,
    app_data_hash: &B256,
) -> Result<String, HostError> {
    if app_data_hash.as_slice() == EMPTY_APP_DATA_HASH.as_slice() {
        return Ok(cowprotocol::EMPTY_APP_DATA_JSON.to_string());
    }

    let hex = encode_hex(app_data_hash);
    let path = format!("/api/v1/app_data/{hex}");
    let response = host.cow_api_request(chain_id, "GET", &path, None)?;

    parse_full_app_data(&response).map_err(|e| HostError {
        domain: "cow-api".into(),
        kind: HostErrorKind::Internal,
        code: 0,
        message: format!("app_data response shape unexpected: {e}"),
        data: Some(response),
    })
}

/// Lowercase `0x`-prefixed hex of a 32-byte appData hash. Delegates
/// to [`alloy_primitives::hex::encode`] (alloy is already a direct
/// dependency of this crate) per mfw78's PR #8 guidance against
/// carrying our own hex formatters.
fn encode_hex(hash: &B256) -> String {
    format!("0x{}", alloy_primitives::hex::encode(hash.as_slice()))
}

/// Parse the orderbook's `/api/v1/app_data/{hash}` response shape:
///
/// ```json
/// {"fullAppData": "<JSON string>"}
/// ```
///
/// Some orderbook versions wrap the document in an outer envelope
/// (`{"appData": "...", "appDataHash": "...", "fullAppData": "..."}`);
/// we always pull `fullAppData` and ignore the rest.
fn parse_full_app_data(body: &str) -> Result<String, &'static str> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| "body is not JSON")?;
    let obj = v.as_object().ok_or("body is not a JSON object")?;
    let full = obj
        .get("fullAppData")
        .ok_or("missing `fullAppData` field")?;
    full.as_str()
        .ok_or("`fullAppData` is not a string")
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostErrorKind;
    use std::cell::RefCell;

    /// Stub that captures the (chain_id, method, path) tuple and
    /// returns a programmable response. Avoids pulling in
    /// shepherd-sdk-test here (which depends on shepherd-sdk).
    struct StubCowApi {
        response: Result<String, HostError>,
        last_call: RefCell<Option<(u64, String, String)>>,
    }

    impl CowApiHost for StubCowApi {
        fn submit_order(&self, _: u64, _: &[u8]) -> Result<String, HostError> {
            unimplemented!()
        }
        fn cow_api_request(
            &self,
            chain_id: u64,
            method: &str,
            path: &str,
            _body: Option<&str>,
        ) -> Result<String, HostError> {
            *self.last_call.borrow_mut() = Some((chain_id, method.to_string(), path.to_string()));
            self.response.clone()
        }
    }

    fn ok_stub(body: &str) -> StubCowApi {
        StubCowApi {
            response: Ok(body.to_string()),
            last_call: RefCell::new(None),
        }
    }

    fn err_stub(code: i32, kind: HostErrorKind) -> StubCowApi {
        StubCowApi {
            response: Err(HostError {
                domain: "cow-api".into(),
                kind,
                code,
                message: "stub".into(),
                data: None,
            }),
            last_call: RefCell::new(None),
        }
    }

    #[test]
    fn empty_hash_short_circuits_without_host_call() {
        let stub = ok_stub("should never be read");
        let resolved =
            resolve_app_data(&stub, 1, &B256::from_slice(EMPTY_APP_DATA_HASH.as_slice())).unwrap();
        assert_eq!(resolved, "{}");
        assert!(
            stub.last_call.borrow().is_none(),
            "host should not have been called"
        );
    }

    #[test]
    fn non_empty_hash_routes_to_orderbook_and_extracts_full_app_data() {
        let stub =
            ok_stub(r#"{"fullAppData":"{\"version\":\"1.1.0\"}","appDataHash":"0xc4bc..."}"#);
        let mut bytes = [0u8; 32];
        bytes[0] = 0xc4;
        bytes[1] = 0xbc;
        let hash = B256::from(bytes);
        let resolved = resolve_app_data(&stub, 11_155_111, &hash).unwrap();
        assert_eq!(resolved, r#"{"version":"1.1.0"}"#);
        let (cid, method, path) = stub.last_call.borrow().clone().unwrap();
        assert_eq!(cid, 11_155_111);
        assert_eq!(method, "GET");
        assert!(path.starts_with("/api/v1/app_data/0x"), "got path={path}");
        assert!(
            path.contains("c4bc"),
            "hex hash must be lower-case and 64 chars; got path={path}"
        );
    }

    #[test]
    fn missing_full_app_data_field_returns_internal_with_body_in_data() {
        let stub = ok_stub(r#"{"appDataHash":"0xabcd","appData":"{}"}"#);
        let mut bytes = [0u8; 32];
        bytes[0] = 0xc4;
        let hash = B256::from(bytes);
        let err = resolve_app_data(&stub, 1, &hash).unwrap_err();
        assert_eq!(err.kind, HostErrorKind::Internal);
        assert!(err.message.contains("fullAppData"), "got: {}", err.message);
        assert!(
            err.data.is_some(),
            "raw body must be carried in data for debug"
        );
    }

    #[test]
    fn host_error_propagates_unchanged() {
        let stub = err_stub(404, HostErrorKind::Unavailable);
        let mut bytes = [0u8; 32];
        bytes[0] = 0xc4;
        let hash = B256::from(bytes);
        let err = resolve_app_data(&stub, 1, &hash).unwrap_err();
        assert_eq!(err.code, 404);
        assert_eq!(err.kind, HostErrorKind::Unavailable);
    }

    #[test]
    fn hex_encoder_is_lower_case_and_64_wide() {
        let mut bytes = [0u8; 32];
        bytes[31] = 0xff;
        bytes[0] = 0xab;
        let hash = B256::from(bytes);
        assert_eq!(encode_hex(&hash), format!("0xab{}ff", "00".repeat(30)));
    }
}
