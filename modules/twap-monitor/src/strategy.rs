//! Pure strategy logic for the twap-monitor module.
//!
//! Every interaction with the world flows through the
//! `nexum_sdk::host::Host` trait seam - no direct calls to wit-
//! bindgen-generated free functions live here. The `lib.rs` glue
//! wraps a `WitBindgenHost` adapter around the per-cdylib wit-bindgen
//! imports and hands it to [`on_chain_logs`] / [`on_block`]; tests under
//! `#[cfg(test)]` hand the same functions a
//! `shepherd_sdk_test::MockHost`.
//!
//! The module owns decode and evaluate only: log decoding into the
//! keeper watch set, and the `getTradeableOrderWithSignature` poll
//! behind [`ConditionalSource`]. Gate discipline, the `submitted:`
//! journal, submission, and retry dispatch live in the shared
//! composition (`shepherd_sdk::cow::run`).

use alloy_primitives::{Address, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use cowprotocol::{
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams, GPv2OrderData,
};
use nexum_sdk::chain::{eth_call_params, parse_eth_call_result};
use nexum_sdk::events::Log;
use nexum_sdk::host::{ChainError, Fault};
use nexum_sdk::keeper::{ConditionalSource, Tick, WatchRef, WatchSet};
use shepherd_sdk::cow::{CowHost, PollOutcome, classify_poll_error, run};

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

/// Poll entry: run every gate-ready watch through the keeper
/// composition. The block timestamp arrives in milliseconds; the tick
/// carries Unix seconds.
pub fn on_block<H: CowHost>(host: &H, block: BlockInfo) -> Result<(), Fault> {
    let tick = Tick {
        chain_id: block.chain_id,
        block: block.number,
        epoch_s: block.timestamp / 1000,
    };
    run(host, &TwapSource, &tick)
}

// ---- indexing path ----

fn decode_conditional_order_created(log: &Log) -> Option<(Address, ConditionalOrderParams)> {
    let decoded = ConditionalOrderCreated::decode_log(&log.inner).ok()?;
    Some((decoded.data.owner, decoded.data.params))
}

/// The watch set overwrites in place, so re-indexing the same log
/// (re-org replay, overlapping subscription windows) produces no
/// observable side effect.
fn persist_watch<H: CowHost>(
    host: &H,
    owner: Address,
    params: &ConditionalOrderParams,
) -> Result<(), Fault> {
    let encoded = params.abi_encode();
    let key = WatchSet::new(host).put(&owner, &keccak256(&encoded), &encoded)?;
    tracing::info!("indexed {key}");
    Ok(())
}

// ---- poll path ----

/// TWAP conditional source: decode the stored `ConditionalOrderParams`
/// and evaluate `getTradeableOrderWithSignature` on chain. A row this
/// source cannot decode polls again next block rather than tearing
/// down the sweep.
struct TwapSource;

impl<H: CowHost> ConditionalSource<H> for TwapSource {
    type Outcome = PollOutcome;

    fn poll(&self, host: &H, watch: WatchRef<'_>, params: &[u8], tick: &Tick) -> PollOutcome {
        let Ok(params) = ConditionalOrderParams::abi_decode(params) else {
            tracing::warn!("watch {} carried unparseable params; skipping", watch.key());
            return PollOutcome::TryNextBlock;
        };
        let Ok(owner) = watch.owner_hex().parse::<Address>() else {
            tracing::warn!(
                "watch {} carried an unparseable owner; skipping",
                watch.key()
            );
            return PollOutcome::TryNextBlock;
        };
        let outcome = poll_one(host, tick.chain_id, &owner, &params);
        tracing::info!("poll {} -> {}", watch.key(), outcome_label(&outcome));
        outcome
    }

    fn label(&self) -> &'static str {
        "twap"
    }
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
        // `classify_poll_error` is the one policy for what a failed
        // poll call means to the watch lifecycle; only a transport
        // fault warrants its own diagnostic here.
        Err(err) => {
            if let ChainError::Fault(fault) = &err {
                tracing::warn!("eth_call failed ({fault}); retrying next block");
            }
            classify_poll_error(&err)
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

// ---- test-only seam mirrors ----
//
// Thin views over the keeper / SDK canon so the dispatch tests can
// seed and inspect the store in the exact shapes production writes.

#[cfg(test)]
fn watch_key(owner: &Address, params_hash: &alloy_primitives::B256) -> String {
    WatchSet::<shepherd_sdk_test::MockHost>::key(owner, params_hash)
}

#[cfg(test)]
fn parse_watch_key(key: &str) -> Option<(&str, &str)> {
    let watch = WatchRef::parse(key)?;
    Some((watch.owner_hex(), watch.hash_hex()))
}

#[cfg(test)]
fn compute_uid_hex(chain_id: u64, order: &GPv2OrderData, owner: Address) -> Option<String> {
    shepherd_sdk::cow::order_uid_hex(chain_id, order, owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, U256, address, b256, hex};
    use cowprotocol::{BuyTokenDestination, OrderKind, SellTokenSource};
    use nexum_sdk::Level;
    use nexum_sdk::host::LocalStoreHost as _;
    use nexum_sdk_test::capture_tracing;
    use shepherd_sdk::cow::{CowApiError, OrderRejection};
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

    #[test]
    fn watch_key_round_trips_via_parse() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let hash = b256!("0202020202020202020202020202020202020202020202020202020202020202");
        let key = watch_key(&owner, &hash);
        let (o, h) = parse_watch_key(&key).expect("parse");
        assert_eq!(o.parse::<Address>().unwrap(), owner);
        assert_eq!(h.parse::<B256>().unwrap(), hash);
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
