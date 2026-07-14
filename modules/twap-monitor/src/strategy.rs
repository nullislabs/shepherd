//! Pure strategy logic for the twap-monitor module.
//!
//! Every interaction with the world flows through the
//! `nexum_sdk::host::Host` trait seam - no direct calls to wit-
//! bindgen-generated free functions live here. The `lib.rs` glue
//! wraps a `WitBindgenHost` adapter around the per-cdylib wit-bindgen
//! imports and hands it to [`on_chain_logs`] / [`on_block`]; tests under
//! `#[cfg(test)]` hand the same functions a
//! `shepherd_sdk_test::MockHost`.

use alloy_primitives::{Address, B256, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use cowprotocol::{
    COMPOSABLE_COW, Chain, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams,
    GPv2OrderData, OrderCreation, Signature,
};
use nexum_sdk::chain::{eth_call_params, parse_eth_call_result};
use nexum_sdk::events::Log;
use nexum_sdk::host::{ChainError, Fault};
use shepherd_sdk::cow::{
    CowApiError, CowHost, PollOutcome, RetryAction, classify_api_error, decode_revert,
    gpv2_to_order_data,
};

/// Block fields the poll path reads on every dispatch.
pub struct BlockInfo {
    pub chain_id: u64,
    pub number: u64,
    pub timestamp: u64,
}

mod abi {
    use alloy_sol_types::sol;

    sol! {
        /// Wire-format mirror of `cowprotocol::ConditionalOrderParams`. sol!
        /// cannot reference Rust types declared in another sol! block, but
        /// the ABI is identical (same field types in the same order) so
        /// the generated call selector matches the real contract.
        struct Params {
            address handler;
            bytes32 salt;
            bytes staticInput;
        }

        /// Selector source for `eth_call`. The successful return path
        /// decodes into the canonical `cowprotocol::GPv2OrderData`
        /// instead of duplicating the 12-field struct here.
        function getTradeableOrderWithSignature(
            address owner,
            Params params,
            bytes offchainInput,
            bytes32[] proof
        ) external view;
    }
}

/// Indexer entry: decode every `ComposableCoW.ConditionalOrderCreated`
/// chain-log in a dispatch batch and persist its watch.
pub fn on_chain_logs<H: CowHost>(host: &H, logs: &[Log]) -> Result<(), Fault> {
    for log in logs {
        if let Some((owner, params)) = decode_conditional_order_created(log) {
            persist_watch(host, owner, &params)?;
        }
    }
    Ok(())
}

/// Poll entry: scan every persisted watch and dispatch ready tranches.
pub fn on_block<H: CowHost>(host: &H, block: BlockInfo) -> Result<(), Fault> {
    poll_all_watches(host, &block)
}

// ---- indexing path ----

fn decode_conditional_order_created(log: &Log) -> Option<(Address, ConditionalOrderParams)> {
    let decoded = ConditionalOrderCreated::decode_log(&log.inner).ok()?;
    Some((decoded.data.owner, decoded.data.params))
}

/// `set` overwrites in place, so re-indexing the same log (re-org
/// replay, overlapping subscription windows) produces no observable
/// side effect.
fn persist_watch<H: CowHost>(
    host: &H,
    owner: Address,
    params: &ConditionalOrderParams,
) -> Result<(), Fault> {
    let encoded = params.abi_encode();
    let params_hash = keccak256(&encoded);
    let key = watch_key(&owner, &params_hash);
    host.set(&key, &encoded)?;
    tracing::info!("indexed {key}");
    Ok(())
}

// ---- poll path ----

fn poll_all_watches<H: CowHost>(host: &H, block: &BlockInfo) -> Result<(), Fault> {
    let now_epoch_s = block.timestamp / 1000;
    let keys = host.list_keys("watch:")?;
    for key in keys {
        let Some((owner_hex, hash_hex)) = parse_watch_key(&key) else {
            continue;
        };
        if !is_ready(host, owner_hex, hash_hex, block.number, now_epoch_s)? {
            continue;
        }
        let Some(value) = host.get(&key)? else {
            continue;
        };
        let Ok(params) = ConditionalOrderParams::abi_decode(&value) else {
            tracing::warn!("watch {key} carried unparseable params; skipping");
            continue;
        };
        let Ok(owner) = owner_hex.parse::<Address>() else {
            continue;
        };
        let outcome = poll_one(host, block.chain_id, &owner, &params);
        tracing::info!("poll {key} -> {}", outcome_label(&outcome));
        match outcome {
            PollOutcome::Ready { order, signature } => {
                submit_ready(
                    host,
                    block.chain_id,
                    owner,
                    &order,
                    signature,
                    &key,
                    now_epoch_s,
                )?;
            }
            non_ready => {
                apply_watch_update(host, outcome_to_update(&non_ready), &key)?;
            }
        }
    }
    Ok(())
}

fn poll_one<H: CowHost>(
    host: &H,
    chain_id: u64,
    owner: &Address,
    params: &ConditionalOrderParams,
) -> PollOutcome {
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
    match host.request(chain_id, "eth_call", &params_json) {
        Ok(result_json) => parse_eth_call_result(&result_json)
            .and_then(|bytes| decode_return(&bytes))
            .unwrap_or(PollOutcome::TryNextBlock),
        // A structured JSON-RPC error (the normal shape for an
        // `eth_call` revert): the chain backend has already hex-decoded
        // the `error.data` payload, so `decode_revert` dispatches
        // `PollTryAtBlock` / `PollTryAtEpoch` / `OrderNotValid` /
        // `PollNever` straight off the bytes. A revert the decoder does
        // not recognise falls through to the safe `TryNextBlock`.
        Err(ChainError::Rpc(rpc)) => rpc
            .data
            .as_deref()
            .and_then(|bytes| decode_revert(bytes))
            .unwrap_or_else(|| {
                tracing::warn!(
                    "eth_call reverted ({}); defaulting to TryNextBlock",
                    rpc.message
                );
                PollOutcome::TryNextBlock
            }),
        // A transport-level fault (timeout, RPC down, ...): retry on the
        // next block.
        Err(ChainError::Fault(fault)) => {
            tracing::warn!("eth_call failed ({fault}); defaulting to TryNextBlock");
            PollOutcome::TryNextBlock
        }
    }
}

/// Decode a successful `getTradeableOrderWithSignature` return into
/// `Ready { order, signature }`. The wire format is `abi.encode(order,
/// signature)` - the canonical Solidity return tuple - so the two-tuple
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

// ---- key conventions ----

fn watch_key(owner: &Address, params_hash: &B256) -> String {
    format!("watch:{owner:#x}:{params_hash:#x}")
}

fn parse_watch_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("watch:")?;
    let (owner, hash) = rest.split_once(':')?;
    Some((owner, hash))
}

fn is_ready<H: CowHost>(
    host: &H,
    owner_hex: &str,
    hash_hex: &str,
    block_number: u64,
    epoch_s: u64,
) -> Result<bool, Fault> {
    if let Some(next) = read_u64(host, &format!("next_block:{owner_hex}:{hash_hex}"))?
        && block_number < next
    {
        return Ok(false);
    }
    if let Some(next) = read_u64(host, &format!("next_epoch:{owner_hex}:{hash_hex}"))?
        && epoch_s < next
    {
        return Ok(false);
    }
    Ok(true)
}

fn read_u64<H: CowHost>(host: &H, key: &str) -> Result<Option<u64>, Fault> {
    let bytes = host.get(key)?;
    Ok(bytes
        .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
        .map(u64::from_le_bytes))
}

// ---- submission path ----

/// `cowprotocol`-side rejection envelope for an `OrderCreation` we
/// failed to assemble. Surfaces in a Warn log; the watch is left in
/// place so the next poll can either re-construct or transition on
/// its own.
///
/// `IntoStaticStr` exposes each variant as a snake_case `&'static
/// str` so the submission warning log can carry `error_kind =
/// unknown_marker` without a match-ladder in the call site.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
enum BuildError {
    /// `GPv2OrderData` carried a marker (`kind`, balance enum) we don't
    /// know how to map.
    #[error("GPv2OrderData carried an unknown enum marker")]
    UnknownMarker,
    /// `cowprotocol` rejected the body - typically `from ==
    /// Address::ZERO` or a `validTo` beyond the client-side horizon.
    #[error(transparent)]
    Cowprotocol(#[from] cowprotocol::Error),
}

/// Assemble the `OrderCreation` body the orderbook expects from a
/// freshly-polled TWAP tranche.
///
/// The signed `order.appData` digest is submitted verbatim (the
/// hash-only `OrderCreationAppData::Hash` wire shape) - watch-tower
/// parity. The orderbook joins the document it already has registered
/// for that digest; when it has none, the submit rejects with
/// `INVALID_APP_DATA` and [`classify_api_error`] dispatches the retry.
fn build_order_creation(
    order: &GPv2OrderData,
    signature: Bytes,
    from: Address,
) -> Result<OrderCreation, BuildError> {
    let order_data = gpv2_to_order_data(order).ok_or(BuildError::UnknownMarker)?;
    let signature = Signature::Eip1271(signature.to_vec());
    let creation = OrderCreation::new_app_data_hash_only(&order_data, signature, from, None)?;
    Ok(creation)
}

fn submit_ready<H: CowHost>(
    host: &H,
    chain_id: u64,
    owner: Address,
    order: &GPv2OrderData,
    signature: Bytes,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), Fault> {
    // Short-circuit if the orderbook UID for this exact
    // (order, owner, chain) tuple is already in our local-store as
    // `submitted:`. The poll-tick can re-fire `Ready` for the same
    // TWAP child in successive blocks - `getTradeableOrderWithSignature`
    // does not know shepherd already POSTed it - and re-submitting
    // wastes a submit_order call and emits a misleading
    // `DuplicatedOrder` Warn. The UID computation is deterministic
    // from on-chain inputs (and matches what the orderbook derives
    // server-side from the signed payload), so we can check before
    // doing any network work. We also reuse the computed value below
    // as the `submitted:{uid}` marker key, so the read and write
    // paths agree.
    let client_uid_hex = compute_uid_hex(chain_id, order, owner);
    if let Some(uid_hex) = client_uid_hex.as_deref()
        && host.get(&format!("submitted:{uid_hex}"))?.is_some()
    {
        tracing::info!("twap {uid_hex} already submitted; skipping poll re-submit");
        return Ok(());
    }

    // CoW Swap UI (and other clients) sign TWAPs with a non-empty
    // `appData` hash that points at a JSON document already registered
    // with the orderbook. Submit the signed digest verbatim (hash-only
    // shape) and let the orderbook join its own registry - watch-tower
    // parity. An unregistered digest rejects as `INVALID_APP_DATA` and
    // `classify_api_error` dispatches the backoff.
    let creation = match build_order_creation(order, signature, owner) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("twap submit skipped for {owner:#x}: {e}");
            return Ok(());
        }
    };
    let body = match serde_json::to_vec(&creation) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("OrderCreation JSON encode failed: {e}");
            return Ok(());
        }
    };
    match host.submit_order(chain_id, &body) {
        Ok(server_uid) => {
            // Prefer the client-computed UID for the marker key so the
            // idempotency check at the top of `submit_ready` reads what
            // we wrote. In production the server-returned
            // UID is the same value (both sides derive it from the
            // signed `OrderData` via the canonical
            // `digest || owner || valid_to` layout); a divergence
            // would be a protocol-level bug worth surfacing rather
            // than silently splitting the keyspace.
            let marker_uid = client_uid_hex.as_deref().unwrap_or(server_uid.as_str());
            let key = format!("submitted:{marker_uid}");
            // Empty marker - presence of the key is the receipt.
            host.set(&key, b"")?;
            if let Some(client_uid) = client_uid_hex.as_deref()
                && client_uid != server_uid
            {
                tracing::warn!(
                    "twap UID divergence: client={client_uid} server={server_uid} \
                     (marker stored under client UID for idempotency consistency)"
                );
            }
            tracing::info!("submitted {key}");
        }
        Err(err) => {
            apply_submit_retry(host, &err, watch_key, now_epoch_s)?;
        }
    }
    Ok(())
}

/// Compute the orderbook UID hex (`0x` + 112 hex chars) for the given
/// on-chain (order, owner, chain) tuple, mirroring what `submit_order`
/// will deduce server-side. Used by [`submit_ready`] to short-circuit
/// poll-tick re-submissions of an already-submitted TWAP child.
///
/// Returns `None` if the chain id is unsupported by `cowprotocol::Chain`
/// or the order carries an unknown enum marker - both cases also stop
/// the regular submit path downstream, so the caller can fall through
/// to the normal flow and let it surface the appropriate diagnostic.
fn compute_uid_hex(chain_id: u64, order: &GPv2OrderData, owner: Address) -> Option<String> {
    let chain = Chain::try_from(chain_id).ok()?;
    let domain = chain.settlement_domain();
    let order_data = gpv2_to_order_data(order)?;
    Some(format!("{}", order_data.uid(&domain, owner)))
}

// ---- OrderPostError -> retry action ----

fn apply_submit_retry<H: CowHost>(
    host: &H,
    err: &CowApiError,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), Fault> {
    // Only a typed orderbook rejection classifies; transport faults and
    // raw HTTP errors are transient, so the watch stays in place.
    let action = match err {
        CowApiError::Rejected(rejection) => classify_api_error(rejection),
        _ => RetryAction::TryNextBlock,
    };
    match action {
        RetryAction::TryNextBlock => {
            tracing::warn!("submit retry-next-block: {err}");
        }
        RetryAction::Backoff { seconds } => {
            let until = now_epoch_s.saturating_add(seconds);
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                host.set(
                    &format!("next_epoch:{owner_hex}:{hash_hex}"),
                    &until.to_le_bytes(),
                )?;
            }
            tracing::warn!("submit backoff {seconds}s -> next_epoch={until}: {err}");
        }
        RetryAction::Drop => {
            host.delete(watch_key)?;
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                let _ = host.delete(&format!("next_block:{owner_hex}:{hash_hex}"));
                let _ = host.delete(&format!("next_epoch:{owner_hex}:{hash_hex}"));
            }
            tracing::warn!("submit dropped watch: {err}");
        }
        // `RetryAction` is `#[non_exhaustive]`; future variants
        // default to "leave the watch in place" (the conservative
        // dispatch choice). Once a new variant gets a real meaning
        // its arm should be added explicitly.
        _ => {
            tracing::warn!("submit unknown retry-action: {err} - leaving watch in place");
        }
    }
    Ok(())
}

// ---- PollOutcome lifecycle dispatch ----

/// What `apply_watch_update` should do for a given outcome. Kept as a
/// data type (rather than running the effects directly) so the
/// decision is host-free testable.
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
    /// Delete the watch and any stale gate keys - TWAP completed,
    /// cancelled, or otherwise irrecoverable.
    DropWatch,
}

/// Pure mapping from a non-Ready `PollOutcome` to the lifecycle effect
/// the contract specifies. `Ready` is handled by the submit
/// path and is rejected here so a caller cannot
/// accidentally erase the watch when an order was actually produced.
fn outcome_to_update(outcome: &PollOutcome) -> WatchUpdate {
    match outcome {
        PollOutcome::Ready { .. } => WatchUpdate::NoOp,
        PollOutcome::TryNextBlock => WatchUpdate::NoOp,
        PollOutcome::TryOnBlock(n) => WatchUpdate::SetNextBlock(*n),
        PollOutcome::TryAtEpoch(t) => WatchUpdate::SetNextEpoch(*t),
        PollOutcome::DontTryAgain => WatchUpdate::DropWatch,
    }
}

fn apply_watch_update<H: CowHost>(
    host: &H,
    update: WatchUpdate,
    watch_key: &str,
) -> Result<(), Fault> {
    match update {
        WatchUpdate::NoOp => Ok(()),
        WatchUpdate::SetNextBlock(n) => {
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                host.set(
                    &format!("next_block:{owner_hex}:{hash_hex}"),
                    &n.to_le_bytes(),
                )?;
            }
            Ok(())
        }
        WatchUpdate::SetNextEpoch(t) => {
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                host.set(
                    &format!("next_epoch:{owner_hex}:{hash_hex}"),
                    &t.to_le_bytes(),
                )?;
            }
            Ok(())
        }
        WatchUpdate::DropWatch => {
            host.delete(watch_key)?;
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                let _ = host.delete(&format!("next_block:{owner_hex}:{hash_hex}"));
                let _ = host.delete(&format!("next_epoch:{owner_hex}:{hash_hex}"));
            }
            tracing::info!("dropped watch {watch_key}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, b256, hex};
    use cowprotocol::OrderCreationAppData;
    use cowprotocol::{BuyTokenDestination, OrderKind, SellTokenSource};
    use nexum_sdk::Level;
    use nexum_sdk::host::LocalStoreHost as _;
    use nexum_sdk_test::capture_tracing;
    use shepherd_sdk::cow::OrderRejection;
    use shepherd_sdk_test::MockHost;

    const SEPOLIA: u64 = 11_155_111;

    /// `validTo` a given number of seconds from now. The constructor's
    /// client-side max-horizon policy reads the wall clock (not the
    /// block clock), so test orders must expire relative to it.
    fn valid_to_in(seconds: u64) -> u32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_secs();
        u32::try_from(now + seconds).expect("test validTo fits u32")
    }

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
            validTo: valid_to_in(3_600),
            appData: cowprotocol::EMPTY_APP_DATA_HASH,
            feeAmount: U256::ZERO,
            kind: OrderKind::SELL,
            partiallyFillable: false,
            sellTokenBalance: SellTokenSource::ERC20,
            buyTokenBalance: BuyTokenDestination::ERC20,
        }
    }

    // ---- existing pure tests ----

    #[test]
    fn decodes_well_formed_log() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let log = make_log(owner, &params);

        let (decoded_owner, decoded_params) =
            decode_conditional_order_created(&log).expect("decode succeeds");
        assert_eq!(decoded_owner, owner);
        assert_eq!(decoded_params, params);
    }

    #[test]
    fn rejects_wrong_topic() {
        let topics = vec![
            b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").to_vec(),
        ];
        let log: Log = nexum_sdk::events::ChainLogParts {
            address: COMPOSABLE_COW.as_slice(),
            topics: &topics,
            ..Default::default()
        }
        .into();
        assert!(decode_conditional_order_created(&log).is_none());
    }

    #[test]
    fn rejects_empty_topics() {
        let log: Log = nexum_sdk::events::ChainLogParts {
            address: COMPOSABLE_COW.as_slice(),
            ..Default::default()
        }
        .into();
        assert!(decode_conditional_order_created(&log).is_none());
    }

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

    /// The signed `appData` digest goes into the body verbatim as the
    /// hash-only shape - no document lookup, no digest re-derivation.
    #[test]
    fn build_order_creation_submits_app_data_hash_verbatim() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let sig: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let mut order = submittable_order();
        order.appData = B256::repeat_byte(0xee);
        let creation = build_order_creation(&order, sig.clone(), owner).expect("build succeeds");
        assert_eq!(creation.from, owner);
        assert_eq!(creation.signing_scheme, cowprotocol::SigningScheme::Eip1271);
        assert_eq!(creation.signature.to_bytes(), sig.to_vec());
        assert_eq!(
            creation.app_data,
            OrderCreationAppData::Hash {
                hash: order.appData
            }
        );
    }

    #[test]
    fn build_order_creation_rejects_zero_from() {
        let err =
            build_order_creation(&submittable_order(), Bytes::new(), Address::ZERO).unwrap_err();
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

    #[test]
    fn outcome_try_next_block_is_no_op() {
        assert_eq!(
            outcome_to_update(&PollOutcome::TryNextBlock),
            WatchUpdate::NoOp
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
            WatchUpdate::DropWatch
        );
    }

    #[test]
    fn outcome_ready_is_handled_by_submit_path_not_lifecycle() {
        let order = Box::new(submittable_order());
        let outcome = PollOutcome::Ready {
            order,
            signature: Bytes::new(),
        };
        assert_eq!(outcome_to_update(&outcome), WatchUpdate::NoOp);
    }

    // ---- MockHost dispatch tests ----

    /// Build the alloy log the indexer expects from a well-formed
    /// `ConditionalOrderCreated`, assembled through the same WIT-edge
    /// path the bind macro uses at runtime.
    fn make_log(owner: Address, params: &ConditionalOrderParams) -> Log {
        let mut owner_topic = vec![0u8; 12];
        owner_topic.extend_from_slice(owner.as_slice());
        let topics = vec![
            ConditionalOrderCreated::SIGNATURE_HASH.to_vec(),
            owner_topic,
        ];
        let data = params.abi_encode();
        nexum_sdk::events::ChainLogParts {
            address: COMPOSABLE_COW.as_slice(),
            topics: &topics,
            data: &data,
            ..Default::default()
        }
        .into()
    }

    /// Build the `params_json` `poll_one` passes to `host.request`.
    fn programmed_eth_call_params(owner: Address, params: &ConditionalOrderParams) -> String {
        let call = abi::getTradeableOrderWithSignatureCall {
            owner,
            params: abi::Params {
                handler: params.handler,
                salt: params.salt,
                staticInput: params.staticInput.clone(),
            },
            offchainInput: Bytes::new(),
            proof: Vec::new(),
        };
        eth_call_params(&COMPOSABLE_COW, &call.abi_encode())
    }

    /// JSON-encode a hex blob as the raw `result` field a JSON-RPC
    /// response carries (a quoted hex string).
    fn quoted_hex(bytes: &[u8]) -> String {
        let hex = alloy_primitives::hex::encode_prefixed(bytes);
        serde_json::to_string(&hex).unwrap()
    }

    /// Pre-seed a `watch:` row identical to what the indexer would
    /// write.
    fn seed_watch(host: &MockHost, owner: Address, params: &ConditionalOrderParams) -> String {
        let encoded = params.abi_encode();
        let key = watch_key(&owner, &keccak256(&encoded));
        host.store.set(&key, &encoded).unwrap();
        key
    }

    fn sample_block(number: u64) -> BlockInfo {
        BlockInfo {
            chain_id: SEPOLIA,
            number,
            timestamp: 1_700_000_000_000,
        }
    }

    #[test]
    fn index_records_new_watch_on_conditional_order_created() {
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let log = make_log(owner, &params);

        on_chain_logs(&host, &[log]).unwrap();

        let expected_key = watch_key(&owner, &keccak256(params.abi_encode()));
        assert_eq!(host.store.len(), 1);
        assert!(host.store.snapshot().contains_key(&expected_key));
    }

    #[test]
    fn index_overwrites_in_place_on_redelivered_log() {
        // Re-indexing the same `(owner, params)`
        // pair must be a no-op on top of the existing watch - re-org
        // replays and overlapping subscription windows are normal.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();

        on_chain_logs(&host, &[make_log(owner, &params)]).unwrap();
        // Re-deliver the same log.
        on_chain_logs(&host, &[make_log(owner, &params)]).unwrap();

        assert_eq!(host.store.len(), 1, "redelivery must not duplicate watches");
    }

    #[test]
    fn poll_skips_when_next_block_gate_is_in_future() {
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let key = seed_watch(&host, owner, &params);
        let (_, hash_hex) = parse_watch_key(&key).unwrap();
        let owner_hex = format!("{owner:#x}");
        // Gate the watch at block 500; poll at block 100.
        host.store
            .set(
                &format!("next_block:{owner_hex}:{hash_hex}"),
                &500u64.to_le_bytes(),
            )
            .unwrap();

        on_block(&host, sample_block(100)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            0,
            "gated watch must not issue eth_call"
        );
        assert_eq!(host.cow_api.call_count(), 0);
    }

    #[test]
    fn poll_ready_submits_order_and_persists_submitted_uid() {
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_watch(&host, owner, &params);

        let ready_order = submittable_order();
        let signature: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (ready_order.clone(), signature.clone()).abi_encode_params();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Ok(quoted_hex(&wire)),
        );
        host.cow_api.respond(Ok("0xfeedface".to_string()));

        let (result, logs) = capture_tracing(|| on_block(&host, sample_block(1_000)));
        result.unwrap();

        let expected_uid = compute_uid_hex(SEPOLIA, &ready_order, owner)
            .expect("Sepolia is supported + canonical markers");
        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(host.cow_api.call_count(), 1);
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{expected_uid}")),
            "expected submitted:{{client_uid}} marker"
        );
        assert!(
            !host.store.snapshot().contains_key("submitted:0xfeedface"),
            "marker must key on the client UID, not the divergent server UID"
        );
        // The MockHost orderbook stub returns `0xfeedface` instead of
        // the canonical UID; the strategy logs a Warn about the
        // divergence (real orderbooks would not diverge).
        let ev = logs
            .expect_one(|e| e.level == Level::WARN && e.message.contains("twap UID divergence"));
        assert!(ev.message.contains(&format!("client={expected_uid}")));
        assert!(ev.message.contains("server=0xfeedface"));
    }

    /// Regression guard: when `getTradeableOrderWithSignature`
    /// returns the same Ready tuple in consecutive poll-ticks (the
    /// on-chain conditional order does not know shepherd already
    /// POSTed it), the second tick must NOT call `submit_order`
    /// again. Without the guard the orderbook responds with
    /// `DuplicatedOrder` and a Warn fires for what is in fact
    /// correct, finished work. The guard is the `submitted:{uid}`
    /// short-circuit at the top of `submit_ready`.
    #[test]
    fn poll_ready_skips_submit_when_submitted_uid_already_in_store() {
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_watch(&host, owner, &params);

        let ready_order = submittable_order();
        let signature: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (ready_order.clone(), signature.clone()).abi_encode_params();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Ok(quoted_hex(&wire)),
        );

        // Seed the marker that a previous successful poll-tick would
        // have written. The poll path must read this and skip; the
        // orderbook submit must not be attempted.
        let already_submitted_uid = compute_uid_hex(SEPOLIA, &ready_order, owner)
            .expect("Sepolia is supported + canonical markers");
        host.store
            .set(&format!("submitted:{already_submitted_uid}"), b"")
            .expect("seed submitted marker");

        on_block(&host, sample_block(1_000)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            1,
            "poll still consults the chain to see Ready",
        );
        assert_eq!(
            host.cow_api.call_count(),
            0,
            "submit_order must NOT be called when submitted:{{uid}} already exists",
        );
        assert_eq!(
            host.cow_api.request_calls().len(),
            0,
            "the REST passthrough must NOT be touched - the guard short-circuits early",
        );
    }

    /// A Ready order with a non-empty `appData` digest submits the
    /// digest verbatim as the hash-only wire shape: `appData` carries
    /// the `0x`-hex digest, `appDataHash` is absent, and no orderbook
    /// GET runs first - watch-tower parity. The absence of
    /// `appDataHash` is load-bearing: with both fields present the
    /// orderbook reads the body as the full-document shape and rejects
    /// it for a digest mismatch.
    #[test]
    fn poll_ready_submits_non_empty_app_data_hash_only() {
        use alloy_primitives::keccak256;
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_watch(&host, owner, &params);

        let app_data_hash = keccak256(b"registered elsewhere; this client never sees the doc");
        let mut ready_order = submittable_order();
        ready_order.appData = app_data_hash;

        let signature: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (ready_order.clone(), signature.clone()).abi_encode_params();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Ok(quoted_hex(&wire)),
        );
        host.cow_api.respond(Ok("0xfeedface".to_string()));

        on_block(&host, sample_block(1_000)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            1,
            "exactly one eth_call to poll Ready"
        );
        assert_eq!(host.cow_api.call_count(), 1, "exactly one orderbook submit");
        assert!(
            host.cow_api.request_calls().is_empty(),
            "no appData GET before submit - the digest goes out verbatim",
        );
        let body = host.cow_api.last_body_as_json().expect("body is JSON");
        assert_eq!(
            body["appData"],
            format!("0x{}", alloy_primitives::hex::encode(app_data_hash)),
        );
        assert!(
            body.get("appDataHash").is_none(),
            "hash-only body must omit appDataHash, got: {body}"
        );
        let expected_uid = compute_uid_hex(SEPOLIA, &ready_order, owner)
            .expect("Sepolia is supported + canonical markers");
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{expected_uid}")),
            "submitted:{{client_uid}} marker must be written after a successful submit"
        );
    }

    #[test]
    fn submit_transient_error_leaves_state_unchanged_for_next_block() {
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let watch_key_str = seed_watch(&host, owner, &params);

        let ready_order = submittable_order();
        let signature: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (ready_order, signature).abi_encode_params();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Ok(quoted_hex(&wire)),
        );

        // InsufficientFee classifies as TryNextBlock per the
        // retriable-error classifier.
        host.cow_api
            .respond(Err(CowApiError::Rejected(OrderRejection {
                status: 400,
                error_type: "InsufficientFee".into(),
                description: "fee too low".into(),
                data: None,
            })));

        let (result, logs) = capture_tracing(|| on_block(&host, sample_block(1_000)));
        result.unwrap();

        // Watch still present, no gate written, no submitted marker.
        assert!(host.store.snapshot().contains_key(&watch_key_str));
        let (owner_hex, hash_hex) = parse_watch_key(&watch_key_str).unwrap();
        assert!(
            !host
                .store
                .snapshot()
                .contains_key(&format!("next_epoch:{owner_hex}:{hash_hex}")),
        );
        assert!(
            !host
                .store
                .snapshot()
                .keys()
                .any(|k| k.starts_with("submitted:")),
        );
        logs.expect_one(|e| {
            e.level == Level::WARN && e.message.contains("submit retry-next-block")
        });
    }

    #[test]
    fn submit_permanent_error_drops_watch() {
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let watch_key_str = seed_watch(&host, owner, &params);

        let ready_order = submittable_order();
        let signature: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (ready_order, signature).abi_encode_params();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Ok(quoted_hex(&wire)),
        );

        // InvalidSignature classifies as Drop.
        host.cow_api
            .respond(Err(CowApiError::Rejected(OrderRejection {
                status: 400,
                error_type: "InvalidSignature".into(),
                description: "bad sig".into(),
                data: None,
            })));

        on_block(&host, sample_block(1_000)).unwrap();

        let store = host.store.snapshot();
        assert!(
            !store.contains_key(&watch_key_str),
            "permanent error must drop the watch"
        );
        let (owner_hex, hash_hex) = parse_watch_key(&watch_key_str).unwrap();
        assert!(!store.contains_key(&format!("next_block:{owner_hex}:{hash_hex}")));
        assert!(!store.contains_key(&format!("next_epoch:{owner_hex}:{hash_hex}")));
        assert!(!store.keys().any(|k| k.starts_with("submitted:")));
    }

    #[test]
    fn poll_dont_try_again_drops_watch_and_gates() {
        // When `decode_revert` produces `DontTryAgain`, the lifecycle
        // layer must delete the watch and any stale gates. Simulate the
        // wire shape the chain backend forwards: a `ChainError::Rpc`
        // carrying the already-decoded `OrderNotValid` revert bytes.
        use alloy_sol_types::SolError;
        use nexum_sdk::host::RpcError;
        use shepherd_sdk::cow::IConditionalOrder;

        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let watch_key_str = seed_watch(&host, owner, &params);
        let (owner_hex, hash_hex) = parse_watch_key(&watch_key_str).unwrap();
        host.store
            .set(
                &format!("next_block:{owner_hex}:{hash_hex}"),
                &0u64.to_le_bytes(),
            )
            .unwrap();

        let revert = IConditionalOrder::OrderNotValid {
            reason: "dead".into(),
        }
        .abi_encode();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Err(ChainError::Rpc(RpcError {
                code: -32000,
                message: "execution reverted".into(),
                data: Some(revert.into()),
            })),
        );

        on_block(&host, sample_block(1_000)).unwrap();

        assert!(!host.store.snapshot().contains_key(&watch_key_str));
        assert!(
            !host
                .store
                .snapshot()
                .contains_key(&format!("next_block:{owner_hex}:{hash_hex}")),
        );
        assert_eq!(
            host.cow_api.call_count(),
            0,
            "revert-to-drop path never submits"
        );
    }
}
