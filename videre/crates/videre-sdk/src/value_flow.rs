//! The value-flow wire types, re-exported from
//! [`bindings`](crate::bindings) with constructors that own the `uint`
//! encoding so callers never hand-roll it.

pub use crate::bindings::videre::value_flow::types::*;

impl AssetAmount {
    /// An ERC-20 amount. Encodes `amount` as the value-flow `uint`:
    /// minimal big-endian, where zero is the empty list.
    #[must_use]
    pub fn erc20(token: nexum_sdk::prelude::Address, amount: nexum_sdk::prelude::U256) -> Self {
        Self {
            asset: Asset::Erc20(Erc20 {
                token: token.as_slice().to_vec(),
            }),
            amount: amount.to_be_bytes_trimmed_vec(),
        }
    }
}
