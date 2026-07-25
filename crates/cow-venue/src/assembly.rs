//! Chain-edge order assembly: projections between the on-chain
//! `GPv2OrderData` tuple, the typed `OrderData`, and the venue wire
//! [`OrderBody`], plus the orderbook submission bodies built from them.

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolCall;
use cowprotocol::{
    BuyTokenDestination, Chain, GPv2OrderData, GPv2Settlement, OrderCreation, OrderData, OrderKind,
    SellTokenSource, Signature,
};

use crate::order::OrderBody;

/// Project a polled or placed [`GPv2OrderData`] into the typed
/// [`OrderData`]. `None` when a `bytes32` enum marker (`kind`,
/// `sellTokenBalance`, `buyTokenBalance`) is unrecognised; the caller
/// skips the order. `receiver = Address::ZERO` normalises to `None`.
#[must_use]
pub fn gpv2_to_order_data(gpv2: &GPv2OrderData) -> Option<OrderData> {
    Some(OrderData {
        sell_token: gpv2.sellToken,
        buy_token: gpv2.buyToken,
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

/// Orderbook UID hex (`0x` + 112 hex chars) for the on-chain (order,
/// owner, chain) tuple, matching the server-side value so a client can
/// key idempotency before any network work.
///
/// `None` on an unsupported chain id or an unknown enum marker. Only
/// the unknown marker also stops the submit path; on an unsupported
/// chain a caller keying idempotency here alone re-submits until
/// `validTo`.
#[must_use]
pub fn order_uid_hex(chain_id: u64, order: &GPv2OrderData, owner: Address) -> Option<String> {
    let chain = Chain::try_from(chain_id).ok()?;
    let order_data = gpv2_to_order_data(order)?;
    Some(format!("{}", order_uid(chain, &order_data, owner)))
}

/// Canonical 56-byte orderbook UID for `order` under `chain`'s
/// settlement domain.
#[must_use]
pub fn order_uid(chain: Chain, order: &OrderData, owner: Address) -> cowprotocol::OrderUid {
    order.uid(&chain.settlement_domain(), owner)
}

/// Project a typed [`OrderData`] into the venue wire [`OrderBody`]. Total.
#[must_use]
pub fn order_data_to_body(order: &OrderData) -> OrderBody {
    OrderBody {
        sell_token: order.sell_token,
        buy_token: order.buy_token,
        receiver: order.receiver,
        sell_amount: order.sell_amount,
        buy_amount: order.buy_amount,
        valid_to: order.valid_to,
        app_data: order.app_data.0,
        fee_amount: order.fee_amount,
        kind: match order.kind {
            OrderKind::Sell => crate::order::OrderKind::Sell,
            OrderKind::Buy => crate::order::OrderKind::Buy,
        },
        partially_fillable: order.partially_fillable,
        sell_token_balance: match order.sell_token_balance {
            SellTokenSource::Erc20 => crate::order::SellTokenSource::Erc20,
            SellTokenSource::External => crate::order::SellTokenSource::External,
            SellTokenSource::Internal => crate::order::SellTokenSource::Internal,
        },
        buy_token_balance: match order.buy_token_balance {
            BuyTokenDestination::Erc20 => crate::order::BuyTokenDestination::Erc20,
            BuyTokenDestination::Internal => crate::order::BuyTokenDestination::Internal,
        },
    }
}

/// [`order_data_to_body`]'s total inverse.
#[must_use]
pub fn body_to_order_data(body: &OrderBody) -> OrderData {
    OrderData {
        sell_token: body.sell_token,
        buy_token: body.buy_token,
        receiver: body.receiver,
        sell_amount: body.sell_amount,
        buy_amount: body.buy_amount,
        valid_to: body.valid_to,
        app_data: body.app_data.into(),
        fee_amount: body.fee_amount,
        kind: match body.kind {
            crate::order::OrderKind::Sell => OrderKind::Sell,
            crate::order::OrderKind::Buy => OrderKind::Buy,
        },
        partially_fillable: body.partially_fillable,
        sell_token_balance: match body.sell_token_balance {
            crate::order::SellTokenSource::Erc20 => SellTokenSource::Erc20,
            crate::order::SellTokenSource::External => SellTokenSource::External,
            crate::order::SellTokenSource::Internal => SellTokenSource::Internal,
        },
        buy_token_balance: match body.buy_token_balance {
            crate::order::BuyTokenDestination::Erc20 => BuyTokenDestination::Erc20,
            crate::order::BuyTokenDestination::Internal => BuyTokenDestination::Internal,
        },
    }
}

/// Assemble the orderbook `OrderCreation` for a polled order: hash-only
/// `appData` wire shape, EIP-1271 signature (the conditional-order
/// contract is the verifier). `Err` is a client-side precondition
/// failure that recurs on retry; the caller drops the watch.
pub fn build_order_creation(
    order_data: &OrderData,
    signature: &[u8],
    from: Address,
) -> Result<OrderCreation, cowprotocol::Error> {
    let signature = Signature::Eip1271(signature.to_vec());
    OrderCreation::new_app_data_hash_only(order_data, signature, from, None)
}

/// Assemble the pre-sign `OrderCreation`: held signature-pending until
/// `from` settles authorisation via [`set_pre_signature_calldata`].
pub fn build_presign_creation(
    order_data: &OrderData,
    from: Address,
) -> Result<OrderCreation, cowprotocol::Error> {
    OrderCreation::new_app_data_hash_only(order_data, Signature::PreSign, from, None)
}

/// ABI-encoded `setPreSignature(uid, true)` calldata to activate a
/// pre-sign order.
#[must_use]
pub fn set_pre_signature_calldata(uid: &cowprotocol::OrderUid) -> Vec<u8> {
    GPv2Settlement::setPreSignatureCall {
        orderUid: Bytes::copy_from_slice(uid.as_slice()),
        signed: true,
    }
    .abi_encode()
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, U256, address, keccak256};
    use cowprotocol::GPV2_SETTLEMENT;

    use super::*;

    fn submittable_gpv2() -> GPv2OrderData {
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

    #[test]
    fn happy_path_round_trips_markers() {
        let g = submittable_gpv2();
        let od = gpv2_to_order_data(&g).expect("known markers");
        assert_eq!(od.sell_token, g.sellToken);
        assert_eq!(od.buy_token, g.buyToken);
        assert_eq!(od.kind, OrderKind::Sell);
        assert_eq!(od.sell_token_balance, SellTokenSource::Erc20);
        assert_eq!(od.buy_token_balance, BuyTokenDestination::Erc20);
    }

    #[test]
    fn zero_receiver_normalises_to_none() {
        let mut g = submittable_gpv2();
        g.receiver = Address::ZERO;
        assert_eq!(gpv2_to_order_data(&g).unwrap().receiver, None);
    }

    #[test]
    fn non_zero_receiver_preserved() {
        let g = submittable_gpv2();
        assert_eq!(gpv2_to_order_data(&g).unwrap().receiver, Some(g.receiver));
    }

    #[test]
    fn unknown_kind_marker_returns_none() {
        let mut g = submittable_gpv2();
        g.kind = B256::repeat_byte(0x42);
        assert!(gpv2_to_order_data(&g).is_none());
    }

    #[test]
    fn unknown_sell_token_balance_returns_none() {
        let mut g = submittable_gpv2();
        g.sellTokenBalance = B256::repeat_byte(0x99);
        assert!(gpv2_to_order_data(&g).is_none());
    }

    #[test]
    fn unknown_buy_token_balance_returns_none() {
        let mut g = submittable_gpv2();
        g.buyTokenBalance = B256::repeat_byte(0x55);
        assert!(gpv2_to_order_data(&g).is_none());
    }

    #[test]
    fn order_data_to_body_projects_every_field() {
        let g = submittable_gpv2();
        let order = gpv2_to_order_data(&g).expect("known markers");
        let body = order_data_to_body(&order);
        assert_eq!(body.sell_token, g.sellToken);
        assert_eq!(body.buy_token, g.buyToken);
        assert_eq!(body.receiver, Some(g.receiver));
        assert_eq!(body.sell_amount, g.sellAmount);
        assert_eq!(body.buy_amount, g.buyAmount);
        assert_eq!(body.valid_to, g.validTo);
        assert_eq!(body.app_data, g.appData.0);
        assert_eq!(body.fee_amount, g.feeAmount);
        assert_eq!(body.kind, crate::order::OrderKind::Sell);
        assert!(!body.partially_fillable);
        assert_eq!(
            body.sell_token_balance,
            crate::order::SellTokenSource::Erc20
        );
        assert_eq!(
            body.buy_token_balance,
            crate::order::BuyTokenDestination::Erc20
        );
    }

    #[test]
    fn body_round_trips_back_to_order_data() {
        let order = gpv2_to_order_data(&submittable_gpv2()).expect("known markers");
        assert_eq!(body_to_order_data(&order_data_to_body(&order)), order);

        for (kind, sell, buy) in [
            (
                OrderKind::Buy,
                SellTokenSource::External,
                BuyTokenDestination::Internal,
            ),
            (
                OrderKind::Sell,
                SellTokenSource::Internal,
                BuyTokenDestination::Erc20,
            ),
        ] {
            let mut varied = order;
            varied.kind = kind;
            varied.sell_token_balance = sell;
            varied.buy_token_balance = buy;
            varied.receiver = None;
            assert_eq!(body_to_order_data(&order_data_to_body(&varied)), varied);
        }
    }

    const SEPOLIA: u64 = 11_155_111;

    #[test]
    fn uid_hex_is_deterministic_and_canonical_shape() {
        let g = submittable_gpv2();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let uid = order_uid_hex(SEPOLIA, &g, owner).expect("supported chain, known markers");
        // 56 bytes: 32 digest + 20 owner + 4 validTo.
        assert_eq!(uid.len(), 2 + 112);
        assert!(uid.starts_with("0x"));
        assert!(
            uid.to_lowercase()
                .contains("00112233445566778899aabbccddeeff00112233",)
        );
        assert_eq!(order_uid_hex(SEPOLIA, &g, owner).unwrap(), uid);
    }

    #[test]
    fn uid_hex_none_on_unsupported_chain_or_unknown_marker() {
        let g = submittable_gpv2();
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        assert!(order_uid_hex(u64::MAX, &g, owner).is_none());

        let mut bad = submittable_gpv2();
        bad.kind = B256::repeat_byte(0x42);
        assert!(order_uid_hex(SEPOLIA, &bad, owner).is_none());
    }

    #[test]
    fn presign_creation_carries_the_presign_scheme() {
        let order = gpv2_to_order_data(&submittable_gpv2()).expect("known markers");
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let creation = build_presign_creation(&order, owner).expect("valid order");
        assert_eq!(creation.signature, Signature::PreSign);
        assert_eq!(creation.from, owner);

        assert!(build_presign_creation(&order, Address::ZERO).is_err());
    }

    #[test]
    fn set_pre_signature_calldata_encodes_the_selector_and_uid() {
        let order = gpv2_to_order_data(&submittable_gpv2()).expect("known markers");
        let owner = address!("00112233445566778899aabbccddeeff00112233");
        let uid = order_uid(Chain::Mainnet, &order, owner);
        let data = set_pre_signature_calldata(&uid);
        assert_eq!(&data[..4], &keccak256("setPreSignature(bytes,bool)")[..4]);
        assert!(data.windows(56).any(|w| w == uid.as_slice()));
        // The call targets the deterministic settlement deployment.
        assert_ne!(GPV2_SETTLEMENT, Address::ZERO);
    }
}
