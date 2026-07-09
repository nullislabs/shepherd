//! `nexum:host/chain`: raw JSON-RPC dispatch over alloy.

use std::time::Instant;

use alloy_chains::Chain;

use crate::bindings::nexum;
use crate::bindings::nexum::host::chain::ChainError;
use crate::host::component::{ChainMethod, ChainProvider, RuntimeTypes};
use crate::host::error::chain_denied;
use crate::host::state::HostState;

/// Resolve a guest method string into the permitted read surface.
///
/// Signing-adjacent and mutating methods have no [`ChainMethod`]
/// variant, so they are rejected here structurally rather than by an
/// ad-hoc name check; the result is a `Denied` chain fault. Every entry
/// of a batch request routes through this same resolver.
fn resolve_method(method: &str) -> Result<ChainMethod, ChainError> {
    ChainMethod::try_from(method).map_err(|_| {
        chain_denied(format!(
            "method `{method}` is not in the permitted read-only surface"
        ))
    })
}

/// Return an error if `body` exceeds `cap` bytes. The check is applied
/// host-side before the response is copied into the guest, so an
/// oversized node response cannot saturate the guest heap.
fn check_response_cap(
    body: &str,
    cap: usize,
    chain_id: u64,
    method: &str,
) -> Result<(), ChainError> {
    if body.len() > cap {
        tracing::warn!(
            chain_id,
            method,
            body_bytes = body.len(),
            cap_bytes = cap,
            "chain response exceeds size cap — rejecting before guest copy"
        );
        metrics::counter!(
            "shepherd_chain_response_capped_total",
            "chain_id" => chain_id.to_string(),
            "method" => method.to_owned(),
        )
        .increment(1);
        return Err(ChainError::Fault(
            crate::bindings::nexum::host::types::Fault::InvalidInput(format!(
                "chain response ({} bytes) exceeds the configured cap ({} bytes)",
                body.len(),
                cap,
            )),
        ));
    }
    Ok(())
}

impl<T: RuntimeTypes> nexum::host::chain::Host for HostState<T> {
    async fn request(
        &mut self,
        chain_id: u64,
        method: String,
        params: String,
    ) -> Result<String, ChainError> {
        let start = Instant::now();
        let chain = Chain::from_id(chain_id);
        let method = match resolve_method(&method) {
            Ok(method) => method,
            Err(err) => {
                tracing::warn!(
                    chain_id,
                    %method,
                    "chain::request rejected: method is not in the permitted read surface"
                );
                metrics::counter!(
                    "shepherd_chain_request_total",
                    "chain_id" => chain_id.to_string(),
                    "method" => "<denied>",
                    "outcome" => "err",
                )
                .increment(1);
                return Err(err);
            }
        };
        let name = method.as_str();
        tracing::debug!(chain_id, method = name, "chain::request");
        let result = self
            .chain
            .request(chain, method, params)
            .await
            .map_err(ChainError::from)
            .and_then(|body| {
                check_response_cap(&body, self.chain_response_max_bytes, chain_id, name)?;
                Ok(body)
            });
        tracing::trace!(elapsed_ms = ?start.elapsed(), "chain::request done");
        let outcome = if result.is_ok() { "ok" } else { "err" };
        metrics::counter!(
            "shepherd_chain_request_total",
            "chain_id" => chain_id.to_string(),
            "method" => name,
            "outcome" => outcome,
        )
        .increment(1);
        result
    }

    /// Dispatch a batch of requests, one `RpcResult` per entry in order.
    ///
    /// The outer `ChainError` is reserved for a failure that stops the
    /// host producing any results at all; this host has no such path, so
    /// it always returns `Ok`. A per-entry failure (a denied
    /// method, a node revert, a transport fault) surfaces as that entry's
    /// `RpcResult::Err`. This impl folds each entry independently, so a
    /// failure leaves its neighbours intact; a different host could instead
    /// short-circuit the batch, so SDK consumers match on each entry, not
    /// on the batch call.
    async fn request_batch(
        &mut self,
        chain_id: u64,
        requests: Vec<nexum::host::chain::RpcRequest>,
    ) -> Result<Vec<nexum::host::chain::RpcResult>, ChainError> {
        let start = Instant::now();
        // Each entry is dispatched sequentially and gets its own full
        // per-chain timeout, so the worst-case blocking time for a batch
        // is N x request_timeout_secs.
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

    use crate::bindings::nexum::host::types::Fault;
    use crate::host::provider_pool::ProviderError;
    use alloy_transport::TransportErrorKind;

    /// Helper: build a synthetic transport-level [`TransportError`].
    /// Transport-level errors carry no structured JSON-RPC `ErrorResp`,
    /// so they project to a [`ChainError::Fault`] rather than a
    /// [`ChainError::Rpc`].
    fn transport_err(msg: &str) -> alloy_transport::TransportError {
        TransportErrorKind::custom_str(msg)
    }

    #[test]
    fn rpc_error_with_revert_data_is_forwarded() {
        // The node returned a structured `ErrorResp` for an `eth_call`
        // revert: `code = -32000`, `data` already hex-decoded to the
        // abi-encoded revert body. The projection forwards both into
        // `ChainError::Rpc` so the SDK can classify the outcome via
        // `decode_revert`.
        let revert = vec![0xab, 0xc1, 0x23];
        let chain_err = ChainError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: Some(-32000),
            data: Some(revert.clone()),
            source: transport_err("execution reverted"),
        });

        let ChainError::Rpc(rpc) = chain_err else {
            panic!("expected ChainError::Rpc, got {chain_err:?}");
        };
        assert_eq!(rpc.code, -32000);
        assert_eq!(rpc.data.as_deref(), Some(revert.as_slice()));
    }

    #[test]
    fn transport_failure_projects_to_unavailable_fault() {
        // A transport-level failure (no `ErrorResp`) with no timeout
        // marker in the message defaults to an `unavailable` fault.
        let chain_err = ChainError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: None,
            data: None,
            source: transport_err("websocket disconnected"),
        });
        assert!(matches!(
            chain_err,
            ChainError::Fault(Fault::Unavailable(_))
        ));
    }

    #[test]
    fn timed_out_request_projects_to_timeout_fault() {
        let chain_err = ChainError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: None,
            data: None,
            source: transport_err("request timed out after 30s"),
        });
        assert!(matches!(chain_err, ChainError::Fault(Fault::Timeout)));
    }

    #[test]
    fn backend_gone_projects_to_unavailable_fault() {
        let chain_err = ChainError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: None,
            data: None,
            source: TransportErrorKind::backend_gone(),
        });
        assert!(matches!(
            chain_err,
            ChainError::Fault(Fault::Unavailable(_))
        ));
    }

    #[test]
    fn out_of_range_rpc_code_saturates_to_internal_fallback() {
        // JSON-RPC codes are conventionally `-32768..-32000`, but the
        // alloy `ErrorPayload.code` field is `i64`. Defensive: an
        // out-of-`i32` code should not poison the projection - clamp
        // to `-32603` so the guest sees a sane code.
        let chain_err = ChainError::from(ProviderError::Rpc {
            method: "eth_call".into(),
            code: Some(i64::from(i32::MAX) + 1),
            data: None,
            source: transport_err("weird code"),
        });
        let ChainError::Rpc(rpc) = chain_err else {
            panic!("expected ChainError::Rpc, got {chain_err:?}");
        };
        assert_eq!(rpc.code, -32603);
    }

    #[test]
    fn unknown_chain_is_unsupported_fault() {
        // Use an id with no `NamedChain` mapping so `Chain`'s `Display`
        // prints the number and the message assertion stays meaningful.
        let chain_err = ChainError::from(ProviderError::UnknownChain(Chain::from_id(424242)));
        let ChainError::Fault(Fault::Unsupported(msg)) = chain_err else {
            panic!("expected Unsupported fault, got {chain_err:?}");
        };
        assert!(msg.contains("424242"));
    }

    #[test]
    fn timeout_maps_to_timeout_fault() {
        // A configured-timeout failure surfaces as the dedicated
        // `timeout` fault, distinct from a revert (`Rpc`) or an
        // unreachable node (`unavailable`).
        let chain_err = ChainError::from(ProviderError::Timeout {
            method: "eth_call".into(),
        });
        assert!(matches!(chain_err, ChainError::Fault(Fault::Timeout)));
    }

    #[test]
    fn invalid_params_maps_to_invalid_input_fault() {
        // `serde_json::from_str::<()>("not json")` is the cheapest
        // way to produce a real `serde_json::Error` for tests.
        let source = serde_json::from_str::<serde_json::Value>("not json")
            .expect_err("`not json` is not valid JSON");
        let chain_err = ChainError::from(ProviderError::InvalidParams {
            method: "eth_call".into(),
            source,
        });
        assert!(matches!(
            chain_err,
            ChainError::Fault(Fault::InvalidInput(_))
        ));
    }

    #[test]
    fn permitted_methods_resolve() {
        for m in ["eth_call", "eth_blockNumber", "eth_getBalance"] {
            assert!(resolve_method(m).is_ok(), "{m} should resolve");
        }
    }

    #[test]
    fn signing_methods_are_denied() {
        // The signing-adjacent surface must map to a `Denied` fault,
        // not reach the provider.
        for m in [
            "eth_sign",
            "eth_sendTransaction",
            "eth_accounts",
            "personal_sign",
            "eth_sendRawTransaction",
        ] {
            let err = resolve_method(m).expect_err(m);
            assert!(
                matches!(err, ChainError::Fault(Fault::Denied(_))),
                "{m} must be a Denied fault, got {err:?}"
            );
        }
    }

    #[test]
    fn unknown_method_is_denied() {
        let err = resolve_method("eth_totallyFakeMethod").expect_err("unknown method");
        assert!(matches!(err, ChainError::Fault(Fault::Denied(_))));
    }

    #[test]
    fn batch_entries_are_classified_independently() {
        // `request_batch` routes every entry through `resolve_method`,
        // so one denied entry neither aborts nor taints the permitted
        // entries around it.
        let batch = ["eth_call", "eth_sign", "eth_getBalance"];
        let resolved: Vec<_> = batch.iter().map(|m| resolve_method(m)).collect();
        assert!(resolved[0].is_ok());
        assert!(matches!(
            resolved[1].as_ref().expect_err("eth_sign"),
            ChainError::Fault(Fault::Denied(_)),
        ));
        assert!(resolved[2].is_ok());
    }

    // ── response size cap tests (#154) ──

    #[test]
    fn response_at_cap_is_accepted() {
        let body = "x".repeat(10);
        assert!(
            check_response_cap(&body, 10, 1, "eth_call").is_ok(),
            "body exactly at cap should pass"
        );
    }

    #[test]
    fn response_over_cap_returns_invalid_input() {
        let body = "x".repeat(11);
        let err =
            check_response_cap(&body, 10, 1, "eth_call").expect_err("over-cap body should fail");
        assert!(
            matches!(err, ChainError::Fault(Fault::InvalidInput(_))),
            "expected InvalidInput fault, got {err:?}"
        );
    }
}
