//! Pure strategy logic for the twap-monitor module.
//!
//! Every interaction with the world flows through the
//! `shepherd_sdk::host::Host` trait seam - no direct calls to wit-
//! bindgen-generated free functions live here. The `lib.rs` glue
//! wraps a `WitBindgenHost` adapter around the per-cdylib wit-bindgen
//! imports and hands it to [`on_logs`] / [`on_block`]; tests under
//! `#[cfg(test)]` hand the same functions a
//! `shepherd_sdk_test::MockHost`.

use alloy_primitives::{Address, B256, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use cowprotocol::{
<<<<<<< HEAD
<<<<<<< HEAD
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams, GPv2OrderData,
    OrderCreation, Signature,
=======
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams,
    EMPTY_APP_DATA_JSON, GPv2OrderData, OrderCreation, Signature,
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams, GPv2OrderData,
    OrderCreation, Signature,
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
};
use shepherd_sdk::chain::{eth_call_params, parse_eth_call_result};
use shepherd_sdk::cow::{PollOutcome, RetryAction, classify_api_error, gpv2_to_order_data};
use shepherd_sdk::host::{Host, HostError, LogLevel};

/// Topics + data slice the indexer path consumes from a wit-bindgen
/// `log`. Carrying borrowed slices keeps `strategy.rs` independent
/// from the wit types generated per-cdylib.
pub struct LogView<'a> {
    pub topics: &'a [Vec<u8>],
    pub data: &'a [u8],
}

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
/// log in a dispatch batch and persist its watch.
pub fn on_logs<H: Host>(host: &H, logs: &[LogView<'_>]) -> Result<(), HostError> {
    for log in logs {
        if let Some((owner, params)) = decode_conditional_order_created(log.topics, log.data) {
            persist_watch(host, owner, &params)?;
        }
    }
    Ok(())
}

/// Poll entry: scan every persisted watch and dispatch ready tranches.
pub fn on_block<H: Host>(host: &H, block: BlockInfo) -> Result<(), HostError> {
    poll_all_watches(host, &block)
}

// ---- BLEU-826: indexing path ----

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

/// `set` overwrites in place, so re-indexing the same log (re-org
/// replay, overlapping subscription windows) produces no observable
/// side effect.
fn persist_watch<H: Host>(
    host: &H,
    owner: Address,
    params: &ConditionalOrderParams,
) -> Result<(), HostError> {
    let encoded = params.abi_encode();
    let params_hash = keccak256(&encoded);
    let key = watch_key(&owner, &params_hash);
    host.set(&key, &encoded)?;
    host.log(LogLevel::Info, &format!("indexed {key}"));
    Ok(())
}

// ---- BLEU-827: poll path ----

fn poll_all_watches<H: Host>(host: &H, block: &BlockInfo) -> Result<(), HostError> {
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
            host.log(
                LogLevel::Warn,
                &format!("watch {key} carried unparseable params; skipping"),
            );
            continue;
        };
        let Ok(owner) = owner_hex.parse::<Address>() else {
            continue;
        };
        let outcome = poll_one(host, block.chain_id, &owner, &params);
        host.log(
            LogLevel::Info,
            &format!("poll {key} -> {}", outcome_label(&outcome)),
        );
        match outcome {
            PollOutcome::Ready { order, signature } => {
<<<<<<< HEAD
                submit_ready(
                    host,
                    block.chain_id,
                    owner,
                    &order,
                    signature,
                    &key,
                    now_epoch_s,
                )?;
=======
                submit_ready(host, block.chain_id, owner, &order, signature, &key, now_epoch_s)?;
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
            }
            non_ready => {
                apply_watch_update(host, outcome_to_update(&non_ready), &key)?;
            }
        }
    }
    Ok(())
}

fn poll_one<H: Host>(
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
        Err(err) => {
            // When the node returns a JSON-RPC `ErrorResp` (the normal
            // shape for an `eth_call` revert) the chain backend forwards
            // the structured `error.data` payload as a hex string in
            // `err.data` (COW-1082). `decode_revert_hex` dispatches
            // `PollTryAtBlock` / `PollTryAtEpoch` / `OrderNotValid` /
            // `PollNever` into the corresponding `PollOutcome`. The
            // `None` branch covers transport-level failures (timeout,
            // serde, websocket drop) - those default to retrying on
            // the next block.
            if let Some(data) = err.data.as_deref()
                && let Some(outcome) = shepherd_sdk::chain::decode_revert_hex(data)
            {
                return outcome;
            }
            host.log(
                LogLevel::Warn,
<<<<<<< HEAD
                &format!(
                    "eth_call failed ({}); defaulting to TryNextBlock",
                    err.message
                ),
=======
                &format!("eth_call failed ({}); defaulting to TryNextBlock", err.message),
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
            );
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

// ---- key conventions shared with BLEU-830 ----

<<<<<<< HEAD
<<<<<<< HEAD
/// Render the first 8 bytes of an `appData` hash as `0x12345678…`
/// for log lines. Full 32-byte hex is too noisy for an INFO log;
/// 8 bytes is unique enough to grep against the orderbook.
///
/// Delegates to [`alloy_primitives::hex::encode`] per mfw78's PR #8
/// guidance against carrying our own hex formatters.
fn hex_short(bytes: &[u8; 32]) -> String {
    format!("0x{}…", alloy_primitives::hex::encode(&bytes[..8]))
}

=======
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
/// Render the first 8 bytes of an `appData` hash as `0x12345678…`
/// for log lines. Full 32-byte hex is too noisy for an INFO log;
/// 8 bytes is unique enough to grep against the orderbook.
fn hex_short(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(2 + 16 + 1);
    out.push_str("0x");
    for b in &bytes[..8] {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out.push('…');
    out
}

>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
fn watch_key(owner: &Address, params_hash: &B256) -> String {
    format!("watch:{owner:#x}:{params_hash:#x}")
}

fn parse_watch_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("watch:")?;
    let (owner, hash) = rest.split_once(':')?;
    Some((owner, hash))
}

fn is_ready<H: Host>(
    host: &H,
    owner_hex: &str,
    hash_hex: &str,
    block_number: u64,
    epoch_s: u64,
) -> Result<bool, HostError> {
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

fn read_u64<H: Host>(host: &H, key: &str) -> Result<Option<u64>, HostError> {
    let bytes = host.get(key)?;
    Ok(bytes
        .and_then(|b| <[u8; 8]>::try_from(b.as_slice()).ok())
        .map(u64::from_le_bytes))
}

// ---- BLEU-828: submission path ----

/// `cowprotocol`-side rejection envelope for an `OrderCreation` we
/// failed to assemble. Surfaces in a Warn log; the watch is left in
/// place so the next poll can either re-construct or transition on
/// its own.
<<<<<<< HEAD
///
/// `IntoStaticStr` exposes each variant as a snake_case `&'static
/// str` so the submission warning log can carry `error_kind =
/// unknown_marker` without a match-ladder in the call site.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
#[non_exhaustive]
=======
#[derive(Debug, thiserror::Error)]
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
enum BuildError {
    /// `GPv2OrderData` carried a marker (`kind`, balance enum) we don't
    /// know how to map.
    #[error("GPv2OrderData carried an unknown enum marker")]
    UnknownMarker,
    /// `cowprotocol` rejected the body - typically
    /// `keccak256(app_data) != order.app_data` or `from ==
    /// Address::ZERO`.
    #[error(transparent)]
    Cowprotocol(#[from] cowprotocol::Error),
}

/// Assemble the `OrderCreation` body the orderbook expects from a
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
/// freshly-polled TWAP tranche.
///
/// `app_data_json` is the canonical JSON document whose
/// `keccak256` matches `order.appData`. The caller is responsible
/// for resolving it via [`shepherd_sdk::cow::resolve_app_data`] (or
/// any equivalent path); passing a mismatching string makes
/// `OrderCreation::from_signed_order_data` reject with
/// "app_data JSON digest does not match signed app_data hash".
<<<<<<< HEAD
=======
/// freshly-polled TWAP tranche. `app_data` is left at
/// `EMPTY_APP_DATA_JSON` - conditional orders that pin a non-empty
/// IPFS document get rejected here and the watch is left in place.
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
fn build_order_creation(
    order: &GPv2OrderData,
    signature: Bytes,
    from: Address,
<<<<<<< HEAD
<<<<<<< HEAD
    app_data_json: String,
) -> Result<OrderCreation, BuildError> {
    let order_data = gpv2_to_order_data(order).ok_or(BuildError::UnknownMarker)?;
    let signature = Signature::Eip1271(signature.to_vec());
    let creation =
        OrderCreation::from_signed_order_data(&order_data, signature, from, app_data_json, None)?;
=======
) -> Result<OrderCreation, BuildError> {
    let order_data = gpv2_to_order_data(order).ok_or(BuildError::UnknownMarker)?;
    let signature = Signature::Eip1271(signature.to_vec());
    let creation = OrderCreation::from_signed_order_data(
        &order_data,
        signature,
        from,
        EMPTY_APP_DATA_JSON.to_string(),
        None,
    )?;
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
    app_data_json: String,
) -> Result<OrderCreation, BuildError> {
    let order_data = gpv2_to_order_data(order).ok_or(BuildError::UnknownMarker)?;
    let signature = Signature::Eip1271(signature.to_vec());
    let creation =
        OrderCreation::from_signed_order_data(&order_data, signature, from, app_data_json, None)?;
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
    Ok(creation)
}

fn submit_ready<H: Host>(
    host: &H,
    chain_id: u64,
    owner: Address,
    order: &GPv2OrderData,
    signature: Bytes,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), HostError> {
<<<<<<< HEAD
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
=======
    // COW-1085: short-circuit if the orderbook UID for this exact
    // (order, owner, chain) tuple is already in our local-store as
    // `submitted:`. The poll-tick can re-fire `Ready` for the same
    // TWAP child in successive blocks - `getTradeableOrderWithSignature`
    // does not know shepherd already POSTed it - and re-submitting
    // wastes an appData GET + submit_order call and emits a
    // misleading `DuplicatedOrder` Warn. The UID computation is
    // deterministic from on-chain inputs (and matches what the
    // orderbook derives server-side from the signed payload), so we
    // can check before doing any network work. We also reuse the
    // computed value below as the `submitted:{uid}` marker key, so
    // the read and write paths agree.
    let client_uid_hex = compute_uid_hex(chain_id, order, owner);
    if let Some(uid_hex) = client_uid_hex.as_deref()
        && host.get(&format!("submitted:{uid_hex}"))?.is_some()
    {
        host.log(
            LogLevel::Info,
            &format!("twap {uid_hex} already submitted; skipping poll re-submit"),
        );
        return Ok(());
    }

>>>>>>> 5375073 (chore(rust-idiomatic): M5 compliance pass (cherry-pick M4 + M5 deploy fixes) (#67))
    // COW-1074: cow-swap UI (and other clients) sign TWAPs with a
    // non-empty `appData` hash that points at a JSON document held
    // by the orderbook's app_data registry. Hard-coding
    // `EMPTY_APP_DATA_JSON` here would produce a body whose
    // `keccak256(appDataJson) != order.appData`, and the orderbook
    // rejects with "app_data JSON digest does not match signed
    // app_data hash". Resolve the document via the orderbook
    // mirror; on 404 (orderbook doesn't know the hash) leave the
    // watch in place - there is no path to recover without
    // operator intervention.
<<<<<<< HEAD
    let app_data_json = match shepherd_sdk::cow::resolve_app_data(host, chain_id, &order.appData) {
=======
    let app_data_json = match shepherd_sdk::cow::resolve_app_data(host, chain_id, &order.appData.0)
    {
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        Ok(json) => json,
        Err(err) if err.code == 404 => {
            host.log(
                    LogLevel::Warn,
                    &format!(
                        "twap submit skipped for {owner:#x}: appData hash not mirrored on orderbook ({})",
                        hex_short(&order.appData.0),
                    ),
                );
            return Ok(());
        }
        Err(err) => {
            host.log(
                LogLevel::Warn,
                &format!(
                    "twap submit skipped for {owner:#x}: appData resolve failed ({}): {}",
                    err.code, err.message,
                ),
            );
            return Ok(());
        }
    };

    let creation = match build_order_creation(order, signature, owner, app_data_json) {
<<<<<<< HEAD
=======
    let creation = match build_order_creation(order, signature, owner) {
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        Ok(c) => c,
        Err(e) => {
            host.log(
                LogLevel::Warn,
                &format!("twap submit skipped for {owner:#x}: {e}"),
            );
            return Ok(());
        }
    };
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
        Ok(uid) => {
            let key = format!("submitted:{uid}");
            // Empty marker - presence of the key is the receipt.
            host.set(&key, b"")?;
            host.log(LogLevel::Info, &format!("submitted {key}"));
        }
        Err(err) => {
            apply_submit_retry(host, &err, watch_key, now_epoch_s)?;
        }
    }
    Ok(())
}

<<<<<<< HEAD
=======
/// Compute the orderbook UID hex (`0x` + 112 hex chars) for the given
/// on-chain (order, owner, chain) tuple, mirroring what `submit_order`
/// will deduce server-side. Used by [`submit_ready`] to short-circuit
/// poll-tick re-submissions of an already-submitted TWAP child
/// (COW-1085).
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

>>>>>>> 5375073 (chore(rust-idiomatic): M5 compliance pass (cherry-pick M4 + M5 deploy fixes) (#67))
// ---- BLEU-829: OrderPostError -> retry action ----

fn apply_submit_retry<H: Host>(
    host: &H,
    err: &HostError,
    watch_key: &str,
    now_epoch_s: u64,
) -> Result<(), HostError> {
    let action = classify_api_error(err.data.as_deref());
    match action {
        RetryAction::TryNextBlock => {
            host.log(
                LogLevel::Warn,
                &format!("submit retry-next-block ({}): {}", err.code, err.message),
            );
        }
        RetryAction::Backoff { seconds } => {
            let until = now_epoch_s.saturating_add(seconds);
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                host.set(
                    &format!("next_epoch:{owner_hex}:{hash_hex}"),
                    &until.to_le_bytes(),
                )?;
            }
            host.log(
                LogLevel::Warn,
                &format!(
                    "submit backoff {seconds}s -> next_epoch={until} ({}): {}",
                    err.code, err.message
                ),
            );
        }
        RetryAction::Drop => {
            host.delete(watch_key)?;
            if let Some((owner_hex, hash_hex)) = parse_watch_key(watch_key) {
                let _ = host.delete(&format!("next_block:{owner_hex}:{hash_hex}"));
                let _ = host.delete(&format!("next_epoch:{owner_hex}:{hash_hex}"));
            }
            host.log(
                LogLevel::Warn,
                &format!("submit dropped watch ({}): {}", err.code, err.message),
            );
        }
<<<<<<< HEAD
        // `RetryAction` is `#[non_exhaustive]`; future variants
        // default to "leave the watch in place" (the conservative
        // dispatch choice). Once a new variant gets a real meaning
        // its arm should be added explicitly.
        _ => {
            host.log(
                LogLevel::Warn,
                &format!(
                    "submit unknown retry-action ({}): {} - leaving watch in place",
                    err.code, err.message,
                ),
            );
        }
=======
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
    }
    Ok(())
}

// ---- BLEU-830: PollOutcome lifecycle dispatch ----

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
/// the BLEU-830 contract specifies. `Ready` is handled by the submit
/// path (BLEU-828) and is rejected here so a caller cannot
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

fn apply_watch_update<H: Host>(
    host: &H,
    update: WatchUpdate,
    watch_key: &str,
) -> Result<(), HostError> {
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
            host.log(LogLevel::Info, &format!("dropped watch {watch_key}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, b256, hex};
    use cowprotocol::{BuyTokenDestination, OrderKind, SellTokenSource};
    use shepherd_sdk::host::{HostErrorKind as Kind, LocalStoreHost as _};
    use shepherd_sdk_test::MockHost;

    const SEPOLIA: u64 = 11_155_111;

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

    // ---- existing pure tests preserved from BLEU-826/827/828/830 ----

    #[test]
    fn decodes_well_formed_log() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let owner_topic = {
            let mut t = vec![0u8; 12];
            t.extend_from_slice(owner.as_slice());
            t
        };
<<<<<<< HEAD
        let topics = vec![
            ConditionalOrderCreated::SIGNATURE_HASH.to_vec(),
            owner_topic,
        ];
=======
        let topics = vec![ConditionalOrderCreated::SIGNATURE_HASH.to_vec(), owner_topic];
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
        let data = params.abi_encode();

        let (decoded_owner, decoded_params) =
            decode_conditional_order_created(&topics, &data).expect("decode succeeds");
        assert_eq!(decoded_owner, owner);
        assert_eq!(decoded_params, params);
    }

    #[test]
    fn rejects_wrong_topic() {
<<<<<<< HEAD
        let topics = vec![
            b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").to_vec(),
        ];
=======
        let topics =
            vec![b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").to_vec()];
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
        assert!(decode_conditional_order_created(&topics, &[]).is_none());
    }

    #[test]
    fn rejects_empty_topics() {
        assert!(decode_conditional_order_created(&[], &[]).is_none());
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

    #[test]
    fn build_order_creation_succeeds_with_empty_app_data() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let sig: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        let creation = build_order_creation(
            &submittable_order(),
            sig.clone(),
            owner,
            cowprotocol::EMPTY_APP_DATA_JSON.to_string(),
        )
        .expect("build succeeds");
<<<<<<< HEAD
=======
        let creation = build_order_creation(&submittable_order(), sig.clone(), owner)
            .expect("build succeeds");
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        assert_eq!(creation.from, owner);
        assert_eq!(creation.signing_scheme, cowprotocol::SigningScheme::Eip1271);
        assert_eq!(creation.signature.to_bytes(), sig.to_vec());
        assert_eq!(creation.app_data, cowprotocol::EMPTY_APP_DATA_JSON);
        assert_eq!(creation.app_data_hash, cowprotocol::EMPTY_APP_DATA_HASH);
    }

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
    /// COW-1074: when the caller supplies the matching JSON for a
    /// non-empty `appData` hash, `build_order_creation` accepts the
    /// body. Caller is responsible for resolving the document (in
    /// production this is `submit_ready` via
    /// `shepherd_sdk::cow::resolve_app_data`).
    #[test]
    fn build_order_creation_accepts_matching_non_empty_app_data() {
        use alloy_primitives::keccak256;
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let app_data_json = r#"{"version":"1.1.0","metadata":{"partnerId":"shepherd-e2e"}}"#;
        let app_data_hash = keccak256(app_data_json.as_bytes());

        let mut order = submittable_order();
        order.appData = app_data_hash;

        let sig: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let creation =
            build_order_creation(&order, sig, owner, app_data_json.to_string()).expect("build");
        assert_eq!(creation.app_data, app_data_json);
        assert_eq!(creation.app_data_hash, app_data_hash);
    }

<<<<<<< HEAD
=======
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
    #[test]
    fn build_order_creation_rejects_non_empty_app_data() {
        let mut order = submittable_order();
        order.appData = B256::repeat_byte(0xee);
        let owner = address!("00112233445566778899aabbccddeeff00112233");
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        let err = build_order_creation(
            &order,
            Bytes::new(),
            owner,
            cowprotocol::EMPTY_APP_DATA_JSON.to_string(),
        )
        .unwrap_err();
<<<<<<< HEAD
=======
        let err = build_order_creation(&order, Bytes::new(), owner).unwrap_err();
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        assert!(matches!(err, BuildError::Cowprotocol(_)));
    }

    #[test]
    fn build_order_creation_rejects_zero_from() {
<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
        let err = build_order_creation(
            &submittable_order(),
            Bytes::new(),
            Address::ZERO,
            cowprotocol::EMPTY_APP_DATA_JSON.to_string(),
        )
        .unwrap_err();
<<<<<<< HEAD
=======
        let err =
            build_order_creation(&submittable_order(), Bytes::new(), Address::ZERO).unwrap_err();
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
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
<<<<<<< HEAD
        assert_eq!(
            outcome_to_update(&PollOutcome::TryNextBlock),
            WatchUpdate::NoOp
        );
=======
        assert_eq!(outcome_to_update(&PollOutcome::TryNextBlock), WatchUpdate::NoOp);
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
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
<<<<<<< HEAD
        assert_eq!(
            outcome_to_update(&PollOutcome::DontTryAgain),
            WatchUpdate::DropWatch
        );
=======
        assert_eq!(outcome_to_update(&PollOutcome::DontTryAgain), WatchUpdate::DropWatch);
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
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

    // ---- BLEU-854: MockHost dispatch tests ----

    /// Build the LogView the indexer expects from a well-formed
    /// `ConditionalOrderCreated`.
    fn make_log_topics_and_data(
        owner: Address,
        params: &ConditionalOrderParams,
    ) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut owner_topic = vec![0u8; 12];
        owner_topic.extend_from_slice(owner.as_slice());
        let topics = vec![
            ConditionalOrderCreated::SIGNATURE_HASH.to_vec(),
            owner_topic,
        ];
        let data = params.abi_encode();
        (topics, data)
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
        let (topics, data) = make_log_topics_and_data(owner, &params);
        let view = LogView {
            topics: &topics,
            data: &data,
        };

        on_logs(&host, &[view]).unwrap();

        let expected_key = watch_key(&owner, &keccak256(params.abi_encode()));
        assert_eq!(host.store.len(), 1);
        assert!(host.store.snapshot().contains_key(&expected_key));
        assert!(host.logging.contains("indexed"));
    }

    #[test]
    fn index_overwrites_in_place_on_redelivered_log() {
        // BLEU-826 invariant: re-indexing the same `(owner, params)`
        // pair must be a no-op on top of the existing watch - re-org
        // replays and overlapping subscription windows are normal.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let (topics, data) = make_log_topics_and_data(owner, &params);
        let view = LogView {
            topics: &topics,
            data: &data,
        };

        on_logs(&host, &[view]).unwrap();
        // Re-deliver the same log.
        let view2 = LogView {
            topics: &topics,
            data: &data,
        };
        on_logs(&host, &[view2]).unwrap();

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

        on_block(&host, sample_block(1_000)).unwrap();

        assert_eq!(host.chain.call_count(), 1);
        assert_eq!(host.cow_api.call_count(), 1);
        assert!(
<<<<<<< HEAD
            host.store.snapshot().contains_key("submitted:0xfeedface"),
=======
            host.store
                .snapshot()
<<<<<<< HEAD
                .contains_key("submitted:0xfeedface"),
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
            "expected submitted:{{uid}} marker"
=======
                .contains_key(&format!("submitted:{expected_uid}")),
            "expected submitted:{{client_uid}} marker (COW-1085: marker key now uses the client-computed UID, not the server-returned one, so the idempotency check at the top of submit_ready reads what we wrote)"
        );
        // The MockHost orderbook stub returns `0xfeedface` instead of
        // the canonical UID; this asserts the strategy logs a Warn
        // about the divergence (real orderbooks would not diverge).
        assert!(
            host.logging.contains("twap UID divergence"),
            "expected divergence Warn when mock orderbook returns a non-canonical UID"
        );
    }

    /// COW-1085 regression guard: when `getTradeableOrderWithSignature`
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
            "appData resolve must NOT be called either - the guard short-circuits early",
        );
        assert!(
            host.logging.contains(&format!(
                "twap {already_submitted_uid} already submitted; skipping poll re-submit"
            )),
            "expected the idempotency-skip Info log line",
>>>>>>> 5375073 (chore(rust-idiomatic): M5 compliance pass (cherry-pick M4 + M5 deploy fixes) (#67))
        );
    }

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
    /// COW-1074: Ready order with a non-empty `appData` field
    /// triggers a `cow_api_request` call to
    /// `/api/v1/app_data/{hex}`; the resolved JSON is passed to
    /// `OrderCreation::from_signed_order_data` so the digest matches
    /// and the submit succeeds. Before this PR the path returned
    /// "app_data JSON digest does not match signed app_data hash"
    /// and the watch sat in retry-loop forever.
    #[test]
    fn poll_ready_resolves_non_empty_app_data_then_submits() {
        use alloy_primitives::keccak256;
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_watch(&host, owner, &params);

        let app_data_json = r#"{"version":"1.1.0","metadata":{"partnerId":"shepherd-e2e"}}"#;
        let app_data_hash = keccak256(app_data_json.as_bytes());

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
        // Mirror the orderbook's `/api/v1/app_data/{hex}` response
        // shape: a JSON envelope carrying `fullAppData` as a string.
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

        on_block(&host, sample_block(1_000)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            1,
            "exactly one eth_call to poll Ready"
        );
        assert_eq!(host.cow_api.call_count(), 1, "exactly one orderbook submit");
        assert_eq!(
            host.cow_api.request_calls().len(),
            1,
            "exactly one app_data resolve",
        );
        assert!(
            host.store.snapshot().contains_key("submitted:0xfeedface"),
            "submitted:{{uid}} marker must be written after a successful resolve+submit"
        );
    }

    /// COW-1074: when the orderbook 404s the appData hash (no
    /// mirror exists), the strategy logs a Warn and leaves the
    /// watch in place - neither a `submitted:` nor a `dropped:`
    /// marker is written, and no submit attempt is made.
    #[test]
    fn poll_ready_skips_submit_when_app_data_hash_not_mirrored() {
        use alloy_primitives::keccak256;
        let host = MockHost::new();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_watch(&host, owner, &params);

        let app_data_hash = keccak256(b"unknown");
        let mut ready_order = submittable_order();
        ready_order.appData = app_data_hash;
        let signature: Bytes = hex!("c0ffeec0ffeec0ffee").to_vec().into();
        let wire = (ready_order, signature).abi_encode_params();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Ok(quoted_hex(&wire)),
        );
        // No `respond_to_request_for` → MockCowApi falls back to
        // the default "no response configured" Unsupported error.
        // Switch the default to a 404 so the strategy hits the
        // typed "appData not mirrored" branch.
        host.cow_api
            .respond_to_request(Err(shepherd_sdk::host::HostError {
                domain: "cow-api".into(),
                kind: shepherd_sdk::host::HostErrorKind::Unavailable,
                code: 404,
                message: "Not Found".into(),
                data: None,
            }));

        on_block(&host, sample_block(1_000)).unwrap();

        assert_eq!(host.cow_api.call_count(), 0, "no submit attempt on 404");
        let store = host.store.snapshot();
        assert!(!store.keys().any(|k| k.starts_with("submitted:")));
        assert!(!store.keys().any(|k| k.starts_with("dropped:")));
        assert!(host.logging.contains("appData hash not mirrored"));
    }

<<<<<<< HEAD
=======
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
=======
>>>>>>> 0a0e7b4 (feat(sdk + twap-monitor): resolve non-empty app_data via orderbook lookup (COW-1074))
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

        // InsufficientFee classifies as TryNextBlock per
        // `OrderPostErrorKind::is_retriable`.
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

        on_block(&host, sample_block(1_000)).unwrap();

        // Watch still present, no gate written, no submitted marker.
        assert!(host.store.snapshot().contains_key(&watch_key_str));
        let (owner_hex, hash_hex) = parse_watch_key(&watch_key_str).unwrap();
        assert!(
<<<<<<< HEAD
            !host
                .store
=======
            !host.store
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
                .snapshot()
                .contains_key(&format!("next_epoch:{owner_hex}:{hash_hex}")),
        );
        assert!(
<<<<<<< HEAD
            !host
                .store
=======
            !host.store
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
                .snapshot()
                .keys()
                .any(|k| k.starts_with("submitted:")),
        );
        assert!(host.logging.contains("retry-next-block"));
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

        on_block(&host, sample_block(1_000)).unwrap();

        assert!(
            !host.store.snapshot().contains_key(&watch_key_str),
            "permanent error must drop the watch"
        );
        assert!(host.logging.contains("dropped watch"));
    }

    #[test]
    fn poll_dont_try_again_drops_watch_and_gates() {
        // BLEU-830: when `decode_revert_hex` produces `DontTryAgain`,
        // the lifecycle layer must delete the watch and any stale
        // gates. Simulate by attaching an `OrderNotValid` revert
        // payload to `host-error.data` - that's the wire shape the
        // chain backend forwards once it surfaces structured RPC
        // errors.
        use alloy_sol_types::SolError;
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
        let revert_hex = serde_json::to_string(&alloy_primitives::hex::encode_prefixed(&revert))
            .expect("hex string serialises");
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Err(HostError {
                domain: "chain".into(),
                kind: Kind::Internal,
                code: -32000,
                message: "execution reverted".into(),
                data: Some(revert_hex),
            }),
        );

        on_block(&host, sample_block(1_000)).unwrap();

        assert!(!host.store.snapshot().contains_key(&watch_key_str));
        assert!(
<<<<<<< HEAD
            !host
                .store
=======
            !host.store
>>>>>>> 99c1bab (refactor(twap-monitor): port to Host trait + MockHost tests (BLEU-854))
                .snapshot()
                .contains_key(&format!("next_block:{owner_hex}:{hash_hex}")),
        );
        assert!(host.logging.contains("dropped watch"));
    }
}
