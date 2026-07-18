//! Parse and validate the shipped classification data.
//!
//! This module is the single source of the TOML schema and the table
//! invariants. It is compiled twice, never into a guest: `build.rs`
//! includes it to turn `data/classification.toml` into a generated
//! lookup table at build time, and the crate's own tests include it to
//! re-parse the same file and assert the generated table agrees. The
//! runtime `client` slice carries only the generated table, so no TOML
//! parser reaches the wasm guest.

use serde::Deserialize;

/// One of the retry actions an `errorType` maps to on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Transient: a fresh submission on a later block may succeed.
    TryNextBlock,
    /// Throttle: gate the watch for `backoff_seconds` before retrying.
    Backoff,
    /// Permanent unless first-seen: retry on the next block once, then
    /// drop on a repeat at a later block.
    DropOnRepeat,
    /// Permanent: remove the watch and its gates.
    Drop,
}

/// A single classification row as it appears in the TOML.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Entry {
    /// The orderbook `errorType` this row classifies.
    pub error_type: String,
    /// The retry action the row selects.
    pub action: Action,
    /// Required (and meaningful) only for `action = "backoff"`.
    #[serde(default)]
    pub backoff_seconds: u64,
    /// Marks a rejection meaning the orderbook already holds this order.
    #[serde(default)]
    pub already_submitted: bool,
}

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    entry: Vec<Entry>,
}

/// Why the shipped classification data could not be turned into a table.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ClassificationError {
    /// The TOML did not parse or a field had the wrong type.
    #[error("classification data is not valid TOML: {0}")]
    Toml(String),
    /// Two entries named the same `errorType`.
    #[error("duplicate errorType `{0}` in classification data")]
    Duplicate(String),
    /// A `backoff` entry left `backoff-seconds` at zero (or absent).
    #[error("errorType `{0}` is backoff but backoff-seconds is not >= 1")]
    ZeroBackoff(String),
    /// An `already-submitted` entry did not classify as try-next-block.
    #[error("errorType `{0}` is already-submitted but action is not try-next-block")]
    AlreadySubmittedAction(String),
}

/// Parse a classification document and validate the table invariants: no
/// duplicate `errorType`, every `backoff` carries a positive delay, and
/// `already-submitted` implies `try-next-block`.
pub fn parse_and_validate(toml: &str) -> Result<Vec<Entry>, ClassificationError> {
    let doc: Document =
        toml::from_str(toml).map_err(|e| ClassificationError::Toml(e.to_string()))?;

    let mut seen: Vec<&str> = Vec::with_capacity(doc.entry.len());
    for entry in &doc.entry {
        if entry.action == Action::Backoff && entry.backoff_seconds == 0 {
            return Err(ClassificationError::ZeroBackoff(entry.error_type.clone()));
        }
        if entry.already_submitted && entry.action != Action::TryNextBlock {
            return Err(ClassificationError::AlreadySubmittedAction(
                entry.error_type.clone(),
            ));
        }
        if seen.contains(&entry.error_type.as_str()) {
            return Err(ClassificationError::Duplicate(entry.error_type.clone()));
        }
        seen.push(&entry.error_type);
    }
    Ok(doc.entry)
}
