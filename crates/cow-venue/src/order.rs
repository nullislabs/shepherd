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

use core::marker::PhantomData;

use borsh::{BorshDeserialize, BorshSerialize};

/// A 20-byte EVM address in wire form.
pub type Address = [u8; 20];

/// A 256-bit amount as its 32-byte big-endian representation.
pub type U256 = [u8; 32];

/// The token an order sells, typed so a builder call cannot swap
/// sides with the buy token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellToken(pub Address);

impl From<Address> for SellToken {
    fn from(address: Address) -> Self {
        Self(address)
    }
}

/// The token an order buys, typed so a builder call cannot swap sides
/// with the sell token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuyToken(pub Address);

impl From<Address> for BuyToken {
    fn from(address: Address) -> Self {
        Self(address)
    }
}

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
/// normalises the zero address to; the adapter round-trips that
/// normalisation on the chain edge.
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

impl OrderBody {
    /// Start a sell order: `amount` of `token` is the fixed side.
    #[must_use]
    pub const fn sell(token: SellToken, amount: U256) -> OrderBuilder<NeedsBuy> {
        OrderBuilder::start(OrderKind::Sell, token.0, amount, [0; 20], [0; 32])
    }

    /// Start a buy order: `amount` of `token` is the fixed side.
    #[must_use]
    pub const fn buy(token: BuyToken, amount: U256) -> OrderBuilder<NeedsSell> {
        OrderBuilder::start(OrderKind::Buy, [0; 20], [0; 32], token.0, amount)
    }
}

/// Builder state: the buy-side limit is unset.
pub enum NeedsBuy {}
/// Builder state: the sell-side limit is unset.
pub enum NeedsSell {}
/// Builder state: the expiry is unset.
pub enum NeedsValidTo {}
/// Builder state: every required field is set.
pub enum Ready {}

/// Typestate builder for [`OrderBody`]: [`OrderBody::sell`] or
/// [`OrderBody::buy`] fixes the kind and its side, the counter-side
/// limit and the expiry are compile-time required, and the optionals
/// default (self-receive, zero `app_data` and `fee_amount`,
/// fill-or-kill, ERC-20 balances).
#[derive(Clone, Debug)]
pub struct OrderBuilder<S> {
    body: OrderBody,
    state: PhantomData<S>,
}

impl<S> OrderBuilder<S> {
    const fn start(
        kind: OrderKind,
        sell_token: Address,
        sell_amount: U256,
        buy_token: Address,
        buy_amount: U256,
    ) -> Self {
        Self {
            body: OrderBody {
                sell_token,
                buy_token,
                receiver: None,
                sell_amount,
                buy_amount,
                valid_to: 0,
                app_data: [0; 32],
                fee_amount: [0; 32],
                kind,
                partially_fillable: false,
                sell_token_balance: SellTokenSource::Erc20,
                buy_token_balance: BuyTokenDestination::Erc20,
            },
            state: PhantomData,
        }
    }

    const fn into_state<T>(self) -> OrderBuilder<T> {
        OrderBuilder {
            body: self.body,
            state: PhantomData,
        }
    }
}

impl OrderBuilder<NeedsBuy> {
    /// Demand at least `amount` of `token` in return.
    #[must_use]
    pub const fn for_at_least(
        mut self,
        token: BuyToken,
        amount: U256,
    ) -> OrderBuilder<NeedsValidTo> {
        self.body.buy_token = token.0;
        self.body.buy_amount = amount;
        self.into_state()
    }
}

impl OrderBuilder<NeedsSell> {
    /// Spend at most `amount` of `token`.
    #[must_use]
    pub const fn for_at_most(
        mut self,
        token: SellToken,
        amount: U256,
    ) -> OrderBuilder<NeedsValidTo> {
        self.body.sell_token = token.0;
        self.body.sell_amount = amount;
        self.into_state()
    }
}

impl OrderBuilder<NeedsValidTo> {
    /// Expire at `valid_to` (Unix seconds).
    #[must_use]
    pub const fn valid_to(mut self, valid_to: u32) -> OrderBuilder<Ready> {
        self.body.valid_to = valid_to;
        self.into_state()
    }
}

impl OrderBuilder<Ready> {
    /// Deliver the buy token to `receiver` instead of the owner.
    #[must_use]
    pub const fn receiver(mut self, receiver: Address) -> Self {
        self.body.receiver = Some(receiver);
        self
    }

    /// Set the 32-byte on-chain app-data hash.
    #[must_use]
    pub const fn app_data(mut self, app_data: [u8; 32]) -> Self {
        self.body.app_data = app_data;
        self
    }

    /// Set the fee taken in the sell token.
    #[must_use]
    pub const fn fee_amount(mut self, fee_amount: U256) -> Self {
        self.body.fee_amount = fee_amount;
        self
    }

    /// Allow the order to fill partially.
    #[must_use]
    pub const fn partially_fillable(mut self) -> Self {
        self.body.partially_fillable = true;
        self
    }

    /// Source the sell token from `source`.
    #[must_use]
    pub const fn sell_token_balance(mut self, source: SellTokenSource) -> Self {
        self.body.sell_token_balance = source;
        self
    }

    /// Deliver the buy token to `destination`.
    #[must_use]
    pub const fn buy_token_balance(mut self, destination: BuyTokenDestination) -> Self {
        self.body.buy_token_balance = destination;
        self
    }

    /// The finished body.
    #[must_use]
    pub const fn build(self) -> OrderBody {
        self.body
    }
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
    fn sell_builder_matches_the_literal() {
        let built = OrderBody::sell(SellToken([0x11; 20]), sample().sell_amount)
            .for_at_least(BuyToken([0x22; 20]), [0xff; 32])
            .valid_to(0xffff_ffff)
            .receiver([0x33; 20])
            .app_data([0x44; 32])
            .build();
        assert_eq!(built, sample());
    }

    #[test]
    fn buy_builder_fixes_the_buy_side() {
        let built = OrderBody::buy(BuyToken([0x22; 20]), [0xff; 32])
            .for_at_most(SellToken([0x11; 20]), [0x01; 32])
            .valid_to(100)
            .partially_fillable()
            .sell_token_balance(SellTokenSource::External)
            .buy_token_balance(BuyTokenDestination::Internal)
            .fee_amount([0x05; 32])
            .build();
        assert_eq!(built.kind, OrderKind::Buy);
        assert_eq!(built.sell_token, [0x11; 20]);
        assert_eq!(built.buy_token, [0x22; 20]);
        assert_eq!(built.sell_amount, [0x01; 32]);
        assert_eq!(built.buy_amount, [0xff; 32]);
        assert_eq!(built.valid_to, 100);
        assert!(built.partially_fillable);
        assert_eq!(built.sell_token_balance, SellTokenSource::External);
        assert_eq!(built.buy_token_balance, BuyTokenDestination::Internal);
        assert_eq!(built.fee_amount, [0x05; 32]);
        assert_eq!(built.receiver, None);
    }

    #[test]
    fn builder_defaults_are_the_wire_defaults() {
        let built = OrderBody::sell(SellToken([0x11; 20]), [0x01; 32])
            .for_at_least(BuyToken([0x22; 20]), [0x02; 32])
            .valid_to(1)
            .build();
        assert_eq!(built.receiver, None);
        assert_eq!(built.app_data, [0; 32]);
        assert_eq!(built.fee_amount, [0; 32]);
        assert!(!built.partially_fillable);
        assert_eq!(built.sell_token_balance, SellTokenSource::Erc20);
        assert_eq!(built.buy_token_balance, BuyTokenDestination::Erc20);
        assert_eq!(built.kind, OrderKind::Sell);
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
