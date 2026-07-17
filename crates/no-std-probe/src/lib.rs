//! Compile-only `#![no_std]` probe: `#[derive(IntentBody)]` must expand
//! without the consumer's std prelude or an `extern crate alloc`.

#![no_std]
#![warn(missing_docs)]

use videre_sdk::IntentBody;

/// The probe schema: one published version over a bare byte payload.
#[derive(IntentBody, Clone, Debug, PartialEq, Eq)]
pub enum ProbeBody {
    /// First published version.
    V1(u8),
}
