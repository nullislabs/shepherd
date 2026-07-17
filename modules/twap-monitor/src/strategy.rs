//! Pure strategy logic for the twap-monitor module.
//!
//! Every interaction with the world flows through a trait seam: the
//! `nexum_sdk::host` traits for chain and store access and the videre
//! [`VenueTransport`] under the typed [`CowClient`] for submission -
//! no direct calls to wit-bindgen-generated free functions live here.
//! The `lib.rs` glue hands [`on_chain_logs`] / [`on_block`] the
//! `WitBindgenHost` adapter and the client over the module's own
//! `videre:venue/client` import; tests under `#[cfg(test)]` hand the
//! same functions a `nexum_sdk_test::MockHost` and a scripted
//! transport.
//!
//! The module owns decode and evaluate only: log decoding into the
//! keeper watch set, and the `getTradeableOrderWithSignature` poll
//! behind [`ConditionalSource`]. Gate discipline, the `submitted:`
//! journal, submission through the pool, and retry dispatch live in
//! the shared composition (`composable_cow::run`).

use alloy_primitives::{Address, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use composable_cow::{LegacyRevertAdapter, Verdict, run};
use cow_venue::CowClient;
use cowprotocol::{
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams, GPv2OrderData,
};
use nexum_sdk::chain::{eth_call_params, parse_eth_call_result};
use nexum_sdk::events::Log;
use nexum_sdk::host::{ChainError, ChainHost, Fault, LocalStoreHost};
use nexum_sdk::keeper::{ConditionalSource, Tick, WatchRef, WatchSet};
use videre_sdk::VenueTransport;

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
pub fn on_chain_logs<H: LocalStoreHost>(host: &H, logs: &[Log]) -> Result<(), Fault> {
    for log in logs {
        if let Some((owner, params)) = decode_conditional_order_created(log) {
            persist_watch(host, owner, &params)?;
        }
    }
    Ok(())
}

/// Poll entry: run the keeper over every gate-ready watch through the
/// shared composition, submitting through the typed client onto the
/// pool. The block timestamp arrives in milliseconds; the tick carries
/// Unix seconds.
pub fn on_block<H, T>(host: &H, venue: &CowClient<T>, block: BlockInfo) -> Result<(), Fault>
where
    H: ChainHost + LocalStoreHost,
    T: VenueTransport,
{
    let tick = Tick {
        chain_id: block.chain_id,
        block: block.number,
        epoch_s: block.timestamp / 1000,
    };
    run(host, venue, &TwapSource, &tick)
}

// ---- indexing path ----

/// Topic-0 gates before the ABI decode; the pin is parity-tested
/// against the `shepherd:cow/cow-events` package of record.
fn decode_conditional_order_created(log: &Log) -> Option<(Address, ConditionalOrderParams)> {
    if log.topics().first() != Some(&ConditionalOrderCreated::SIGNATURE_HASH) {
        return None;
    }
    let decoded = ConditionalOrderCreated::decode_log(&log.inner).ok()?;
    Some((decoded.data.owner, decoded.data.params))
}

/// The watch set overwrites in place, so re-indexing the same log
/// (re-org replay, overlapping subscription windows) produces no
/// observable side effect.
fn persist_watch<H: LocalStoreHost>(
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

impl<H: ChainHost> ConditionalSource<H> for TwapSource {
    type Outcome = Verdict;

    fn poll(&self, host: &H, watch: WatchRef<'_>, params: &[u8], tick: &Tick) -> Verdict {
        let Ok(params) = ConditionalOrderParams::abi_decode(params) else {
            tracing::warn!("watch {} carried unparseable params; skipping", watch.key());
            return Verdict::TryNextBlock { reason: [0; 4] };
        };
        let Ok(owner) = watch.owner_hex().parse::<Address>() else {
            tracing::warn!(
                "watch {} carried an unparseable owner; skipping",
                watch.key()
            );
            return Verdict::TryNextBlock { reason: [0; 4] };
        };
        let outcome = poll_one(host, tick.chain_id, &owner, &params);
        tracing::info!("poll {} -> {}", watch.key(), outcome_label(&outcome));
        outcome
    }

    fn label(&self) -> &'static str {
        "twap"
    }
}

fn poll_one<H: ChainHost>(
    host: &H,
    chain_id: u64,
    owner: &Address,
    params: &ConditionalOrderParams,
) -> Verdict {
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
            .unwrap_or(Verdict::TryNextBlock { reason: [0; 4] }),
        // `LegacyRevertAdapter::classify` is the one policy for what a failed
        // poll call means to the watch lifecycle; the diagnostics here
        // cover the cases where the raw error carries information the
        // outcome alone does not.
        Err(err) => {
            let outcome = LegacyRevertAdapter::classify(&err);
            match &err {
                ChainError::Fault(fault) => {
                    tracing::warn!("eth_call failed ({fault}); retrying next block");
                }
                // A permanent drop deserves its cause on the record:
                // the revert selector and the node's message are
                // unrecoverable once the watch is gone.
                ChainError::Rpc(rpc) if matches!(outcome, Verdict::Invalid { .. }) => {
                    let selector = rpc
                        .data
                        .as_deref()
                        .and_then(|data| data.get(..4))
                        .map(alloy_primitives::hex::encode_prefixed)
                        .unwrap_or_else(|| "none".to_string());
                    tracing::warn!(
                        "eth_call reverted permanently (selector {selector}, {}); \
                         dropping watch",
                        rpc.message,
                    );
                }
                _ => {}
            }
            outcome
        }
    }
}

/// Decode a successful `getTradeableOrderWithSignature` return into
/// `Post { order, signature, .. }`. The wire format is the canonical
/// Solidity return tuple `abi.encode(order, signature)`, so the
/// two-tuple parameter decode lines up. The deployed 1.x contract
/// carries no next-poll hint, so `next_poll_timestamp` is synthetic
/// (`0`).
fn decode_return(data: &[u8]) -> Option<Verdict> {
    let (order, signature) = <(GPv2OrderData, Bytes)>::abi_decode_params(data).ok()?;
    Some(Verdict::Post {
        order: Box::new(order),
        signature,
        next_poll_timestamp: 0,
    })
}

fn outcome_label(o: &Verdict) -> &'static str {
    match o {
        Verdict::Post { .. } => "Post",
        Verdict::WaitTimestamp { .. } => "WaitTimestamp",
        Verdict::WaitBlock { .. } => "WaitBlock",
        Verdict::TryNextBlock { .. } => "TryNextBlock",
        Verdict::Invalid { .. } => "Invalid",
        Verdict::NeedsInput { .. } => "NeedsInput",
    }
}

// ---- test-only seam mirrors ----
//
// Thin views over the keeper / venue canon so the dispatch tests can
// seed and inspect the store in the exact shapes production writes.

#[cfg(test)]
use nexum_sdk::keeper::watch_key;

#[cfg(test)]
fn parse_watch_key(key: &str) -> Option<(&str, &str)> {
    let watch = WatchRef::parse(key)?;
    Some((watch.owner_hex(), watch.hash_hex()))
}

#[cfg(test)]
fn signed_intent_body(
    order: &GPv2OrderData,
    signature: &Bytes,
    owner: Address,
) -> Option<cow_venue::CowIntentBody> {
    use cow_venue::assembly::{gpv2_to_order_data, order_data_to_body};
    use cow_venue::{CowIntent, CowIntentBody, SignedOrder};
    let order_data = gpv2_to_order_data(order)?;
    Some(CowIntentBody::V1(CowIntent::Signed(SignedOrder {
        order: order_data_to_body(&order_data),
        owner: owner.into_array(),
        signature: signature.to_vec(),
    })))
}

#[cfg(test)]
fn compute_intent_id(order: &GPv2OrderData, signature: &Bytes, owner: Address) -> Option<String> {
    cow_venue::intent_id(&signed_intent_body(order, signature, owner)?).ok()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use alloy_primitives::{B256, U256, address, b256, hex};
    use cow_venue::CowVenue;
    use cowprotocol::{BuyTokenDestination, OrderKind, SellTokenSource};
    use nexum_sdk::Level;
    use nexum_sdk::host::LocalStoreHost as _;
    use nexum_sdk_test::{MockHost, capture_tracing};
    use videre_sdk::client::sealed::SealedTransport;
    use videre_sdk::{
        IntentBody as _, IntentStatus, Quotation, SubmitOutcome, Venue as _, VenueFault, VenueId,
    };

    use super::*;

    const SEPOLIA: u64 = 11_155_111;

    /// Scripted [`VenueTransport`]: one submit outcome per queued entry,
    /// every submit recorded. Quote, status, and cancel are off the
    /// module's poll path.
    #[derive(Default)]
    struct MockVenue {
        outcomes: RefCell<VecDeque<Result<SubmitOutcome, VenueFault>>>,
        submits: RefCell<Vec<(String, Vec<u8>)>>,
    }

    impl MockVenue {
        fn enqueue_submit(&self, outcome: Result<SubmitOutcome, VenueFault>) {
            self.outcomes.borrow_mut().push_back(outcome);
        }

        fn submits(&self) -> Vec<(String, Vec<u8>)> {
            self.submits.borrow().clone()
        }

        fn submit_count(&self) -> usize {
            self.submits.borrow().len()
        }
    }

    impl SealedTransport for &MockVenue {}

    impl VenueTransport for &MockVenue {
        async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
            unreachable!("quote not exercised")
        }

        async fn submit(
            &self,
            venue: &VenueId,
            body: Vec<u8>,
        ) -> Result<SubmitOutcome, VenueFault> {
            self.submits.borrow_mut().push((venue.to_string(), body));
            self.outcomes.borrow_mut().pop_front().unwrap_or_else(|| {
                Err(VenueFault::Unavailable(
                    "MockVenue: unscripted submit".into(),
                ))
            })
        }

        async fn status(
            &self,
            _venue: &VenueId,
            _receipt: &[u8],
        ) -> Result<IntentStatus, VenueFault> {
            unreachable!("status not exercised")
        }

        async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
            unreachable!("cancel not exercised")
        }
    }

    /// Dispatch one block through `on_block` with the typed client over
    /// the scripted transport.
    fn dispatch(host: &MockHost, venue: &MockVenue, block: BlockInfo) -> Result<(), Fault> {
        on_block(host, &CowClient::with_transport(venue), block)
    }

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
            Verdict::Post {
                order: o,
                signature: s,
                next_poll_timestamp,
            } => {
                assert_eq!(o.sellToken, order.sellToken);
                assert_eq!(o.buyAmount, order.buyAmount);
                assert_eq!(s, sig);
                assert_eq!(next_poll_timestamp, 0, "legacy path carries no hint");
            }
            other => panic!("expected Post, got {other:?}"),
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

    // ---- MockHost + MockVenue dispatch tests ----

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
        let venue = MockVenue::default();
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

        dispatch(&host, &venue, sample_block(100)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            0,
            "gated watch must not issue eth_call"
        );
        assert_eq!(venue.submit_count(), 0);
    }

    #[test]
    fn poll_ready_submits_the_intent_body_through_the_pool() {
        let host = MockHost::new();
        let venue = MockVenue::default();
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
        venue.enqueue_submit(Ok(SubmitOutcome::Accepted(hex!("feedface").to_vec())));

        dispatch(&host, &venue, sample_block(1_000)).unwrap();

        let expected_body = signed_intent_body(&ready_order, &signature, owner)
            .expect("canonical markers")
            .to_bytes()
            .expect("body encodes");
        let expected_id =
            compute_intent_id(&ready_order, &signature, owner).expect("canonical markers");
        assert_eq!(host.chain.call_count(), 1);
        let submits = venue.submits();
        assert_eq!(submits.len(), 1);
        assert_eq!(
            submits[0].0,
            CowVenue::ID.as_str(),
            "routed to the cow venue"
        );
        assert_eq!(
            submits[0].1, expected_body,
            "the wire carries the intent body"
        );
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{expected_id}")),
            "expected submitted:{{intent_id}} marker"
        );
        assert!(
            !host.store.snapshot().contains_key("submitted:0xfeedface"),
            "marker must key on the pre-submit intent-id, not the venue receipt"
        );
    }

    /// Regression guard: when `getTradeableOrderWithSignature`
    /// returns the same Ready tuple in consecutive poll-ticks (the
    /// on-chain conditional order does not know shepherd already
    /// posted it), the second tick must NOT submit again. Without the
    /// guard the venue refuses the duplicate and a Warn fires for what
    /// is in fact correct, finished work. The guard is the
    /// `submitted:{intent_id}` short-circuit at the top of
    /// `submit_ready`.
    #[test]
    fn poll_ready_skips_submit_when_the_intent_id_is_already_journalled() {
        let host = MockHost::new();
        let venue = MockVenue::default();
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
        // venue submit must not be attempted.
        let already_submitted =
            compute_intent_id(&ready_order, &signature, owner).expect("canonical markers");
        host.store
            .set(&format!("submitted:{already_submitted}"), b"")
            .expect("seed submitted marker");

        dispatch(&host, &venue, sample_block(1_000)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            1,
            "poll still consults the chain to see Ready",
        );
        assert_eq!(
            venue.submit_count(),
            0,
            "the pool must NOT be touched when submitted:{{intent_id}} already exists",
        );
    }

    /// A Ready order with a non-empty `appData` digest rides the intent
    /// body verbatim: assembly into the orderbook wire shape is the
    /// adapter's, so the keeper ships exactly the digest the chain
    /// returned - watch-tower parity.
    #[test]
    fn poll_ready_carries_a_non_empty_app_data_digest_in_the_body() {
        let host = MockHost::new();
        let venue = MockVenue::default();
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
        venue.enqueue_submit(Ok(SubmitOutcome::Accepted(hex!("feedface").to_vec())));

        dispatch(&host, &venue, sample_block(1_000)).unwrap();

        assert_eq!(
            host.chain.call_count(),
            1,
            "exactly one eth_call to poll Ready"
        );
        let submits = venue.submits();
        assert_eq!(submits.len(), 1, "exactly one pool submit");
        let cow_venue::CowIntentBody::V1(cow_venue::CowIntent::Signed(signed)) =
            cow_venue::CowIntentBody::from_bytes(&submits[0].1).expect("body decodes")
        else {
            panic!("expected a signed V1 intent");
        };
        assert_eq!(
            signed.order.app_data, app_data_hash.0,
            "the digest goes out verbatim in the body",
        );
        let expected_id =
            compute_intent_id(&ready_order, &signature, owner).expect("canonical markers");
        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("submitted:{expected_id}")),
            "submitted:{{intent_id}} marker must be written after a successful submit"
        );
    }

    #[test]
    fn submit_transient_fault_leaves_state_unchanged_for_next_block() {
        let host = MockHost::new();
        let venue = MockVenue::default();
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

        // The adapter projects a retriable rejection onto `unavailable`,
        // which the retry table folds to TryNextBlock.
        venue.enqueue_submit(Err(VenueFault::Unavailable(
            "InsufficientFee: fee too low".into(),
        )));

        let (result, logs) = capture_tracing(|| dispatch(&host, &venue, sample_block(1_000)));
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

    /// The venue's throttle hint survives the pool seam: a rate-limited
    /// refusal backs the watch off on the epoch clock instead of
    /// hot-looping the submit every block.
    #[test]
    fn submit_rate_limited_backs_off_on_the_epoch_gate() {
        let host = MockHost::new();
        let venue = MockVenue::default();
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
        venue.enqueue_submit(Err(VenueFault::RateLimited {
            retry_after_ms: Some(2_500),
        }));

        dispatch(&host, &venue, sample_block(1_000)).unwrap();

        let snapshot = host.store.snapshot();
        assert!(
            snapshot.contains_key(&watch_key_str),
            "backoff must keep the watch"
        );
        let (owner_hex, hash_hex) = parse_watch_key(&watch_key_str).unwrap();
        assert_eq!(
            snapshot
                .get(&format!("next_epoch:{owner_hex}:{hash_hex}"))
                .unwrap(),
            &(1_700_000_000_u64 + 3).to_le_bytes().to_vec(),
            "2500ms rounds up to a 3s backoff from the tick clock",
        );
        assert!(!snapshot.keys().any(|k| k.starts_with("submitted:")));
    }

    #[test]
    fn submit_denied_drops_watch() {
        let host = MockHost::new();
        let venue = MockVenue::default();
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

        // The adapter projects a permanent rejection onto `denied`,
        // which the retry table folds to Drop.
        venue.enqueue_submit(Err(VenueFault::Denied("InvalidSignature: bad sig".into())));

        dispatch(&host, &venue, sample_block(1_000)).unwrap();

        let store = host.store.snapshot();
        assert!(
            !store.contains_key(&watch_key_str),
            "permanent refusal must drop the watch"
        );
        let (owner_hex, hash_hex) = parse_watch_key(&watch_key_str).unwrap();
        assert!(!store.contains_key(&format!("next_block:{owner_hex}:{hash_hex}")));
        assert!(!store.contains_key(&format!("next_epoch:{owner_hex}:{hash_hex}")));
        assert!(!store.keys().any(|k| k.starts_with("submitted:")));
    }

    #[test]
    fn poll_invalid_drops_watch_and_gates() {
        // When `LegacyRevertAdapter` produces `Invalid`, the lifecycle
        // layer must delete the watch and any stale gates. Simulate the
        // wire shape the chain backend forwards: a `ChainError::Rpc`
        // carrying the already-decoded `OrderNotValid` revert bytes.
        use alloy_sol_types::SolError;
        use composable_cow::IConditionalOrder;
        use nexum_sdk::host::RpcError;

        let host = MockHost::new();
        let venue = MockVenue::default();
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

        let (result, logs) = capture_tracing(|| dispatch(&host, &venue, sample_block(1_000)));
        result.unwrap();

        assert!(!host.store.snapshot().contains_key(&watch_key_str));
        assert!(
            !host
                .store
                .snapshot()
                .contains_key(&format!("next_block:{owner_hex}:{hash_hex}")),
        );
        assert_eq!(venue.submit_count(), 0, "revert-to-drop path never submits");
        // The destructive drop carries its cause: the revert selector
        // and the node's message ride the Warn, and the keeper logs
        // the removal itself.
        let warn = logs.expect_one(|e| {
            e.level == Level::WARN && e.message.contains("eth_call reverted permanently")
        });
        assert!(warn.message.contains("execution reverted"));
        let selector_hex =
            alloy_primitives::hex::encode_prefixed(&IConditionalOrder::OrderNotValid::SELECTOR[..]);
        assert!(
            warn.message.contains(&selector_hex),
            "the four-byte selector must be greppable: {}",
            warn.message,
        );
        logs.expect_one(|e| {
            e.message
                .contains(&format!("dropped watch {watch_key_str}"))
        });
    }

    /// Guard: the `sol!` decoder's topic-0 matches the
    /// `shepherd:cow/cow-events` package of record. A typo or ABI
    /// drift would silently miss every registration event.
    #[test]
    fn topic0_matches_the_cow_events_package_of_record() {
        let wit = include_str!("../../../wit/shepherd-cow/cow-events.wit");
        let expected = format!("{:#x}", ConditionalOrderCreated::SIGNATURE_HASH);
        assert!(
            wit.contains(&expected),
            "sol! topic-0 must match the shepherd:cow/cow-events pin ({expected})",
        );
    }

    /// Read the shipped `module.toml` and assert its pinned
    /// `event_signature` equals the decoder topic-0 - catches a
    /// manifest/code drift the wit assertion cannot see.
    #[test]
    fn manifest_topic0_matches_conditional_order_created_signature_hash() {
        let manifest = include_str!("../module.toml");
        let expected = format!("{:#x}", ConditionalOrderCreated::SIGNATURE_HASH);
        assert!(
            manifest.contains(&expected),
            "module.toml event_signature must equal the decoder topic-0 ({expected})",
        );
    }
}
