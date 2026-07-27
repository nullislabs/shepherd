//! The venue-neutral CoW order body.
//!
//! The 12-field `GPv2Order` tuple over the alloy primitives, so it
//! borsh-encodes without the on-chain stack. The marker enums are
//! canonical wire forms, not on-chain keccak markers; the adapter owns
//! the projection to and from chain.

use core::fmt;

use alloy_primitives::{Address, U256};
use borsh::{BorshDeserialize, BorshSerialize};

/// The token an order sells, typed against side swaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellToken(pub Address);

impl From<Address> for SellToken {
    fn from(address: Address) -> Self {
        Self(address)
    }
}

/// The token an order buys, typed against side swaps.
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
    /// A sell order: `sell_amount` fixed, at least `buy_amount` in
    /// return, expiring at `valid_to`.
    #[must_use]
    pub const fn sell(
        sell: SellToken,
        sell_amount: U256,
        buy: BuyToken,
        buy_amount: U256,
        valid_to: u32,
    ) -> OrderBuilder {
        OrderBuilder::new(
            OrderKind::Sell,
            sell.0,
            sell_amount,
            buy.0,
            buy_amount,
            valid_to,
        )
    }

    /// A buy order: `buy_amount` fixed, spending at most `sell_amount`,
    /// expiring at `valid_to`.
    #[must_use]
    pub const fn buy(
        buy: BuyToken,
        buy_amount: U256,
        sell: SellToken,
        sell_amount: U256,
        valid_to: u32,
    ) -> OrderBuilder {
        OrderBuilder::new(
            OrderKind::Buy,
            sell.0,
            sell_amount,
            buy.0,
            buy_amount,
            valid_to,
        )
    }
}

/// Builder for [`OrderBody`]: required fields are constructor args, the
/// optionals default (self-receive, zero `app_data`/`fee_amount`,
/// fill-or-kill, ERC-20 balances). Start from [`OrderBody::sell`] or
/// [`OrderBody::buy`].
#[derive(Clone, Debug)]
pub struct OrderBuilder {
    body: OrderBody,
}

impl OrderBuilder {
    const fn new(
        kind: OrderKind,
        sell_token: Address,
        sell_amount: U256,
        buy_token: Address,
        buy_amount: U256,
        valid_to: u32,
    ) -> Self {
        Self {
            body: OrderBody {
                sell_token,
                buy_token,
                receiver: None,
                sell_amount,
                buy_amount,
                valid_to,
                app_data: [0; 32],
                fee_amount: U256::ZERO,
                kind,
                partially_fillable: false,
                sell_token_balance: SellTokenSource::Erc20,
                buy_token_balance: BuyTokenDestination::Erc20,
            },
        }
    }

    /// Set the absolute `validTo` (Unix seconds), overriding the
    /// constructor.
    #[must_use]
    pub const fn valid_to(mut self, secs: u32) -> Self {
        self.body.valid_to = secs;
        self
    }

    /// Expire `duration` seconds after `now`, saturating at `u32::MAX`.
    /// `now` is the block timestamp, not a wall clock: `valid_to` feeds
    /// the submission dedup key, so a wall clock would break replay
    /// idempotency.
    #[must_use]
    pub const fn valid_for(mut self, now: u32, duration: u32) -> Self {
        self.body.valid_to = now.saturating_add(duration);
        self
    }

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

/// An owner-signed order ready for the orderbook.
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct SignedOrder {
    /// The order to place.
    pub order: OrderBody,
    /// Order owner: the EIP-1271 verifier and the `from` of the
    /// orderbook submission.
    pub owner: Address,
    /// Raw EIP-1271 signature bytes; the settlement verifies them
    /// against `owner`.
    pub signature: Vec<u8>,
}

/// Canonical 56-byte orderbook UID (order digest, owner, `valid_to`)
/// in wire form.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OrderUid(pub [u8; 56]);

impl OrderUid {
    /// The raw 56 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 56] {
        &self.0
    }
}

impl From<[u8; 56]> for OrderUid {
    fn from(bytes: [u8; 56]) -> Self {
        Self(bytes)
    }
}

impl TryFrom<&[u8]> for OrderUid {
    type Error = core::array::TryFromSliceError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self(<[u8; 56]>::try_from(bytes)?))
    }
}

impl From<OrderUid> for Vec<u8> {
    fn from(uid: OrderUid) -> Self {
        uid.0.to_vec()
    }
}

impl fmt::Display for OrderUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0x")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for OrderUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OrderBody {
        OrderBody {
            sell_token: Address::repeat_byte(0x11),
            buy_token: Address::repeat_byte(0x22),
            receiver: Some(Address::repeat_byte(0x33)),
            sell_amount: U256::from(0x2a_u64),
            buy_amount: U256::MAX,
            valid_to: 0xffff_ffff,
            app_data: [0x44; 32],
            fee_amount: U256::ZERO,
            kind: OrderKind::Sell,
            partially_fillable: false,
            sell_token_balance: SellTokenSource::Erc20,
            buy_token_balance: BuyTokenDestination::Erc20,
        }
    }

    #[test]
    fn sell_builder_matches_the_literal() {
        let built = OrderBody::sell(
            SellToken(Address::repeat_byte(0x11)),
            sample().sell_amount,
            BuyToken(Address::repeat_byte(0x22)),
            U256::MAX,
            0xffff_ffff,
        )
        .receiver(Address::repeat_byte(0x33))
        .app_data([0x44; 32])
        .build();
        assert_eq!(built, sample());
    }

    #[test]
    fn buy_builder_fixes_the_buy_side() {
        let built = OrderBody::buy(
            BuyToken(Address::repeat_byte(0x22)),
            U256::MAX,
            SellToken(Address::repeat_byte(0x11)),
            U256::from(1u64),
            100,
        )
        .partially_fillable()
        .sell_token_balance(SellTokenSource::External)
        .buy_token_balance(BuyTokenDestination::Internal)
        .fee_amount(U256::from(5u64))
        .build();
        assert_eq!(built.kind, OrderKind::Buy);
        assert_eq!(built.sell_token, Address::repeat_byte(0x11));
        assert_eq!(built.buy_token, Address::repeat_byte(0x22));
        assert_eq!(built.sell_amount, U256::from(1u64));
        assert_eq!(built.buy_amount, U256::MAX);
        assert_eq!(built.valid_to, 100);
        assert!(built.partially_fillable);
        assert_eq!(built.sell_token_balance, SellTokenSource::External);
        assert_eq!(built.buy_token_balance, BuyTokenDestination::Internal);
        assert_eq!(built.fee_amount, U256::from(5u64));
        assert_eq!(built.receiver, None);
    }

    #[test]
    fn builder_defaults_are_the_wire_defaults() {
        let built = OrderBody::sell(
            SellToken(Address::repeat_byte(0x11)),
            U256::from(1u64),
            BuyToken(Address::repeat_byte(0x22)),
            U256::from(2u64),
            1,
        )
        .build();
        assert_eq!(built.receiver, None);
        assert_eq!(built.app_data, [0; 32]);
        assert_eq!(built.fee_amount, U256::ZERO);
        assert!(!built.partially_fillable);
        assert_eq!(built.sell_token_balance, SellTokenSource::Erc20);
        assert_eq!(built.buy_token_balance, BuyTokenDestination::Erc20);
        assert_eq!(built.kind, OrderKind::Sell);
    }

    #[test]
    fn valid_to_setter_overrides_the_constructor_argument() {
        let built = OrderBody::sell(
            SellToken(Address::repeat_byte(0x11)),
            U256::from(1u64),
            BuyToken(Address::repeat_byte(0x22)),
            U256::from(2u64),
            1,
        )
        .valid_to(0x1234_5678)
        .build();
        assert_eq!(built.valid_to, 0x1234_5678);
    }

    #[test]
    fn valid_for_adds_the_duration_to_now() {
        let built = OrderBody::sell(
            SellToken(Address::repeat_byte(0x11)),
            U256::from(1u64),
            BuyToken(Address::repeat_byte(0x22)),
            U256::from(2u64),
            1,
        )
        .valid_for(1_700_000_000, 3_600)
        .build();
        assert_eq!(built.valid_to, 1_700_003_600);
    }

    #[test]
    fn valid_for_saturates_on_overflow() {
        let built = OrderBody::sell(
            SellToken(Address::repeat_byte(0x11)),
            U256::from(1u64),
            BuyToken(Address::repeat_byte(0x22)),
            U256::from(2u64),
            1,
        )
        .valid_for(u32::MAX - 10, 3_600)
        .build();
        assert_eq!(built.valid_to, u32::MAX);
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

    #[test]
    fn signed_order_borsh_round_trips() {
        let signed = SignedOrder {
            order: sample(),
            owner: Address::repeat_byte(0x55),
            signature: vec![0xC0, 0xFF, 0xEE],
        };
        let bytes = borsh::to_vec(&signed).expect("encode");
        assert_eq!(SignedOrder::try_from_slice(&bytes).expect("decode"), signed);
    }

    #[test]
    fn order_uid_converts_only_from_56_bytes() {
        let uid = OrderUid([0xAB; 56]);
        assert_eq!(OrderUid::try_from(&uid.0[..]).expect("56 bytes"), uid);
        assert!(OrderUid::try_from(&uid.0[..55]).is_err());
        assert_eq!(Vec::from(uid), vec![0xAB; 56]);
    }

    #[test]
    fn order_uid_displays_as_prefixed_hex() {
        let mut bytes = [0u8; 56];
        bytes[0] = 0x01;
        bytes[55] = 0xFF;
        let uid = OrderUid(bytes);
        let hex = uid.to_string();
        assert_eq!(hex.len(), 2 + 56 * 2);
        assert!(hex.starts_with("0x01"));
        assert!(hex.ends_with("ff"));
        assert_eq!(format!("{uid:?}"), hex);
    }
}
