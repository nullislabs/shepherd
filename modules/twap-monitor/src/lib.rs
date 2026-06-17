// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use alloy_primitives::{Address, B256, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use cowprotocol::{
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams,
    EMPTY_APP_DATA_JSON, GPv2OrderData, OrderCreation, Signature,
};
use shepherd_sdk::chain::{eth_call_params, parse_eth_call_result};
use shepherd_sdk::cow::{
    PollOutcome, RetryAction, classify_api_error, gpv2_to_order_data,
};

use nexum::host::{chain, local_store, logging, types};
use shepherd::cow::cow_api;

mod abi {
    use alloy_sol_types::sol;

    sol! {
        /// Wire-format mirror of `cowprotocol::ConditionalOrderParams`. sol!
        /// cannot reference Rust types declared in another sol! block, but
        /// the ABI is identical (same field types in the same order) so the
        /// generated call selector matches the real contract.
        struct Params {
            address handler;
            bytes32 salt;
            bytes staticInput;
        }

        /// Selector source for `eth_call`. The successful return path
        /// decodes into the canonical `cowprotocol::GPv2OrderData` instead
        /// of duplicating the 12-field struct here.
        function getTradeableOrderWithSignature(
            address owner,
            Params params,
            bytes offchainInput,
            bytes32[] proof
        ) external view;
    }
}

struct TwapMonitor;

impl Guest for TwapMonitor {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "twap-monitor init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        match event {
            types::Event::Logs(logs) => {
                for log in &logs {
                    if let Some((owner, params)) =
                        decode_conditional_order_created(&log.topics, &log.data)
                    {
                        persist_watch(owner, &params)?;
                    }
                }
            }
            types::Event::Block(block) => poll_all_watches(&block)?,
            // Tick / Message are not used by this module.
            _ => {}
        }
        Ok(())
    }
}

// ---- BLEU-826: indexing path ----

/// Decode a raw event log against `ComposableCoW.ConditionalOrderCreated`.
///
/// Returns `None` when topic0 does not match the event signature or the
/// payload fails ABI decoding — both are non-fatal for an indexer that
/// shares a subscription with adjacent events.
fn decode_conditional_order_created(
    topics: &[Vec<u8>],
    data: &[u8],
) -> Option<(Address, ConditionalOrderParams)> {
    let topic0 = topics.first()?;
    if topic0.len() != 32 || B256::from_slice(topic0) != ConditionalOrderCreated::SIGNATURE_HASH {
        return None;
    }
    let words: Vec<B256> = topics
        .iter()
        .filter(|t| t.len() == 32)
        .map(|t| B256::from_slice(t))
        .collect();
    let decoded = ConditionalOrderCreated::decode_raw_log(words, data).ok()?;
    Some((decoded.owner, decoded.params))
}

/// `set` overwrites in place, so re-indexing the same log (re-org replay,
/// overlapping subscription windows) produces no observable side effect.
fn persist_watch(owner: Address, params: &ConditionalOrderParams) -> Result<(), HostError> {
    let encoded = params.abi_encode();
    let params_hash = keccak256(&encoded);
    let key = watch_key(&owner, &params_hash);
    local_store::set(&key, &encoded)?;
    logging::log(logging::Level::Info, &format!("indexed {key}"));
    Ok(())
}

// ---- BLEU-827: poll path ----

/// Iterate every persisted watch, skip the ones gated by a future
/// `next_block:` / `next_epoch:` entry, and dispatch the ready ones via
/// `eth_call`.
fn poll_all_watches(block: &types::Block) -> Result<(), HostError> {
    let now_epoch_s = block.timestamp / 1000;
    let keys = local_store::list_keys("watch:")?;
    for key in keys {
        let Some((owner_hex, hash_hex)) = parse_watch_key(&key) else {
            continue;
        };
        if !is_ready(owner_hex, hash_hex, block.number, now_epoch_s)? {
            continue;
        }
        let Some(value) = local_store::get(&key)? else {
            continue;
        };
        let Ok(params) = ConditionalOrderParams::abi_decode(&value) else {
            logging::log(
                logging::Level::Warn,
                &format!("watch {key} carried unparseable params; skipping"),
            );
            continue;
        };
        let Ok(owner) = owner_hex.parse::<Address>() else {
            continue;
        };
        let outcome = poll_one(block.chain_id, &owner, &params);
        logging::log(
            logging::Level::Info,
            &format!("poll {key} -> {}", outcome_label(&outcome)),
        );
        match outcome {
            PollOutcome::Ready { order, signature } => {
                submit_ready(block.chain_id, owner, &order, signature, &key, now_epoch_s)?;
            }
            non_ready => {
                apply_watch_update(outcome_to_update(&non_ready), &key)?;
            }
        }
    }
    Ok(())
}

fn poll_one(chain_id: u64, owner: &Address, params: &ConditionalOrderParams) -> PollOutcome {
    let call = abi::getTradeableOrderWithSignatureCall {
        owner: *owner,
        params: abi::Params {
            handler: params.handler,
            salt: params.salt,
            staticInput: params.staticInput.clone(),
        },
        offchainInput: Bytes::new(),
        proof: Vec::new(),
    };
    let params_json = eth_call_params(&COMPOSABLE_COW, &call.abi_encode());
    match chain::request(chain_id, "eth_call", &params_json) {
        Ok(result_json) => parse_eth_call_result(&result_json)
            .and_then(|bytes| decode_return(&bytes))
            .unwrap_or(PollOutcome::TryNextBlock),
        Err(err) => {
            // The host's chain backend currently stuffs the formatted RPC
            // error into `message` with `data: None`; once it forwards the
            // structured `error.data` from alloy's `RpcError::ErrorResp`,
            // those bytes feed into `shepherd_sdk::chain::decode_revert_hex`
            // here. Until then, the `data` branch is unreachable on real
            // traffic and the safe default is to retry on the next block.
            if let Some(data) = err.data.as_deref()
                && let Some(outcome) = shepherd_sdk::chain::decode_revert_hex(data)
            {
                return outcome;
            }
            logging::log(
                logging::Level::Warn,
                &format!("eth_call failed ({}); defaulting to TryNextBlock", err.message),
            );
            PollOutcome::TryNextBlock
        }
    }
}

/// Decode a successful `getTradeableOrderWithSignature` return into
/// `Ready { order, signature }`. The wire format is `abi.encode(order,
/// signature)` — the canonical Solidity return tuple — so the two-tuple
/// parameter decode lines up.
fn decode_return(data: &[u8]) -> Option<PollOutcome> {
    let (order, signature) = <(GPv2OrderData, Bytes)>::abi_decode_params(data).ok()?;
    Some(PollOutcome::Ready {
        order: Box::new(order),
        signature,
    })
}

fn outcome_label(o: &PollOutcome) -> &'static str {
    match o {
        PollOutcome::Ready { .. } => "Ready",
        PollOutcome::TryAtEpoch(_) => "TryAtEpoch",
        PollOutcome::TryOnBlock(_) => "TryOnBlock",
        PollOutcome::TryNextBlock => "TryNextBlock",
        PollOutcome::DontTryAgain => "DontTryAgain",
    }
}

// ---- key conventions shared with BLEU-830 ----

fn watch_key(owner: &Address, params_hash: &B256) -> String {
    format!("watch:{owner:#x}:{params_hash:#x}")
}

fn parse_watch_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("watch:")?;
    let (owner, hash) = rest.split_once(':')?;
    Some((owner, hash))
}

fn is_ready(
    owner_hex: &str,
    hash_hex: &str,
    block_number: u64,
    epoch_s: u64,
) -> Result<bool, HostError> {
    if let Some(next) = read_u64(&format!("next_block:{owner_hex}:{hash_hex}"))?
        && block_number < next
    {
        return Ok(false);
    }
    if let Some(next) = read_u64(&format!("next_epoch:{owner_hex}:{hash_hex}"))?
        && epoch_s < next
    {
        return Ok(false);
    }
    Ok(true)
}

fn read_u64(key: &str) -> Result<Option<u64>, HostError> {
    let bytes = local_store::get(key)?;
    Ok(bytes
        .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
        .map(u64::from_le_bytes))
}

// ---- BLEU-828: submission path ----

/// `cowprotocol`-side rejection envelope for an `OrderCreation` we
/// failed to assemble. Surfaces in a Warn log; the watch is left in
/// place so the next poll can either re-construct or transition on
/// its own (the typical case is the conditional order's `app_data`
/// pinning a non-empty IPFS document we cannot resolve).
#[derive(Debug)]
enum BuildError {
    /// `GPv2OrderData` carried a marker (`kind`, balance enum) we don't
    /// know how to map.
    UnknownMarker,
    /// `cowprotocol` rejected the body — typically `keccak256(app_data)
    /// != order.app_data` or `from == Address::ZERO`.
    Cowprotocol(cowprotocol::Error),
}

impl core::fmt::Display for BuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownMarker => f.write_str("GPv2OrderData carried an unknown enum marker"),
            Self::Cowprotocol(e) => write!(f, "{e}"),
        }
    }
}

/// Assemble the `OrderCreation` body the orderbook expects from a
/// freshly-polled TWAP tranche.
///
/// `signature` is the EIP-1271 blob `ComposableCoW.
/// getTradeableOrderWithSignature` returns — in orderbook wire form
/// (raw verifier bytes; the orderbook re-prepends `from` before
/// settlement). `from` is the watch owner.
///
/// `app_data` is left at `EMPTY_APP_DATA_JSON`. Conditional orders that
/// pin a non-empty IPFS document get rejected by
/// `from_signed_order_data` (digest mismatch) and the watch is left in
/// place — resolving the document is a future concern.
fn build_order_creation(
    order: &GPv2OrderData,
    signature: Bytes,
    from: Address,
) -> Result<OrderCreation, BuildError> {
    let order_data = gpv2_to_order_data(order).ok_or(BuildError::UnknownMarker)?;
    let signature = Signature::Eip1271(signature.to_vec());
    OrderCreation::from_signed_order_data(
        &order_data,
        signature,
        from,
        EMPTY_APP_DATA_JSON.to_string(),
        None,
    )
    .map_err(BuildError::Cowprotocol)
}

fn submit_ready(
    chain_id: u64,
    owner: Address,
    order: &GPv2OrderData,
    signature: Bytes,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), HostError> {
    let creation = match build_order_creation(order, signature, owner) {
        Ok(c) => c,
        Err(e) => {
            logging::log(
                logging::Level::Warn,
                &format!("twap submit skipped for {owner:#x}: {e}"),
            );
            return Ok(());
        }
    };
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
        Ok(uid) => {
            let key = format!("submitted:{uid}");
            // Empty marker — presence of the key is the receipt. BLEU-830
            // may later attach metadata (block, attempt count) but the
            // bare flag is enough to suppress double submits.
            local_store::set(&key, b"")?;
            logging::log(logging::Level::Info, &format!("submitted {key}"));
        }
        Err(err) => {
            apply_submit_retry(&err, watch_key, now_epoch_s)?;
        }
    }
    Ok(())
}

// ---- BLEU-829: OrderPostError -> retry action ----

fn apply_submit_retry(
    err: &HostError,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), HostError> {
    let action = classify_api_error(err.data.as_deref());
    match action {
        RetryAction::TryNextBlock => {
            logging::log(
                logging::Level::Warn,
                &format!("submit retry-next-block ({}): {}", err.code, err.message),
            );
        }
        RetryAction::Backoff { seconds } => {
            let until = now_epoch_s.saturating_add(seconds);
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                local_store::set(
                    &format!("next_epoch:{owner_hex}:{hash_hex}"),
                    &until.to_le_bytes(),
                )?;
            }
            logging::log(
                logging::Level::Warn,
                &format!(
                    "submit backoff {seconds}s -> next_epoch={until} ({}): {}",
                    err.code, err.message
                ),
            );
        }
        RetryAction::Drop => {
            // Drop the watch, plus any stale gating entries the lifecycle
            // layer may have written.
            local_store::delete(watch_key)?;
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                let _ = local_store::delete(&format!("next_block:{owner_hex}:{hash_hex}"));
                let _ = local_store::delete(&format!("next_epoch:{owner_hex}:{hash_hex}"));
            }
            logging::log(
                logging::Level::Warn,
                &format!("submit dropped watch ({}): {}", err.code, err.message),
            );
        }
    }
    Ok(())
}

// ---- BLEU-830: PollOutcome lifecycle dispatch ----

/// What `apply_watch_update` should do for a given outcome. Kept as a
/// data type (rather than running the effects directly) so the decision
/// is host-free testable; `apply_watch_update` is the impure other half.
#[derive(Debug, Eq, PartialEq)]
enum WatchUpdate {
    /// Leave the store untouched. Next block re-polls the watch.
    NoOp,
    /// Write `next_block:` so subsequent polls skip until the given
    /// block number is reached.
    SetNextBlock(u64),
    /// Write `next_epoch:` so subsequent polls skip until the given
    /// Unix-seconds timestamp is reached.
    SetNextEpoch(u64),
    /// Delete the watch and any stale gate keys — TWAP completed,
    /// cancelled, or otherwise irrecoverable.
    DropWatch,
}

/// Pure mapping from a non-Ready `PollOutcome` to the lifecycle effect
/// the BLEU-830 contract specifies. `Ready` is handled by the submit
/// path (BLEU-828) and is rejected here so a caller cannot accidentally
/// erase the watch when an order was actually produced.
fn outcome_to_update(outcome: &PollOutcome) -> WatchUpdate {
    match outcome {
        PollOutcome::Ready { .. } => WatchUpdate::NoOp, // belt-and-braces; caller routes Ready to submit_ready
        PollOutcome::TryNextBlock => WatchUpdate::NoOp,
        PollOutcome::TryOnBlock(n) => WatchUpdate::SetNextBlock(*n),
        PollOutcome::TryAtEpoch(t) => WatchUpdate::SetNextEpoch(*t),
        PollOutcome::DontTryAgain => WatchUpdate::DropWatch,
    }
}

fn apply_watch_update(update: WatchUpdate, watch_key: &str) -> Result<(), HostError> {
    match update {
        WatchUpdate::NoOp => Ok(()),
        WatchUpdate::SetNextBlock(n) => {
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                local_store::set(
                    &format!("next_block:{owner_hex}:{hash_hex}"),
                    &n.to_le_bytes(),
                )?;
            }
            Ok(())
        }
        WatchUpdate::SetNextEpoch(t) => {
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                local_store::set(
                    &format!("next_epoch:{owner_hex}:{hash_hex}"),
                    &t.to_le_bytes(),
                )?;
            }
            Ok(())
        }
        WatchUpdate::DropWatch => {
            local_store::delete(watch_key)?;
            // Best-effort: drop any stale gates the previous lifecycle
            // step may have written. `delete` is a no-op for absent keys
            // already, so the `let _` discards a benign error if the
            // underlying store complains.
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                let _ = local_store::delete(&format!("next_block:{owner_hex}:{hash_hex}"));
                let _ = local_store::delete(&format!("next_epoch:{owner_hex}:{hash_hex}"));
            }
            logging::log(
                logging::Level::Info,
                &format!("dropped watch {watch_key}"),
            );
            Ok(())
        }
    }
}

export!(TwapMonitor);

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, b256, hex};
    use cowprotocol::{BuyTokenDestination, OrderKind, SellTokenSource};

    fn sample_params() -> ConditionalOrderParams {
        ConditionalOrderParams {
            handler: address!("ffeeddccbbaa00998877665544332211ffeeddcc"),
            salt: b256!("0101010101010101010101010101010101010101010101010101010101010101"),
            staticInput: hex!("deadbeef").to_vec().into(),
        }
    }

    fn sample_order() -> GPv2OrderData {
        GPv2OrderData {
            sellToken: address!("6810e776880C02933D47DB1b9fc05908e5386b96"),
            buyToken: address!("DAE5F1590db13E3B40423B5b5c5fbf175515910b"),
            receiver: address!("DeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF"),
            sellAmount: U256::from(1_000_u64),
            buyAmount: U256::from(2_000_u64),
            validTo: 1_700_000_000,
            appData: B256::repeat_byte(0xaa),
            feeAmount: U256::ZERO,
            kind: B256::repeat_byte(0xbb),
            partiallyFillable: false,
            sellTokenBalance: B256::repeat_byte(0xcc),
            buyTokenBalance: B256::repeat_byte(0xdd),
        }
    }

    fn submittable_order() -> GPv2OrderData {
        GPv2OrderData {
            sellToken: address!("6810e776880C02933D47DB1b9fc05908e5386b96"),
            buyToken: address!("DAE5F1590db13E3B40423B5b5c5fbf175515910b"),
            receiver: Address::ZERO,
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

    // ---- BLEU-826: indexer ----

    #[test]
    fn decodes_well_formed_log() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let owner_topic = {
            let mut t = vec![0u8; 12];
            t.extend_from_slice(owner.as_slice());
            t
        };
        let topics = vec![ConditionalOrderCreated::SIGNATURE_HASH.to_vec(), owner_topic];
        let data = params.abi_encode();

        let (decoded_owner, decoded_params) =
            decode_conditional_order_created(&topics, &data).expect("decode succeeds");
        assert_eq!(decoded_owner, owner);
        assert_eq!(decoded_params, params);
    }

    #[test]
    fn rejects_wrong_topic() {
        let topics =
            vec![b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").to_vec()];
        assert!(decode_conditional_order_created(&topics, &[]).is_none());
    }

    #[test]
    fn rejects_empty_topics() {
        assert!(decode_conditional_order_created(&[], &[]).is_none());
    }

    // ---- BLEU-827: return decoder ----

    #[test]
    fn decode_return_round_trip() {
        let order = sample_order();
        let sig: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (order.clone(), sig.clone()).abi_encode_params();

        match decode_return(&wire).expect("decode succeeds") {
            PollOutcome::Ready {
                order: o,
                signature: s,
            } => {
                assert_eq!(o.sellToken, order.sellToken);
                assert_eq!(o.buyAmount, order.buyAmount);
                assert_eq!(s, sig);
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    // ---- BLEU-828: order construction ----

    #[test]
    fn build_order_creation_succeeds_with_empty_app_data() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let sig: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let creation = build_order_creation(&submittable_order(), sig.clone(), owner)
            .expect("build succeeds");
        assert_eq!(creation.from, owner);
        assert_eq!(
            creation.signing_scheme,
            cowprotocol::SigningScheme::Eip1271
        );
        assert_eq!(creation.signature.to_bytes(), sig.to_vec());
        assert_eq!(creation.app_data, cowprotocol::EMPTY_APP_DATA_JSON);
        assert_eq!(creation.app_data_hash, cowprotocol::EMPTY_APP_DATA_HASH);
    }

    #[test]
    fn build_order_creation_rejects_non_empty_app_data() {
        let mut order = submittable_order();
        order.appData = B256::repeat_byte(0xee);
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let err = build_order_creation(&order, Bytes::new(), owner).unwrap_err();
        assert!(matches!(err, BuildError::Cowprotocol(_)));
    }

    #[test]
    fn build_order_creation_rejects_zero_from() {
        let err = build_order_creation(&submittable_order(), Bytes::new(), Address::ZERO)
            .unwrap_err();
        assert!(matches!(err, BuildError::Cowprotocol(_)));
    }

    #[test]
    fn watch_key_round_trips_via_parse() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let hash = b256!("0202020202020202020202020202020202020202020202020202020202020202");
        let key = watch_key(&owner, &hash);
        let (o, h) = parse_watch_key(&key).expect("parse");
        assert_eq!(o.parse::<Address>().unwrap(), owner);
        assert_eq!(h.parse::<B256>().unwrap(), hash);
    }

    // ---- BLEU-830: PollOutcome -> lifecycle effect ----

    #[test]
    fn outcome_try_next_block_is_no_op() {
        assert_eq!(
            outcome_to_update(&PollOutcome::TryNextBlock),
            WatchUpdate::NoOp,
        );
    }

    #[test]
    fn outcome_try_on_block_sets_next_block_gate() {
        assert_eq!(
            outcome_to_update(&PollOutcome::TryOnBlock(12_345)),
            WatchUpdate::SetNextBlock(12_345),
        );
    }

    #[test]
    fn outcome_try_at_epoch_sets_next_epoch_gate() {
        assert_eq!(
            outcome_to_update(&PollOutcome::TryAtEpoch(1_700_000_000)),
            WatchUpdate::SetNextEpoch(1_700_000_000),
        );
    }

    #[test]
    fn outcome_dont_try_again_drops_watch() {
        assert_eq!(
            outcome_to_update(&PollOutcome::DontTryAgain),
            WatchUpdate::DropWatch,
        );
    }

    #[test]
    fn outcome_ready_is_handled_by_submit_path_not_lifecycle() {
        // Ready never reaches outcome_to_update in poll_all_watches (the
        // match routes it to submit_ready). The mapping is a safety net:
        // if a future refactor accidentally pipes Ready through here, the
        // watch must NOT be erased — submit_ready owns the post-submit
        // book-keeping.
        let order = Box::new(submittable_order());
        let outcome = PollOutcome::Ready {
            order,
            signature: Bytes::new(),
        };
        assert_eq!(outcome_to_update(&outcome), WatchUpdate::NoOp);
    }
}
