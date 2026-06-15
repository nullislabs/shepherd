// wit_bindgen::generate! expands to host-import shims whose arity matches
// the WIT signatures, which can exceed clippy's too-many-arguments threshold.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: ["../../wit/nexum-host", "../../wit/shepherd-cow"],
    world: "shepherd:cow/shepherd",
    generate_all,
});

use alloy_primitives::{Address, B256, keccak256};
use alloy_sol_types::{SolEvent, SolValue};
use cowprotocol::{ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams};
use nexum::host::{local_store, logging, types};

struct TwapMonitor;

impl Guest for TwapMonitor {
    fn init(_config: Vec<(String, String)>) -> Result<(), HostError> {
        logging::log(logging::Level::Info, "twap-monitor init");
        Ok(())
    }

    fn on_event(event: types::Event) -> Result<(), HostError> {
        if let types::Event::Logs(logs) = event {
            for log in &logs {
                if let Some((owner, params)) =
                    decode_conditional_order_created(&log.topics, &log.data)
                {
                    persist_watch(owner, &params)?;
                }
            }
        }
        // Event::Block (TWAP poll) lands in BLEU-827; Tick / Message are not
        // used by this module.
        Ok(())
    }
}

/// Decode a raw event log against `ComposableCoW.ConditionalOrderCreated`.
///
/// Returns `None` when topic0 does not match the event signature or the
/// payload fails ABI decoding — both are non-fatal for an indexer that
/// shares a subscription with adjacent events. Kept on plain slices so
/// the host-free unit tests under `#[cfg(test)]` can call it without
/// wit-bindgen scaffolding.
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

/// Persist a watch entry. `set` overwrites in place, so re-indexing the
/// same log (re-org replay, overlapping subscription windows) produces no
/// observable side effect — the idempotency the issue asks for.
fn persist_watch(owner: Address, params: &ConditionalOrderParams) -> Result<(), HostError> {
    let encoded = params.abi_encode();
    let params_hash = keccak256(&encoded);
    let key = format!("watch:{owner:#x}:{params_hash:#x}");
    local_store::set(&key, &encoded)?;
    logging::log(logging::Level::Info, &format!("indexed {key}"));
    Ok(())
}

export!(TwapMonitor);

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, hex};

    #[test]
    fn decodes_well_formed_log() {
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let params = ConditionalOrderParams {
            handler: address!("ffeeddccbbaa00998877665544332211ffeeddcc"),
            salt: b256!("0101010101010101010101010101010101010101010101010101010101010101"),
            staticInput: hex!("deadbeef").to_vec().into(),
        };
        // address indexed: 20-byte address left-padded to 32 bytes.
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
        let data = vec![];
        assert!(decode_conditional_order_created(&topics, &data).is_none());
    }

    #[test]
    fn rejects_empty_topics() {
        assert!(decode_conditional_order_created(&[], &[]).is_none());
    }
}
