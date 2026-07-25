//! Pure strategy logic for the ethflow-watcher module.
//!
//! Observes EthFlow placements through the venue registry: decode the
//! `OrderPlacement` log, compute the orderbook UID (the receipt at the
//! cow venue), and `observe` it. The registry polls the cow adapter's
//! `status` and fans transitions back as `intent-status` events; the
//! first records `observed:{uid}` in the journal so log re-delivery
//! no-ops. A refused observe stays unjournalled, so re-delivery
//! retries. World access flows through the `nexum_sdk::host` traits and
//! the typed [`CowClient`] over [`VenueTransport`].

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolEvent;
use cow_venue::assembly;
use cow_venue::client::{CowClient, CowVenue};
use cowprotocol::{
    Chain, CoWSwapOnchainOrders::OrderPlacement, ETH_FLOW_PRODUCTION, ETH_FLOW_STAGING,
    GPv2OrderData, OnchainSignature, OrderUid,
};
use nexum_sdk::events::Log;
use nexum_sdk::host::{Fault, LocalStoreHost};
use nexum_sdk::keeper::Journal;
use videre_sdk::client::{Venue, VenueTransport};
use videre_sdk::status_body::StatusBody;

/// Decoded payload of a `CoWSwapOnchainOrders.OrderPlacement` log.
#[derive(Debug)]
pub(crate) struct DecodedPlacement {
    /// EthFlow contract that emitted the event; the EIP-1271 owner and
    /// the UID `owner` input.
    pub(crate) contract: Address,
    /// Original native-token seller; not the orderbook owner.
    pub(crate) sender: Address,
    pub(crate) order: Box<GPv2OrderData>,
    /// Decoded signature; not consumed by the observe path.
    #[allow(dead_code)]
    pub(crate) signature: OnchainSignature,
    /// Opaque placer metadata from the event; kept for decoder parity.
    #[allow(dead_code)]
    pub(crate) data: Bytes,
}

/// Decode every `OrderPlacement` log in a dispatch batch and put each
/// placement's UID under the host's status watch.
pub async fn on_chain_logs<H: LocalStoreHost, T: VenueTransport>(
    host: &H,
    venue: &CowClient<T>,
    chain_id: u64,
    logs: &[Log],
) -> Result<(), Fault> {
    for log in logs {
        if let Some(placement) = decode_order_placement(log) {
            observe_placement(host, venue, chain_id, &placement).await?;
        }
    }
    Ok(())
}

/// A registry status transition for a watched receipt. Foreign venues
/// are ignored; the first cow transition records `observed:{uid}`, and
/// every transition is logged.
pub fn on_intent_status<H: LocalStoreHost>(
    host: &H,
    venue: &str,
    receipt: &[u8],
    status: &[u8],
) -> Result<(), Fault> {
    if venue != CowVenue::ID.as_str() {
        return Ok(());
    }
    let Ok(uid) = OrderUid::try_from(receipt) else {
        tracing::warn!(
            "ethflow status update with a non-uid receipt ({} bytes)",
            receipt.len(),
        );
        return Ok(());
    };
    let body = StatusBody::decode(status).map_err(|e| Fault::InvalidInput(e.to_string()))?;
    let uid_hex = format!("{uid}");
    let journal = Journal::observed(host);
    if !journal.contains(&uid_hex)? {
        journal.record(&uid_hex)?;
    }
    tracing::info!("ethflow observed {uid_hex}: {:?}", body.status);
    Ok(())
}

// ---- decode ----

/// Decode a raw event log against `CoWSwapOnchainOrders.OrderPlacement`.
/// `None` when the contract address is neither `ETH_FLOW_PRODUCTION`
/// nor `ETH_FLOW_STAGING`, topic-0 misses the `shepherd:cow/cow-events`
/// pin, or the ABI body fails to decode.
pub(crate) fn decode_order_placement(log: &Log) -> Option<DecodedPlacement> {
    let contract = log.address();
    if contract != ETH_FLOW_PRODUCTION && contract != ETH_FLOW_STAGING {
        return None;
    }
    if log.topics().first() != Some(&OrderPlacement::SIGNATURE_HASH) {
        return None;
    }
    let decoded = OrderPlacement::decode_log(&log.inner).ok()?;
    Some(DecodedPlacement {
        contract,
        sender: decoded.data.sender,
        order: Box::new(decoded.data.order),
        signature: decoded.data.signature,
        data: decoded.data.data,
    })
}

// ---- observe + verify (venue registry) ----

/// Compute the orderbook UID and put it under the host's status watch.
/// A refused observe stays unjournalled, so re-delivery retries.
async fn observe_placement<H: LocalStoreHost, T: VenueTransport>(
    host: &H,
    venue: &CowClient<T>,
    chain_id: u64,
    placement: &DecodedPlacement,
) -> Result<(), Fault> {
    let Some(uid) = compute_uid(chain_id, placement) else {
        tracing::warn!(
            "ethflow uid build skipped (sender={:#x}): unsupported chain {chain_id} or unknown order marker",
            placement.sender,
        );
        return Ok(());
    };
    let uid_hex = format!("{uid}");

    // Idempotency: once observed, do not re-watch on log re-delivery
    // (engine restart, reorg replay, supervisor restart).
    let journal = Journal::observed(host);
    if journal.contains(&uid_hex)? {
        return Ok(());
    }

    match venue.observe(uid.as_slice()).await {
        Ok(()) => {
            tracing::info!(
                "ethflow watching {uid_hex} (sender={:#x})",
                placement.sender,
            );
        }
        Err(err) => {
            tracing::warn!(
                "ethflow watch failed {uid_hex}: {err} (sender={:#x})",
                placement.sender,
            );
        }
    }
    Ok(())
}

/// Canonical 56-byte orderbook UID: `digest || owner || valid_to`,
/// where owner is the EthFlow contract (EIP-1271 signer), not the
/// sender.
fn compute_uid(chain_id: u64, placement: &DecodedPlacement) -> Option<OrderUid> {
    let chain = Chain::try_from(chain_id).ok()?;
    let order = assembly::gpv2_to_order_data(&placement.order)?;
    Some(assembly::order_uid(chain, &order, placement.contract))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use alloy_primitives::{U256, address, hex};
    use alloy_sol_types::SolValue;
    use cowprotocol::{BuyTokenDestination, OnchainSigningScheme, OrderKind, SellTokenSource};
    use nexum_sdk::Level;
    use nexum_sdk::host::LocalStoreHost as _;
    use nexum_sdk_test::{MockHost, capture_tracing};
    use videre_sdk::client::VenueId;
    use videre_sdk::client::poll_once;
    use videre_sdk::status_body::IntentStatus as Lifecycle;
    use videre_sdk::{IntentStatus, Quotation, SubmitOutcome, VenueFault};

    use super::*;

    const SEPOLIA: u64 = 11_155_111;

    /// One recorded transport call; `observe` also records venue and
    /// receipt.
    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Quote,
        Submit,
        Observe(String, Vec<u8>),
        Status,
        Cancel,
    }

    /// Records every call; `observe` pops a scripted response,
    /// defaulting to accepted once the script drains. The other verbs
    /// refuse. Cloneable over shared state so the test keeps a handle.
    #[derive(Clone, Default)]
    struct SpyVenues {
        calls: Rc<RefCell<Vec<Call>>>,
        observe_script: Rc<RefCell<VecDeque<Result<(), VenueFault>>>>,
    }

    impl SpyVenues {
        fn script_observe(&self, result: Result<(), VenueFault>) {
            self.observe_script.borrow_mut().push_back(result);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }

        fn observe_count(&self) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|c| matches!(c, Call::Observe(..)))
                .count()
        }
    }

    impl videre_sdk::client::sealed::SealedTransport for SpyVenues {}

    impl VenueTransport for SpyVenues {
        async fn quote(&self, _venue: &VenueId, _body: Vec<u8>) -> Result<Quotation, VenueFault> {
            self.calls.borrow_mut().push(Call::Quote);
            Err(VenueFault::Unsupported)
        }

        async fn submit(
            &self,
            _venue: &VenueId,
            _body: Vec<u8>,
        ) -> Result<SubmitOutcome, VenueFault> {
            self.calls.borrow_mut().push(Call::Submit);
            Err(VenueFault::Unsupported)
        }

        async fn observe(&self, venue: &VenueId, receipt: &[u8]) -> Result<(), VenueFault> {
            self.calls
                .borrow_mut()
                .push(Call::Observe(venue.to_string(), receipt.to_vec()));
            self.observe_script
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(()))
        }

        async fn status(
            &self,
            _venue: &VenueId,
            _receipt: &[u8],
        ) -> Result<IntentStatus, VenueFault> {
            self.calls.borrow_mut().push(Call::Status);
            Err(VenueFault::Unsupported)
        }

        async fn cancel(&self, _venue: &VenueId, _receipt: &[u8]) -> Result<(), VenueFault> {
            self.calls.borrow_mut().push(Call::Cancel);
            Err(VenueFault::Unsupported)
        }
    }

    /// Drive the async strategy on the synchronous test boundary.
    fn run_logs(
        host: &MockHost,
        spy: &SpyVenues,
        chain_id: u64,
        logs: &[Log],
    ) -> Result<(), Fault> {
        let client = CowClient::with_transport(spy.clone());
        match poll_once(on_chain_logs(host, &client, chain_id, logs)) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("guest futures complete in one poll"),
        }
    }

    fn open_status() -> Vec<u8> {
        StatusBody {
            status: Lifecycle::Open,
            proof: None,
            reason: None,
        }
        .encode()
        .expect("status body encodes")
    }

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

    /// The alloy log a placement decodes from.
    fn make_log(address_bytes: &[u8], topics: &[Vec<u8>], data: &[u8]) -> Log {
        nexum_sdk::events::ChainLogParts {
            address: address_bytes,
            topics,
            data,
            ..Default::default()
        }
        .into()
    }

    fn sample_log() -> Log {
        let (topics, data) = encode_log(&sample_event());
        make_log(ETH_FLOW_PRODUCTION.as_slice(), &topics, &data)
    }

    fn sample_uid() -> OrderUid {
        let placement = decode_order_placement(&sample_log()).expect("decode succeeds");
        compute_uid(SEPOLIA, &placement).expect("sepolia + canonical markers")
    }

    // ---- decode (invariants preserved) ----

    #[test]
    fn decodes_well_formed_placement() {
        let event = sample_event();
        let decoded = decode_order_placement(&sample_log()).expect("decode succeeds");
        assert_eq!(decoded.contract, ETH_FLOW_PRODUCTION);
        assert_eq!(decoded.sender, event.sender);
        assert_eq!(decoded.signature.scheme, OnchainSigningScheme::Eip1271);
    }

    #[test]
    fn rejects_unrelated_contract_address() {
        let event = sample_event();
        let (topics, data) = encode_log(&event);
        let stranger = address!("dead00000000000000000000000000000000dead");
        let log = make_log(stranger.as_slice(), &topics, &data);
        assert!(decode_order_placement(&log).is_none());
    }

    #[test]
    fn rejects_wrong_topic_signature() {
        let event = sample_event();
        let (_, data) = encode_log(&event);
        let bad_topic = vec![0xaa_u8; 32];
        let sender_topic = vec![0u8; 32];
        let log = make_log(
            ETH_FLOW_PRODUCTION.as_slice(),
            &[bad_topic, sender_topic],
            &data,
        );
        assert!(decode_order_placement(&log).is_none());
    }

    // ---- UID computation ----

    #[test]
    fn compute_uid_pins_owner_to_ethflow_contract_and_validto() {
        let event = sample_event();
        let uid = sample_uid();
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
        let decoded = decode_order_placement(&sample_log()).unwrap();
        assert!(compute_uid(9999, &decoded).is_none());
    }

    // ---- observe via the venue registry (transport integration) ----

    /// A placement registers one cow status watch keyed on the computed
    /// UID, and journals nothing until the first status transition.
    #[test]
    fn placement_log_registers_the_uid_watch() {
        let host = MockHost::new();
        let spy = SpyVenues::default();
        let uid = sample_uid();

        run_logs(&host, &spy, SEPOLIA, &[sample_log()]).unwrap();

        assert_eq!(
            spy.calls(),
            vec![Call::Observe("cow".to_owned(), uid.as_slice().to_vec())],
            "exactly one observe, nothing else",
        );
        assert!(
            host.store.snapshot().is_empty(),
            "observed:{{uid}} waits for the first status transition",
        );
    }

    /// A refused observe warns, journals nothing, and re-delivery
    /// retries.
    #[test]
    fn watch_refusal_warns_and_redelivery_retries() {
        let host = MockHost::new();
        let spy = SpyVenues::default();
        spy.script_observe(Err(VenueFault::Unavailable("venue down".to_owned())));

        let (result, logs) = capture_tracing(|| run_logs(&host, &spy, SEPOLIA, &[sample_log()]));
        result.unwrap();

        assert!(host.store.snapshot().is_empty());
        logs.expect_one(|e| e.level == Level::WARN && e.message.contains("watch failed"));

        // Unjournalled, so the re-delivered log observes again.
        run_logs(&host, &spy, SEPOLIA, &[sample_log()]).unwrap();
        assert_eq!(spy.observe_count(), 2);
    }

    /// A placement already carrying `observed:{uid}` does not touch the
    /// venue on re-delivery.
    #[test]
    fn previously_observed_placement_is_skipped_on_redelivery() {
        let host = MockHost::new();
        let spy = SpyVenues::default();
        let uid = sample_uid();

        host.store
            .set(&format!("observed:{uid}"), b"")
            .expect("seed observed marker");

        run_logs(&host, &spy, SEPOLIA, &[sample_log()]).unwrap();

        assert!(
            spy.calls().is_empty(),
            "observed:{{uid}} must short-circuit before the venue call",
        );
    }

    /// An unsupported chain id warns without panicking or touching the
    /// venue.
    #[test]
    fn unsupported_chain_logs_warn_without_venue_call() {
        let host = MockHost::new();
        let spy = SpyVenues::default();

        // 9999 is not in cowprotocol::Chain.
        let (result, logs) = capture_tracing(|| run_logs(&host, &spy, 9999, &[sample_log()]));
        result.unwrap();

        assert!(spy.calls().is_empty());
        assert!(host.store.snapshot().is_empty());
        logs.expect_one(|e| {
            e.level == Level::WARN && e.message.contains("ethflow uid build skipped")
        });
    }

    /// Observer-only: no call path reaches quote, submit, status, or
    /// cancel.
    #[test]
    fn strategy_never_submits() {
        let host = MockHost::new();
        let spy = SpyVenues::default();

        run_logs(&host, &spy, SEPOLIA, &[sample_log()]).unwrap();

        assert!(
            spy.calls().iter().all(|c| matches!(c, Call::Observe(..))),
            "observe is the only verb the strategy may use",
        );
    }

    // ---- intent-status transitions ----

    /// The first cow transition journals `observed:{uid}` and logs it.
    #[test]
    fn status_update_journals_the_observed_marker() {
        let host = MockHost::new();
        let uid = sample_uid();

        let (result, logs) =
            capture_tracing(|| on_intent_status(&host, "cow", uid.as_slice(), &open_status()));
        result.unwrap();

        assert!(
            host.store
                .snapshot()
                .contains_key(&format!("observed:{uid}")),
            "the first transition must write observed:{{uid}}",
        );
        let ev = logs.expect_one(|e| e.message.contains("ethflow observed"));
        assert_eq!(ev.level, Level::INFO);
    }

    /// Later transitions keep the single marker and stay Ok.
    #[test]
    fn repeated_transitions_keep_one_marker() {
        let host = MockHost::new();
        let uid = sample_uid();

        on_intent_status(&host, "cow", uid.as_slice(), &open_status()).unwrap();
        let fulfilled = StatusBody {
            status: Lifecycle::Fulfilled,
            proof: None,
            reason: None,
        }
        .encode()
        .expect("status body encodes");
        on_intent_status(&host, "cow", uid.as_slice(), &fulfilled).unwrap();

        assert_eq!(host.store.snapshot().len(), 1);
    }

    /// A transition from a foreign venue is not ethflow's: ignored.
    #[test]
    fn foreign_venue_status_update_is_ignored() {
        let host = MockHost::new();
        on_intent_status(&host, "echo-venue", sample_uid().as_slice(), &open_status()).unwrap();
        assert!(host.store.snapshot().is_empty());
    }

    /// A cow receipt that is not a 56-byte UID warns, no marker.
    #[test]
    fn non_uid_receipt_warns_without_marker() {
        let host = MockHost::new();
        let (result, logs) =
            capture_tracing(|| on_intent_status(&host, "cow", b"abc", &open_status()));
        result.unwrap();
        assert!(host.store.snapshot().is_empty());
        logs.expect_one(|e| e.level == Level::WARN && e.message.contains("non-uid receipt"));
    }

    /// An undecodable status body is a typed fault, never a marker.
    #[test]
    fn malformed_status_body_is_a_typed_fault() {
        let host = MockHost::new();
        let err = on_intent_status(&host, "cow", sample_uid().as_slice(), &[0xFF, 0x00])
            .expect_err("undecodable status body");
        assert!(matches!(err, Fault::InvalidInput(_)));
        assert!(host.store.snapshot().is_empty());
    }

    // ---- package-of-record parity ----

    /// The `sol!` decoder's topic-0 matches the
    /// `shepherd:cow/cow-events` pin; a drift would silently miss every
    /// EthFlow event.
    #[test]
    fn topic0_matches_the_cow_events_package_of_record() {
        let wit = include_str!("../../../wit/shepherd-cow/cow-events.wit");
        let expected = format!("{:#x}", OrderPlacement::SIGNATURE_HASH);
        assert!(
            wit.contains(&expected),
            "sol! topic-0 must match the shepherd:cow/cow-events pin ({expected})",
        );
    }

    /// The shipped `module.toml` `event_signature` equals the decoder
    /// topic-0; catches a manifest/code drift the wit assertion cannot.
    #[test]
    fn manifest_topic0_matches_order_placement_signature_hash() {
        let manifest = include_str!("../module.toml");
        let expected = format!("{:#x}", OrderPlacement::SIGNATURE_HASH);
        assert!(
            manifest.contains(&expected),
            "module.toml event_signature must equal the decoder topic-0 ({expected})",
        );
    }
}
