//! `nexum:host/chain` backend.
//!
//! Per-chain alloy provider, opened from the engine config at boot.
//! `request` is a raw JSON-RPC dispatch: the host hands `(method,
//! params)` straight to alloy's transport and returns the result body
//! verbatim. The method is a typed [`ChainMethod`], so only the
//! permitted read surface can reach the transport; params are passed
//! through without re-encoding.
//!
//! Transports:
//! - `ws://` / `wss://`  - `WsConnect`; required for `eth_subscribe`.
//! - `http://` / `https://` - alloy's HTTP transport; request/response only.

use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use alloy_chains::Chain;
use alloy_primitives::Bytes;
use alloy_provider::{DynProvider, Provider, ProviderBuilder, WsConnect};
use alloy_rpc_types_eth::{Filter, Header, Log};
use futures::stream::Stream;
use futures::stream::StreamExt as _;
use serde_json::value::RawValue;
use strum::IntoStaticStr;
use thiserror::Error;
use tracing::info;

use crate::engine_config::EngineConfig;
use crate::host::component::ChainMethod;

/// Pool of alloy providers keyed by chain.
#[derive(Debug, Clone)]
pub struct ProviderPool {
    providers: Arc<HashMap<Chain, DynProvider>>,
}

impl ProviderPool {
    /// Open one provider per chain in `cfg.chains`. WebSocket URLs
    /// engage alloy's pubsub transport; HTTP URLs use the HTTP
    /// transport. Connection failures propagate to the caller; the
    /// engine treats them as fatal at boot.
    pub async fn from_config(cfg: &EngineConfig) -> Result<Self, ProviderError> {
        let mut providers: HashMap<Chain, DynProvider> = HashMap::new();
        // Sort by numeric id so the boot logs are deterministic
        // (`Chain` is not `Ord`).
        let mut entries: Vec<_> = cfg.chains.iter().collect();
        entries.sort_by_key(|(c, _)| c.id());
        for (chain, chain_cfg) in entries {
            let url = chain_cfg.rpc_url.as_str();
            // The boot log carries the URL with embedded API keys
            // redacted - log aggregators (Loki, Datadog, splunk) often
            // ingest these lines and the key shouldn't end up in
            // long-term storage. The engine still uses the full URL
            // when actually connecting to the provider below.
            info!(
                chain_id = chain.id(),
                url = %crate::engine_config::redact_url(url),
                "opening chain RPC provider",
            );
            let provider = if url.starts_with("ws://") || url.starts_with("wss://") {
                ProviderBuilder::new()
                    .connect_ws(WsConnect::new(url))
                    .await
                    .map_err(|source| ProviderError::Connect {
                        chain: *chain,
                        source,
                    })?
                    .erased()
            } else {
                let parsed: url::Url = url.parse().map_err(|source| ProviderError::ConnectUrl {
                    chain: *chain,
                    source,
                })?;
                ProviderBuilder::new().connect_http(parsed).erased()
            };
            providers.insert(*chain, provider);
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
            providers: Arc::new(HashMap::new()),
        }
    }

    /// Open a new-blocks (`eth_subscribe newHeads`) stream on
    /// `chain_id`. Requires a WS / IPC transport at construction
    /// time; HTTP-only providers surface `UnknownChain` here.
    pub async fn subscribe_blocks(&self, chain: Chain) -> Result<BlockStream, ProviderError> {
        let provider = self
            .providers
            .get(&chain)
            .ok_or(ProviderError::UnknownChain(chain))?;
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
    pub async fn subscribe_chain_logs(
        &self,
        chain: Chain,
        filter: Filter,
    ) -> Result<ChainLogStream, ProviderError> {
        let provider = self
            .providers
            .get(&chain)
            .ok_or(ProviderError::UnknownChain(chain))?;
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

    /// Raw JSON-RPC dispatch. `method` is a permitted read-surface
    /// method; `params_json` must be the JSON encoding of the params
    /// array (e.g. `"[\"0x...\",\"latest\"]"`), as produced by the
    /// SDK's `chain::request` glue.
    pub async fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
        params_json: String,
    ) -> Result<String, ProviderError> {
        let provider = self
            .providers
            .get(&chain)
            .ok_or(ProviderError::UnknownChain(chain))?;
        let name = method.as_str();
        // Pass the params through as a raw JSON value so alloy does
        // not re-encode them on the way to the node.
        let params: Box<RawValue> =
            RawValue::from_string(params_json).map_err(|source| ProviderError::InvalidParams {
                method: name.to_owned(),
                source,
            })?;
        let result: Box<RawValue> = provider
            .raw_request(Cow::Borrowed(name), params)
            .await
            .map_err(|source| {
                // When the node returns a JSON-RPC error response
                // (`{"error": {"code":..., "data":...}}`) - typically
                // an `eth_call` revert - capture the structured
                // payload and decode the hex `error.data` into raw
                // bytes once here, so a guest receives the abi-encoded
                // revert body directly. Transport-side failures
                // (timeouts, serde, etc.) leave both `code` and `data`
                // `None` so the projection can tell "no ErrorResp"
                // apart from "ErrorResp with code = 0".
                let (code, data) = match source.as_error_resp() {
                    Some(payload) => (
                        Some(payload.code),
                        // alloy decodes the hex `error.data` JSON string into
                        // `Bytes` in one step; the guest binding is `Vec<u8>`,
                        // so land it there once.
                        payload
                            .try_data_as::<Bytes>()
                            .and_then(Result::ok)
                            .map(|b| b.to_vec()),
                    ),
                    None => (None, None),
                };
                ProviderError::Rpc {
                    method: name.to_owned(),
                    code,
                    data,
                    source,
                }
            })?;
        // Unbox the raw result into the returned String without
        // copying the body; the WIT boundary copy is the only one left.
        Ok(String::from(Box::<str>::from(result)))
    }
}

/// Boxed stream of `newHeads`-style block headers.
pub type BlockStream = Pin<Box<dyn Stream<Item = Result<Header, ProviderError>> + Send>>;
/// Boxed stream of `logs`-filtered chain-log events.
pub type ChainLogStream = Pin<Box<dyn Stream<Item = Result<Log, ProviderError>> + Send>>;

/// Errors surfaced by [`ProviderPool`].
///
/// `IntoStaticStr` produces the snake_case variant name as
/// `&'static str` for metric labels and structured-log fields; the
/// per-variant Display still carries the detail via `thiserror`.
#[derive(Debug, Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderError {
    /// Chain absent from the engine config.
    #[error("unknown chain {0} (no engine.toml entry)")]
    UnknownChain(Chain),
    /// Could not open the underlying transport.
    #[error("connect chain {chain}: {source}")]
    Connect {
        /// Chain we failed to dial.
        chain: Chain,
        /// Transport-side error.
        #[source]
        source: alloy_transport::TransportError,
    },
    /// HTTP RPC URL did not parse as a [`url::Url`].
    #[error("connect chain {chain}: invalid URL: {source}")]
    ConnectUrl {
        /// Chain whose `rpc_url` was malformed.
        chain: Chain,
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
        /// Decoded `ErrorResp.data` payload - for `eth_call` reverts
        /// this is the abi-encoded revert body, hex-decoded from the
        /// upstream JSON string once here (consumed directly by
        /// `shepherd_sdk::cow::decode_revert`). `None` when the failure
        /// was transport-level or the payload was not a hex string.
        data: Option<Vec<u8>>,
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
            .request(Chain::from_id(1), ChainMethod::EthBlockNumber, "[]".into())
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::UnknownChain(c) if c == Chain::from_id(1)));
    }

    #[tokio::test]
    async fn empty_pool_rejects_block_subscribe() {
        let pool = ProviderPool::empty();
        // Can't use .unwrap_err() because BlockStream doesn't impl Debug.
        assert!(matches!(
            pool.subscribe_blocks(Chain::from_id(1)).await,
            Err(ProviderError::UnknownChain(c)) if c == Chain::from_id(1)
        ));
    }

    #[tokio::test]
    async fn empty_pool_rejects_chain_log_subscribe() {
        let pool = ProviderPool::empty();
        let filter = alloy_rpc_types_eth::Filter::new();
        assert!(matches!(
            pool.subscribe_chain_logs(Chain::from_id(1), filter).await,
            Err(ProviderError::UnknownChain(c)) if c == Chain::from_id(1)
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
    fn test_config(chain: Chain, rpc_url: &str) -> EngineConfig {
        use crate::engine_config::{ChainConfig, EngineConfig};
        let mut chains = HashMap::new();
        chains.insert(
            chain,
            ChainConfig {
                rpc_url: rpc_url.to_owned(),
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
        let cfg = test_config(Chain::from_id(1), "http://127.0.0.1:1");
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(
                Chain::from_id(1),
                ChainMethod::EthBlockNumber,
                "not json {{{".into(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::InvalidParams { .. }),
            "expected InvalidParams, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn rpc_error_on_unreachable_node() {
        let cfg = test_config(Chain::from_id(1), "http://127.0.0.1:1");
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(Chain::from_id(1), ChainMethod::EthBlockNumber, "[]".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Rpc { .. }),
            "expected Rpc error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn request_returns_result_body_verbatim() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        // The raw `result` bytes must come back byte-identical: no
        // re-encoding, no DOM round trip, quotes preserved.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"jsonrpc":"2.0","id":0,"result":{"number":"0x10","extra":[1,2]}}"#,
            ))
            .mount(&server)
            .await;

        let cfg = test_config(Chain::from_id(1), &server.uri());
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let body = pool
            .request(Chain::from_id(1), ChainMethod::EthBlockNumber, "[]".into())
            .await
            .unwrap();
        assert_eq!(body, r#"{"number":"0x10","extra":[1,2]}"#);
    }

    #[tokio::test]
    async fn rpc_error_on_malformed_node_response() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let cfg = test_config(Chain::from_id(1), &server.uri());
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(Chain::from_id(1), ChainMethod::EthBlockNumber, "[]".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Rpc { .. }),
            "expected Rpc error from malformed response, got: {err:?}"
        );
    }

    #[test]
    fn error_data_decodes_hex_string_and_ignores_non_hex() {
        // The `try_data_as::<Bytes>` seam decodes the upstream
        // `error.data` JSON string into bytes; a structured object or a
        // non-hex string fails to deserialise, which the projection
        // swallows to `None` (treated the same as "no revert body").
        let decode = |json: &str| serde_json::from_str::<Bytes>(json).ok().map(|b| b.to_vec());
        assert_eq!(decode("\"0x08c379a0\""), Some(vec![0x08, 0xc3, 0x79, 0xa0]));
        assert_eq!(decode("{\"reason\":\"x\"}"), None);
        assert_eq!(decode("\"not hex\""), None);
    }

    #[tokio::test]
    async fn rpc_error_data_is_hex_decoded_from_upstream() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        // The node returns a JSON-RPC `ErrorResp` with a hex `data`
        // payload (the `eth_call` revert shape). The host must capture
        // the code and the DECODED revert bytes on `ProviderError::Rpc`.
        let revert_bytes = vec![0x08, 0xc3, 0x79, 0xa0, 0xde, 0xad, 0xbe, 0xef];
        let revert_hex = alloy_primitives::hex::encode_prefixed(&revert_bytes);
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"jsonrpc":"2.0","id":0,"error":{{"code":-32000,"message":"execution reverted","data":"{revert_hex}"}}}}"#,
            )))
            .mount(&server)
            .await;

        let cfg = test_config(Chain::from_id(1), &server.uri());
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(Chain::from_id(1), ChainMethod::EthCall, "[]".into())
            .await
            .unwrap_err();
        let ProviderError::Rpc { code, data, .. } = err else {
            panic!("expected Rpc error, got: {err:?}");
        };
        assert_eq!(code, Some(-32000));
        assert_eq!(data, Some(revert_bytes));
    }
}
