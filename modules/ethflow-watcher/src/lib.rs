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
    CoWSwapOnchainOrders::OrderPlacement, ETH_FLOW_PRODUCTION, ETH_FLOW_STAGING, GPv2OrderData,
    OnchainSignature,
};
use nexum::host::{logging, types};

/// Fully decoded payload of a `CoWSwapOnchainOrders.OrderPlacement` log.
/// `GPv2OrderData` is ~300 bytes; box it so the struct stays cache-
/// friendly when it later lands in the BLEU-833 submission path.
#[derive(Debug)]
#[allow(dead_code)] // Fields consumed by BLEU-833.
struct DecodedPlacement {
    sender: Address,
    order: Box<GPv2OrderData>,
    signature: OnchainSignature,
    data: Bytes,
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
                if let Some(placement) = decode_order_placement(&log.address, &log.topics, &log.data)
                {
                    log_placement(&placement);
                    // BLEU-833 will build OrderCreation + submit + apply
                    // OrderPostError::retry_hint right here.
                }
            }
        }
        // Block / Tick / Message are not used by this module.
        Ok(())
    }
}

/// Decode a raw event log against `CoWSwapOnchainOrders.OrderPlacement`,
/// keeping the four fields the BLEU-833 submission path needs.
///
/// Returns `None` when:
/// - the log's contract address is not one of the canonical `ETH_FLOW_*`
///   deployments (defensive — the host's `[[subscription]]` filter
///   already pins the address, but a misconfigured engine could still
///   leak through);
/// - topic0 does not match the event signature; or
/// - the ABI body fails to decode (truncated, wrong layout).
///
/// Kept on plain slices so the host-free unit tests can call it without
/// wit-bindgen scaffolding.
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
        sender: decoded.sender,
        order: Box::new(decoded.order),
        signature: decoded.signature,
        data: decoded.data,
    })
}

fn log_placement(p: &DecodedPlacement) {
    logging::log(
        logging::Level::Info,
        &format!(
            "ethflow OrderPlacement sender={:#x} sell={:#x} buy={:#x} valid_to={} sig_scheme={:?}",
            p.sender,
            p.order.sellToken,
            p.order.buyToken,
            p.order.validTo,
            p.signature.scheme,
        ),
    );
}

export!(EthFlowWatcher);

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{U256, address, hex};
    use alloy_sol_types::SolValue;
    use cowprotocol::OnchainSigningScheme;

    fn sample_order() -> GPv2OrderData {
        GPv2OrderData {
            sellToken: address!("6810e776880C02933D47DB1b9fc05908e5386b96"),
            buyToken: address!("DAE5F1590db13E3B40423B5b5c5fbf175515910b"),
            receiver: address!("DeaDbeefdEAdbeefdEadbEEFdeadbeEFdEaDbeeF"),
            sellAmount: U256::from(1_000_000_u64),
            buyAmount: U256::from(999_u64),
            validTo: 1_700_000_000,
            appData: B256::repeat_byte(0xaa),
            feeAmount: U256::ZERO,
            kind: B256::repeat_byte(0xbb),
            partiallyFillable: false,
            sellTokenBalance: B256::repeat_byte(0xcc),
            buyTokenBalance: B256::repeat_byte(0xdd),
        }
    }

    fn sample_event() -> (Address, OrderPlacement) {
        let sender = address!("00112233445566778899aabbccddeeff00112233");
        let event = OrderPlacement {
            sender,
            order: sample_order(),
            signature: OnchainSignature {
                scheme: OnchainSigningScheme::Eip1271,
                data: hex!("c0ffeec0ffeec0ffee").to_vec().into(),
            },
            data: hex!("deadbeef").to_vec().into(),
        };
        (sender, event)
    }

    /// Build `(topics, data)` the way the EVM would emit them. The
    /// indexed `sender` becomes topic1 (left-padded address); the three
    /// non-indexed fields become the abi-encoded body.
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

    #[test]
    fn decodes_well_formed_placement() {
        let (sender, event) = sample_event();
        let (topics, data) = encode_log(&event);
        let address = ETH_FLOW_PRODUCTION.as_slice();

        let decoded = decode_order_placement(address, &topics, &data).expect("decode succeeds");
        assert_eq!(decoded.sender, sender);
        assert_eq!(decoded.order.sellToken, event.order.sellToken);
        assert_eq!(decoded.order.buyAmount, event.order.buyAmount);
        assert_eq!(decoded.signature.scheme, OnchainSigningScheme::Eip1271);
        assert_eq!(
            decoded.signature.data.as_ref(),
            event.signature.data.as_ref()
        );
        assert_eq!(decoded.data.as_ref(), event.data.as_ref());
    }

    #[test]
    fn accepts_staging_address() {
        let (_, event) = sample_event();
        let (topics, data) = encode_log(&event);
        assert!(decode_order_placement(ETH_FLOW_STAGING.as_slice(), &topics, &data).is_some());
    }

    #[test]
    fn rejects_unrelated_contract_address() {
        let (_, event) = sample_event();
        let (topics, data) = encode_log(&event);
        let stranger = address!("dead00000000000000000000000000000000dead");
        assert!(decode_order_placement(stranger.as_slice(), &topics, &data).is_none());
    }

    #[test]
    fn rejects_wrong_topic_signature() {
        let (_, event) = sample_event();
        let (_, data) = encode_log(&event);
        let bad_topic = vec![0xaa_u8; 32];
        let sender_topic = vec![0u8; 32];
        assert!(
            decode_order_placement(
                ETH_FLOW_PRODUCTION.as_slice(),
                &[bad_topic, sender_topic],
                &data,
            )
            .is_none()
        );
    }

    #[test]
    fn rejects_truncated_address() {
        let (_, event) = sample_event();
        let (topics, data) = encode_log(&event);
        assert!(decode_order_placement(&[0u8; 19], &topics, &data).is_none());
    }

    #[test]
    fn rejects_truncated_data() {
        let (topics, _) = encode_log(&sample_event().1);
        assert!(decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &topics, &[]).is_none());
    }

    #[test]
    fn rejects_empty_topics() {
        let (_, data) = encode_log(&sample_event().1);
        assert!(decode_order_placement(ETH_FLOW_PRODUCTION.as_slice(), &[], &data).is_none());
    }
}
