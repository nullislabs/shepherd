//! Table-driven CoW retry classification.
//!
//! The `errorType -> {try-next-block, backoff, drop}` policy is shipped
//! as data in `data/classification.toml`. `build.rs` parses and
//! validates that file at build time and emits a static lookup table, so
//! [`classify`] and [`is_already_submitted`] read generated data rather
//! than a hand-coded `match` and no TOML parser reaches the guest. A
//! non-Rust author edits the TOML; a parity test re-parses the same file
//! and asserts the generated table agrees.
//!
//! The one non-obvious invariant: an `errorType` absent from the table
//! classifies as [`RetryAction::Drop`]. An unrecognised structured
//! rejection is a permanent contract-level refusal, not a transient
//! transport error, so it must not be retried every block forever.

use nexum_sdk::keeper::RetryAction;

/// The shipped classification data, embedded verbatim so a parity test
/// can re-parse the exact bytes `build.rs` generated the table from.
pub const CLASSIFICATION_TOML: &str = include_str!("../data/classification.toml");

/// The retry action a generated row selects, mirroring the TOML `action`
/// field. Turned into a keeper [`RetryAction`] by [`GeneratedRow`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenAction {
    TryNextBlock,
    Backoff,
    Drop,
}

/// One classification row, generated from the shipped data table.
#[derive(Clone, Copy, Debug)]
struct GeneratedRow {
    error_type: &'static str,
    action: GenAction,
    backoff_seconds: u64,
    already_submitted: bool,
}

impl GeneratedRow {
    fn retry_action(&self) -> RetryAction {
        match self.action {
            GenAction::TryNextBlock => RetryAction::TryNextBlock,
            // `build.rs` validated backoff rows non-zero; `.max(1)` keeps
            // the mapping total even if that guard is ever relaxed.
            GenAction::Backoff => RetryAction::Backoff {
                seconds: self.backoff_seconds.max(1),
            },
            GenAction::Drop => RetryAction::Drop,
        }
    }
}

// `static GENERATED_ROWS: &[GeneratedRow]`, one row per TOML entry.
include!(concat!(env!("OUT_DIR"), "/classification_table.rs"));

/// The shipped classification: a lookup over the generated rows.
/// Unlisted types classify as [`RetryAction::Drop`].
#[derive(Clone, Copy, Debug)]
pub struct ClassificationTable {
    rows: &'static [GeneratedRow],
}

impl ClassificationTable {
    fn row(&self, error_type: &str) -> Option<&GeneratedRow> {
        self.rows.iter().find(|r| r.error_type == error_type)
    }

    /// The retry action for an orderbook `errorType`. Unlisted types are
    /// permanent: [`RetryAction::Drop`].
    pub fn classify(&self, error_type: &str) -> RetryAction {
        self.row(error_type)
            .map_or(RetryAction::Drop, GeneratedRow::retry_action)
    }

    /// Whether the orderbook is reporting that it already holds this
    /// exact order. Such a rejection keeps the watch and records the
    /// receipt rather than retrying a fresh submission.
    pub fn is_already_submitted(&self, error_type: &str) -> bool {
        self.row(error_type).is_some_and(|r| r.already_submitted)
    }

    /// Number of classified `errorType`s, for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table carries no entries.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The classification table generated from the shipped data.
pub fn table() -> ClassificationTable {
    ClassificationTable {
        rows: GENERATED_ROWS,
    }
}

/// Classify an orderbook `errorType` into a keeper [`RetryAction`] via
/// the shipped table. Unlisted types are permanent ([`RetryAction::Drop`]).
pub fn classify(error_type: &str) -> RetryAction {
    table().classify(error_type)
}

/// Whether an orderbook `errorType` means the order is already held.
pub fn is_already_submitted(error_type: &str) -> bool {
    table().is_already_submitted(error_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification_data::{Action, ClassificationError, parse_and_validate};

    /// The generated table is non-empty.
    #[test]
    fn shipped_data_parses() {
        assert!(!table().is_empty());
    }

    /// Data-vs-code parity: re-parse the shipped file independently and
    /// assert the generated table (code) agrees with it (data) on every
    /// entry. This catches a data edit the generated table missed and a
    /// generator bug that drifts from the file.
    #[test]
    fn data_matches_code_contract() {
        let entries = parse_and_validate(CLASSIFICATION_TOML).expect("shipped data is valid");
        assert_eq!(table().len(), entries.len(), "row count matches the data");
        for entry in &entries {
            let expected = match entry.action {
                Action::TryNextBlock => RetryAction::TryNextBlock,
                Action::Backoff => RetryAction::Backoff {
                    seconds: entry.backoff_seconds.max(1),
                },
                Action::Drop => RetryAction::Drop,
            };
            assert_eq!(
                classify(&entry.error_type),
                expected,
                "classify {}",
                entry.error_type,
            );
            assert_eq!(
                is_already_submitted(&entry.error_type),
                entry.already_submitted,
                "already-submitted {}",
                entry.error_type,
            );
        }
    }

    /// A spot check of the contract in code, independent of the parse:
    /// the exemplar rows the slice must carry, including the `Backoff`
    /// producer the hand-coded classifier lacked.
    #[test]
    fn known_rows_classify_as_documented() {
        assert_eq!(classify("InsufficientFee"), RetryAction::TryNextBlock);
        assert_eq!(
            classify("TooManyLimitOrders"),
            RetryAction::Backoff { seconds: 30 },
        );
        assert_eq!(classify("InvalidSignature"), RetryAction::Drop);
        assert!(is_already_submitted("DuplicatedOrder"));
        assert!(is_already_submitted("DuplicateOrder"));
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
            parse_and_validate(toml).unwrap_err(),
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
            parse_and_validate(toml).unwrap_err(),
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
            parse_and_validate(toml).unwrap_err(),
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
