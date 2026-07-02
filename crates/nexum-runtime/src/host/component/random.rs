//! Randomness seam mirroring the direct `getrandom::fill` call in the
//! random host impl. Zero-fill fallback stays in the WIT glue.

/// CSPRNG byte source.
pub trait Random {
    /// Fill `buf` with random bytes.
    fn fill(&self, buf: &mut [u8]) -> Result<(), getrandom::Error>;
}

/// Default source: the OS CSPRNG via `getrandom`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), getrandom::Error> {
        getrandom::fill(buf)
    }
}
