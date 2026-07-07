//! The venue-neutral CoW order body.
//!
//! On the wire a CoW order is the 12-field `GPv2Order` tuple. The
//! host-side path speaks it through the on-chain alloy types; this body
//! type is the same shape reduced to plain wire primitives (byte arrays
//! for addresses and 256-bit amounts, small enums for the balance and
//! kind markers) so it borsh-encodes and links without the on-chain
//! stack. The one non-obvious invariant: `amount`, `receiver`, and the
//! marker enums are canonical wire forms, not on-chain keccak markers,
//! so the adapter, not this type, owns the projection to and from chain.

use borsh::{BorshDeserialize, BorshSerialize};

/// A 20-byte EVM address in wire form.
pub type Address = [u8; 20];

/// A 256-bit amount as its 32-byte big-endian representation.
pub type U256 = [u8; 32];

/// Which side of the trade is fixed.
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderKind {
    /// Sell a fixed `sell_amount`; `buy_amount` is the limit.
    Sell,
    /// Buy a fixed `buy_amount`; `sell_amount` is the limit.
    Buy,
}

/// Where the settlement pulls the sell token from.
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SellTokenSource {
    /// Ordinary ERC-20 `transferFrom`.
    Erc20,
    /// Balancer external balance.
    External,
    /// Balancer internal balance.
    Internal,
}

/// Where the settlement delivers the buy token.
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuyTokenDestination {
    /// Ordinary ERC-20 transfer.
    Erc20,
    /// Balancer internal balance.
    Internal,
}

/// The venue-neutral order body: the `GPv2Order` fields in wire form.
///
/// `receiver` is `None` for the self-receive default the orderbook
/// normalizes the zero address to; the adapter round-trips that
/// normalization on the chain edge.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct OrderBody {
    /// Token the owner sells.
    pub sell_token: Address,
    /// Token the owner buys.
    pub buy_token: Address,
    /// Recipient of the buy token; `None` sends it back to the owner.
    pub receiver: Option<Address>,
    /// Sell amount, or its limit when `kind` is `Buy`.
    pub sell_amount: U256,
    /// Buy amount, or its limit when `kind` is `Sell`.
    pub buy_amount: U256,
    /// Unix-seconds expiry.
    pub valid_to: u32,
    /// The 32-byte on-chain app-data hash.
    pub app_data: [u8; 32],
    /// Fee amount taken in the sell token.
    pub fee_amount: U256,
    /// Which side is fixed.
    pub kind: OrderKind,
    /// Whether the order may partially fill.
    pub partially_fillable: bool,
    /// Where the sell token is sourced from.
    pub sell_token_balance: SellTokenSource,
    /// Where the buy token is delivered to.
    pub buy_token_balance: BuyTokenDestination,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OrderBody {
        OrderBody {
            sell_token: [0x11; 20],
            buy_token: [0x22; 20],
            receiver: Some([0x33; 20]),
            sell_amount: {
                let mut a = [0u8; 32];
                a[31] = 0x2a;
                a
            },
            buy_amount: [0xff; 32],
            valid_to: 0xffff_ffff,
            app_data: [0x44; 32],
            fee_amount: [0u8; 32],
            kind: OrderKind::Sell,
            partially_fillable: false,
            sell_token_balance: SellTokenSource::Erc20,
            buy_token_balance: BuyTokenDestination::Erc20,
        }
    }

    #[test]
    fn order_body_borsh_round_trips() {
        let body = sample();
        let bytes = borsh::to_vec(&body).expect("encode");
        assert_eq!(OrderBody::try_from_slice(&bytes).expect("decode"), body);
    }

    #[test]
    fn none_receiver_round_trips() {
        let mut body = sample();
        body.receiver = None;
        let bytes = borsh::to_vec(&body).expect("encode");
        assert_eq!(OrderBody::try_from_slice(&bytes).unwrap().receiver, None);
    }

    #[test]
    fn marker_enums_round_trip() {
        for kind in [OrderKind::Sell, OrderKind::Buy] {
            for sell in [
                SellTokenSource::Erc20,
                SellTokenSource::External,
                SellTokenSource::Internal,
            ] {
                for buy in [BuyTokenDestination::Erc20, BuyTokenDestination::Internal] {
                    let mut body = sample();
                    body.kind = kind;
                    body.sell_token_balance = sell;
                    body.buy_token_balance = buy;
                    let bytes = borsh::to_vec(&body).unwrap();
                    assert_eq!(OrderBody::try_from_slice(&bytes).unwrap(), body);
                }
            }
        }
    }
}
