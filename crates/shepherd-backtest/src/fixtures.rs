//! JSON deserialization for the Python collector's
//! `tools/backtest-collect/fixtures-YYYY-MM-DD.json` output.
//!
//! Mirrors `tools/backtest-collect/backtest_collect.py` exactly:
//! every field present in the JSON must round-trip into a
//! [`Fixtures`] without information loss, since the replay
//! harness relies on raw `eth_getLogs` topics + data to reconstruct
//! a faithful `ChainLogView`. TWAP fields are deserialised but not yet
//! consumed by the replay (Phase 2B); keep them on the struct so
//! the fixture file is the canonical schema.

#![allow(dead_code)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Fixtures {
    pub metadata: Metadata,
    pub ethflow_orders: Vec<EthFlowFixture>,
    pub twap_conditionals: Vec<TwapFixture>,
}

#[derive(Debug, Deserialize)]
pub struct Metadata {
    pub collected_at: String,
    pub chain_id: u64,
    pub chain_name: String,
    pub window_days: u32,
    pub from_block: u64,
    pub to_block: u64,
    pub rpc_url: String,
    pub cow_api: String,
    pub ethflow_owner: String,
    pub composable_cow: String,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct EthFlowFixture {
    pub uid: String,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub tx_hash: Option<String>,
    pub log_index: u64,
    pub contract: String,
    pub sender: Option<String>,
    pub app_data_hash: String,
    /// Resolved app_data document fetched from
    /// `GET /api/v1/app_data/{hash}` at collection time. `None` if
    /// the hash 404'd (no mirror in the orderbook's app_data store).
    pub app_data_resolved: Option<serde_json::Value>,
    pub raw_log: RawLog,
}

#[derive(Debug, Deserialize)]
pub struct TwapFixture {
    pub owner: Option<String>,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub tx_hash: Option<String>,
    pub log_index: u64,
    pub params: TwapParams,
    pub raw_log: RawLog,
}

#[derive(Debug, Deserialize)]
pub struct TwapParams {
    pub handler: String,
    pub salt: String,
    pub static_input: String,
}

#[derive(Debug, Deserialize)]
pub struct RawLog {
    /// Each topic is a 32-byte hex string with `0x` prefix. The
    /// `OrderPlacement` and `ConditionalOrderCreated` events both
    /// carry exactly 2 topics: `topic0` (the signature hash) and
    /// `topic1` (the indexed `sender` / `owner` address).
    pub topics: Vec<String>,
    /// ABI-encoded payload, hex-prefixed.
    pub data: String,
}

impl RawLog {
    /// Decode each `0x...` topic into a 32-byte vector. The strategy
    /// layer reads topics as `&[u8]` (right-padded address in topic1
    /// for indexed parameters), so we preserve the byte order.
    pub fn topics_bytes(&self) -> Result<Vec<Vec<u8>>, hex::FromHexError> {
        self.topics
            .iter()
            .map(|t| hex::decode(t.strip_prefix("0x").unwrap_or(t.as_str())))
            .collect()
    }

    /// Decode the `data` hex string.
    pub fn data_bytes(&self) -> Result<Vec<u8>, hex::FromHexError> {
        hex::decode(self.data.strip_prefix("0x").unwrap_or(self.data.as_str()))
    }
}

/// Decode a `0x...` address string into the 20-byte representation
/// the strategy consumes. Thin wrapper around the shared
/// [`nexum_sdk::address::parse_address`] helper (JC5
/// consolidation) so this crate, balance-tracker, and any future
/// strategy module surface the same typed error.
pub fn parse_address(s: &str) -> Result<[u8; 20], nexum_sdk::address::AddressParse> {
    nexum_sdk::address::parse_address(s).map(|addr| addr.into_array())
}
