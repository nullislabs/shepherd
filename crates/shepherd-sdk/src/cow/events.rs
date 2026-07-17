//! CoW on-chain event ABIs, mirroring `shepherd:cow/cow-events`.
//!
//! `wit/shepherd-cow/cow-events.wit` is the package of record; the
//! constants here are parity-tested against it, the `cowprotocol`
//! `sol!` types, and each keeper's `module.toml`.

use alloy_primitives::{B256, b256};

/// One on-chain event surface: the canonical Solidity signature and
/// its keccak256 topic-0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventAbi {
    /// Canonical Solidity event signature.
    pub signature: &'static str,
    /// keccak256 of [`Self::signature`]: the log's topic-0.
    pub topic0: B256,
}

/// `ComposableCoW.ConditionalOrderCreated`.
pub const CONDITIONAL_ORDER_CREATED: EventAbi = EventAbi {
    signature: "ConditionalOrderCreated(address,(address,bytes32,bytes))",
    topic0: b256!("2cceac5555b0ca45a3744ced542f54b56ad2eb45e521962372eef212a2cbf361"),
};

/// `CoWSwapOnchainOrders.OrderPlacement` (EthFlow).
pub const ORDER_PLACEMENT: EventAbi = EventAbi {
    signature: "OrderPlacement(address,(address,address,address,uint256,uint256,uint32,bytes32,\
                uint256,bytes32,bool,bytes32,bytes32),(uint8,bytes),bytes)",
    topic0: b256!("cf5f9de2984132265203b5c335b25727702ca77262ff622e136baa7362bf1da9"),
};

/// Every event surface the keepers decode.
pub const ALL: &[EventAbi] = &[CONDITIONAL_ORDER_CREATED, ORDER_PLACEMENT];

#[cfg(test)]
mod tests {
    use alloy_primitives::keccak256;
    use alloy_sol_types::SolEvent;
    use cowprotocol::{CoWSwapOnchainOrders, ComposableCoW};

    use super::*;

    #[test]
    fn topic0_is_keccak_of_signature() {
        for abi in ALL {
            assert_eq!(abi.topic0, keccak256(abi.signature), "{}", abi.signature);
        }
    }

    #[test]
    fn matches_the_sol_decoder_types() {
        assert_eq!(
            ComposableCoW::ConditionalOrderCreated::SIGNATURE,
            CONDITIONAL_ORDER_CREATED.signature,
        );
        assert_eq!(
            ComposableCoW::ConditionalOrderCreated::SIGNATURE_HASH,
            CONDITIONAL_ORDER_CREATED.topic0,
        );
        assert_eq!(
            CoWSwapOnchainOrders::OrderPlacement::SIGNATURE,
            ORDER_PLACEMENT.signature,
        );
        assert_eq!(
            CoWSwapOnchainOrders::OrderPlacement::SIGNATURE_HASH,
            ORDER_PLACEMENT.topic0,
        );
    }

    #[test]
    fn wit_package_of_record_pins_every_surface() {
        let wit = include_str!("../../../../wit/shepherd-cow/cow-events.wit");
        let flat: String = wit
            .lines()
            .map(|l| l.trim().trim_start_matches("/// "))
            .collect();
        for abi in ALL {
            assert!(
                flat.contains(abi.signature),
                "cow-events.wit must pin the signature {}",
                abi.signature,
            );
            assert!(
                flat.contains(&format!("{:#x}", abi.topic0)),
                "cow-events.wit must pin the topic-0 {:#x}",
                abi.topic0,
            );
        }
    }

    /// Layering gate: no generic WIT package references `shepherd:cow`.
    #[test]
    fn generic_wit_packages_never_reference_shepherd_cow() {
        let wit_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit");
        for pkg in std::fs::read_dir(&wit_root).expect("wit dir") {
            let pkg = pkg.expect("wit dir entry").path();
            if pkg.file_name().is_some_and(|n| n == "shepherd-cow") {
                continue;
            }
            for file in std::fs::read_dir(&pkg).expect("wit package dir") {
                let path = file.expect("wit package entry").path();
                let text = std::fs::read_to_string(&path).expect("read wit file");
                assert!(
                    !text.contains("shepherd:cow"),
                    "{} references shepherd:cow",
                    path.display(),
                );
            }
        }
    }
}
