//! `nexum:host/chain` backend.
//!
//! Per-chain alloy provider, opened from the engine config at boot.
//! `request` is a raw JSON-RPC dispatch: the host hands `(method,
//! params)` straight to alloy's transport and returns the result body
//! verbatim. No method allowlist, no re-encoding of params - the
//! contract is "give us a JSON-RPC pair, we'll return what the node
//! returns".
//!
//! Transports:
//! - `ws://` / `wss://`  - `WsConnect`; required for `eth_subscribe`.
//! - `http://` / `https://` - alloy's HTTP transport; request/response only.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use alloy_provider::{DynProvider, Provider, ProviderBuilder, WsConnect};
use alloy_rpc_types_eth::{Filter, Header, Log};
use futures::stream::Stream;
use futures::stream::StreamExt as _;
use serde_json::value::RawValue;
use strum::IntoStaticStr;
use thiserror::Error;
use tracing::info;

use crate::engine_config::EngineConfig;

/// Pool of alloy providers keyed by chain id.
#[derive(Debug, Clone)]
pub struct ProviderPool {
    providers: Arc<BTreeMap<u64, DynProvider>>,
}

impl ProviderPool {
    /// Open one provider per chain in `cfg.chains`. WebSocket URLs
    /// engage alloy's pubsub transport; HTTP URLs use the HTTP
    /// transport. Connection failures propagate to the caller; the
    /// engine treats them as fatal at boot.
    pub async fn from_config(cfg: &EngineConfig) -> Result<Self, ProviderError> {
        let mut providers: BTreeMap<u64, DynProvider> = BTreeMap::new();
        for (chain_id, chain_cfg) in &cfg.chains {
            let url = chain_cfg.rpc_url.as_str();
            // The boot log carries the URL with embedded API keys
            // redacted - log aggregators (Loki, Datadog, splunk) often
            // ingest these lines and the key shouldn't end up in
            // long-term storage. The engine still uses the full URL
            // when actually connecting to the provider below.
            info!(
                chain_id,
                url = %crate::engine_config::redact_url(url),
                "opening chain RPC provider",
            );
            let provider = if url.starts_with("ws://") || url.starts_with("wss://") {
                ProviderBuilder::new()
                    .connect_ws(WsConnect::new(url))
                    .await
                    .map_err(|source| ProviderError::Connect {
                        chain_id: *chain_id,
                        source,
                    })?
                    .erased()
            } else {
                let parsed: url::Url = url.parse().map_err(|source| ProviderError::ConnectUrl {
                    chain_id: *chain_id,
                    source,
                })?;
                ProviderBuilder::new().connect_http(parsed).erased()
            };
            providers.insert(*chain_id, provider);
        }
        Ok(Self {
            providers: Arc::new(providers),
        })
    }

    /// Empty pool - used by tests. Every `request` call returns
    /// `UnknownChain`.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            providers: Arc::new(BTreeMap::new()),
        }
    }

    /// Open a new-blocks (`eth_subscribe newHeads`) stream on
    /// `chain_id`. Requires a WS / IPC transport at construction
    /// time; HTTP-only providers surface `UnknownChain` here.
    pub async fn subscribe_blocks(&self, chain_id: u64) -> Result<BlockStream, ProviderError> {
        let provider = self
            .providers
            .get(&chain_id)
            .ok_or(ProviderError::UnknownChain(chain_id))?;
        let sub = provider
            .subscribe_blocks()
            .await
            .map_err(|source| ProviderError::Rpc {
                method: "eth_subscribe(newHeads)".into(),
                code: None,
                data: None,
                source,
            })?;
        let stream = sub.into_stream().map(Ok::<_, ProviderError>);
        Ok(Box::pin(stream))
    }

    /// Open an `eth_subscribe(logs, filter)` stream on `chain_id`.
    pub async fn subscribe_logs(
        &self,
        chain_id: u64,
        filter: Filter,
    ) -> Result<LogStream, ProviderError> {
        let provider = self
            .providers
            .get(&chain_id)
            .ok_or(ProviderError::UnknownChain(chain_id))?;
        let sub = provider
            .subscribe_logs(&filter)
            .await
            .map_err(|source| ProviderError::Rpc {
                method: "eth_subscribe(logs)".into(),
                code: None,
                data: None,
                source,
            })?;
        let stream = sub.into_stream().map(Ok::<_, ProviderError>);
        Ok(Box::pin(stream))
    }

    /// Raw JSON-RPC dispatch. `params_json` must be the JSON encoding
    /// of the params array (e.g. `"[\"0x...\",\"latest\"]"`), as
    /// produced by the SDK's `chain::request` glue.
    pub async fn request(
        &self,
        chain_id: u64,
        method: String,
        params_json: String,
    ) -> Result<String, ProviderError> {
        let provider = self
            .providers
            .get(&chain_id)
            .ok_or(ProviderError::UnknownChain(chain_id))?;
        // Pass the params through as a raw JSON value so alloy does
        // not re-encode them on the way to the node.
        let params: Box<RawValue> =
            RawValue::from_string(params_json).map_err(|source| ProviderError::InvalidParams {
                method: method.clone(),
                source,
            })?;
        // `raw_request` consumes the method name; clone once for the
        // error branch so the success path moves the original string
        // straight into alloy without an extra allocation.
        let method_for_err = method.clone();
        let result: Box<RawValue> =
            provider
                .raw_request(method.into(), params)
                .await
                .map_err(|source| {
                    // When the node returns a JSON-RPC error response
                    // (`{"error": {"code":..., "data":...}}`) - typically
                    // an `eth_call` revert - capture the structured
                    // payload so the host can forward it to
                    // `HostError.data`. Transport-side
                    // failures (timeouts, serde, etc.) leave both
                    // `code` and `data` `None` so the projection can
                    // tell "no ErrorResp" apart from "ErrorResp with
                    // code = 0".
                    let (code, data) = match source.as_error_resp() {
                        Some(payload) => (
                            Some(payload.code),
                            payload.data.as_ref().map(|d| d.get().to_owned()),
                        ),
                        None => (None, None),
                    };
                    ProviderError::Rpc {
                        method: method_for_err,
                        code,
                        data,
                        source,
                    }
                })?;
        Ok(result.get().to_owned())
    }
}

/// Boxed stream of `newHeads`-style block headers.
pub type BlockStream = Pin<Box<dyn Stream<Item = Result<Header, ProviderError>> + Send>>;
/// Boxed stream of `logs`-filtered log events.
pub type LogStream = Pin<Box<dyn Stream<Item = Result<Log, ProviderError>> + Send>>;

/// Errors surfaced by [`ProviderPool`].
///
/// `IntoStaticStr` produces the snake_case variant name as
/// `&'static str` for metric labels and structured-log fields; the
/// per-variant Display still carries the detail via `thiserror`.
#[derive(Debug, Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderError {
    /// Chain id absent from the engine config.
    #[error("unknown chain {0} (no engine.toml entry)")]
    UnknownChain(u64),
    /// Could not open the underlying transport.
    #[error("connect chain {chain_id}: {source}")]
    Connect {
        /// Chain id we failed to dial.
        chain_id: u64,
        /// Transport-side error.
        #[source]
        source: alloy_transport::TransportError,
    },
    /// HTTP RPC URL did not parse as a [`url::Url`].
    #[error("connect chain {chain_id}: invalid URL: {source}")]
    ConnectUrl {
        /// Chain id whose `rpc_url` was malformed.
        chain_id: u64,
        /// Underlying parse failure.
        #[source]
        source: url::ParseError,
    },
    /// The guest-supplied JSON params did not parse.
    #[error("invalid params JSON for `{method}`: {source}")]
    InvalidParams {
        /// RPC method name.
        method: String,
        /// JSON-parser detail.
        #[source]
        source: serde_json::Error,
    },
    /// The node returned an error for the dispatched call.
    ///
    /// When the underlying alloy `RpcError` carries a JSON-RPC
    /// `ErrorResp` payload (the normal shape for `eth_call` reverts)
    /// the structured `code` and `data` fields are propagated; for
    /// transport-side failures both are `None`.
    #[error("rpc `{method}` failed: {source}")]
    Rpc {
        /// RPC method name.
        method: String,
        /// JSON-RPC error code from `ErrorResp.code`. `None` when
        /// the failure was transport-level (no structured response).
        code: Option<i64>,
        /// JSON-encoded `ErrorResp.data` payload - for `eth_call`
        /// reverts this is the quoted hex string of the abi-encoded
        /// revert body (consumed by `shepherd_sdk::chain::
        /// decode_revert_hex`). `None` when the failure was
        /// transport-level.
        data: Option<String>,
        /// Transport-side typed error.
        #[source]
        source: alloy_transport::TransportError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_pool_rejects_lookups() {
        let pool = ProviderPool::empty();
        let err = pool
            .request(1, "eth_blockNumber".into(), "[]".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::UnknownChain(1)));
    }

    #[tokio::test]
    async fn empty_pool_rejects_block_subscribe() {
        let pool = ProviderPool::empty();
        // Can't use .unwrap_err() because BlockStream doesn't impl Debug.
        assert!(matches!(
            pool.subscribe_blocks(1).await,
            Err(ProviderError::UnknownChain(1))
        ));
    }

    #[tokio::test]
    async fn empty_pool_rejects_log_subscribe() {
        let pool = ProviderPool::empty();
        let filter = alloy_rpc_types_eth::Filter::new();
        assert!(matches!(
            pool.subscribe_logs(1, filter).await,
            Err(ProviderError::UnknownChain(1))
        ));
    }

    #[tokio::test]
    async fn invalid_params_json_is_rejected_before_network() {
        // RawValue::from_string rejects non-JSON; verify the parse layer
        // we rely on before forwarding to alloy.
        let bad = "not json at all {{{";
        let result = RawValue::from_string(bad.to_owned());
        assert!(result.is_err(), "invalid JSON should fail RawValue parse");
    }

    /// Helper: build an `EngineConfig` with a single HTTP chain entry.
    fn test_config(chain_id: u64, rpc_url: &str) -> EngineConfig {
        use crate::engine_config::{ChainConfig, EngineConfig};
        let mut chains = BTreeMap::new();
        chains.insert(
            chain_id,
            ChainConfig {
                rpc_url: rpc_url.to_owned(),
                orderbook_url: None,
                require_ws: false,
            },
        );
        EngineConfig {
            chains,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn invalid_params_through_request_produces_error() {
        let cfg = test_config(1, "http://127.0.0.1:1");
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(1, "eth_blockNumber".into(), "not json {{{".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::InvalidParams { .. }),
            "expected InvalidParams, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn rpc_error_on_unreachable_node() {
        let cfg = test_config(1, "http://127.0.0.1:1");
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(1, "eth_blockNumber".into(), "[]".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Rpc { .. }),
            "expected Rpc error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn rpc_error_on_malformed_node_response() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let cfg = test_config(1, &server.uri());
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(1, "eth_blockNumber".into(), "[]".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Rpc { .. }),
            "expected Rpc error from malformed response, got: {err:?}"
        );
    }
}
