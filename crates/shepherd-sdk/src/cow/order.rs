//! `GPv2OrderData` -> `OrderData` bridging.
//!
//! ComposableCoW and CoWSwapEthFlow both emit / return the 12-field
//! `GPv2OrderData` Solidity tuple, with `kind` / `sellTokenBalance` /
//! `buyTokenBalance` as 32-byte keccak markers. The orderbook signs
//! against the typed `OrderData` shape, with those markers projected
//! into Rust enums. [`gpv2_to_order_data`] is the bridge.

use alloy_primitives::Address;
use cowprotocol::{
    BuyTokenDestination, Chain, GPv2OrderData, OrderData, OrderKind, SellTokenSource,
};

/// Convert a freshly-polled / freshly-placed [`GPv2OrderData`] into the
/// typed [`OrderData`] shape `OrderCreation::new` expects.
///
/// The `kind`, `sellTokenBalance`, and `buyTokenBalance` fields ride
/// the wire as `bytes32` markers (the `keccak256` of the lowercase
/// variant name). This helper hands them off to cowprotocol's
/// `from_contract_bytes` classifiers and returns `None` when the on-
/// chain payload carries a marker the SDK doesn't recognise - the
/// caller skips the order rather than ship a malformed body.
///
/// `receiver = Address::ZERO` is normalised to `None`;
/// `OrderCreation::new` does the same downstream, but doing it here
/// keeps the EIP-712 hash inputs verbatim if a caller bypasses that
/// constructor later.
///
/// # Example
///
/// ```
/// use cowprotocol::{
///     BuyTokenDestination, GPv2OrderData, OrderKind, SellTokenSource,
/// };
/// use shepherd_sdk::cow::gpv2_to_order_data;
/// use nexum_sdk::prelude::{Address, U256};
///
/// let gpv2 = GPv2OrderData {
///     sellToken: Address::repeat_byte(1),
///     buyToken: Address::repeat_byte(2),
///     receiver: Address::ZERO, // normalised to None
///     sellAmount: U256::from(1_000u64),
///     buyAmount: U256::from(999u64),
///     validTo: u32::MAX,
///     appData: cowprotocol::EMPTY_APP_DATA_HASH,
///     feeAmount: U256::ZERO,
///     kind: OrderKind::SELL,
///     partiallyFillable: false,
///     sellTokenBalance: SellTokenSource::ERC20,
///     buyTokenBalance: BuyTokenDestination::ERC20,
/// };
///
/// let order = gpv2_to_order_data(&gpv2).expect("known markers");
/// assert_eq!(order.sell_amount, U256::from(1_000u64));
/// assert_eq!(order.receiver, None);
/// ```
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

/// Orderbook UID hex (`0x` + 112 hex chars) for the given on-chain
/// (order, owner, chain) tuple - the same value the orderbook derives
/// server-side from the signed payload, so a client can key
/// idempotency state before any network work.
///
/// `None` when the chain id has no settlement domain or the order
/// carries an unknown enum marker. Only the unknown-marker case also
/// stops the submit path downstream ([`gpv2_to_order_data`] fails the
/// same way there); an unsupported chain id does not, so a caller
/// keying idempotency on this value alone re-submits until `validTo`
/// on such a chain - bounded, but callers adding new chains should
/// teach `cowprotocol::Chain` about them first.
#[must_use]
pub fn order_uid_hex(chain_id: u64, order: &GPv2OrderData, owner: Address) -> Option<String> {
    let chain = Chain::try_from(chain_id).ok()?;
    let domain = chain.settlement_domain();
    let order_data = gpv2_to_order_data(order)?;
    Some(format!("{}", order_data.uid(&domain, owner)))
}

/// Project a typed [`OrderData`] into the venue wire
/// [`OrderBody`](cow_venue::OrderBody) a keeper emits. Total: every
/// typed field has exactly one wire form.
#[must_use]
pub fn order_data_to_body(order: &OrderData) -> cow_venue::OrderBody {
    cow_venue::OrderBody {
        sell_token: order.sell_token.into_array(),
        buy_token: order.buy_token.into_array(),
        receiver: order.receiver.map(Address::into_array),
        sell_amount: order.sell_amount.to_be_bytes(),
        buy_amount: order.buy_amount.to_be_bytes(),
        valid_to: order.valid_to,
        app_data: order.app_data.0,
        fee_amount: order.fee_amount.to_be_bytes(),
        kind: match order.kind {
            OrderKind::Sell => cow_venue::OrderKind::Sell,
            OrderKind::Buy => cow_venue::OrderKind::Buy,
        },
        partially_fillable: order.partially_fillable,
        sell_token_balance: match order.sell_token_balance {
            SellTokenSource::Erc20 => cow_venue::SellTokenSource::Erc20,
            SellTokenSource::External => cow_venue::SellTokenSource::External,
            SellTokenSource::Internal => cow_venue::SellTokenSource::Internal,
        },
        buy_token_balance: match order.buy_token_balance {
            BuyTokenDestination::Erc20 => cow_venue::BuyTokenDestination::Erc20,
            BuyTokenDestination::Internal => cow_venue::BuyTokenDestination::Internal,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{B256, U256, address};

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

    // ---- order_data_to_body ----

    #[test]
    fn order_data_to_body_projects_every_field() {
        let g = submittable_gpv2();
        let order = gpv2_to_order_data(&g).expect("known markers");
        let body = order_data_to_body(&order);
        assert_eq!(body.sell_token, g.sellToken.into_array());
        assert_eq!(body.buy_token, g.buyToken.into_array());
        assert_eq!(body.receiver, Some(g.receiver.into_array()));
        assert_eq!(body.sell_amount, g.sellAmount.to_be_bytes::<32>());
        assert_eq!(body.buy_amount, g.buyAmount.to_be_bytes::<32>());
        assert_eq!(body.valid_to, g.validTo);
        assert_eq!(body.app_data, g.appData.0);
        assert_eq!(body.fee_amount, g.feeAmount.to_be_bytes::<32>());
        assert_eq!(body.kind, cow_venue::OrderKind::Sell);
        assert!(!body.partially_fillable);
        assert_eq!(body.sell_token_balance, cow_venue::SellTokenSource::Erc20);
        assert_eq!(
            body.buy_token_balance,
            cow_venue::BuyTokenDestination::Erc20
        );
    }

    // ---- order_uid_hex ----

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
}
