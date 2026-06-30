//! # ethflow-watcher (Shepherd module)
//!
//! Subscribes to `CoWSwapOnchainOrders.OrderPlacement` logs from the
//! CoWSwap EthFlow contracts and resubmits each placed order through
//! the orderbook API with `Signature::Eip1271`. The EthFlow contract
//! is the EIP-1271 verifier, so the `from` field on the resubmission
//! is the contract address (not the original native-token seller).
//!
//! ## Module layout (BLEU-855)
//!
//! - `strategy.rs` holds the pure logic and unit tests against
//!   `shepherd_sdk::host::Host`. It does not know `wit-bindgen`
//!   exists.
//! - `lib.rs` (this file) is the per-cdylib glue: wit-bindgen import
//!   shims, the `WitBindgenHost` adapter that bridges the generated
//!   free functions to the SDK traits, and the `Guest` impl that
//!   delegates the `Logs` event variant to `strategy::on_logs`.

// wit_bindgen::generate! expands to host-import shims whose arity
// matches the WIT signatures, which can exceed clippy's
// too-many-arguments threshold.
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

mod strategy;

use shepherd_sdk::host::{
    ChainHost, CowApiHost, HostError as SdkHostError, HostErrorKind as SdkHostErrorKind,
    LocalStoreHost, LogLevel as SdkLogLevel, LoggingHost,
};

use nexum::host::types::HostErrorKind;
use nexum::host::{chain, local_store, logging, types};
use shepherd::cow::cow_api;

struct WitBindgenHost;

impl ChainHost for WitBindgenHost {
    fn request(&self, chain_id: u64, method: &str, params: &str) -> Result<String, SdkHostError> {
        chain::request(chain_id, method, params).map_err(convert_err)
    }
}

impl LocalStoreHost for WitBindgenHost {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SdkHostError> {
        local_store::get(key).map_err(convert_err)
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<(), SdkHostError> {
        local_store::set(key, value).map_err(convert_err)
    }
    fn delete(&self, key: &str) -> Result<(), SdkHostError> {
        local_store::delete(key).map_err(convert_err)
    }
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, SdkHostError> {
        local_store::list_keys(prefix).map_err(convert_err)
    }
}

impl CowApiHost for WitBindgenHost {
    fn submit_order(&self, chain_id: u64, body: &[u8]) -> Result<String, SdkHostError> {
        cow_api::submit_order(chain_id, body).map_err(convert_err)
    }
    fn cow_api_request(
        &self,
        chain_id: u64,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, SdkHostError> {
        cow_api::request(chain_id, method, path, body).map_err(convert_err)
    }
}

impl LoggingHost for WitBindgenHost {
    fn log(&self, level: SdkLogLevel, message: &str) {
        logging::log(convert_level(level), message);
    }
}

fn convert_err(e: HostError) -> SdkHostError {
    SdkHostError {
        domain: e.domain,
        kind: match e.kind {
            HostErrorKind::Unsupported => SdkHostErrorKind::Unsupported,
            HostErrorKind::Unavailable => SdkHostErrorKind::Unavailable,
            HostErrorKind::Denied => SdkHostErrorKind::Denied,
            HostErrorKind::RateLimited => SdkHostErrorKind::RateLimited,
            HostErrorKind::Timeout => SdkHostErrorKind::Timeout,
            HostErrorKind::InvalidInput => SdkHostErrorKind::InvalidInput,
            HostErrorKind::Internal => SdkHostErrorKind::Internal,
            _ => SdkHostErrorKind::Internal,
        },
        code: e.code,
        message: e.message,
        data: e.data,
    }
}

fn sdk_err_into_wit(e: SdkHostError) -> HostError {
    HostError {
        domain: e.domain,
        kind: match e.kind {
            SdkHostErrorKind::Unsupported => HostErrorKind::Unsupported,
            SdkHostErrorKind::Unavailable => HostErrorKind::Unavailable,
            SdkHostErrorKind::Denied => HostErrorKind::Denied,
            SdkHostErrorKind::RateLimited => HostErrorKind::RateLimited,
            SdkHostErrorKind::Timeout => HostErrorKind::Timeout,
            SdkHostErrorKind::InvalidInput => HostErrorKind::InvalidInput,
            SdkHostErrorKind::Internal => HostErrorKind::Internal,
            _ => HostErrorKind::Internal,
        },
        code: e.code,
        message: e.message,
        data: e.data,
    }
}

fn convert_level(l: SdkLogLevel) -> logging::Level {
    match l {
        SdkLogLevel::Trace => logging::Level::Trace,
        SdkLogLevel::Debug => logging::Level::Debug,
        SdkLogLevel::Info => logging::Level::Info,
        SdkLogLevel::Warn => logging::Level::Warn,
        SdkLogLevel::Error => logging::Level::Error,
        _ => logging::Level::Info,
    }
}

struct EthFlowWatcher;

impl Guest for EthFlowWatcher {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "ethflow-watcher init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        if let types::Event::Logs(logs) = event {
            let views: Vec<strategy::LogView<'_>> = logs
                .iter()
                .map(|log| strategy::LogView {
                    chain_id: log.chain_id,
                    address: &log.address,
                    topics: &log.topics,
                    data: &log.data,
                })
                .collect();
            strategy::on_logs(&WitBindgenHost, &views).map_err(sdk_err_into_wit)?;
        }
        // Block / Tick / Message are not used by this module.
        Ok(())
    }
}

export!(EthFlowWatcher);
