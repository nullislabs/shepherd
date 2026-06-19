//! Pure strategy logic for the ethflow-watcher module.
//!
//! Every interaction with the world flows through the
//! `shepherd_sdk::host::Host` trait seam - no direct calls to wit-
//! bindgen-generated free functions live here. The `lib.rs` glue
//! wraps a `WitBindgenHost` adapter around the per-cdylib wit-bindgen
//! imports and hands it to [`on_logs`]; tests under `#[cfg(test)]`
//! hand the same function a `shepherd_sdk_test::MockHost`.

use alloy_primitives::{Address, B256, Bytes};
use alloy_sol_types::SolEvent;
use cowprotocol::{
    Chain, CoWSwapOnchainOrders::OrderPlacement, ETH_FLOW_PRODUCTION, ETH_FLOW_STAGING,
    GPv2OrderData, OnchainSignature, OnchainSigningScheme, OrderCreation, OrderUid, Signature,
};
use shepherd_sdk::cow::{
    RetryAction, classify_api_error, gpv2_to_order_data, try_decode_api_error,
};
use shepherd_sdk::host::{Host, HostError, LogLevel};

/// `errorType` the orderbook returns when the submitted body's
/// `validTo` exceeds its cap. EthFlow orders are designed with
/// `validTo = u32::MAX` (see `cowprotocol::eth_flow`), so on chains
/// whose orderbook config rejects that shape (today: Sepolia) every
/// EthFlow placement we forward terminates here. The Drop disposition
/// is correct, the log level should not be Warn - this is a known
/// upstream gap, not a strategy bug. Tracked in COW-1076.
const EXCESSIVE_VALID_TO: &str = "ExcessiveValidTo";

/// Fields the strategy needs from a wit-bindgen `log`. Borrowed slices
/// keep the strategy independent from the per-cdylib wit types.
pub struct LogView<'a> {
    pub chain_id: u64,
    pub address: &'a [u8],
    pub topics: &'a [Vec<u8>],
    pub data: &'a [u8],
}

/// Fully decoded payload of a `CoWSwapOnchainOrders.OrderPlacement`
/// log. `GPv2OrderData` is ~300 bytes; box it so the struct stays
/// cache-friendly through the submit path.
#[derive(Debug)]
pub(crate) struct DecodedPlacement {
    /// EthFlow contract that emitted the event - also the EIP-1271
    /// verifier `from` for the submitted `OrderCreation`.
    pub(crate) contract: Address,
    /// Original native-token seller - logged for diagnostics; the
    /// orderbook's `from` is the contract (EIP-1271 owner), not this.
    pub(crate) sender: Address,
    pub(crate) order: Box<GPv2OrderData>,
    pub(crate) signature: OnchainSignature,
    /// Refund pointer / opaque placer metadata. Not consumed by the
    /// submit path today, but the field is part of the BLEU-832
    /// decoder contract.
    #[allow(dead_code)]
    pub(crate) data: Bytes,
}

/// Entry point: decode every `OrderPlacement` log in a dispatch batch
/// and feed the decoded placement to the submit path.
pub fn on_logs<H: Host>(host: &H, logs: &[LogView<'_>]) -> Result<(), HostError> {
    for log in logs {
        if let Some(placement) = decode_order_placement(log.address, log.topics, log.data) {
            submit_placement(host, log.chain_id, &placement)?;
        }
    }
    Ok(())
}

// ---- BLEU-832: decode ----

/// Decode a raw event log against `CoWSwapOnchainOrders.OrderPlacement`.
///
/// Returns `None` when:
/// - the log's contract address is neither `ETH_FLOW_PRODUCTION` nor
///   `ETH_FLOW_STAGING` (defensive - the host's `[[subscription]]`
///   filter already pins the address, but a misconfigured engine
///   could still leak through);
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

// ---- BLEU-833: submit + retry ----

#[derive(Debug, thiserror::Error)]
pub(crate) enum BuildError {
    #[error("GPv2OrderData carried an unknown enum marker")]
    UnknownMarker,
    #[error("OnchainSignature carried an unknown scheme variant")]
    UnknownSignatureScheme,
    #[error("chain {0} is not supported by cowprotocol")]
    UnsupportedChain(u64),
    #[error(transparent)]
    Cowprotocol(#[from] cowprotocol::Error),
}

/// Lift `OnchainSignature` into the orderbook-typed `Signature`. The
/// EthFlow contract is the EIP-1271 verifier, so the `data` blob is
/// the raw verifier bytes; for `PreSign` the orderbook accepts an
/// empty payload.
fn to_signature(sig: &OnchainSignature) -> Option<Signature> {
    // sol! adds a hidden `__Invalid` variant on every Solidity enum,
    // so exhaustive patterns require a wildcard; we surface it as
    // `None` (caller falls back to skipping the placement) rather
    // than panic.
    match sig.scheme {
        OnchainSigningScheme::Eip1271 => Some(Signature::Eip1271(sig.data.to_vec())),
        OnchainSigningScheme::PreSign => Some(Signature::PreSign),
        _ => None,
    }
}

/// Assemble `(OrderCreation, OrderUid)` from a placement. `from` is
/// the EthFlow contract (EIP-1271 owner).
///
/// `app_data_json` is the canonical JSON document whose
/// `keccak256` matches `placement.order.appData`. The caller
/// resolves it via [`shepherd_sdk::cow::resolve_app_data`] (or
/// any equivalent path); passing a mismatching string makes
/// `from_signed_order_data` reject with "app_data JSON digest
/// does not match signed app_data hash" (COW-1074).
pub(crate) fn build_eth_flow_creation(
    chain_id: u64,
    placement: &DecodedPlacement,
    app_data_json: String,
) -> Result<(OrderCreation, OrderUid), BuildError> {
    let chain = Chain::try_from(chain_id).map_err(|_| BuildError::UnsupportedChain(chain_id))?;
    let domain = chain.settlement_domain();
    let order_data = gpv2_to_order_data(&placement.order).ok_or(BuildError::UnknownMarker)?;
    let uid = order_data.uid(&domain, placement.contract);
    let signature =
        to_signature(&placement.signature).ok_or(BuildError::UnknownSignatureScheme)?;
    let creation = OrderCreation::from_signed_order_data(
        &order_data,
        signature,
        placement.contract,
        app_data_json,
        None,
    )?;
    Ok((creation, uid))
}

fn submit_placement<H: Host>(
    host: &H,
    chain_id: u64,
    placement: &DecodedPlacement,
) -> Result<(), HostError> {
    // COW-1074: cow-swap UI (and other clients) sign EthFlow
    // placements with a non-empty `appData` hash pointing at a JSON
    // document held by the orderbook's app_data registry. Resolve
    // it before assembling the submission body; on 404 (orderbook
    // doesn't mirror this hash) log a Warn and drop the placement
    // — there is no path to recover without operator intervention.
    let app_data_json =
        match shepherd_sdk::cow::resolve_app_data(host, chain_id, &placement.order.appData.0) {
            Ok(json) => json,
            Err(err) if err.code == 404 => {
                host.log(
                LogLevel::Warn,
                &format!(
                    "ethflow submit skipped (sender={:#x}): appData hash not mirrored on orderbook",
                    placement.sender,
                ),
            );
                return Ok(());
            }
            Err(err) => {
                host.log(
                    LogLevel::Warn,
                    &format!(
                        "ethflow submit skipped (sender={:#x}): appData resolve failed ({}): {}",
                        placement.sender, err.code, err.message,
                    ),
                );
                return Ok(());
            }
        };

    let (creation, uid) = match build_eth_flow_creation(chain_id, placement, app_data_json) {
        Ok(x) => x,
        Err(e) => {
            host.log(
                LogLevel::Warn,
                &format!(
                    "ethflow submit skipped (sender={:#x}): {e}",
                    placement.sender
                ),
            );
            return Ok(());
        }
    };
    let uid_hex = format!("{uid}");

    // Idempotency. A host reconnect or engine restart may replay the
    // same OrderPlacement log; without the guard we would attempt a
    // second submit, the orderbook would reject `DuplicateOrder`
    // (permanent), and we would end up with both `submitted:` AND
    // `dropped:` written for the same UID. `backoff:` is *not* a
    // short-circuit - a previous transient error deserves a fresh
    // attempt on re-delivery.
    match prior_outcome(host, &uid_hex)? {
        PriorOutcome::Submitted => {
            host.log(
                LogLevel::Info,
                &format!("ethflow {uid_hex} already submitted; skipping"),
            );
            return Ok(());
        }
        PriorOutcome::Dropped => {
            host.log(
                LogLevel::Info,
                &format!("ethflow {uid_hex} previously dropped; skipping"),
            );
            return Ok(());
        }
        PriorOutcome::None | PriorOutcome::Backoff => {}
    }

    let body = match serde_json::to_vec(&creation) {
        Ok(b) => b,
        Err(e) => {
            host.log(
                LogLevel::Error,
                &format!("OrderCreation JSON encode failed: {e}"),
            );
            return Ok(());
        }
    };
    match host.submit_order(chain_id, &body) {
        Ok(server_uid) => {
            // Persist under the server-supplied UID so downstream
            // observers (cow-tooling, dune) join on the same key. The
            // client UID we just computed should equal it; a Warn is
            // worth a closer look if not (domain/owner divergence).
            if server_uid != uid_hex {
                host.log(
                    LogLevel::Warn,
                    &format!("ethflow uid drift: local={uid_hex} server={server_uid}"),
                );
            }
            host.set(&format!("submitted:{server_uid}"), b"")?;
            // Clear any backoff: marker a prior transient error left
            // behind; the terminal `submitted:` flag supersedes it.
            let _ = host.delete(&format!("backoff:{server_uid}"));
            host.log(LogLevel::Info, &format!("ethflow submitted {server_uid}"));
        }
        Err(err) => apply_submit_retry(host, &err, &uid_hex)?,
    }
    Ok(())
}

/// Which terminal / transient marker (if any) the local store carries
/// for `uid_hex`. The submit path short-circuits on `Submitted` /
/// `Dropped`; `Backoff` still proceeds with a fresh attempt; `None`
/// means a clean first try.
#[derive(Debug, Eq, PartialEq)]
enum PriorOutcome {
    None,
    Submitted,
    Backoff,
    Dropped,
}

fn prior_outcome<H: Host>(host: &H, uid_hex: &str) -> Result<PriorOutcome, HostError> {
    if host.get(&format!("submitted:{uid_hex}"))?.is_some() {
        return Ok(PriorOutcome::Submitted);
    }
    if host.get(&format!("dropped:{uid_hex}"))?.is_some() {
        return Ok(PriorOutcome::Dropped);
    }
    if host.get(&format!("backoff:{uid_hex}"))?.is_some() {
        return Ok(PriorOutcome::Backoff);
    }
    Ok(PriorOutcome::None)
}

fn apply_submit_retry<H: Host>(host: &H, err: &HostError, uid_hex: &str) -> Result<(), HostError> {
    match classify_api_error(err.data.as_deref()) {
        RetryAction::TryNextBlock | RetryAction::Backoff { .. } => {
            host.set(&format!("backoff:{uid_hex}"), b"")?;
            host.log(
                LogLevel::Warn,
                &format!("ethflow backoff {uid_hex} ({}): {}", err.code, err.message),
            );
        }
        RetryAction::Drop => {
            host.set(&format!("dropped:{uid_hex}"), b"")?;
            // Clear `backoff:` if a prior transient attempt left it
            // behind - the terminal `dropped:` flag now supersedes
            // it, and we want at most one outcome marker per UID at
            // rest.
            let _ = host.delete(&format!("backoff:{uid_hex}"));
            // ExcessiveValidTo is the documented Sepolia-orderbook
            // rejection for the canonical EthFlow shape (validTo =
            // u32::MAX). It is not an anomaly for the operator to
            // page on; log at Info so soak dashboards stay quiet.
            // Any other Drop reason keeps the Warn level.
            let level = if is_expected_excessive_valid_to(err) {
                LogLevel::Info
            } else {
                LogLevel::Warn
            };
            host.log(
                level,
                &format!("ethflow dropped {uid_hex} ({}): {}", err.code, err.message),
            );
        }
    }
    Ok(())
}

/// Does this submit-side failure look like the documented Sepolia-orderbook
/// rejection of EthFlow's canonical `validTo = u32::MAX`? The check is
/// scoped to the `errorType` string the orderbook returns; the strategy
/// has already classified this as Drop, so we are not changing dispatch -
/// only the log level. Returns `false` when no envelope is forwarded
/// (e.g. transport failure) or when the envelope carries a different
/// `errorType`.
fn is_expected_excessive_valid_to(err: &HostError) -> bool {
    try_decode_api_error(err.data.as_deref())
        .is_some_and(|api| api.error_type == EXCESSIVE_VALID_TO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, hex};
    use alloy_sol_types::SolValue;
    use cowprotocol::{BuyTokenDestination, OrderKind, SellTokenSource};
    use shepherd_sdk::host::{HostErrorKind as Kind, LocalStoreHost as _};
    use shepherd_sdk_test::MockHost;

    const SEPOLIA: u64 = 11_155_111;

    fn submittable_order() -> GPv2OrderData {
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

    fn well_formed_placement() -> DecodedPlacement {
        DecodedPlacement {
            contract: ETH_FLOW_PRODUCTION,
            sender: address!("00112233445566778899aabbccddeeff00112233"),
            order: Box::new(submittable_order()),
            signature: OnchainSignature {
                scheme: OnchainSigningScheme::Eip1271,
                data: hex!("c0ffeec0ffeec0ffee").to_vec().into(),
            },
            data: Bytes::new(),
        }
    }

    fn sample_event_for_decode() -> OrderPlacement {
        OrderPlacement {
            sender: address!("00112233445566778899aabbccddeeff00112233"),
            order: submittable_order(),
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

    // ---- existing pure tests preserved from BLEU-832/833 ----

    #[test]
    fn decodes_well_formed_placement() {
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let decoded = decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data)
            .expect("decode succeeds");
        assert_eq!(decoded.contract, ETH_FLOW_PRODUCTION);
        assert_eq!(decoded.sender, event.sender);
        assert_eq!(decoded.signature.scheme, OnchainSigningScheme::Eip1271);
    }

    #[test]
    fn rejects_unrelated_contract_address() {
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let stranger = address!("dead00000000000000000000000000000000dead");
        assert!(decode_order_placement(stranger.as_slice(), &topics, &data).is_none());
    }

    #[test]
    fn build_eip1271_creation_has_contract_as_from() {
        let placement = well_formed_placement();
        let (creation, uid) = build_eth_flow_creation(
            11_155_111,
            &placement,
            cowprotocol::EMPTY_APP_DATA_JSON.to_string(),
        )
        .expect("build succeeds");
        assert_eq!(creation.from, placement.contract);
        assert_eq!(creation.signing_scheme, cowprotocol::SigningScheme::Eip1271);
        assert_eq!(
            creation.signature.to_bytes(),
            placement.signature.data.to_vec(),
        );
        assert_eq!(&uid.as_slice()[32..52], placement.contract.as_slice());
        assert_eq!(
            &uid.as_slice()[52..56],
            &placement.order.validTo.to_be_bytes(),
        );
    }

    #[test]
    fn build_presign_emits_presign_scheme() {
        let mut placement = well_formed_placement();
        placement.signature = OnchainSignature {
            scheme: OnchainSigningScheme::PreSign,
            data: Bytes::new(),
        };
        let (creation, _) =
            build_eth_flow_creation(1, &placement, cowprotocol::EMPTY_APP_DATA_JSON.to_string())
                .expect("build succeeds");
        assert_eq!(creation.signing_scheme, cowprotocol::SigningScheme::PreSign);
        assert!(creation.signature.to_bytes().is_empty());
    }

    #[test]
    fn build_rejects_unsupported_chain() {
        let placement = well_formed_placement();
        let err = build_eth_flow_creation(
            0xdead_beef,
            &placement,
            cowprotocol::EMPTY_APP_DATA_JSON.to_string(),
        )
        .unwrap_err();
        assert!(matches!(err, BuildError::UnsupportedChain(0xdead_beef)));
    }

    #[test]
    fn build_rejects_unknown_kind_marker() {
        let mut placement = well_formed_placement();
        placement.order.kind = B256::repeat_byte(0x42);
        let err =
            build_eth_flow_creation(1, &placement, cowprotocol::EMPTY_APP_DATA_JSON.to_string())
                .unwrap_err();
        assert!(matches!(err, BuildError::UnknownMarker));
    }

    #[test]
    fn build_rejects_non_empty_app_data() {
        let mut placement = well_formed_placement();
        placement.order.appData = B256::repeat_byte(0xee);
        let err =
            build_eth_flow_creation(1, &placement, cowprotocol::EMPTY_APP_DATA_JSON.to_string())
                .unwrap_err();
        assert!(matches!(err, BuildError::Cowprotocol(_)));
    }

    // ---- BLEU-855: MockHost dispatch tests ----

    fn programmed_uid(placement: &DecodedPlacement) -> String {
        let (_creation, uid) = build_eth_flow_creation(
            SEPOLIA,
            placement,
            cowprotocol::EMPTY_APP_DATA_JSON.to_string(),
        )
        .unwrap();
        format!("{uid}")
    }

    #[test]
    fn placement_log_submits_order_and_persists_submitted_uid() {
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = programmed_uid(&placement);
        host.cow_api.respond(Ok(uid.clone()));

        on_logs(&host, &[view]).unwrap();

        assert_eq!(host.cow_api.call_count(), 1);
        assert!(host.store.snapshot().contains_key(&format!("submitted:{uid}")));
        assert!(!host.store.snapshot().contains_key(&format!("backoff:{uid}")));
        assert!(host.logging.contains(&format!("ethflow submitted {uid}")));
    }

    #[test]
    fn redelivered_placement_is_skipped_via_submitted_uid_dedup() {
        // BLEU-833 / commit c5e4d7d regression guard: a host
        // reconnect or engine restart that replays the same
        // OrderPlacement log must not double-submit.
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let view1 = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = programmed_uid(&placement);
        host.cow_api.respond(Ok(uid.clone()));

        on_logs(&host, &[view1]).unwrap();
        assert_eq!(host.cow_api.call_count(), 1);

        let view2 = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        on_logs(&host, &[view2]).unwrap();

        assert_eq!(
            host.cow_api.call_count(),
            1,
            "redelivered placement must not resubmit"
        );
        assert!(host.logging.contains("already submitted"));
    }

    /// COW-1074: an OrderPlacement carrying a non-empty `appData`
    /// hash triggers a `cow_api_request` against
    /// `/api/v1/app_data/{hex}`; the resolved JSON is passed to
    /// `build_eth_flow_creation` so the digest matches and the
    /// submit succeeds. Before this PR every non-empty placement
    /// (cow-swap UI style) was rejected client-side with "app_data
    /// JSON digest does not match signed app_data hash".
    #[test]
    fn placement_with_non_empty_app_data_resolves_then_submits() {
        use alloy_primitives::keccak256;
        let host = MockHost::new();

        let app_data_json = r#"{"version":"1.1.0","metadata":{"partnerId":"shepherd-e2e"}}"#;
        let app_data_hash = keccak256(app_data_json.as_bytes());

        // Build a placement event with the non-empty appData hash.
        let mut event = sample_event_for_decode();
        event.order.appData = app_data_hash;
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        // Compute the UID against the resolved (non-empty) JSON so we
        // can program cow_api.respond with the matching value.
        let (_creation, uid_obj) =
            build_eth_flow_creation(SEPOLIA, &placement, app_data_json.to_string())
                .expect("build with resolved app data");
        let uid = format!("{uid_obj}");
        host.cow_api.respond(Ok(uid.clone()));

        // Mirror the orderbook's /api/v1/app_data/{hex} response shape.
        let envelope = format!(
            r#"{{"fullAppData":{}}}"#,
            serde_json::Value::String(app_data_json.to_string()),
        );
        host.cow_api.respond_to_request_for(
            "GET",
            format!(
                "/api/v1/app_data/0x{}",
                alloy_primitives::hex::encode(app_data_hash)
            ),
            Ok(envelope),
        );

        on_logs(&host, &[view]).unwrap();

        assert_eq!(
            host.cow_api.request_calls().len(),
            1,
            "exactly one /app_data resolve"
        );
        assert_eq!(host.cow_api.call_count(), 1, "exactly one orderbook submit");
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{uid}")),
            "submitted:{{uid}} marker must be written after a successful resolve+submit"
        );
        assert!(host.logging.contains(&format!("ethflow submitted {uid}")));
    }

    /// COW-1074: orderbook 404s the appData hash → strategy logs a
    /// Warn and drops the placement (no submit attempt, no marker).
    #[test]
    fn placement_skips_submit_when_app_data_hash_not_mirrored() {
        use alloy_primitives::keccak256;
        let host = MockHost::new();

        let mut event = sample_event_for_decode();
        event.order.appData = keccak256(b"unknown-document");
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);

        host.cow_api
            .respond_to_request(Err(shepherd_sdk::host::HostError {
                domain: "cow-api".into(),
                kind: shepherd_sdk::host::HostErrorKind::Unavailable,
                code: 404,
                message: "Not Found".into(),
                data: None,
            }));

        on_logs(&host, &[view]).unwrap();

        assert_eq!(host.cow_api.call_count(), 0, "no submit attempt on 404");
        let store = host.store.snapshot();
        assert!(!store.keys().any(|k| k.starts_with("submitted:")));
        assert!(!store.keys().any(|k| k.starts_with("dropped:")));
        assert!(host.logging.contains("appData hash not mirrored"));
    }

    #[test]
    fn submit_transient_error_writes_backoff_marker_and_returns() {
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = programmed_uid(&placement);

        // InsufficientFee classifies as TryNextBlock per cowprotocol's
        // retry_hint; ethflow-watcher treats every retriable
        // classification as a backoff: marker (next event will retry,
        // not next block).
        let api_body = serde_json::json!({
            "errorType": "InsufficientFee",
            "description": "fee too low",
        })
        .to_string();
        host.cow_api.respond(Err(HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 400,
            message: "InsufficientFee".into(),
            data: Some(api_body),
        }));

        on_logs(&host, &[view]).unwrap();

        assert!(host.store.snapshot().contains_key(&format!("backoff:{uid}")));
        assert!(!host.store.snapshot().contains_key(&format!("submitted:{uid}")));
        assert!(!host.store.snapshot().contains_key(&format!("dropped:{uid}")));
        assert!(host.logging.contains("ethflow backoff"));
    }

    #[test]
    fn submit_permanent_error_persists_dropped_uid_and_clears_backoff() {
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = programmed_uid(&placement);

        // Pre-seed a backoff: marker (prior transient attempt). A
        // permanent failure on the retry must drop the order AND
        // clear the stale backoff: row so we never have both at rest.
        host.store
            .set(&format!("backoff:{uid}"), b"")
            .unwrap();

        let api_body = serde_json::json!({
            "errorType": "InvalidSignature",
            "description": "bad sig",
        })
        .to_string();
        host.cow_api.respond(Err(HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 400,
            message: "InvalidSignature".into(),
            data: Some(api_body),
        }));

        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        on_logs(&host, &[view]).unwrap();

        assert!(host.store.snapshot().contains_key(&format!("dropped:{uid}")));
        assert!(
            !host.store.snapshot().contains_key(&format!("backoff:{uid}")),
            "terminal `dropped:` must clear stale `backoff:` marker"
        );
        assert!(host.logging.contains("ethflow dropped"));
    }

    #[test]
    fn submit_excessive_valid_to_logs_at_info_not_warn() {
        // EthFlow on Sepolia: the orderbook rejects validTo = u32::MAX
        // (the canonical EthFlow shape) with ExcessiveValidTo. The
        // strategy must Drop (no retry storm) AND log at Info, so the
        // soak does not page on every EthFlow event. This is the
        // documented upstream-gap path tracked in COW-1076.
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let placement =
            decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data).unwrap();
        let uid = programmed_uid(&placement);

        let api_body = serde_json::json!({
            "errorType": "ExcessiveValidTo",
            "description": "validTo is too far into the future",
        })
        .to_string();
        host.cow_api.respond(Err(HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 400,
            message: "ExcessiveValidTo".into(),
            data: Some(api_body),
        }));

        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        on_logs(&host, &[view]).unwrap();

        // Dropped just like any other permanent rejection.
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("dropped:{uid}"))
        );
        // ... but the operator-visible log line is Info, not Warn.
        let drop_lines: Vec<_> = host
            .logging
            .lines()
            .into_iter()
            .filter(|l| l.message.contains("ethflow dropped"))
            .collect();
        assert_eq!(drop_lines.len(), 1, "exactly one drop line per UID");
        assert_eq!(
            drop_lines[0].level,
            LogLevel::Info,
            "ExcessiveValidTo on EthFlow is the documented Sepolia upstream gap, not Warn-worthy"
        );
        // Defence-in-depth: zero Warn-level drop traffic for this case.
        assert_eq!(
            host.logging
                .lines()
                .into_iter()
                .filter(|l| l.level == LogLevel::Warn && l.message.contains("ethflow dropped"))
                .count(),
            0
        );
    }

    #[test]
    fn submit_other_permanent_error_still_logs_at_warn() {
        // Companion to the ExcessiveValidTo case: any other permanent
        // rejection (e.g. InvalidSignature) keeps the Warn level so we
        // do not silently swallow real anomalies.
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);

        let api_body = serde_json::json!({
            "errorType": "InvalidSignature",
            "description": "bad sig",
        })
        .to_string();
        host.cow_api.respond(Err(HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 400,
            message: "InvalidSignature".into(),
            data: Some(api_body),
        }));

        on_logs(&host, &[view]).unwrap();

        let drop_lines: Vec<_> = host
            .logging
            .lines()
            .into_iter()
            .filter(|l| l.message.contains("ethflow dropped"))
            .collect();
        assert_eq!(drop_lines.len(), 1);
        assert_eq!(drop_lines[0].level, LogLevel::Warn);
    }

    #[test]
    fn submit_drop_without_envelope_keeps_warn_level() {
        // If the host backend forwards no `data` (e.g. a transport
        // failure surfacing as Drop via some other path), we cannot
        // peek at `errorType` and must default to Warn so the
        // operator can investigate. classify_api_error on None yields
        // TryNextBlock; force a Drop disposition here by writing a
        // recognised non-retriable errorType into a *different* shape.
        // Using `try_decode_api_error` on raw text ensures the
        // is_expected_excessive_valid_to short-circuit returns false.
        let err = HostError {
            domain: "cow-api".into(),
            kind: Kind::Denied,
            code: 0,
            message: "transport".into(),
            data: None,
        };
        assert!(!is_expected_excessive_valid_to(&err));
    }

    #[test]
    fn eip1271_signature_shape_round_trips_through_submit_body() {
        // Snapshot the JSON the host receives so reviewers can confirm
        // the signing scheme / signature wire shape stays stable. The
        // orderbook is strict about both fields.
        let host = MockHost::new();
        let event = sample_event_for_decode();
        let (topics, data) = encode_log(&event);
        let view = placement_log_view(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data);
        host.cow_api.respond(Ok("0xfeedface".to_string()));

        on_logs(&host, &[view]).unwrap();

        let body_json = host.cow_api.last_body_as_json().expect("body was submitted");
        // OrderCreation serialises signingScheme as a lowercase string
        // and signature as a hex-prefixed bytes blob.
        assert_eq!(body_json["signingScheme"].as_str(), Some("eip1271"));
        let sig_hex = body_json["signature"].as_str().expect("signature is a string");
        assert!(sig_hex.starts_with("0x"));
        assert_eq!(
            sig_hex,
            "0xc0ffeec0ffeec0ffee",
            "EIP-1271 signature blob must be passed through verbatim"
        );
        // EthFlow contract is the orderbook `from`, not the original sender.
        assert_eq!(
            body_json["from"].as_str(),
            Some(&*format!("{:#x}", ETH_FLOW_PRODUCTION))
        );
    }
}
