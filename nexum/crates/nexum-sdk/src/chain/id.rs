//! Zero-cost chain identity newtypes.

use core::fmt;

/// EIP-155 chain id, typed so a bare `u64` never crosses an SDK API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChainId(u64);

impl ChainId {
    /// Wrap a raw EIP-155 id.
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The raw id, for the WIT edge.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for ChainId {
    fn from(id: u64) -> Self {
        Self::new(id)
    }
}

impl From<ChainId> for u64 {
    fn from(id: ChainId) -> Self {
        id.get()
    }
}

impl fmt::Display for ChainId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A chain a strategy targets, keyed by its [`ChainId`]. The type the
/// provider seam takes; events deliver a raw id, so `ev.chain_id.into()`
/// bridges at the handler edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chain(ChainId);

impl Chain {
    /// Ethereum mainnet.
    pub const MAINNET: Self = Self::from_id(1);
    /// Gnosis Chain.
    pub const GNOSIS: Self = Self::from_id(100);
    /// Base.
    pub const BASE: Self = Self::from_id(8_453);
    /// Arbitrum One.
    pub const ARBITRUM: Self = Self::from_id(42_161);
    /// Sepolia testnet.
    pub const SEPOLIA: Self = Self::from_id(11_155_111);

    /// Chain with the given raw EIP-155 id.
    pub const fn from_id(id: u64) -> Self {
        Self(ChainId::new(id))
    }

    /// The chain's id.
    pub const fn id(self) -> ChainId {
        self.0
    }
}

impl From<u64> for Chain {
    fn from(id: u64) -> Self {
        Self::from_id(id)
    }
}

impl From<ChainId> for Chain {
    fn from(id: ChainId) -> Self {
        Self(id)
    }
}

impl From<Chain> for ChainId {
    fn from(chain: Chain) -> Self {
        chain.id()
    }
}

impl From<Chain> for u64 {
    fn from(chain: Chain) -> Self {
        chain.id().get()
    }
}

impl fmt::Display for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{Chain, ChainId};

    #[test]
    fn ids_round_trip() {
        assert_eq!(u64::from(ChainId::new(100)), 100);
        assert_eq!(ChainId::from(7u64).get(), 7);
        assert_eq!(u64::from(Chain::from_id(42)), 42);
        assert_eq!(Chain::from(ChainId::new(1)), Chain::MAINNET);
        assert_eq!(ChainId::from(Chain::SEPOLIA).get(), 11_155_111);
    }

    #[test]
    fn named_chains_carry_canonical_ids() {
        assert_eq!(u64::from(Chain::MAINNET), 1);
        assert_eq!(u64::from(Chain::GNOSIS), 100);
        assert_eq!(u64::from(Chain::BASE), 8_453);
        assert_eq!(u64::from(Chain::ARBITRUM), 42_161);
        assert_eq!(u64::from(Chain::SEPOLIA), 11_155_111);
    }

    #[test]
    fn display_is_the_raw_id() {
        assert_eq!(Chain::GNOSIS.to_string(), "100");
        assert_eq!(ChainId::new(1).to_string(), "1");
    }
}
