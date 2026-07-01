//! Pure strategy logic for the ethflow-watcher module.
//!
//! Every interaction with the world flows through the
//! `shepherd_sdk::host::Host` trait seam - no direct calls to wit-
//! bindgen-generated free functions live here. The `lib.rs` glue
//! wraps a `WitBindgenHost` adapter around the per-cdylib wit-bindgen
//! imports and hands it to [`on_logs`]; tests under `#[cfg(test)]`
//! drive the same function with `shepherd_sdk_test::MockHost`.
//!
//! ## Design (redesign)
//!
//! The original design POSTed each on-chain `OrderPlacement`
//! to `/api/v1/orders` with the EthFlow contract as the EIP-1271 owner.
//! Empirical evidence (2026-06-22 Sepolia soak) showed that path cannot
//! succeed: the orderbook backend indexes EthFlow `OrderPlacement`
//! events natively and writes server-only fields (`onchainUser`,
//! `onchainOrderData`, `ethflowData.userValidTo`) the public POST body
//! does not carry. Submissions through `/api/v1/orders` are rejected
//! with `ExcessiveValidTo` even though the same UID is `fulfilled` on
//! the orderbook by the time we look.
//!
//! This strategy therefore **observes + verifies** instead of
//! submitting:
//!
//! 1. Decode the `OrderPlacement` log against the canonical EthFlow
//!    contract addresses.
//! 2. Compute the orderbook UID from the on-chain order shape
//!    (`OrderData::uid(domain, contract)`).
//! 3. GET `/api/v1/orders/{uid}` to confirm the orderbook indexer
//!    picked up the placement. On 200, mark `observed:{uid}` so log
//!    re-delivery is a no-op. On 404, log at Info - typical indexer
//!    lag, do not write the marker so the next re-delivery rechecks.
//!    Any other error is logged at Warn for operator follow-up.

use alloy_primitives::{Address, B256, Bytes};
use alloy_sol_types::SolEvent;
use cowprotocol::{
    Chain, CoWSwapOnchainOrders::OrderPlacement, ETH_FLOW_PRODUCTION, ETH_FLOW_STAGING,
    GPv2OrderData, OnchainSignature, OrderUid,
};
use shepherd_sdk::cow::gpv2_to_order_data;
use shepherd_sdk::host::{Host, HostError, LogLevel};

/// Fields the strategy needs from a wit-bindgen `log`. Borrowed slices
/// keep the strategy independent from the per-cdylib wit types.
pub struct LogView<'a> {
    pub chain_id: u64,
    pub address: &'a [u8],
    pub topics: &'a [Vec<u8>],
    pub data: &'a [u8],
}

/// Decoded payload of a `CoWSwapOnchainOrders.OrderPlacement` log.
/// `GPv2OrderData` is ~300 bytes; box it so the struct stays
/// cache-friendly when threaded through the observe path.
#[derive(Debug)]
pub(crate) struct DecodedPlacement {
    /// EthFlow contract that emitted the event - also the EIP-1271
    /// owner of the resulting orderbook entry, used as the UID
    /// `owner` input.
    pub(crate) contract: Address,
    /// Original native-token seller. Logged for operator diagnostics;
    /// not the orderbook owner.
    pub(crate) sender: Address,
    pub(crate) order: Box<GPv2OrderData>,
    /// Decoded signature. Recorded by the orderbook indexer itself;
    /// not consumed by the observe path.
    #[allow(dead_code)]
    pub(crate) signature: OnchainSignature,
    /// Refund pointer / opaque placer metadata embedded in the
    /// `OrderPlacement` event. The orderbook indexer derives
    /// `ethflowData.userValidTo` from this blob; we keep it on the
    /// struct for parity with the decoder contract.
    #[allow(dead_code)]
    pub(crate) data: Bytes,
}

/// Entry point: decode every `OrderPlacement` log in a dispatch batch
/// and feed each decoded placement to the observe path.
pub fn on_logs<H: Host>(host: &H, logs: &[LogView<'_>]) -> Result<(), HostError> {
    for log in logs {
        if let Some(placement) = decode_order_placement(log.address, log.topics, log.data) {
            observe_placement(host, log.chain_id, &placement)?;
        }
    }
    Ok(())
}

// ---- decode ----

/// Decode a raw event log against `CoWSwapOnchainOrders.OrderPlacement`.
///
/// Returns `None` when:
/// - the log's contract address is neither `ETH_FLOW_PRODUCTION` nor
///   `ETH_FLOW_STAGING` (defensive - the host's `[[subscription]]`
///   filter already pins the address, but a misconfigured engine could
///   still leak through);
/// - topic0 does not match the event signature; or
/// - the ABI body fails to decode.
pub(crate) fn decode_order_placement(
    address: &[u8],
    topics: &[Vec<u8>],
    data: &[u8],
) -> Option<DecodedPlacement> {
    if address.len() != 20 {
        return None;
    }
    let contract = Address::from_slice(address);
    if contract != ETH_FLOW_PRODUCTION && contract != ETH_FLOW_STAGING {
        return None;
    }
    let topic0 = topics.first()?;
    if topic0.len() != 32 || B256::from_slice(topic0) != OrderPlacement::SIGNATURE_HASH {
        return None;
    }
    let words: Vec<B256> = topics
        .iter()
        .filter(|t| t.len() == 32)
        .map(|t| B256::from_slice(t))
        .collect();
    let decoded = OrderPlacement::decode_raw_log(words, data).ok()?;
    Some(DecodedPlacement {
        contract,
        sender: decoded.sender,
        order: Box::new(decoded.order),
        signature: decoded.signature,
        data: decoded.data,
    })
}

// ---- observe + verify (redesign) ----

/// Compute the orderbook UID for the placement and confirm the
/// orderbook's native EthFlow indexer picked it up.
fn observe_placement<H: Host>(
    host: &H,
    chain_id: u64,
    placement: &DecodedPlacement,
) -> Result<(), HostError> {
    let uid_hex = match compute_uid(chain_id, placement) {
        Some(uid) => format!("{uid}"),
        None => {
            host.log(
                LogLevel::Warn,
                &format!(
                    "ethflow uid build skipped (sender={:#x}): unsupported chain {chain_id} or unknown order marker",
                    placement.sender,
                ),
            );
            return Ok(());
        }
    };

    // Idempotency: once verified, do not re-check on log re-delivery
    // (engine restart, reorg replay, supervisor restart).
    if host.get(&format!("observed:{uid_hex}"))?.is_some() {
        return Ok(());
    }

    let path = format!("/api/v1/orders/{uid_hex}");
    match host.cow_api_request(chain_id, "GET", &path, None) {
        Ok(_) => {
            host.set(&format!("observed:{uid_hex}"), b"")?;
            host.log(
                LogLevel::Info,
                &format!(
                    "ethflow observed {uid_hex} (orderbook indexed, sender={:#x})",
                    placement.sender,
                ),
            );
        }
        Err(err) if err.code == 404 => {
            // Indexer lag is expected immediately after the block lands -
            // shepherd's WebSocket can deliver the log a few hundred
            // milliseconds before the orderbook's own indexer commits.
            // Do NOT write the marker so a later re-delivery (or a future
            // block-tick poll) can recheck. Info keeps the soak dashboard
            // quiet on normal lag.
            host.log(
                LogLevel::Info,
                &format!(
                    "ethflow not yet indexed {uid_hex} (sender={:#x}); will recheck on re-delivery",
                    placement.sender,
                ),
            );
        }
        Err(err) => {
            host.log(
                LogLevel::Warn,
                &format!(
                    "ethflow indexer check failed {uid_hex} ({}): {} (sender={:#x})",
                    err.code, err.message, placement.sender,
                ),
            );
        }
    }
    Ok(())
}

/// Compute the canonical 56-byte orderbook UID for the placement.
/// `OrderData::uid` packs `digest || owner || valid_to`; the owner
/// input is the EthFlow contract (which signs via EIP-1271), not the
/// native-token sender.
fn compute_uid(chain_id: u64, placement: &DecodedPlacement) -> Option<OrderUid> {
    let chain = Chain::try_from(chain_id).ok()?;
    let domain = chain.settlement_domain();
    let order_data = gpv2_to_order_data(&placement.order)?;
    Some(order_data.uid(&domain, placement.contract))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, hex};
    use alloy_sol_types::SolValue;
    use cowprotocol::{BuyTokenDestination, OnchainSigningScheme, OrderKind, SellTokenSource};
    use shepherd_sdk::host::{HostError as SdkHostError, HostErrorKind, LocalStoreHost as _};
    use shepherd_sdk_test::MockHost;

    const SEPOLIA: u64 = 11_155_111;

    fn sample_order() -> GPv2OrderData {
        GPv2OrderData {
            sellToken: address!("6810e776880C02933D47DB1b9fc05908e5386b96"),
            buyToken: address!("DAE5F1590db13E3B40423B5b5c5fbf175515910b"),
            receiver: address!("DeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF"),
            sellAmount: U256::from(1_000_000_u64),
            buyAmount: U256::from(999_u64),
            validTo: 0xffff_ffff,
            appData: cowprotocol::EMPTY_APP_DATA_HASH,
            feeAmount: U256::ZERO,
            kind: OrderKind::SELL,
            partiallyFillable: false,
            sellTokenBalance: SellTokenSource::ERC20,
            buyTokenBalance: BuyTokenDestination::ERC20,
        }
    }

    fn sample_event() -> OrderPlacement {
        OrderPlacement {
            sender: address!("00112233445566778899aabbccddeeff00112233"),
            order: sample_order(),
            signature: OnchainSignature {
                scheme: OnchainSigningScheme::Eip1271,
                data: hex!("c0ffeec0ffeec0ffee").to_vec().into(),
            },
            data: hex!("deadbeef").to_vec().into(),
        }
    }

    fn encode_log(event: &OrderPlacement) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut sender_topic = vec![0u8; 12];
        sender_topic.extend_from_slice(event.sender.as_slice());
        let topics = vec![OrderPlacement::SIGNATURE_HASH.to_vec(), sender_topic];
        let data = (
            event.order.clone(),
            event.signature.clone(),
            event.data.clone(),
        )
            .abi_encode_params();
        (topics, data)
    }

    fn placement_log_view<'a>(
        address_bytes: &'a [u8],
        topics: &'a [Vec<u8>],
        data: &'a [u8],
    ) -> LogView<'a> {
        LogView {
            chain_id: SEPOLIA,
            address: address_bytes,
            topics,
            data,
        }
    }

    fn computed_uid(placement: &DecodedPlacement) -> String {
        format!(
            "{}",
            compute_uid(SEPOLIA, placement).expect("sepolia + canonical markers")
        )
    }

    // ---- decode (invariants preserved) ----

    #[test]
    fn decodes_well_formed_placement() {
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let decoded = decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data)
            .expect("decode succeeds");
        assert_eq!(decoded.contract, ETH_FLOW_PRODUCTION);
        assert_eq!(decoded.sender, event.sender);
        assert_eq!(decoded.signature.scheme, OnchainSigningScheme::Eip1271);
    }

    #[test]
    fn rejects_unrelated_contract_address() {
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let stranger = address!("dead00000000000000000000000000000000dead");
        assert!(decode_order_placement(stranger.as_slice(), &topics, &data).is_none());
    }

    #[test]
    fn rejects_wrong_topic_signature() {
        let event = sample_event();
        let (_, data) = encode_log(&event);
        let bad_topic = vec![0xaa_u8; 32];
        let sender_topic = vec![0u8; 32];
        assert!(
            decode_order_placement(
                ETH_FLOW_PRODUCTION.as_slice(),
                &[bad_topic, sender_topic],
                &data,
            )
            .is_none()
        );
    }

    // ---- UID computation ----

    #[test]
    fn compute_uid_pins_owner_to_ethflow_contract_and_validto() {
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let decoded =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();

        let uid = compute_uid(SEPOLIA, &decoded).expect("sepolia + canonical markers");
        let bytes: [u8; 56] = uid.into();
        // owner suffix (bytes 32..52) = EthFlow contract address.
        assert_eq!(&bytes[32..52], ETH_FLOW_PRODUCTION.as_slice());
        // valid_to suffix (bytes 52..56) = u32 BE of the on-chain validTo.
        assert_eq!(
            u32::from_be_bytes(bytes[52..56].try_into().unwrap()),
            event.order.validTo,
        );
    }

    #[test]
    fn compute_uid_returns_none_on_unsupported_chain() {
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let decoded =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        assert!(compute_uid(9999, &decoded).is_none());
    }

    // ---- observe + verify dispatch (Host-trait integration) ----

    /// 200 from `GET /api/v1/orders/{uid}` → `observed:{uid}` written
    /// + Info log + zero submit attempts.
    #[test]
    fn placement_log_marks_observed_on_orderbook_200() {
        let host = MockHost::new();
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = computed_uid(&placement);

        // Minimal stub of the orderbook's GET response - strategy only
        // checks for 200 vs 404 vs other, the body is opaque to it.
        host.cow_api.respond_to_request_for(
            "GET",
            format!("/api/v1/orders/{uid}"),
            Ok(r#"{"status":"fulfilled"}"#.to_string()),
        );

        on_logs(&host, &[view]).unwrap();

        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("observed:{uid}")),
            "200 response must write observed:{{uid}} marker"
        );
        assert_eq!(
            host.cow_api.request_calls().len(),
            1,
            "exactly one orderbook GET per log"
        );
        assert_eq!(
            host.cow_api.call_count(),
            0,
            "observe path must never call submit_order"
        );
        assert!(
            host.logging
                .contains(&format!("ethflow observed {uid} (orderbook indexed"))
        );
    }

    /// 404 from `GET /api/v1/orders/{uid}` → no marker written + Info
    /// log + the next re-delivery rechecks (no early dedup).
    #[test]
    fn placement_log_does_not_mark_observed_on_orderbook_404() {
        let host = MockHost::new();
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = computed_uid(&placement);

        host.cow_api.respond_to_request(Err(SdkHostError {
            domain: "cow-api".into(),
            kind: HostErrorKind::Unavailable,
            code: 404,
            message: "Not Found".into(),
            data: None,
        }));

        on_logs(&host, &[view]).unwrap();

        assert!(
            !host
                .store
                .snapshot()
                .contains_key(&format!("observed:{uid}")),
            "404 must NOT write observed: so re-delivery can recheck"
        );
        let lines: Vec<_> = host
            .logging
            .lines()
            .into_iter()
            .filter(|l| l.message.contains("not yet indexed"))
            .collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].level,
            LogLevel::Info,
            "indexer lag is expected; Info keeps soak dashboards quiet"
        );
    }

    /// Non-404 error from the orderbook check → Warn log + no marker.
    #[test]
    fn placement_log_warns_on_orderbook_other_error() {
        let host = MockHost::new();
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);

        host.cow_api.respond_to_request(Err(SdkHostError {
            domain: "cow-api".into(),
            kind: HostErrorKind::Internal,
            code: 502,
            message: "bad gateway".into(),
            data: None,
        }));

        on_logs(&host, &[view]).unwrap();

        assert!(
            host.store.snapshot().is_empty(),
            "non-404 error must not write any marker"
        );
        let lines: Vec<_> = host
            .logging
            .lines()
            .into_iter()
            .filter(|l| l.message.contains("indexer check failed"))
            .collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].level, LogLevel::Warn);
    }

    /// Idempotency: a placement that already has `observed:{uid}` in
    /// local store does NOT trigger a fresh GET on re-delivery.
    #[test]
    fn previously_observed_placement_is_skipped_on_redelivery() {
        let host = MockHost::new();
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = computed_uid(&placement);

        host.store
            .set(&format!("observed:{uid}"), b"")
            .expect("seed observed marker");

        on_logs(&host, &[view]).unwrap();

        assert_eq!(
            host.cow_api.request_calls().len(),
            0,
            "observed:{{uid}} must short-circuit before the orderbook GET"
        );
        assert_eq!(
            host.cow_api.call_count(),
            0,
            "and certainly no submit_order"
        );
    }

    /// Defensive: unsupported chain id surfaces a Warn but does not
    /// panic and does not touch the orderbook.
    #[test]
    fn unsupported_chain_logs_warn_without_orderbook_call() {
        let host = MockHost::new();
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let view = LogView {
            chain_id: 9999, // not in cowprotocol::Chain
            address: ETH_FLOW_PRODUCTION.as_slice(),
            topics: &topics,
            data: &data,
        };

        on_logs(&host, &[view]).unwrap();

        assert_eq!(host.cow_api.request_calls().len(), 0);
        assert_eq!(host.cow_api.call_count(), 0);
        assert!(host.logging.contains("ethflow uid build skipped"));
    }

    /// Strategy must never call `submit_order` - the trait still
    /// exposes it for other modules (twap-monitor legitimately
    /// submits), but ethflow-watcher's observe design never does.
    /// Belt-and-suspenders regression guard.
    #[test]
    fn strategy_never_calls_submit_order() {
        let host = MockHost::new();
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        host.cow_api.respond_to_request(Ok("{}".to_string()));

        on_logs(&host, &[view]).unwrap();

        assert_eq!(
            host.cow_api.call_count(),
            0,
            "submit_order count must stay at zero - ethflow-watcher is observer-only"
        );
    }
}
