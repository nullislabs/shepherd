// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use alloy_primitives::{Address, B256, Bytes};
use alloy_sol_types::SolEvent;
use cowprotocol::{
    ApiError, BuyTokenDestination, Chain, CoWSwapOnchainOrders::OrderPlacement,
    EMPTY_APP_DATA_JSON, ETH_FLOW_PRODUCTION, ETH_FLOW_STAGING, GPv2OrderData, OnchainSignature,
    OnchainSigningScheme, OrderCreation, OrderData, OrderKind, OrderUid, SellTokenSource,
    Signature,
};
use nexum::host::{local_store, logging, types};
use shepherd::cow::cow_api;

/// Fully decoded payload of a `CoWSwapOnchainOrders.OrderPlacement` log.
/// `GPv2OrderData` is ~300 bytes; box it so the struct stays cache-
/// friendly through the submit path.
#[derive(Debug)]
struct DecodedPlacement {
    /// EthFlow contract that emitted the event — also the EIP-1271
    /// verifier `from` for the submitted `OrderCreation`.
    contract: Address,
    /// Original native-token seller — logged for diagnostics; the
    /// orderbook's `from` is the contract (EIP-1271 owner), not this.
    sender: Address,
    order: Box<GPv2OrderData>,
    signature: OnchainSignature,
    /// Refund pointer / opaque placer metadata. Not consumed by the
    /// submit path today, but the field is part of the BLEU-832
    /// decoder contract.
    #[allow(dead_code)]
    data: Bytes,
}

/// What the lifecycle layer should do after a failed submission.
/// Mirrors the BLEU-829 dispatch contract on the TWAP module; the
/// `Backoff` arm has no producer until a server-supplied hint exists.
#[derive(Debug, Eq, PartialEq)]
enum RetryAction {
    TryNextBlock,
    #[allow(dead_code)]
    Backoff {
        seconds: u64,
    },
    Drop,
}

struct EthFlowWatcher;

impl Guest for EthFlowWatcher {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "ethflow-watcher init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        if let types::Event::Logs(logs) = event {
            for log in &logs {
                if let Some(placement) =
                    decode_order_placement(&log.address, &log.topics, &log.data)
                {
                    submit_placement(log.chain_id, &placement)?;
                }
            }
        }
        // Block / Tick / Message are not used by this module.
        Ok(())
    }
}

// ---- BLEU-832: decode ----

/// Decode a raw event log against `CoWSwapOnchainOrders.OrderPlacement`.
///
/// Returns `None` when:
/// - the log's contract address is neither `ETH_FLOW_PRODUCTION` nor
///   `ETH_FLOW_STAGING` (defensive — the host's `[[subscription]]`
///   filter already pins the address, but a misconfigured engine could
///   still leak through);
/// - topic0 does not match the event signature; or
/// - the ABI body fails to decode.
fn decode_order_placement(
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

#[derive(Debug)]
enum BuildError {
    UnknownMarker,
    UnknownSignatureScheme,
    UnsupportedChain(u64),
    Cowprotocol(cowprotocol::Error),
}

impl core::fmt::Display for BuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownMarker => f.write_str("GPv2OrderData carried an unknown enum marker"),
            Self::UnknownSignatureScheme => {
                f.write_str("OnchainSignature carried an unknown scheme variant")
            }
            Self::UnsupportedChain(id) => write!(f, "chain {id} is not supported by cowprotocol"),
            Self::Cowprotocol(e) => write!(f, "{e}"),
        }
    }
}

fn gpv2_to_order_data(gpv2: &GPv2OrderData) -> Option<OrderData> {
    Some(OrderData {
        sell_token: gpv2.sellToken,
        buy_token: gpv2.buyToken,
        receiver: (gpv2.receiver != Address::ZERO).then_some(gpv2.receiver),
        sell_amount: gpv2.sellAmount,
        buy_amount: gpv2.buyAmount,
        valid_to: gpv2.validTo,
        app_data: gpv2.appData,
        fee_amount: gpv2.feeAmount,
        kind: OrderKind::from_contract_bytes(gpv2.kind)?,
        partially_fillable: gpv2.partiallyFillable,
        sell_token_balance: SellTokenSource::from_contract_bytes(gpv2.sellTokenBalance)?,
        buy_token_balance: BuyTokenDestination::from_contract_bytes(gpv2.buyTokenBalance)?,
    })
}

/// Lift `OnchainSignature` into the orderbook-typed `Signature`. The
/// EthFlow contract is the EIP-1271 verifier, so the `data` blob is
/// the raw verifier bytes; for `PreSign` the orderbook accepts an
/// empty payload.
fn to_signature(sig: &OnchainSignature) -> Option<Signature> {
    // sol! adds a hidden `__Invalid` variant on every Solidity enum, so
    // exhaustive patterns require a wildcard; we surface it as `None`
    // (caller falls back to skipping the placement) rather than panic.
    match sig.scheme {
        OnchainSigningScheme::Eip1271 => Some(Signature::Eip1271(sig.data.to_vec())),
        OnchainSigningScheme::PreSign => Some(Signature::PreSign),
        _ => None,
    }
}

/// Assemble `(OrderCreation, OrderUid)` from a placement. `from` is the
/// EthFlow contract (EIP-1271 owner). `app_data` is fixed to
/// `EMPTY_APP_DATA_JSON` — placements pinning a real IPFS document get
/// rejected by `from_signed_order_data` (digest mismatch) and skipped,
/// same scope limitation as the TWAP module.
fn build_eth_flow_creation(
    chain_id: u64,
    placement: &DecodedPlacement,
) -> Result<(OrderCreation, OrderUid), BuildError> {
    let chain = Chain::try_from(chain_id).map_err(|_| BuildError::UnsupportedChain(chain_id))?;
    let domain = chain.settlement_domain();
    let order_data = gpv2_to_order_data(&placement.order).ok_or(BuildError::UnknownMarker)?;
    let uid = order_data.uid(&domain, placement.contract);
    let signature = to_signature(&placement.signature).ok_or(BuildError::UnknownSignatureScheme)?;
    let creation = OrderCreation::from_signed_order_data(
        &order_data,
        signature,
        placement.contract,
        EMPTY_APP_DATA_JSON.to_string(),
        None,
    )
    .map_err(BuildError::Cowprotocol)?;
    Ok((creation, uid))
}

fn submit_placement(chain_id: u64, placement: &DecodedPlacement) -> Result<(), HostError> {
    let (creation, uid) = match build_eth_flow_creation(chain_id, placement) {
        Ok(x) => x,
        Err(e) => {
            logging::log(
                logging::Level::Warn,
                &format!(
                    "ethflow submit skipped (sender={:#x}): {e}",
                    placement.sender
                ),
            );
            return Ok(());
        }
    };
    let uid_hex = format!("{uid}");

    // Idempotency. A host reconnect or engine restart may replay the same
    // OrderPlacement log; without the guard we would attempt a second
    // submit, the orderbook would reject `DuplicateOrder` (permanent), and
    // we would end up with both `submitted:` AND `dropped:` written for
    // the same UID. `backoff:` is *not* a short-circuit — a previous
    // transient error deserves a fresh attempt on re-delivery.
    match prior_outcome(&uid_hex)? {
        PriorOutcome::Submitted => {
            logging::log(
                logging::Level::Info,
                &format!("ethflow {uid_hex} already submitted; skipping"),
            );
            return Ok(());
        }
        PriorOutcome::Dropped => {
            logging::log(
                logging::Level::Info,
                &format!("ethflow {uid_hex} previously dropped; skipping"),
            );
            return Ok(());
        }
        PriorOutcome::None | PriorOutcome::Backoff => {}
    }

    let body = match serde_json::to_vec(&creation) {
        Ok(b) => b,
        Err(e) => {
            logging::log(
                logging::Level::Error,
                &format!("OrderCreation JSON encode failed: {e}"),
            );
            return Ok(());
        }
    };
    match cow_api::submit_order(chain_id, &body) {
        Ok(server_uid) => {
            // Persist under the server-supplied UID so downstream
            // observers (cow-tooling, dune) join on the same key. The
            // client UID we just computed should equal it; a Warn is
            // worth a closer look if not (domain/owner divergence).
            if server_uid != uid_hex {
                logging::log(
                    logging::Level::Warn,
                    &format!("ethflow uid drift: local={uid_hex} server={server_uid}"),
                );
            }
            local_store::set(&format!("submitted:{server_uid}"), b"")?;
            // Clear any backoff: marker a prior transient error left
            // behind; the terminal `submitted:` flag now supersedes it.
            let _ = local_store::delete(&format!("backoff:{server_uid}"));
            logging::log(
                logging::Level::Info,
                &format!("ethflow submitted {server_uid}"),
            );
        }
        Err(err) => apply_submit_retry(&err, &uid_hex)?,
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

fn prior_outcome(uid_hex: &str) -> Result<PriorOutcome, HostError> {
    // Terminal markers take precedence over `backoff:`. `submitted:` is
    // checked first because a successful prior attempt is the most
    // common reason a log gets re-delivered.
    if local_store::get(&format!("submitted:{uid_hex}"))?.is_some() {
        return Ok(PriorOutcome::Submitted);
    }
    if local_store::get(&format!("dropped:{uid_hex}"))?.is_some() {
        return Ok(PriorOutcome::Dropped);
    }
    if local_store::get(&format!("backoff:{uid_hex}"))?.is_some() {
        return Ok(PriorOutcome::Backoff);
    }
    Ok(PriorOutcome::None)
}

fn try_decode_api_error(err: &HostError) -> Option<ApiError> {
    let data = err.data.as_deref()?;
    serde_json::from_str::<ApiError>(data).ok()
}

fn classify_submit_error(err: &HostError) -> RetryAction {
    match try_decode_api_error(err) {
        Some(api) if api.retry_hint() => RetryAction::TryNextBlock,
        Some(_) => RetryAction::Drop,
        // Safe default — a flaky orderbook should not be treated as a
        // permanent rejection.
        None => RetryAction::TryNextBlock,
    }
}

fn apply_submit_retry(err: &HostError, uid_hex: &str) -> Result<(), HostError> {
    match classify_submit_error(err) {
        RetryAction::TryNextBlock | RetryAction::Backoff { .. } => {
            local_store::set(&format!("backoff:{uid_hex}"), b"")?;
            logging::log(
                logging::Level::Warn,
                &format!("ethflow backoff {uid_hex} ({}): {}", err.code, err.message),
            );
        }
        RetryAction::Drop => {
            local_store::set(&format!("dropped:{uid_hex}"), b"")?;
            // Clear `backoff:` if a prior transient attempt left it
            // behind — the terminal `dropped:` flag now supersedes it,
            // and we want at most one "outcome" marker per UID at rest.
            let _ = local_store::delete(&format!("backoff:{uid_hex}"));
            logging::log(
                logging::Level::Warn,
                &format!("ethflow dropped {uid_hex} ({}): {}", err.code, err.message),
            );
        }
    }
    Ok(())
}

export!(EthFlowWatcher);

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, hex};
    use alloy_sol_types::SolValue;

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

    // ---- BLEU-832 regressions ----

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

    // ---- BLEU-833: order construction ----

    #[test]
    fn build_eip1271_creation_has_contract_as_from() {
        let placement = well_formed_placement();
        let (creation, uid) =
            build_eth_flow_creation(11_155_111, &placement).expect("build succeeds");
        assert_eq!(creation.from, placement.contract);
        assert_eq!(creation.signing_scheme, cowprotocol::SigningScheme::Eip1271);
        assert_eq!(
            creation.signature.to_bytes(),
            placement.signature.data.to_vec(),
        );
        // UID layout = digest || owner || valid_to. Owner bytes must
        // match the EthFlow contract.
        assert_eq!(&uid.as_slice()[32..52], placement.contract.as_slice());
        // Last 4 bytes = validTo big-endian.
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
        let (creation, _) = build_eth_flow_creation(1, &placement).expect("build succeeds");
        assert_eq!(creation.signing_scheme, cowprotocol::SigningScheme::PreSign);
        assert!(creation.signature.to_bytes().is_empty());
    }

    #[test]
    fn build_rejects_unsupported_chain() {
        let placement = well_formed_placement();
        let err = build_eth_flow_creation(0xdead_beef, &placement).unwrap_err();
        assert!(matches!(err, BuildError::UnsupportedChain(0xdead_beef)));
    }

    #[test]
    fn build_rejects_unknown_kind_marker() {
        let mut placement = well_formed_placement();
        placement.order.kind = B256::repeat_byte(0x42);
        let err = build_eth_flow_creation(1, &placement).unwrap_err();
        assert!(matches!(err, BuildError::UnknownMarker));
    }

    #[test]
    fn build_rejects_non_empty_app_data() {
        let mut placement = well_formed_placement();
        placement.order.appData = B256::repeat_byte(0xee);
        let err = build_eth_flow_creation(1, &placement).unwrap_err();
        assert!(matches!(err, BuildError::Cowprotocol(_)));
    }

    // ---- BLEU-833: error classification ----

    fn host_error_with_api(error_type: &str) -> HostError {
        let body = serde_json::json!({
            "errorType": error_type,
            "description": "test",
        });
        HostError {
            domain: "cow-api".into(),
            kind: nexum::host::types::HostErrorKind::Denied,
            code: 400,
            message: format!("{error_type}: test"),
            data: Some(body.to_string()),
        }
    }

    #[test]
    fn classify_retriable_returns_try_next_block() {
        for kind in [
            "InsufficientFee",
            "TooManyLimitOrders",
            "PriceExceedsMarketPrice",
        ] {
            assert_eq!(
                classify_submit_error(&host_error_with_api(kind)),
                RetryAction::TryNextBlock,
            );
        }
    }

    #[test]
    fn classify_permanent_returns_drop() {
        for kind in [
            "InvalidSignature",
            "WrongOwner",
            "DuplicateOrder",
            "InvalidErc1271Signature",
        ] {
            assert_eq!(
                classify_submit_error(&host_error_with_api(kind)),
                RetryAction::Drop,
            );
        }
    }

    #[test]
    fn classify_missing_data_defaults_to_try_next_block() {
        let err = HostError {
            domain: "cow-api".into(),
            kind: nexum::host::types::HostErrorKind::Internal,
            code: 0,
            message: "network reset".into(),
            data: None,
        };
        assert_eq!(classify_submit_error(&err), RetryAction::TryNextBlock);
    }

    /// COW-1095: verify the hardcoded topic-0 in module.toml matches
    /// keccak256 of the canonical event signature.
    #[test]
    fn topic0_matches_keccak256_of_order_placement() {
        let sig = "OrderPlacement(address,(address,address,address,uint256,uint256,uint32,bytes32,uint256,bytes32,bool,bytes32,bytes32),(uint8,bytes),bytes)";
        let hash = alloy_primitives::keccak256(sig.as_bytes());
        let expected: B256 = "0xcf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"
            .parse()
            .unwrap();
        assert_eq!(
            hash, expected,
            "module.toml event_signature must equal keccak256(\"{sig}\")"
        );
    }
}
