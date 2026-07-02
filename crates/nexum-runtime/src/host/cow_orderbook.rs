//! `shepherd:cow/cow-api` backend.
//!
//! Two responsibilities:
//!
//! 1. `request` - generic REST passthrough. Module gives the HTTP
//!    method, path (relative to the chain's orderbook base URL), and
//!    optional JSON body. We dispatch via `reqwest`, return the
//!    response body verbatim.
//! 2. `submit_order` - typed submission. Module gives a JSON-encoded
//!    `cowprotocol::OrderCreation`; we parse, dispatch via
//!    `cowprotocol::OrderBookApi::post_order`, return the assigned
//!    `OrderUid` as a `0x`-prefixed hex string.
//!
//! Per-chain `OrderBookApi` instances are constructed once at engine
//! boot from the discriminated chain set in `cowprotocol::Chain`.
//! Chains the SDK does not know about return `Unsupported` at the
//! host call boundary.

use std::collections::BTreeMap;
use std::time::Duration;

use cowprotocol::{Chain, OrderBookApi, OrderCreation, OrderUid};
use strum::IntoStaticStr;
use thiserror::Error;

/// Process-wide pool of `OrderBookApi` clients keyed by EVM chain id.
#[derive(Debug, Clone)]
pub struct OrderBookPool {
    clients: BTreeMap<u64, OrderBookApi>,
    http: reqwest::Client,
}

/// Canonical CoW Protocol chain set the engine ships clients for.
///
/// Both `Default::default()` and `OrderBookPool::from_config` walk
/// this single source of truth so a new chain joining CoW protocol
/// only needs a one-line addition here instead of two parallel
/// arrays.
const DEFAULT_CHAINS: &[Chain] = &[
    Chain::Mainnet,
    Chain::Gnosis,
    Chain::Sepolia,
    Chain::ArbitrumOne,
    Chain::Base,
];

impl Default for OrderBookPool {
    /// Build a pool covering every `cowprotocol::Chain` variant. Each entry
    /// uses the canonical `api.cow.fi/{slug}/api/v1` base URL from the SDK.
    /// Override individual entries via `OrderBookApi::new_with_base_url` for
    /// barn or staging targets.
    fn default() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builder");
        let clients = DEFAULT_CHAINS
            .iter()
            .map(|c| (c.id(), OrderBookApi::new(*c)))
            .collect();
        Self { clients, http }
    }
}

impl OrderBookPool {
    /// Build a pool from engine config, honouring any
    /// `[chains.<id>] orderbook_url = "..."` overrides. Chains
    /// without an override fall back to the canonical
    /// `cowprotocol::Chain` URLs (same as [`OrderBookPool::default`]).
    ///
    /// Used by the load test to point all submissions at
    /// `tools/orderbook-mock`, and by staging/barn deployments that
    /// run against a non-production orderbook.
    pub fn from_config(cfg: &crate::engine_config::EngineConfig) -> Self {
        let http = reqwest::Client::new();
        let mut clients: BTreeMap<u64, OrderBookApi> = DEFAULT_CHAINS
            .iter()
            .map(|c| (c.id(), OrderBookApi::new(*c)))
            .collect();
        for (chain_id, chain_cfg) in &cfg.chains {
            if let Some(url) = chain_cfg.orderbook_url.as_deref() {
                match url.parse::<url::Url>() {
                    Ok(parsed) => {
                        tracing::info!(chain_id, url, "cow-api: orderbook URL override");
                        clients.insert(*chain_id, OrderBookApi::new_with_base_url(parsed));
                    }
                    Err(e) => {
                        tracing::warn!(chain_id, url, error = %e, "cow-api: bad orderbook_url, falling back to canonical");
                    }
                }
            }
        }
        Self { clients, http }
    }

    /// Look up the client for a chain.
    pub fn get(&self, chain_id: u64) -> Result<&OrderBookApi, CowApiError> {
        self.clients
            .get(&chain_id)
            .ok_or(CowApiError::UnknownChain(chain_id))
    }

    /// REST passthrough. The base URL is whichever URL the pool's
    /// `OrderBookApi` client carries - overrides set via
    /// `OrderBookApi::new_with_base_url` (staging, wiremock) flow
    /// through here too, which keeps the passthrough and the typed
    /// `submit_order_json` path aimed at the same orderbook.
    pub async fn request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, CowApiError> {
        let api = self.get(chain_id)?;
        let base = api.base_url().clone();
        // `path` may or may not lead with a slash; `Url::join` handles
        // both, but we strip a single leading `/` so consumers can
        // write either `/orders/...` or `orders/...` interchangeably.
        let trimmed = path.strip_prefix('/').unwrap_or(path);
        let url = base
            .join(trimmed)
            .map_err(|e| CowApiError::BadPath(format!("{path:?}: {e}")))?;

        let request = match method.to_ascii_uppercase().as_str() {
            "GET" => self.http.get(url),
            "POST" => self.http.post(url),
            "PUT" => self.http.put(url),
            "DELETE" => self.http.delete(url),
            other => return Err(CowApiError::BadMethod(other.to_owned())),
        };
        let request = if let Some(body) = body {
            request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_owned())
        } else {
            request
        };

        let response = request.send().await.map_err(CowApiError::Network)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(CowApiError::Network)?;
        // Non-2xx responses are surfaced as HttpError so the guest can
        // distinguish 404 (not found) from 200 (success) via HostError.code.
        // The full response body is preserved in the error for structured
        // decoding (e.g. `{"errorType": "...", "description": "..."}`).
        if status >= 400 {
            return Err(CowApiError::HttpError { status, body: text });
        }
        Ok(text)
    }

    /// Typed submission. `body` is the JSON encoding of
    /// `cowprotocol::OrderCreation`. The chain's orderbook validates
    /// `from`, the EIP-712 hash, and (if `Eip1271`) the contract
    /// signature; we return whatever UID it assigns.
    pub async fn submit_order_json(
        &self,
        chain_id: u64,
        body: &[u8],
    ) -> Result<OrderUid, CowApiError> {
        let creation: OrderCreation = serde_json::from_slice(body).map_err(CowApiError::Decode)?;
        let api = self.get(chain_id)?;
        let uid = api.post_order(&creation).await?;
        Ok(uid)
    }
}

/// `IntoStaticStr` exposes the snake_case variant name as a
/// `&'static str` (`"unknown_chain"`, `"bad_method"`, ...) so the
/// `shepherd_cow_api_*` metric labels and structured-log fields stay
/// in sync with the Rust source of truth instead of growing a
/// `match err { ... => "decode" ... }` ladder per call site.
#[derive(Debug, Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
pub enum CowApiError {
    #[error("unknown chain {0} (no cowprotocol::Chain variant)")]
    UnknownChain(u64),
    #[error("bad HTTP method `{0}` (expected GET/POST/PUT/DELETE)")]
    BadMethod(String),
    #[error("invalid path: {0}")]
    BadPath(String),
    #[error("HTTP {status}")]
    HttpError { status: u16, body: String },
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    #[error("decode OrderCreation JSON: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("orderbook: {0}")]
    Orderbook(#[from] cowprotocol::Error),
}

#[cfg(test)]
mod tests;
