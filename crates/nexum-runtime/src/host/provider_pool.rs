//! `nexum:host/chain` backend: per-chain provider opened from the engine
//! config at boot.
//!
//! `request` is a raw JSON-RPC dispatch over a typed [`ChainMethod`], so only
//! the permitted read surface reaches the transport; params pass through
//! unencoded and the result body returns verbatim. WS/WSS push `newHeads`;
//! HTTP polls `eth_getBlockByNumber`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use alloy_chains::Chain;
use alloy_primitives::Bytes;
use alloy_provider::{CanonicalEvent, DynProvider, Provider, ProviderBuilder, WsConnect};
use alloy_rpc_client::ClientBuilder;
use alloy_rpc_types_eth::{Filter, Header, Log};
use alloy_transport::layers::RetryBackoffLayer;
use futures::stream::Stream;
use futures::stream::StreamExt as _;
use serde_json::value::RawValue;
use strum::IntoStaticStr;
use thiserror::Error;
use tracing::info;

use crate::engine_config::EngineConfig;
use crate::host::component::ChainMethod;

/// Head re-poll cadence for chains without a block-time hint; known chains
/// derive it from [`Chain::average_blocktime_hint`].
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Transport retry-layer parameters; heal transient RPC blips below the
/// poller so a node hiccup does not force a re-open.
const RPC_MAX_RETRIES: u32 = 10;
const RPC_RETRY_BACKOFF_MS: u64 = 300;
/// Compute-units-per-second budget for rate-limited nodes; generous, this
/// pool is read-only and low-QPS.
const RPC_RETRY_CUPS: u64 = 100;

/// Transport retry layer applied to every provider in the pool.
fn retry_layer() -> RetryBackoffLayer {
    RetryBackoffLayer::new(RPC_MAX_RETRIES, RPC_RETRY_BACKOFF_MS, RPC_RETRY_CUPS)
}

/// One chain's opened provider plus how to drive it.
#[derive(Debug, Clone)]
struct ChainEndpoint {
    provider: DynProvider,
    timeout: Duration,
    /// WS/IPC drives block following by pubsub; HTTP polls.
    supports_pubsub: bool,
}

/// Providers keyed by chain.
#[derive(Debug, Clone)]
pub struct ProviderPool {
    providers: Arc<HashMap<Chain, ChainEndpoint>>,
    /// In-flight `eth_getLogs` groups during gap backfill; `0` clamps to `1`.
    log_backfill_concurrency: usize,
}

impl ProviderPool {
    /// Open one provider per chain in `cfg.chains`; connection failures
    /// propagate and are fatal at boot.
    pub async fn from_config(cfg: &EngineConfig) -> Result<Self, ProviderError> {
        let mut providers: HashMap<Chain, ChainEndpoint> = HashMap::new();
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
            let supports_pubsub = url.starts_with("ws://") || url.starts_with("wss://");
            let provider = if supports_pubsub {
                let client = ClientBuilder::default()
                    .layer(retry_layer())
                    .ws(WsConnect::new(url))
                    .await
                    .map_err(|source| ProviderError::Connect {
                        chain: *chain,
                        source,
                    })?;
                ProviderBuilder::new().connect_client(client).erased()
            } else {
                let parsed: url::Url = url.parse().map_err(|source| ProviderError::ConnectUrl {
                    chain: *chain,
                    source,
                })?;
                let client = ClientBuilder::default().layer(retry_layer()).http(parsed);
                ProviderBuilder::new().connect_client(client).erased()
            };
            if chain_cfg.request_timeout_secs == 0 {
                return Err(ProviderError::ZeroTimeout { chain: *chain });
            }
            let timeout = Duration::from_secs(chain_cfg.request_timeout_secs);
            providers.insert(
                *chain,
                ChainEndpoint {
                    provider,
                    timeout,
                    supports_pubsub,
                },
            );
        }
        Ok(Self {
            providers: Arc::new(providers),
            log_backfill_concurrency: cfg.engine.log_backfill_concurrency,
        })
    }

    /// Empty pool; every `request` returns `UnknownChain`.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self {
            providers: Arc::new(HashMap::new()),
            log_backfill_concurrency: 16,
        }
    }

    /// Follow canonical block headers on `chain`: WS via
    /// `eth_subscribe(newHeads)`, HTTP by polling at the chain's block time.
    pub async fn subscribe_blocks(&self, chain: Chain) -> Result<BlockStream, ProviderError> {
        let ep = self
            .providers
            .get(&chain)
            .ok_or(ProviderError::UnknownChain(chain))?;
        if ep.supports_pubsub {
            let sub =
                ep.provider
                    .subscribe_blocks()
                    .await
                    .map_err(|source| ProviderError::Rpc {
                        method: "eth_subscribe(newHeads)".into(),
                        code: None,
                        data: None,
                        source,
                    })?;
            let stream = sub.into_stream().map(Ok::<_, ProviderError>);
            return Ok(Box::pin(stream));
        }
        // HTTP fallback: poll the head, then follow canonical blocks by
        // number at roughly the chain's block time.
        let head = ep
            .provider
            .get_block_number()
            .await
            .map_err(|source| ProviderError::Rpc {
                method: "eth_blockNumber".into(),
                code: None,
                data: None,
                source,
            })?;
        let poll_interval = chain
            .average_blocktime_hint()
            .unwrap_or(DEFAULT_POLL_INTERVAL);
        let stream = ep
            .provider
            .watch_canonical_blocks_from(head)
            .poll_interval(poll_interval)
            .into_stream()
            // Reorg `Removed` events are dropped for now; the newHeads push
            // path never signalled reorgs either.
            .filter_map(|item| async move {
                match item {
                    Ok(CanonicalEvent::Added(block)) => Some(Ok(block.header.clone())),
                    Ok(CanonicalEvent::Removed(_)) => None,
                    Err(source) => Some(Err(ProviderError::Rpc {
                        method: "eth_getBlockByNumber".into(),
                        code: None,
                        data: None,
                        source,
                    })),
                }
            });
        Ok(Box::pin(stream))
    }

    /// Current head block number (`eth_blockNumber`).
    pub async fn block_number(&self, chain: Chain) -> Result<u64, ProviderError> {
        let ep = self
            .providers
            .get(&chain)
            .ok_or(ProviderError::UnknownChain(chain))?;
        ep.provider
            .get_block_number()
            .await
            .map_err(|source| ProviderError::Rpc {
                method: "eth_blockNumber".into(),
                code: None,
                data: None,
                source,
            })
    }

    /// Canonical (reorg-aware) log stream on `chain` from `start_block`. Each
    /// item is one block's matching logs (possibly empty); reorg rollbacks
    /// carry `removed == true`.
    pub fn watch_chain_logs(
        &self,
        chain: Chain,
        filter: Filter,
        start_block: u64,
    ) -> Result<CanonicalLogStream, ProviderError> {
        let ep = self
            .providers
            .get(&chain)
            .ok_or(ProviderError::UnknownChain(chain))?;
        // Poll at roughly the chain's block time: known chains carry a
        // hint, unknown (custom / dev) chains fall back to the default.
        let poll_interval = chain
            .average_blocktime_hint()
            .unwrap_or(DEFAULT_POLL_INTERVAL);
        let stream = ep
            .provider
            .watch_canonical_logs_from(start_block, &filter)
            .rpc_concurrency(self.log_backfill_concurrency)
            .poll_interval(poll_interval)
            .into_stream()
            .map(|item| {
                item.map(|event| {
                    // Stamp `removed` from the canonical event so a
                    // reorged-away log reaches the module flagged, letting
                    // it unwind state it built from the earlier delivery.
                    let (removed, block_logs) = match event {
                        CanonicalEvent::Added(block_logs) => (false, block_logs),
                        CanonicalEvent::Removed(block_logs) => (true, block_logs),
                    };
                    block_logs
                        .logs
                        .into_iter()
                        .map(|mut log| {
                            log.removed = removed;
                            log
                        })
                        .collect::<Vec<Log>>()
                })
                .map_err(|source| ProviderError::Rpc {
                    method: "eth_getLogs".into(),
                    code: None,
                    data: None,
                    source,
                })
            });
        Ok(Box::pin(stream))
    }

    /// Raw JSON-RPC dispatch; `params_json` is the JSON-encoded params array.
    pub async fn request(
        &self,
        chain: Chain,
        method: ChainMethod,
        params_json: String,
    ) -> Result<String, ProviderError> {
        let ep = self
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
        let result: Box<RawValue> = tokio::time::timeout(
            ep.timeout,
            ep.provider.raw_request(Cow::Borrowed(name), params),
        )
        .await
        .map_err(|_| ProviderError::Timeout {
            method: name.to_owned(),
        })?
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
/// Boxed canonical per-block log stream; reorg rollbacks carry
/// `removed == true`.
pub type CanonicalLogStream = Pin<Box<dyn Stream<Item = Result<Vec<Log>, ProviderError>> + Send>>;

/// Errors surfaced by [`ProviderPool`]. Variant names serialize snake_case as
/// `&'static str` for metric labels.
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
    /// Guest-supplied JSON params did not parse.
    #[error("invalid params JSON for `{method}`: {source}")]
    InvalidParams {
        /// RPC method name.
        method: String,
        /// JSON-parser detail.
        #[source]
        source: serde_json::Error,
    },
    /// `request_timeout_secs = 0`; rejected at boot.
    #[error("chain {chain}: request_timeout_secs must not be 0")]
    ZeroTimeout {
        /// Chain with the misconfigured timeout.
        chain: Chain,
    },
    /// RPC node did not respond within the per-request timeout.
    #[error("rpc `{method}` timed out")]
    Timeout {
        /// RPC method name.
        method: String,
    },
    /// Node returned an error for the dispatched call. JSON-RPC `ErrorResp`
    /// payloads propagate `code`/`data`; transport failures leave both `None`.
    #[error("rpc `{method}` failed: {source}")]
    Rpc {
        /// RPC method name.
        method: String,
        /// `ErrorResp.code`, `None` for transport-level failures.
        code: Option<i64>,
        /// Decoded `ErrorResp.data` (abi-encoded revert body), else `None`.
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
    async fn empty_pool_rejects_block_number() {
        let pool = ProviderPool::empty();
        assert!(matches!(
            pool.block_number(Chain::from_id(1)).await,
            Err(ProviderError::UnknownChain(c)) if c == Chain::from_id(1)
        ));
    }

    #[test]
    fn empty_pool_rejects_watch_chain_logs() {
        let pool = ProviderPool::empty();
        let filter = alloy_rpc_types_eth::Filter::new();
        // Can't use .unwrap_err() because CanonicalLogStream doesn't impl Debug.
        assert!(matches!(
            pool.watch_chain_logs(Chain::from_id(1), filter, 0),
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
        test_config_with_timeout(chain, rpc_url, 30)
    }

    /// As [`test_config`], with an explicit per-request timeout.
    fn test_config_with_timeout(chain: Chain, rpc_url: &str, timeout_secs: u64) -> EngineConfig {
        use crate::engine_config::{ChainConfig, EngineConfig};
        let mut chains = HashMap::new();
        chains.insert(
            chain,
            ChainConfig {
                rpc_url: rpc_url.to_owned(),
                request_timeout_secs: timeout_secs,
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

    #[tokio::test]
    async fn request_times_out_when_node_hangs() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        let server = MockServer::start().await;
        // Respond after 60 s - the pool is configured with a 1 s timeout,
        // so `raw_request` is cancelled well before the body arrives. The
        // large gap keeps the test from flaking on slow CI runners.
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(60))
                    .set_body_string(r#"{"jsonrpc":"2.0","id":0,"result":"0x1"}"#),
            )
            .mount(&server)
            .await;

        let cfg = test_config_with_timeout(Chain::from_id(1), &server.uri(), 1);
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        let err = pool
            .request(Chain::from_id(1), ChainMethod::EthBlockNumber, "[]".into())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Timeout { .. }),
            "expected Timeout, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn http_config_block_subscribe_takes_poll_path() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

        // An HTTP transport has no pubsub, so `subscribe_blocks` must fall
        // back to polling rather than erroring. The head fetch
        // (`eth_blockNumber`) is the only call made at setup - the block
        // poller stream is lazy - so one mocked response proves the poll
        // path opens cleanly.
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"jsonrpc":"2.0","id":0,"result":"0x10"}"#),
            )
            .mount(&server)
            .await;

        let cfg = test_config(Chain::from_id(1), &server.uri());
        let pool = ProviderPool::from_config(&cfg).await.unwrap();
        // BlockStream doesn't impl Debug, so assert on `is_ok` rather than
        // unwrapping.
        assert!(
            pool.subscribe_blocks(Chain::from_id(1)).await.is_ok(),
            "http config should open the block poll path without erroring",
        );
    }
}
