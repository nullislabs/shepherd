//! Pure logic for the twap-monitor keeper: decode ComposableCoW
//! registration and removal logs into the commitment set, and poll
//! `getTradeableOrderWithSignature` behind [`Poller`]. World access
//! flows through the `nexum_sdk::host` traits and the typed
//! [`CowClient`] over [`VenueTransport`]. Gate discipline, the
//! `submitted:` journal, and retry dispatch live in
//! `composable_cow::run`.

use alloy_primitives::{Address, B256, Bytes, keccak256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use composable_cow::{LegacyRevertAdapter, Verdict, run};
use cow_venue::CowClient;
use cowprotocol::{
    COMPOSABLE_COW,
    ComposableCoW::{ConditionalOrderCreated, ConditionalOrderRemoved},
    ConditionalOrderParams, GPv2OrderData,
};
use nexum_sdk::chain::{eth_call_params, parse_eth_call_result};
use nexum_sdk::host::{ChainError, ChainHost, Fault, LocalStoreHost};
use nexum_sdk::keeper::{CommitmentRef, CommitmentSet, Poller, Tick, commitment_key};
use nexum_sdk::sol_events::Log;
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
        /// Wire-format mirror of `cowprotocol::ConditionalOrderParams`; the
        /// ABI matches so the generated selector matches the contract.
        struct Params {
            address handler;
            bytes32 salt;
            bytes staticInput;
        }

        /// Selector source for `eth_call`; the return decodes into
        /// `cowprotocol::GPv2OrderData`.
        function getTradeableOrderWithSignature(
            address owner,
            Params params,
            bytes offchainInput,
            bytes32[] proof
        ) external view;
    }
}

/// Decode one ComposableCoW registration or removal log. A create
/// persists a commitment stamped with the log's chain position; a
/// removal drops the commitment and its gates only when it postdates
/// that stamp. Streams merge in arrival order, not chain order, so the
/// stamp keeps a stale removal from dropping a re-registered
/// commitment.
pub fn on_event<H: LocalStoreHost>(host: &H, log: &Log) -> Result<(), Fault> {
    if let Some((owner, params)) = decode_conditional_order_created(log) {
        persist_commitment(host, owner, &params, LogPosition::of(log))?;
    } else if let Some((owner, hash)) = decode_conditional_order_removed(log) {
        remove_commitment(host, owner, &hash, LogPosition::of(log))?;
    }
    Ok(())
}

/// Run the keeper over every gate-ready commitment through the shared
/// composition, submitting through the typed client. Block timestamp is
/// milliseconds; the tick carries Unix seconds.
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

/// Chain position of a mined log, ordered as the chain orders logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LogPosition {
    block: u64,
    index: u64,
}

impl LogPosition {
    /// `None` for a pending log.
    fn of(log: &Log) -> Option<Self> {
        Some(Self {
            block: log.block_number?,
            index: log.log_index?,
        })
    }
}

/// Header tag + `(block, log-index)` little-endian words.
const ROW_HEADER_LEN: usize = 17;

/// Commitment row payload: a position header ahead of the ABI-encoded
/// `ConditionalOrderParams`, pinning where the create sits on chain.
fn encode_row(indexed_at: Option<LogPosition>, params: &[u8]) -> Vec<u8> {
    let mut row = Vec::with_capacity(ROW_HEADER_LEN + params.len());
    match indexed_at {
        Some(at) => {
            row.push(1);
            row.extend_from_slice(&at.block.to_le_bytes());
            row.extend_from_slice(&at.index.to_le_bytes());
        }
        None => row.extend_from_slice(&[0; ROW_HEADER_LEN]),
    }
    row.extend_from_slice(params);
    row
}

/// Split a commitment row into its position stamp and params bytes;
/// `None` on a malformed row.
fn decode_row(row: &[u8]) -> Option<(Option<LogPosition>, &[u8])> {
    let (header, params) = row.split_first_chunk::<ROW_HEADER_LEN>()?;
    let [tag, position @ ..] = header;
    let (block, index) = position.split_first_chunk::<8>()?;
    let indexed_at = match tag {
        0 => None,
        1 => Some(LogPosition {
            block: u64::from_le_bytes(*block),
            index: u64::from_le_bytes(index.try_into().ok()?),
        }),
        _ => return None,
    };
    Some((indexed_at, params))
}

/// Topic-0 gates before the ABI decode; pin parity-tested against
/// `shepherd:cow/cow-events`.
fn decode_conditional_order_created(log: &Log) -> Option<(Address, ConditionalOrderParams)> {
    if log.topics().first() != Some(&ConditionalOrderCreated::SIGNATURE_HASH) {
        return None;
    }
    let decoded = ConditionalOrderCreated::decode_log(&log.inner).ok()?;
    Some((decoded.data.owner, decoded.data.params))
}

/// Overwrites in place, keeping the latest indexed stamp, so
/// re-indexing (re-org replay, cursor rewind) never ages the row.
fn persist_commitment<H: LocalStoreHost>(
    host: &H,
    owner: Address,
    params: &ConditionalOrderParams,
    indexed_at: Option<LogPosition>,
) -> Result<(), Fault> {
    let encoded = params.abi_encode();
    let hash = keccak256(&encoded);
    let commitments = CommitmentSet::new(host);
    let prior = CommitmentRef::parse(&commitment_key(&owner, &hash))
        .map(|commitment| commitments.get(commitment))
        .transpose()?
        .flatten()
        .and_then(|row| decode_row(&row).and_then(|(at, _)| at));
    let row = encode_row(prior.max(indexed_at), &encoded);
    let key = commitments.put(&owner, &hash, &row)?;
    tracing::info!("indexed {key}");
    Ok(())
}

/// Topic-0 gates before the ABI decode; pin parity-tested against
/// `shepherd:cow/cow-events`.
fn decode_conditional_order_removed(log: &Log) -> Option<(Address, B256)> {
    if log.topics().first() != Some(&ConditionalOrderRemoved::SIGNATURE_HASH) {
        return None;
    }
    let decoded = ConditionalOrderRemoved::decode_log(&log.inner).ok()?;
    Some((decoded.data.owner, decoded.data.singleOrderHash))
}

/// Drops the commitment only when the removal provably postdates its
/// create stamp; an unprovable ordering keeps the commitment
/// (self-heals via the poll drop path). An unknown or already-dropped
/// order is a no-op.
fn remove_commitment<H: LocalStoreHost>(
    host: &H,
    owner: Address,
    hash: &B256,
    removed_at: Option<LogPosition>,
) -> Result<(), Fault> {
    let key = commitment_key(&owner, hash);
    let Some(commitment) = CommitmentRef::parse(&key) else {
        return Ok(());
    };
    let commitments = CommitmentSet::new(host);
    let Some(row) = commitments.get(commitment)? else {
        return Ok(());
    };
    let indexed_at = decode_row(&row).and_then(|(at, _)| at);
    match (indexed_at, removed_at) {
        (Some(indexed_at), Some(removed_at)) if indexed_at < removed_at => {
            commitments.remove(commitment)?;
            tracing::info!("removed {key}");
        }
        _ => tracing::info!("kept {key}: removal does not postdate its create"),
    }
    Ok(())
}

/// TWAP conditional source: decode the stored row and evaluate
/// `getTradeableOrderWithSignature` on chain. An undecodable row polls
/// again next block rather than tearing down the run.
struct TwapSource;

impl<H: ChainHost> Poller<H> for TwapSource {
    type Outcome = Verdict;

    fn poll(&self, host: &H, commitment: CommitmentRef<'_>, params: &[u8], tick: &Tick) -> Verdict {
        let Some((_, params)) = decode_row(params) else {
            tracing::warn!(
                "commitment {} carried an unparseable row; skipping",
                commitment.key()
            );
            return Verdict::TryNextBlock { reason: [0; 4] };
        };
        let Ok(params) = ConditionalOrderParams::abi_decode(params) else {
            tracing::warn!(
                "commitment {} carried unparseable params; skipping",
                commitment.key()
            );
            return Verdict::TryNextBlock { reason: [0; 4] };
        };
        let Ok(owner) = commitment.owner_hex().parse::<Address>() else {
            tracing::warn!(
                "commitment {} carried an unparseable owner; skipping",
                commitment.key()
            );
            return Verdict::TryNextBlock { reason: [0; 4] };
        };
        let outcome = poll_one(host, tick.chain_id, &owner, &params);
        tracing::info!("poll {} -> {}", commitment.key(), outcome_label(&outcome));
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
        // poll call means to the commitment lifecycle; the diagnostics here
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
                // unrecoverable once the commitment is gone.
                ChainError::Rpc(rpc) if matches!(outcome, Verdict::Invalid { .. }) => {
                    let selector = rpc
                        .data
                        .as_deref()
                        .and_then(|data| data.get(..4))
                        .map(alloy_primitives::hex::encode_prefixed)
                        .unwrap_or_else(|| "none".to_string());
                    tracing::warn!(
                        "eth_call reverted permanently (selector {selector}, {}); \
                         dropping commitment",
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
/// `Verdict::Post`. The 1.x contract carries no next-poll hint, so
/// `next_poll_timestamp` is `None`.
fn decode_return(data: &[u8]) -> Option<Verdict> {
    let (order, signature) = <(GPv2OrderData, Bytes)>::abi_decode_params(data).ok()?;
    Some(Verdict::Post {
        order: Box::new(order),
        signature,
        next_poll_timestamp: None,
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

#[cfg(test)]
fn parse_commitment_key(key: &str) -> Option<(&str, &str)> {
    let commitment = CommitmentRef::parse(key)?;
    Some((commitment.owner_hex(), commitment.hash_hex()))
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
        owner,
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

    /// Scripted [`VenueTransport`]: one submit outcome per queued entry.
    /// Quote, status, and cancel are off the poll path.
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

    /// Dispatch one block through `on_block` over the scripted transport.
    fn dispatch(host: &MockHost, venue: &MockVenue, block: BlockInfo) -> Result<(), Fault> {
        on_block(host, &CowClient::with_transport(venue), block)
    }

    /// `validTo` `seconds` from the wall clock, saturating.
    fn valid_to_in(seconds: u32) -> u32 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_secs();
        u32::try_from(now)
            .expect("wall clock fits u32")
            .saturating_add(seconds)
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

    #[test]
    fn decodes_well_formed_log() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let log = make_log(owner, &params, at(1, 0));

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
        let log: Log = nexum_sdk::sol_events::LogParts {
            address: COMPOSABLE_COW.as_slice(),
            topics: &topics,
            ..Default::default()
        }
        .into();
        assert!(decode_conditional_order_created(&log).is_none());
    }

    #[test]
    fn rejects_empty_topics() {
        let log: Log = nexum_sdk::sol_events::LogParts {
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
                assert_eq!(next_poll_timestamp, None, "legacy path carries no hint");
            }
            other => panic!("expected Post, got {other:?}"),
        }
    }

    #[test]
    fn commitment_key_round_trips_via_parse() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let hash = b256!("0202020202020202020202020202020202020202020202020202020202020202");
        let key = commitment_key(&owner, &hash);
        let (o, h) = parse_commitment_key(&key).expect("parse");
        assert_eq!(o.parse::<Address>().unwrap(), owner);
        assert_eq!(h.parse::<B256>().unwrap(), hash);
    }

    /// Chain position shorthand for the log builders.
    fn at(block: u64, index: u64) -> LogPosition {
        LogPosition { block, index }
    }

    /// A mined ComposableCoW log at `position`.
    fn make_event_log(owner: Address, topic0: B256, data: &[u8], position: LogPosition) -> Log {
        let mut owner_topic = vec![0u8; 12];
        owner_topic.extend_from_slice(owner.as_slice());
        let topics = vec![topic0.to_vec(), owner_topic];
        nexum_sdk::sol_events::LogParts {
            address: COMPOSABLE_COW.as_slice(),
            topics: &topics,
            data,
            block_number: Some(position.block),
            log_index: Some(position.index),
            ..Default::default()
        }
        .into()
    }

    /// A well-formed `ConditionalOrderCreated` mined at `position`.
    fn make_log(owner: Address, params: &ConditionalOrderParams, position: LogPosition) -> Log {
        make_event_log(
            owner,
            ConditionalOrderCreated::SIGNATURE_HASH,
            &params.abi_encode(),
            position,
        )
    }

    /// A well-formed v2 `ConditionalOrderRemoved` mined at `position`.
    fn make_removed_log(owner: Address, hash: B256, position: LogPosition) -> Log {
        make_event_log(
            owner,
            ConditionalOrderRemoved::SIGNATURE_HASH,
            &hash.abi_encode(),
            position,
        )
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

    /// JSON-encode a hex blob as a JSON-RPC `result` field.
    fn quoted_hex(bytes: &[u8]) -> String {
        let hex = alloy_primitives::hex::encode_prefixed(bytes);
        serde_json::to_string(&hex).unwrap()
    }

    /// Pre-seed a `commitment:` row as the indexer would for a create at
    /// block 1, index 0.
    fn seed_commitment(host: &MockHost, owner: Address, params: &ConditionalOrderParams) -> String {
        let encoded = params.abi_encode();
        let key = commitment_key(&owner, &keccak256(&encoded));
        host.store
            .set(&key, &encode_row(Some(at(1, 0)), &encoded))
            .unwrap();
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
    fn index_records_new_commitment_on_conditional_order_created() {
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let log = make_log(owner, &params, at(42, 9));

        on_event(&host, &log).unwrap();

        let expected_key = commitment_key(&owner, &keccak256(params.abi_encode()));
        assert_eq!(host.store.len(), 1);
        let store = host.store.snapshot();
        let row = store.get(&expected_key).expect("commitment row present");
        let (indexed_at, stored) = decode_row(row).expect("row decodes");
        assert_eq!(indexed_at, Some(at(42, 9)), "row carries the log position");
        assert_eq!(stored, params.abi_encode(), "row carries the params");
    }

    #[test]
    fn index_overwrites_in_place_on_redelivered_log() {
        // Re-indexing the same `(owner, params)`
        // pair must be a no-op on top of the existing commitment - re-org
        // replays and overlapping trigger windows are normal.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();

        on_event(&host, &make_log(owner, &params, at(42, 9))).unwrap();
        // Re-deliver the same log.
        on_event(&host, &make_log(owner, &params, at(42, 9))).unwrap();

        assert_eq!(
            host.store.len(),
            1,
            "redelivery must not duplicate commitments"
        );
    }

    #[test]
    fn decodes_well_formed_removed_log() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let hash = b256!("0303030303030303030303030303030303030303030303030303030303030303");
        let log = make_removed_log(owner, hash, at(1, 0));

        let (decoded_owner, decoded_hash) =
            decode_conditional_order_removed(&log).expect("decode succeeds");
        assert_eq!(decoded_owner, owner);
        assert_eq!(decoded_hash, hash);
        // The two decoders never cross-match: topic-0 keeps them apart.
        assert!(decode_conditional_order_created(&log).is_none());
        assert!(
            decode_conditional_order_removed(&make_log(owner, &sample_params(), at(1, 0)))
                .is_none()
        );
    }

    #[test]
    fn commitment_row_codec_round_trips() {
        for indexed_at in [None, Some(at(3, 7))] {
            let row = encode_row(indexed_at, b"payload");
            let (decoded_at, params) = decode_row(&row).expect("row decodes");
            assert_eq!(decoded_at, indexed_at);
            assert_eq!(params, b"payload");
        }
        assert!(decode_row(&[]).is_none(), "short row is malformed");
        let mut bad_tag = encode_row(None, b"payload");
        bad_tag[0] = 2;
        assert!(decode_row(&bad_tag).is_none(), "unknown tag is malformed");
    }

    #[test]
    fn removal_drops_commitment_and_gates_and_spares_the_rest() {
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let key = seed_commitment(&host, owner, &params);
        let (owner_hex, hash_hex) = parse_commitment_key(&key).unwrap();
        host.store
            .set(
                &format!("next_block:{owner_hex}:{hash_hex}"),
                &500u64.to_le_bytes(),
            )
            .unwrap();
        host.store
            .set(
                &format!("next_epoch:{owner_hex}:{hash_hex}"),
                &1_700_000_000u64.to_le_bytes(),
            )
            .unwrap();
        // A sibling commitment under a different hash must survive.
        let mut other = sample_params();
        other.salt = b256!("0202020202020202020202020202020202020202020202020202020202020202");
        let other_key = seed_commitment(&host, owner, &other);

        let hash = keccak256(params.abi_encode());
        on_event(&host, &make_removed_log(owner, hash, at(2, 0))).unwrap();

        let store = host.store.snapshot();
        assert!(
            !store.contains_key(&key),
            "removal must drop the commitment"
        );
        assert!(!store.contains_key(&format!("next_block:{owner_hex}:{hash_hex}")));
        assert!(!store.contains_key(&format!("next_epoch:{owner_hex}:{hash_hex}")));
        assert!(
            store.contains_key(&other_key),
            "sibling commitment survives"
        );
    }

    #[test]
    fn removal_of_an_unindexed_commitment_is_a_no_op() {
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let hash = keccak256(sample_params().abi_encode());

        on_event(&host, &make_removed_log(owner, hash, at(2, 0))).unwrap();

        assert_eq!(host.store.len(), 0);
    }

    #[test]
    fn later_removal_in_its_own_dispatch_drops_the_commitment() {
        // The runtime dispatches each log singly; a create and its
        // genuine removal always arrive as two separate calls.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let hash = keccak256(params.abi_encode());

        on_event(&host, &make_log(owner, &params, at(7, 5))).unwrap();
        on_event(&host, &make_removed_log(owner, hash, at(9, 0))).unwrap();

        assert_eq!(host.store.len(), 0, "a postdating removal lands");
    }

    #[test]
    fn same_block_later_removal_drops_the_commitment() {
        // A create and its removal can share a block, so ordering rests
        // on the log index alone. This is the later-index half of the
        // boundary; the stale test below pins the earlier-index half.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let hash = keccak256(params.abi_encode());

        on_event(&host, &make_log(owner, &params, at(7, 5))).unwrap();
        on_event(&host, &make_removed_log(owner, hash, at(7, 6))).unwrap();

        assert_eq!(
            host.store.len(),
            0,
            "a same-block removal at a later log index lands",
        );
    }

    #[test]
    fn stale_removal_arriving_after_a_re_registered_create_is_ignored() {
        // `remove(hash)` + `create(same params)` in one call
        // re-registers the same hash at a later log index. The two
        // event streams merge in arrival order, so the earlier
        // remove can arrive after the later create, each as its own
        // single-log dispatch. The live commitment must survive.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let hash = keccak256(params.abi_encode());
        let key = commitment_key(&owner, &hash);

        on_event(&host, &make_log(owner, &params, at(7, 5))).unwrap();
        on_event(&host, &make_removed_log(owner, hash, at(7, 4))).unwrap();

        assert!(
            host.store.snapshot().contains_key(&key),
            "a stale removal must not drop the re-registered commitment",
        );
    }

    #[test]
    fn redelivered_older_create_keeps_the_later_stamp() {
        // A cursor rewind can redeliver an old create after a newer
        // registration of the same params; the stamp must not age, or
        // the stale removal between them would land.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let hash = keccak256(params.abi_encode());
        let key = commitment_key(&owner, &hash);

        on_event(&host, &make_log(owner, &params, at(7, 5))).unwrap();
        on_event(&host, &make_log(owner, &params, at(2, 0))).unwrap();
        on_event(&host, &make_removed_log(owner, hash, at(7, 4))).unwrap();

        assert!(
            host.store.snapshot().contains_key(&key),
            "the stamp keeps the latest indexed position",
        );
    }

    #[test]
    fn removal_without_a_mined_position_keeps_the_commitment() {
        // A removal whose position cannot be proven later than the
        // create is ignored; the poll path's drop verdict is the
        // self-healing teardown for a truly removed order.
        let host = MockHost::new();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let hash = keccak256(params.abi_encode());
        let key = commitment_key(&owner, &hash);

        on_event(&host, &make_log(owner, &params, at(7, 5))).unwrap();
        let mut pending = make_removed_log(owner, hash, at(9, 0));
        pending.block_number = None;
        pending.log_index = None;
        on_event(&host, &pending).unwrap();

        assert!(host.store.snapshot().contains_key(&key));
    }

    #[test]
    fn poll_skips_when_next_block_gate_is_in_future() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = sample_params();
        let key = seed_commitment(&host, owner, &params);
        let (_, hash_hex) = parse_commitment_key(&key).unwrap();
        let owner_hex = format!("{owner:#x}");
        // Gate the commitment at block 500; poll at block 100.
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
            "gated commitment must not issue eth_call"
        );
        assert_eq!(venue.submit_count(), 0);
    }

    #[test]
    fn poll_ready_submits_the_intent_body_through_the_pool() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_commitment(&host, owner, &params);

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

    /// Guard: a repeated Ready tuple in consecutive ticks must not
    /// re-submit; the `submitted:{intent_id}` short-circuit in
    /// `submit_ready` prevents it.
    #[test]
    fn poll_ready_skips_submit_when_the_intent_id_is_already_journalled() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_commitment(&host, owner, &params);

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
            "the venue must NOT be touched when submitted:{{intent_id}} already exists",
        );
    }

    /// A Ready order's non-empty `appData` digest rides the intent body
    /// verbatim; assembly into the orderbook wire shape is the adapter's.
    #[test]
    fn poll_ready_carries_a_non_empty_app_data_digest_in_the_body() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        seed_commitment(&host, owner, &params);

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
        assert_eq!(submits.len(), 1, "exactly one venue submit");
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
        let commitment_key_str = seed_commitment(&host, owner, &params);

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

        // Commitment still present, no gate written, no submitted marker.
        assert!(host.store.snapshot().contains_key(&commitment_key_str));
        let (owner_hex, hash_hex) = parse_commitment_key(&commitment_key_str).unwrap();
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

    /// A rate-limited refusal backs the commitment off on the epoch clock
    /// instead of hot-looping the submit.
    #[test]
    fn submit_rate_limited_backs_off_on_the_epoch_gate() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let commitment_key_str = seed_commitment(&host, owner, &params);

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
            snapshot.contains_key(&commitment_key_str),
            "backoff must keep the commitment"
        );
        let (owner_hex, hash_hex) = parse_commitment_key(&commitment_key_str).unwrap();
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
    fn submit_denied_drops_commitment() {
        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let commitment_key_str = seed_commitment(&host, owner, &params);

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
            !store.contains_key(&commitment_key_str),
            "permanent refusal must drop the commitment"
        );
        let (owner_hex, hash_hex) = parse_commitment_key(&commitment_key_str).unwrap();
        assert!(!store.contains_key(&format!("next_block:{owner_hex}:{hash_hex}")));
        assert!(!store.contains_key(&format!("next_epoch:{owner_hex}:{hash_hex}")));
        assert!(!store.keys().any(|k| k.starts_with("submitted:")));
    }

    #[test]
    fn poll_invalid_drops_commitment_and_gates() {
        // When `LegacyRevertAdapter` produces `Invalid`, the lifecycle
        // layer must delete the commitment and any stale gates. Simulate the
        // wire shape the chain backend forwards: a `ChainError::Rpc`
        // carrying the already-decoded `OrderNotValid` revert bytes.
        use alloy_sol_types::SolError;
        use composable_cow::IConditionalOrder;
        use nexum_sdk::host::RpcError;

        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let commitment_key_str = seed_commitment(&host, owner, &params);
        let (owner_hex, hash_hex) = parse_commitment_key(&commitment_key_str).unwrap();
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

        assert!(!host.store.snapshot().contains_key(&commitment_key_str));
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
        // The drop line is `composable_cow::run`'s, so match on the key
        // and the verb rather than on its exact wording.
        logs.expect_one(|e| {
            e.message.contains("dropped") && e.message.contains(&commitment_key_str)
        });
    }

    #[test]
    fn removal_before_its_create_self_heals_via_the_first_poll() {
        // Streams merge in arrival order, and independent resume cursors
        // make a removal preceding its create routine. `SingleOrderNotAuthed()`
        // is unrecognised, so the drop rides `classify`'s fallback.
        use nexum_sdk::host::RpcError;

        let host = MockHost::new();
        let venue = MockVenue::default();
        let owner = address!("0011223344556677889900AABBCCDDEEFF001122");
        let params = sample_params();
        let hash = keccak256(params.abi_encode());
        let key = commitment_key(&owner, &hash);

        on_event(&host, &make_removed_log(owner, hash, at(2, 0))).unwrap();
        assert_eq!(
            host.store.len(),
            0,
            "removal of an unindexed commitment is a no-op"
        );

        on_event(&host, &make_log(owner, &params, at(1, 0))).unwrap();
        assert!(
            host.store.snapshot().contains_key(&key),
            "the create persists the commitment with no memory of the earlier removal",
        );

        let selector = keccak256(b"SingleOrderNotAuthed()")[..4].to_vec();
        host.chain.respond_to(
            "eth_call",
            programmed_eth_call_params(owner, &params),
            Err(ChainError::Rpc(RpcError {
                code: 3,
                message: "execution reverted".into(),
                data: Some(selector.into()),
            })),
        );

        dispatch(&host, &venue, sample_block(1_000)).unwrap();

        assert!(
            !host.store.snapshot().contains_key(&key),
            "the first poll against a chain-dead order must self-heal the stale commitment",
        );
        assert_eq!(venue.submit_count(), 0, "a dead order is never submitted");
    }

    /// The supervisor builds log filters from this manifest's event
    /// trigger `event_signature` pins, so a drift from a decoder topic-0
    /// subscribes to one topic and decodes another. Compares the two
    /// sets, so a missing or unhandled pin fails too.
    #[test]
    fn manifest_topics_match_the_decoder_signature_hashes() {
        let manifest: toml::Value =
            toml::from_str(include_str!("../component.toml")).expect("component.toml parses");
        let pinned: std::collections::BTreeSet<alloy_primitives::B256> = manifest["trigger"]
            .as_array()
            .expect("component.toml declares triggers")
            .iter()
            .filter(|trigger| trigger.get("on").and_then(toml::Value::as_str) == Some("event"))
            .map(|trigger| {
                trigger
                    .get("event_signature")
                    .and_then(toml::Value::as_str)
                    .expect("every event trigger pins an event_signature")
                    .parse()
                    .expect("event_signature is a b256")
            })
            .collect();
        let decoded = std::collections::BTreeSet::from([
            ConditionalOrderCreated::SIGNATURE_HASH,
            ConditionalOrderRemoved::SIGNATURE_HASH,
        ]);
        assert_eq!(
            pinned, decoded,
            "component.toml event topics and the sol! decoder topic-0s have diverged",
        );
    }
}
