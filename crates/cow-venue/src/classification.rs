//! Table-driven CoW retry classification.
//!
//! The `errorType -> {try-next-block, backoff, drop}` policy is shipped
//! as data in `data/classification.toml`, embedded here at compile time
//! and parsed once. [`classify`] and [`is_already_submitted`] read the
//! parsed table, so the policy lives in the data file rather than in a
//! hand-coded `match`; a non-Rust author edits the TOML and a parity
//! test guards the Rust contract against it.
//!
//! The one non-obvious invariant: an `errorType` absent from the table
//! classifies as [`RetryAction::Drop`]. An unrecognized structured
//! rejection is a permanent contract-level refusal, not a transient
//! transport error, so it must not be retried every block forever.

use std::collections::HashMap;
use std::sync::LazyLock;

use nexum_sdk::keeper::RetryAction;
use serde::Deserialize;

/// The shipped classification data, embedded verbatim. The parsed
/// [`table`] reads this; a parity test also re-parses it independently.
pub const CLASSIFICATION_TOML: &str = include_str!("../data/classification.toml");

/// One of the three retry actions an `errorType` maps to on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Action {
    TryNextBlock,
    Backoff,
    Drop,
}

/// A single classification row as it appears in the TOML.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Entry {
    error_type: String,
    action: Action,
    /// Required (and meaningful) only for `action = "backoff"`.
    #[serde(default)]
    backoff_seconds: u64,
    #[serde(default)]
    already_submitted: bool,
}

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    entry: Vec<Entry>,
}

/// Why the shipped classification data could not be turned into a table.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
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

/// The parsed classification: an `errorType` lookup over the shipped
/// data. Unlisted types classify as [`RetryAction::Drop`].
#[derive(Clone, Debug)]
pub struct ClassificationTable {
    by_type: HashMap<String, Entry>,
}

impl ClassificationTable {
    /// Parse a classification document, validating the table invariants
    /// (no duplicate types, backoff carries a positive delay,
    /// already-submitted implies try-next-block).
    pub fn parse(toml: &str) -> Result<Self, ClassificationError> {
        let doc: Document =
            toml::from_str(toml).map_err(|e| ClassificationError::Toml(e.to_string()))?;

        let mut by_type = HashMap::with_capacity(doc.entry.len());
        for entry in doc.entry {
            if entry.action == Action::Backoff && entry.backoff_seconds == 0 {
                return Err(ClassificationError::ZeroBackoff(entry.error_type));
            }
            if entry.already_submitted && entry.action != Action::TryNextBlock {
                return Err(ClassificationError::AlreadySubmittedAction(
                    entry.error_type,
                ));
            }
            if by_type
                .insert(entry.error_type.clone(), entry.clone())
                .is_some()
            {
                return Err(ClassificationError::Duplicate(entry.error_type));
            }
        }
        Ok(Self { by_type })
    }

    /// The retry action for an orderbook `errorType`. Unlisted types are
    /// permanent: [`RetryAction::Drop`].
    pub fn classify(&self, error_type: &str) -> RetryAction {
        self.by_type
            .get(error_type)
            .map_or(RetryAction::Drop, Entry::retry_action)
    }

    /// Whether the orderbook is reporting that it already holds this
    /// exact order. Such a rejection keeps the watch and records the
    /// receipt rather than retrying a fresh submission.
    pub fn is_already_submitted(&self, error_type: &str) -> bool {
        self.by_type
            .get(error_type)
            .is_some_and(|e| e.already_submitted)
    }

    /// Number of classified `errorType`s, for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.by_type.len()
    }

    /// Whether the table carries no entries.
    pub fn is_empty(&self) -> bool {
        self.by_type.is_empty()
    }
}

impl Entry {
    fn retry_action(&self) -> RetryAction {
        match self.action {
            Action::TryNextBlock => RetryAction::TryNextBlock,
            // Validated non-zero at parse; `.max(1)` keeps the mapping
            // total even if that guard is ever relaxed.
            Action::Backoff => RetryAction::Backoff {
                seconds: self.backoff_seconds.max(1),
            },
            Action::Drop => RetryAction::Drop,
        }
    }
}

static TABLE: LazyLock<ClassificationTable> = LazyLock::new(|| {
    ClassificationTable::parse(CLASSIFICATION_TOML)
        .expect("shipped cow classification.toml is well formed")
});

/// The process-wide classification table parsed from the shipped data.
pub fn table() -> &'static ClassificationTable {
    &TABLE
}

/// Classify an orderbook `errorType` into a keeper [`RetryAction`] via
/// the shipped table. Unlisted types are permanent ([`RetryAction::Drop`]).
pub fn classify(error_type: &str) -> RetryAction {
    TABLE.classify(error_type)
}

/// Whether an orderbook `errorType` means the order is already held.
pub fn is_already_submitted(error_type: &str) -> bool {
    TABLE.is_already_submitted(error_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped file parses and the lazy table builds without panic.
    #[test]
    fn shipped_data_parses() {
        assert!(!table().is_empty());
    }

    /// Data-vs-code parity: the classification the Rust contract
    /// promises, spelled out here in code, must match what the shipped
    /// data produces. This is the guard that catches a data edit that
    /// silently changes behaviour and a code assumption that drifts from
    /// the file.
    #[test]
    fn data_matches_code_contract() {
        let expected: &[(&str, RetryAction, bool)] = &[
            ("InsufficientFee", RetryAction::TryNextBlock, false),
            ("PriceExceedsMarketPrice", RetryAction::TryNextBlock, false),
            (
                "TooManyLimitOrders",
                RetryAction::Backoff { seconds: 30 },
                false,
            ),
            ("DuplicatedOrder", RetryAction::TryNextBlock, true),
            ("DuplicateOrder", RetryAction::TryNextBlock, true),
            ("InvalidSignature", RetryAction::Drop, false),
            ("WrongOwner", RetryAction::Drop, false),
            ("UnsupportedToken", RetryAction::Drop, false),
            ("InvalidAppData", RetryAction::Drop, false),
        ];
        for (error_type, action, already) in expected {
            assert_eq!(classify(error_type), *action, "classify {error_type}");
            assert_eq!(
                is_already_submitted(error_type),
                *already,
                "already-submitted {error_type}",
            );
        }
    }

    /// Unlisted (including newly minted) types are permanent, so a
    /// contract-level rejection is never retried every block forever.
    #[test]
    fn unlisted_type_drops() {
        assert_eq!(classify("NewlyMintedErrorType"), RetryAction::Drop);
        assert!(!is_already_submitted("NewlyMintedErrorType"));
    }

    /// All three retry arms are reachable from the table alone.
    #[test]
    fn table_reaches_every_arm() {
        assert_eq!(classify("InsufficientFee"), RetryAction::TryNextBlock);
        assert!(matches!(
            classify("TooManyLimitOrders"),
            RetryAction::Backoff { .. }
        ));
        assert_eq!(classify("InvalidSignature"), RetryAction::Drop);
    }

    #[test]
    fn duplicate_type_is_rejected() {
        let toml = r#"
            [[entry]]
            error-type = "Dup"
            action = "drop"
            [[entry]]
            error-type = "Dup"
            action = "drop"
        "#;
        assert_eq!(
            ClassificationTable::parse(toml).unwrap_err(),
            ClassificationError::Duplicate("Dup".to_string()),
        );
    }

    #[test]
    fn backoff_without_delay_is_rejected() {
        let toml = r#"
            [[entry]]
            error-type = "Slow"
            action = "backoff"
        "#;
        assert_eq!(
            ClassificationTable::parse(toml).unwrap_err(),
            ClassificationError::ZeroBackoff("Slow".to_string()),
        );
    }

    #[test]
    fn already_submitted_must_try_next_block() {
        let toml = r#"
            [[entry]]
            error-type = "Held"
            action = "drop"
            already-submitted = true
        "#;
        assert_eq!(
            ClassificationTable::parse(toml).unwrap_err(),
            ClassificationError::AlreadySubmittedAction("Held".to_string()),
        );
    }

    /// A non-Rust reader sees the same file as plain data: parsing it
    /// with the untyped TOML value model (no Rust schema) exposes the
    /// entries and their fields, proving any TOML library reads it.
    #[test]
    fn non_rust_reader_sees_plain_toml() {
        let value: toml::Table =
            toml::from_str(CLASSIFICATION_TOML).expect("valid TOML for any reader");
        let entries = value["entry"].as_array().expect("entry is an array");
        assert!(!entries.is_empty());
        let first = entries[0].as_table().expect("entry is a table");
        assert!(first.contains_key("error-type"));
        assert!(first.contains_key("action"));
    }
}
