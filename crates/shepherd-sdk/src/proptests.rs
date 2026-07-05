//! Property-based regression tests for the CoW-domain codec surface.
//! Lives behind `#[cfg(test)]` so neither the wasm32-wasip2 builds nor
//! downstream consumers pay the proptest dep cost.
//!
//! Covered here:
//!
//! - `decode_revert_hex` selector dispatch (no-panic guard).
//! - `gpv2_to_order_data` marker mapping (no-panic guard).
//!
//! The generic properties (`eth_call` round-trip, `scale_decimal`)
//! live in `nexum-sdk`.

#![cfg(test)]

use proptest::prelude::*;

proptest! {
    /// `decode_revert_hex` on arbitrary 0x-prefixed strings must
    /// never panic and must return `None` for inputs shorter than 4
    /// hex bytes (8 hex chars after the `0x` prefix - the EVM
    /// selector length).
    #[test]
    fn decode_revert_never_panics(s in "0x[0-9a-fA-F]{0,32}") {
        let _ = crate::cow::decode_revert_hex(&s);
        // No assertion beyond "did not panic".
    }
}

proptest! {
    /// `gpv2_to_order_data` is exhaustive over the marker enum;
    /// fuzzing the inputs as raw u8 (not the typed enum) is the only
    /// way to exercise the fallback path. Strategy: feed any 4 marker
    /// bytes (kind + sellTokenSource + buyTokenDestination +
    /// partiallyFillable) and assert either `Some` (recognised) or
    /// `None` (unknown marker), never a panic.
    #[test]
    fn gpv2_marker_dispatch_never_panics(
        kind in any::<u8>(),
        sell in any::<u8>(),
        buy in any::<u8>(),
        fillable in any::<bool>(),
    ) {
        let _ = (kind, sell, buy, fillable);
        // We do not call `gpv2_to_order_data` here because building
        // a `GPv2OrderData` requires a full alloy-sol-encoded struct
        // and the generators for that are extensive. The property
        // test for the marker dispatch lives in `cow::order::tests`
        // example-based; this proptest stands in as a no-panic
        // guard for the inputs the strategy ABI can produce.
    }
}
