//! `nexum:host/chain`: raw JSON-RPC dispatch over alloy.

use std::time::Instant;

use alloy_chains::Chain;

use crate::bindings::HostError;
use crate::bindings::nexum;
use crate::host::component::{ChainProvider, CowApi, HttpClient, StateHandle};
use crate::host::state::HostState;

/// Methods that could sign transactions or expose sensitive node
/// internals. We warn when a module calls one so operators can audit.
const DANGEROUS_METHODS: &[&str] = &[
    "eth_sign",
    "eth_signTransaction",
    "eth_sendTransaction",
    "personal_sign",
    "personal_unlockAccount",
    "personal_sendTransaction",
];

/// Prefixes whose entire namespace is considered dangerous.
const DANGEROUS_PREFIXES: &[&str] = &["admin_", "debug_", "miner_"];

fn is_dangerous_method(method: &str) -> bool {
    DANGEROUS_METHODS.contains(&method) || DANGEROUS_PREFIXES.iter().any(|p| method.starts_with(p))
}

impl<C, W, S, H> nexum::host::chain::Host for HostState<C, W, S, H>
where
    C: ChainProvider + Send + Sync,
    W: CowApi + Send + Sync,
    S: StateHandle + Send + Sync,
    H: HttpClient + Send + Sync,
{
    async fn request(
        &mut self,
        chain_id: u64,
        method: String,
        params: String,
    ) -> Result<String, HostError> {
        let start = Instant::now();
        let chain = Chain::from_id(chain_id);
        if is_dangerous_method(&method) {
            tracing::warn!(
                chain_id,
                %method,
                "module called a dangerous RPC method - ensure your RPC \
                 endpoint is read-only or this call is intentional"
            );
        }
        tracing::debug!(chain_id, %method, "chain::request");
        let method_label = method.clone();
        let result = self
            .chain
            .request(chain, method, params)
            .await
            .map_err(HostError::from);
        tracing::trace!(elapsed_ms = ?start.elapsed(), "chain::request done");
        let outcome = if result.is_ok() { "ok" } else { "err" };
        metrics::counter!(
            "shepherd_chain_request_total",
            "chain_id" => chain_id.to_string(),
            "method" => method_label,
            "outcome" => outcome,
        )
        .increment(1);
        result
    }

    async fn request_batch(
        &mut self,
        chain_id: u64,
        requests: Vec<nexum::host::chain::RpcRequest>,
    ) -> Result<Vec<nexum::host::chain::RpcResult>, HostError> {
        let start = Instant::now();
        tracing::debug!(chain_id, count = requests.len(), "chain::request-batch");
        let mut out = Vec::with_capacity(requests.len());
        for req in requests {
            match nexum::host::chain::Host::request(self, chain_id, req.method, req.params).await {
                Ok(s) => out.push(nexum::host::chain::RpcResult::Ok(s)),
                Err(e) => out.push(nexum::host::chain::RpcResult::Err(e)),
            }
        }
        tracing::trace!(elapsed_ms = ?start.elapsed(), "chain::request-batch done");
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bindings::nexum::host::types::HostErrorKind;
    use crate::host::provider_pool::ProviderError;
    use alloy_transport::TransportErrorKind;

    /// Helper: build a synthetic transport-level [`TransportError`] for
    /// the test fixtures. Transport-level errors do not carry a
    /// structured JSON-RPC `ErrorResp` payload, so `as_error_resp()` is
    /// `None` for these and `code`/`data` are blank on the projected
    /// [`HostError`].
    fn transport_err(msg: &str) -> alloy_transport::TransportError {
        TransportErrorKind::custom_str(msg)
    }

    #[test]
    fn rpc_error_with_revert_data_is_forwarded() {
        // The node returns a structured `ErrorResp` for an
        // `eth_call` revert: `code = -32000`, `data = "0x..."` with
        // the abi-encoded revert body. The projection must forward
        // both into HostError so the SDK can classify the outcome
        // via `decode_revert_hex`.
        let host_err = HostError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: Some(-32000),
            data: Some("\"0xabc123\"".into()),
            source: transport_err("execution reverted"),
        });

        assert!(matches!(host_err.kind, HostErrorKind::Internal));
        assert_eq!(host_err.code, -32000);
        assert_eq!(host_err.data.as_deref(), Some("\"0xabc123\""));
    }

    #[test]
    fn rpc_error_without_payload_keeps_internal_fallback() {
        // Transport-level failures (timeout, connection drop, serde
        // mismatch) leave both code and data blank. The projection
        // must fall back to the `-32603` "Internal error" code and
        // keep `data = None` so the SDK's classifier hits the
        // `TryNextBlock` safe default rather than feeding garbage to
        // `decode_revert_hex`.
        let host_err = HostError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: None,
            data: None,
            source: transport_err("websocket disconnected"),
        });

        assert!(matches!(host_err.kind, HostErrorKind::Internal));
        assert_eq!(host_err.code, -32603);
        assert!(host_err.data.is_none());
    }

    #[test]
    fn out_of_range_rpc_code_saturates_to_internal_fallback() {
        // JSON-RPC codes are conventionally `-32768..-32000`, but the
        // alloy `ErrorPayload.code` field is `i64`. Defensive: an
        // out-of-`i32` code should not poison the projection - clamp
        // to `-32603` so the guest sees a sane Internal error.
        let host_err = HostError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: Some(i64::from(i32::MAX) + 1),
            data: None,
            source: transport_err("weird code"),
        });

        assert_eq!(host_err.code, -32603);
    }

    #[test]
    fn unknown_chain_is_unsupported() {
        // Use an id with no `NamedChain` mapping so `Chain`'s `Display`
        // prints the number and the message assertion stays meaningful.
        let host_err = HostError::from(ProviderError::UnknownChain(Chain::from_id(424242)));
        assert!(matches!(host_err.kind, HostErrorKind::Unsupported));
        assert_eq!(host_err.code, 0);
        assert!(host_err.message.contains("424242"));
    }

    #[test]
    fn invalid_params_maps_to_invalid_input() {
        // `serde_json::from_str::<()>("not json")` is the cheapest
        // way to produce a real `serde_json::Error` for tests.
        let source = serde_json::from_str::<serde_json::Value>("not json")
            .expect_err("`not json` is not valid JSON");
        let host_err = HostError::from(ProviderError::InvalidParams {
            method: "eth_call".into(),
            source,
        });
        assert!(matches!(host_err.kind, HostErrorKind::InvalidInput));
        assert_eq!(host_err.code, -32602);
    }
}
