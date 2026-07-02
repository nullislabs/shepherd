//! Chainlink Aggregator V3 reader.
//!
//! [`read_latest_answer`] performs the full `eth_call → decode →
//! latestRoundData.answer` flow against a Chainlink AggregatorV3
//! oracle. Returns `Some(answer)` on success or `None` on any host /
//! decode failure (logging the failure at Warn). Used by oracle-driven
//! example modules (price-alert, stop-loss) so they consume the SDK
//! instead of redefining the `AggregatorV3` ABI + read loop locally.
//!
//! The shape is deliberately `Option<I256>` rather than
//! `Result<I256, HostError>`: every observed caller treats a fetch
//! failure as "skip this block, try next one", and `Option` makes
//! that the only path without forcing a discard pattern at the call
//! site.

use alloy_primitives::{Address, I256};
use alloy_sol_types::{SolCall, sol};

use crate::chain::{eth_call_params, parse_eth_call_result};
use crate::host::{Host, LogLevel};

sol! {
    /// Chainlink AggregatorV3Interface - only the function the
    /// shepherd SDK needs.
    interface AggregatorV3 {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
    }
}

/// Fetch the oracle's latest answer via `eth_call(latestRoundData)`.
///
/// Returns `Some(answer)` on success. Logs a Warn (prefixed with
/// `domain`) and returns `None` on any of:
///
/// - `host.request("eth_call", …)` returning `Err(HostError)`;
/// - the JSON-RPC result not parsing as `0x`-prefixed hex bytes;
/// - the ABI decode failing.
///
/// `domain` is embedded in the log line so a single host log stream
/// can disambiguate which module's oracle failed.
#[must_use]
pub fn read_latest_answer<H: Host>(
    host: &H,
    chain_id: u64,
    oracle: Address,
    domain: &str,
) -> Option<I256> {
    let call_data = AggregatorV3::latestRoundDataCall {}.abi_encode();
    let params = eth_call_params(&oracle, &call_data);
    let result_json = match host.request(chain_id, "eth_call", &params) {
        Ok(s) => s,
        Err(err) => {
            host.log(
                LogLevel::Warn,
                &format!(
                    "{domain}: chainlink oracle eth_call failed ({}): {}",
                    err.code, err.message
                ),
            );
            return None;
        }
    };
    let bytes = match parse_eth_call_result(&result_json) {
        Some(b) => b,
        None => {
            host.log(
                LogLevel::Warn,
                &format!("{domain}: chainlink oracle: cannot decode result hex {result_json}"),
            );
            return None;
        }
    };
    match AggregatorV3::latestRoundDataCall::abi_decode_returns(&bytes) {
        Ok(decoded) => Some(decoded.answer),
        Err(e) => {
            host.log(
                LogLevel::Warn,
                &format!("{domain}: chainlink oracle decode failed: {e}"),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    //! `MockHost`-driven coverage of the read path. Encodes a synthetic
    //! `latestRoundData` return into the `"0x..."` JSON the
    //! `chain::request` mock returns, then asserts the helper
    //! extracts the `answer` field.

    use super::*;
    use crate::host::{HostError, HostErrorKind};

    // We need `shepherd-sdk-test::MockHost` for these tests, but
    // `shepherd-sdk` cannot depend on `shepherd-sdk-test` (it's the
    // reverse). So we hand-roll a minimal Host impl here.

    struct StubHost<R> {
        response: std::cell::RefCell<Option<R>>,
        log_lines: std::cell::RefCell<Vec<String>>,
    }

    impl<R> StubHost<R> {
        fn new(response: R) -> Self {
            Self {
                response: std::cell::RefCell::new(Some(response)),
                log_lines: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl crate::host::LoggingHost for StubHost<Result<String, HostError>> {
        fn log(&self, _level: LogLevel, message: &str) {
            self.log_lines.borrow_mut().push(message.to_owned());
        }
    }
    impl crate::host::ChainHost for StubHost<Result<String, HostError>> {
        fn request(
            &self,
            _chain_id: u64,
            _method: &str,
            _params: &str,
        ) -> Result<String, HostError> {
            self.response
                .borrow_mut()
                .take()
                .expect("StubHost::request called more than once")
        }
    }
    impl crate::host::LocalStoreHost for StubHost<Result<String, HostError>> {
        fn get(&self, _key: &str) -> Result<Option<Vec<u8>>, HostError> {
            unreachable!("not used in this test")
        }
        fn set(&self, _key: &str, _value: &[u8]) -> Result<(), HostError> {
            unreachable!("not used in this test")
        }
        fn delete(&self, _key: &str) -> Result<(), HostError> {
            unreachable!("not used in this test")
        }
        fn list_keys(&self, _prefix: &str) -> Result<Vec<String>, HostError> {
            unreachable!("not used in this test")
        }
    }
    impl crate::host::CowApiHost for StubHost<Result<String, HostError>> {
        fn submit_order(&self, _chain_id: u64, _body: &[u8]) -> Result<String, HostError> {
            unreachable!("not used in this test")
        }
        fn cow_api_request(
            &self,
            _chain_id: u64,
            _method: &str,
            _path: &str,
            _body: Option<&str>,
        ) -> Result<String, HostError> {
            unreachable!("not used in this test")
        }
    }

    fn encode_round(answer: i64) -> String {
        let returns = AggregatorV3::latestRoundDataReturn {
            roundId: alloy_primitives::aliases::U80::from(1u64),
            answer: I256::try_from(answer).unwrap(),
            startedAt: alloy_primitives::U256::from(0u64),
            updatedAt: alloy_primitives::U256::from(0u64),
            answeredInRound: alloy_primitives::aliases::U80::from(1u64),
        };
        let bytes = AggregatorV3::latestRoundDataCall::abi_encode_returns(&returns);
        format!("\"0x{}\"", alloy_primitives::hex::encode(&bytes))
    }

    const ORACLE: Address = alloy_primitives::address!("694AA1769357215DE4FAC081bf1f309aDC325306");

    #[test]
    fn returns_some_on_happy_path() {
        let host = StubHost::new(Ok(encode_round(1_700_000_000_000)));
        let v = read_latest_answer(&host, 11_155_111, ORACLE, "test-domain");
        assert_eq!(v, Some(I256::try_from(1_700_000_000_000_i64).unwrap()));
    }

    #[test]
    fn returns_none_and_logs_on_host_error() {
        let host = StubHost::new(Err(HostError {
            domain: "chain".into(),
            kind: HostErrorKind::Unavailable,
            code: 503,
            message: "rpc down".into(),
            data: None,
        }));
        let v = read_latest_answer(&host, 11_155_111, ORACLE, "my-mod");
        assert!(v.is_none());
        let logs = host.log_lines.borrow();
        assert!(logs.iter().any(|l| l.contains("my-mod")));
        assert!(logs.iter().any(|l| l.contains("eth_call failed")));
    }

    #[test]
    fn returns_none_on_garbage_hex() {
        let host = StubHost::new(Ok("\"not-hex\"".to_owned()));
        let v = read_latest_answer(&host, 11_155_111, ORACLE, "my-mod");
        assert!(v.is_none());
        assert!(
            host.log_lines
                .borrow()
                .iter()
                .any(|l| l.contains("cannot decode result hex"))
        );
    }
}
