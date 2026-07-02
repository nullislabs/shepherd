//! `GPv2OrderData` -> `OrderData` bridging.
//!
//! ComposableCoW and CoWSwapEthFlow both emit / return the 12-field
//! `GPv2OrderData` Solidity tuple, with `kind` / `sellTokenBalance` /
//! `buyTokenBalance` as 32-byte keccak markers. The orderbook signs
//! against the typed `OrderData` shape, with those markers projected
//! into Rust enums. [`gpv2_to_order_data`] is the bridge.

use alloy_primitives::Address;
use cowprotocol::{BuyTokenDestination, GPv2OrderData, OrderData, OrderKind, SellTokenSource};

/// Convert a freshly-polled / freshly-placed [`GPv2OrderData`] into the
/// typed [`OrderData`] shape `OrderCreation::from_signed_order_data`
/// expects.
///
/// The `kind`, `sellTokenBalance`, and `buyTokenBalance` fields ride
/// the wire as `bytes32` markers (the `keccak256` of the lowercase
/// variant name). This helper hands them off to cowprotocol's
/// `from_contract_bytes` classifiers and returns `None` when the on-
/// chain payload carries a marker the SDK doesn't recognise - the
/// caller skips the order rather than ship a malformed body.
///
/// `receiver = Address::ZERO` is normalised to `None`; `OrderCreation::
/// from_signed_order_data` does the same downstream, but doing it here
/// keeps the EIP-712 hash inputs verbatim if a caller bypasses that
/// helper later.
///
/// # Example
///
/// ```
/// use cowprotocol::{
///     BuyTokenDestination, GPv2OrderData, OrderKind, SellTokenSource,
/// };
/// use shepherd_sdk::cow::gpv2_to_order_data;
/// use shepherd_sdk::prelude::{Address, U256};
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
}
