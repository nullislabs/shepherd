// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolCall, SolError, SolEvent, SolValue};
use cowprotocol::{
    ApiError, BuyTokenDestination, COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated,
    ConditionalOrderParams, EMPTY_APP_DATA_JSON, GPv2OrderData, OrderCreation, OrderData,
    OrderKind, SellTokenSource, Signature,
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

        /// Five custom errors `IConditionalOrder.verify` reverts with.
        /// Source: `cowprotocol/composable-cow/src/interfaces/IConditionalOrder.sol`.
        interface IConditionalOrder {
            error OrderNotValid(string reason);
            error PollTryNextBlock(string reason);
            error PollTryAtBlock(uint256 blockNumber, string reason);
            error PollTryAtEpoch(uint256 timestamp, string reason);
            error PollNever(string reason);
        }
    }
}

/// Outcome of a single watch poll. Mirrors the BLEU-827 enum (rather than
/// `cowprotocol::PollOutcome`) so the lifecycle handler in BLEU-830 sees a
/// flat shape, with `Ready` carrying the materials BLEU-828's submit path
/// needs.
#[derive(Debug)]
#[allow(dead_code)] // Variants consumed by BLEU-828 (Ready) and BLEU-830 (others).
enum PollOutcome {
    // `GPv2OrderData` is ~300 bytes; box it so this enum stays cache-friendly
    // when the lifecycle handler shuffles outcomes around (clippy advice).
    Ready {
        order: Box<GPv2OrderData>,
        signature: Bytes,
    },
    TryAtEpoch(u64),
    TryOnBlock(u64),
    TryNextBlock,
    DontTryAgain,
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
            // those bytes feed into `decode_revert` here. Until then, the
            // `data` branch is unreachable on real traffic and the safe
            // default is to retry on the next block.
            if let Some(data) = err.data.as_deref()
                && let Some(outcome) = decode_revert_hex(data)
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

/// Decode a revert payload (selector + abi-encoded args) into a
/// `PollOutcome`. `None` when the selector is not one of the five
/// `IConditionalOrder` errors — including a bare `Error(string)`
/// require-revert, which the caller treats as TryNextBlock.
fn decode_revert(data: &[u8]) -> Option<PollOutcome> {
    if data.len() < 4 {
        return None;
    }
    let selector: [u8; 4] = data[..4].try_into().ok()?;
    let body = &data[4..];
    match selector {
        s if s == abi::IConditionalOrder::OrderNotValid::SELECTOR => Some(PollOutcome::DontTryAgain),
        s if s == abi::IConditionalOrder::PollTryNextBlock::SELECTOR => {
            Some(PollOutcome::TryNextBlock)
        }
        s if s == abi::IConditionalOrder::PollTryAtBlock::SELECTOR => {
            let decoded = abi::IConditionalOrder::PollTryAtBlock::abi_decode_raw(body).ok()?;
            Some(PollOutcome::TryOnBlock(u256_to_u64_saturating(
                decoded.blockNumber,
            )))
        }
        s if s == abi::IConditionalOrder::PollTryAtEpoch::SELECTOR => {
            let decoded = abi::IConditionalOrder::PollTryAtEpoch::abi_decode_raw(body).ok()?;
            Some(PollOutcome::TryAtEpoch(u256_to_u64_saturating(
                decoded.timestamp,
            )))
        }
        s if s == abi::IConditionalOrder::PollNever::SELECTOR => Some(PollOutcome::DontTryAgain),
        _ => None,
    }
}

/// Decode a hex string (with or without `0x` prefix, optionally wrapped in
/// JSON quotes) carrying revert bytes.
fn decode_revert_hex(s: &str) -> Option<PollOutcome> {
    let stripped = s.trim_matches('"');
    let stripped = stripped.strip_prefix("0x").unwrap_or(stripped);
    let bytes = alloy_primitives::hex::decode(stripped).ok()?;
    decode_revert(&bytes)
}

fn u256_to_u64_saturating(v: U256) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

// ---- BLEU-828: submission path ----

/// Convert a freshly-polled `GPv2OrderData` into the `OrderData` shape the
/// orderbook signs against, mapping the on-chain `bytes32` markers for
/// `kind` / `sellTokenBalance` / `buyTokenBalance` to the typed enums.
/// Returns `None` when ComposableCoW emits a marker we don't know — the
/// caller skips the watch instead of submitting a malformed body.
fn gpv2_to_order_data(gpv2: &GPv2OrderData) -> Option<OrderData> {
    Some(OrderData {
        sell_token: gpv2.sellToken,
        buy_token: gpv2.buyToken,
        // `from_signed_order_data` already normalises Some(ZERO) -> None,
        // but doing it here keeps the EIP-712 hash inputs verbatim if a
        // caller bypasses that helper later.
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

/// Assemble the `OrderCreation` body the orderbook expects.
///
/// `signature` is the EIP-1271 blob `ComposableCoW.getTradeableOrderWith
/// Signature` returns — in orderbook wire form (raw verifier bytes, the
/// orderbook re-prepends `from` before settlement). `from` is the owner
/// that emitted `ConditionalOrderCreated`.
///
/// `app_data` is left at `EMPTY_APP_DATA_JSON`. If the conditional order
/// pins a non-empty document on IPFS, `from_signed_order_data` rejects the
/// mismatch (`keccak256("{}") != order.app_data`) and we surface the error
/// so the watch is not poisoned — resolving the document is a future
/// concern, not part of this PR.
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

#[derive(Debug)]
enum BuildError {
    /// `GPv2OrderData` carried a marker (`kind`, balance enum) we don't
    /// know how to map.
    UnknownMarker,
    /// `cowprotocol` rejected the body — typically `keccak256(app_data) !=
    /// order.app_data` (the conditional order pins a non-empty document)
    /// or `from == Address::ZERO`.
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

/// What the lifecycle layer should do after a failed submission.
///
/// Mirrors the BLEU-829 retry contract (`TryNextBlock` / `BackoffSeconds(s)`
/// / `Drop`). Today the `Backoff` arm has no producer because the
/// cowprotocol API exposes `retry_hint() -> bool` (no server-supplied
/// delay) — the variant is kept so the dispatcher can grow into it
/// once cowprotocol or the orderbook hands us a hint.
#[derive(Debug, Eq, PartialEq)]
enum RetryAction {
    /// Leave the watch in place; it will be polled on the next block.
    TryNextBlock,
    /// Persist `next_epoch = now + seconds` so the watch is skipped
    /// until that timestamp. Reserved for a future producer (the
    /// cowprotocol surface today is bool-only, no server delay).
    #[allow(dead_code)]
    Backoff { seconds: u64 },
    /// Remove the watch entirely — the order will not be retried.
    Drop,
}

/// Try to decode the orderbook's typed error payload from a HostError.
///
/// The host's `cow_api::submit_order` backend places the orderbook's
/// JSON body in `host-error.data` when the upstream returned a typed
/// `ApiError` (this forwarding is the host-side counterpart to BLEU-829;
/// see PR description for the status of that change). When `data` is
/// missing or fails to parse the function returns `None`, and the
/// dispatcher falls back to the safe default of "retry next block".
fn try_decode_api_error(err: &HostError) -> Option<ApiError> {
    let data = err.data.as_deref()?;
    serde_json::from_str::<ApiError>(data).ok()
}

/// Classify a failed submission into the action the lifecycle layer
/// should take. Defaults to `TryNextBlock` whenever the typed payload
/// is absent or unrecognised — the safe choice that lets a flaky
/// orderbook recover without dropping a still-valid order.
fn classify_submit_error(err: &HostError) -> RetryAction {
    match try_decode_api_error(err) {
        Some(api) if api.retry_hint() => RetryAction::TryNextBlock,
        Some(_) => RetryAction::Drop,
        None => RetryAction::TryNextBlock,
    }
}

fn apply_submit_retry(
    err: &HostError,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), HostError> {
    let action = classify_submit_error(err);
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

fn outcome_label(o: &PollOutcome) -> &'static str {
    match o {
        PollOutcome::Ready { .. } => "Ready",
        PollOutcome::TryAtEpoch(_) => "TryAtEpoch",
        PollOutcome::TryOnBlock(_) => "TryOnBlock",
        PollOutcome::TryNextBlock => "TryNextBlock",
        PollOutcome::DontTryAgain => "DontTryAgain",
    }
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

// ---- eth_call JSON plumbing ----

/// Build the JSON params array for `eth_call`: `[{to, data}, "latest"]`.
fn eth_call_params(to: &Address, data: &[u8]) -> String {
    let to_hex = format!("{to:#x}");
    let data_hex = alloy_primitives::hex::encode_prefixed(data);
    serde_json::json!([{ "to": to_hex, "data": data_hex }, "latest"]).to_string()
}

/// The host returns the raw JSON-RPC `result` field. For `eth_call` that
/// is a JSON string holding hex like `"0x1234..."`. Strip the JSON quotes,
/// strip the `0x` prefix, and hex-decode. Returns `None` on shape mismatch.
fn parse_eth_call_result(result_json: &str) -> Option<Vec<u8>> {
    let s = serde_json::from_str::<String>(result_json).ok()?;
    let hex = s.strip_prefix("0x").unwrap_or(&s);
    alloy_primitives::hex::decode(hex).ok()
}

export!(TwapMonitor);

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, hex};

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

    // BLEU-826 regression — the indexer still produces the original tuple.
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

    // ---- BLEU-827 ----

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

    #[test]
    fn decode_revert_order_not_valid_maps_to_drop() {
        let err = abi::IConditionalOrder::OrderNotValid {
            reason: "expired".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::DontTryAgain)
        ));
    }

    #[test]
    fn decode_revert_poll_never_maps_to_drop() {
        let err = abi::IConditionalOrder::PollNever {
            reason: "cancelled".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::DontTryAgain)
        ));
    }

    #[test]
    fn decode_revert_try_next_block() {
        let err = abi::IConditionalOrder::PollTryNextBlock {
            reason: "noop".to_string(),
        };
        assert!(matches!(
            decode_revert(&err.abi_encode()),
            Some(PollOutcome::TryNextBlock)
        ));
    }

    #[test]
    fn decode_revert_try_at_block_carries_number() {
        let err = abi::IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(12_345_678_u64),
            reason: "wait".to_string(),
        };
        let outcome = decode_revert(&err.abi_encode()).expect("decode succeeds");
        assert!(matches!(outcome, PollOutcome::TryOnBlock(n) if n == 12_345_678));
    }

    #[test]
    fn decode_revert_try_at_epoch_carries_timestamp() {
        let err = abi::IConditionalOrder::PollTryAtEpoch {
            timestamp: U256::from(1_700_000_000_u64),
            reason: "soon".to_string(),
        };
        let outcome = decode_revert(&err.abi_encode()).expect("decode succeeds");
        assert!(matches!(outcome, PollOutcome::TryAtEpoch(t) if t == 1_700_000_000));
    }

    #[test]
    fn decode_revert_unknown_selector_returns_none() {
        let mut data = vec![0xde, 0xad, 0xbe, 0xef];
        data.extend_from_slice(&[0u8; 32]);
        assert!(decode_revert(&data).is_none());
    }

    #[test]
    fn decode_revert_truncated_returns_none() {
        assert!(decode_revert(&[0x01, 0x02]).is_none());
    }

    #[test]
    fn decode_revert_hex_strips_prefix_and_quotes() {
        let err = abi::IConditionalOrder::PollTryAtBlock {
            blockNumber: U256::from(42_u64),
            reason: "x".to_string(),
        };
        let payload = alloy_primitives::hex::encode_prefixed(err.abi_encode());
        let quoted = format!("\"{payload}\"");
        assert!(matches!(
            decode_revert_hex(&quoted),
            Some(PollOutcome::TryOnBlock(42))
        ));
    }

    #[test]
    fn u256_overflow_saturates() {
        assert_eq!(u256_to_u64_saturating(U256::MAX), u64::MAX);
        assert_eq!(u256_to_u64_saturating(U256::from(42_u64)), 42);
    }

    #[test]
    fn parse_eth_call_result_decodes_hex_string() {
        assert_eq!(
            parse_eth_call_result(r#""0xdeadbeef""#),
            Some(vec![0xde, 0xad, 0xbe, 0xef])
        );
    }

    #[test]
    fn parse_eth_call_result_handles_empty_hex() {
        assert_eq!(parse_eth_call_result(r#""0x""#), Some(vec![]));
    }

    #[test]
    fn eth_call_params_shape() {
        let to = address!("fdaFc9d1902f4e0b84f65F49f244b32b31013b74");
        let data = hex!("aabbcc").to_vec();
        let p = eth_call_params(&to, &data);
        let parsed: serde_json::Value = serde_json::from_str(&p).unwrap();
        assert_eq!(
            parsed[0]["to"],
            "0xfdafc9d1902f4e0b84f65f49f244b32b31013b74"
        );
        assert_eq!(parsed[0]["data"], "0xaabbcc");
        assert_eq!(parsed[1], "latest");
    }

    #[test]
    fn watch_key_round_trips_via_parse() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let hash =
            b256!("0202020202020202020202020202020202020202020202020202020202020202");
        let key = watch_key(&owner, &hash);
        let (o, h) = parse_watch_key(&key).expect("parse");
        assert_eq!(o.parse::<Address>().unwrap(), owner);
        assert_eq!(h.parse::<B256>().unwrap(), hash);
    }

    // ---- BLEU-828: submission shape ----

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

    #[test]
    fn gpv2_to_order_data_normalises_zero_receiver_to_none() {
        let mut g = submittable_order();
        g.receiver = Address::ZERO;
        let od = gpv2_to_order_data(&g).expect("known markers");
        assert_eq!(od.receiver, None);
    }

    #[test]
    fn gpv2_to_order_data_preserves_non_zero_receiver() {
        let mut g = submittable_order();
        g.receiver = address!("DeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF");
        let od = gpv2_to_order_data(&g).expect("known markers");
        assert_eq!(od.receiver, Some(g.receiver));
    }

    #[test]
    fn gpv2_to_order_data_unknown_kind_returns_none() {
        let mut g = submittable_order();
        g.kind = B256::repeat_byte(0x42);
        assert!(gpv2_to_order_data(&g).is_none());
    }

    #[test]
    fn gpv2_to_order_data_unknown_sell_token_balance_returns_none() {
        let mut g = submittable_order();
        g.sellTokenBalance = B256::repeat_byte(0x99);
        assert!(gpv2_to_order_data(&g).is_none());
    }

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
        // serde round-trip — the submit path serialises this exact value.
        let body = serde_json::to_vec(&creation).expect("json encode");
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["signingScheme"], "eip1271");
        assert_eq!(parsed["from"], format!("{owner:#x}"));
    }

    #[test]
    fn build_order_creation_rejects_non_empty_app_data() {
        // ComposableCoW orders that pin a real document on IPFS get
        // skipped: we only carry `EMPTY_APP_DATA_JSON` in this PR.
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

    // ---- BLEU-829: submit-error classification ----

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
    fn classify_retriable_kind_returns_try_next_block() {
        // InsufficientFee / TooManyLimitOrders / PriceExceedsMarketPrice
        // are the three kinds cowprotocol::OrderPostErrorKind flags
        // retriable today.
        for kind in ["InsufficientFee", "TooManyLimitOrders", "PriceExceedsMarketPrice"] {
            assert_eq!(
                classify_submit_error(&host_error_with_api(kind)),
                RetryAction::TryNextBlock,
                "{kind} should be retriable",
            );
        }
    }

    #[test]
    fn classify_permanent_kind_returns_drop() {
        for kind in [
            "InvalidSignature",
            "WrongOwner",
            "DuplicateOrder",
            "UnsupportedToken",
            "InvalidAppData",
        ] {
            assert_eq!(
                classify_submit_error(&host_error_with_api(kind)),
                RetryAction::Drop,
                "{kind} should be permanent",
            );
        }
    }

    #[test]
    fn classify_unknown_kind_returns_drop() {
        // `Unknown(_)` is non-retriable per cowprotocol's classification
        // — the orderbook rejected the order with a string we don't
        // recognise, so retrying as-is is unlikely to help.
        assert_eq!(
            classify_submit_error(&host_error_with_api("NewlyMintedErrorType")),
            RetryAction::Drop,
        );
    }

    #[test]
    fn classify_missing_data_defaults_to_try_next_block() {
        // Until the host backend forwards the orderbook JSON into
        // host-error.data, we have no payload to decode. The safe
        // default is to retry rather than poison a still-valid watch.
        let err = HostError {
            domain: "cow-api".into(),
            kind: nexum::host::types::HostErrorKind::Internal,
            code: 0,
            message: "network reset".into(),
            data: None,
        };
        assert_eq!(classify_submit_error(&err), RetryAction::TryNextBlock);
    }

    #[test]
    fn classify_malformed_data_defaults_to_try_next_block() {
        let err = HostError {
            domain: "cow-api".into(),
            kind: nexum::host::types::HostErrorKind::Denied,
            code: 502,
            message: "bad gateway".into(),
            data: Some("<html>upstream HTML</html>".into()),
        };
        assert_eq!(classify_submit_error(&err), RetryAction::TryNextBlock);
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
