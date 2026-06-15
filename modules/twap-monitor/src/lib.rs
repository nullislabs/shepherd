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
    COMPOSABLE_COW, ComposableCoW::ConditionalOrderCreated, ConditionalOrderParams, GPv2OrderData,
};
use nexum::host::{chain, local_store, logging, types};

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
        // BLEU-830 will persist next_block / next_epoch / remove the watch
        // based on `outcome`; BLEU-828 will submit on `Ready`.
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
}
