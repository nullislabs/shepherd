//! Table-driven CoW retry classification.
//!
//! The `errorType -> {try-next-block, backoff, drop}` policy ships as
//! data in `data/classification.toml`; `build.rs` validates it and
//! emits the static lookup table [`classify`] and
//! [`is_already_submitted`] read, so no TOML parser reaches the guest.
//!
//! Invariant: an `errorType` absent from the table (including
//! [`OrderbookApiErrorType::Unknown`]) classifies as
//! [`RetryAction::Drop`], a permanent refusal never retried every block.

use cowprotocol::OrderbookApiErrorType;
use nexum_sdk::keeper::RetryAction;

/// The shipped classification data, embedded verbatim for the parity test.
pub const CLASSIFICATION_TOML: &str = include_str!("../data/classification.toml");

/// The retry action a generated row selects; mapped to a keeper
/// [`RetryAction`] by [`GeneratedRow`]. Which variants appear is a
/// property of the shipped data, hence `allow(dead_code)`.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenAction {
    TryNextBlock,
    Backoff,
    DropOnRepeat,
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
            GenAction::DropOnRepeat => RetryAction::DropOnRepeat,
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
    /// Find the row for `error_type`; `Unknown` is unlisted by definition.
    fn row(&self, error_type: &OrderbookApiErrorType) -> Option<&GeneratedRow> {
        match error_type {
            OrderbookApiErrorType::Unknown(_) => None,
            known => self.rows.iter().find(|r| r.error_type == known.as_str()),
        }
    }

    /// The retry action for an orderbook `errorType`. Unlisted types
    /// (including [`OrderbookApiErrorType::Unknown`]) are permanent:
    /// [`RetryAction::Drop`].
    pub fn classify(&self, error_type: &OrderbookApiErrorType) -> RetryAction {
        self.row(error_type)
            .map_or(RetryAction::Drop, GeneratedRow::retry_action)
    }

    /// Whether `error_type` means the orderbook already holds this order.
    pub fn is_already_submitted(&self, error_type: &OrderbookApiErrorType) -> bool {
        self.row(error_type).is_some_and(|r| r.already_submitted)
    }

    /// Number of classified `errorType`s.
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

/// Classify an orderbook `errorType` via the shipped table; unlisted
/// types are permanent ([`RetryAction::Drop`]).
pub fn classify(error_type: OrderbookApiErrorType) -> RetryAction {
    table().classify(&error_type)
}

/// Whether an orderbook `errorType` means the order is already held.
pub fn is_already_submitted(error_type: OrderbookApiErrorType) -> bool {
    table().is_already_submitted(&error_type)
}

/// Retry action for a coarse `denied` refusal: the `{errorType}:`
/// prefix re-enters the table.
///
/// The action is carried whole. Narrowing it here previously turned
/// every row except `drop-on-repeat` into [`RetryAction::Drop`], so a
/// `backoff` row could not reach the retrier at all: a commitment
/// refused for a condition its owner could clear was removed instead
/// of retried.
pub fn classify_denied(detail: &str) -> RetryAction {
    let error_type = detail.split_once(':').map_or(detail, |(prefix, _)| prefix);
    classify(OrderbookApiErrorType::from(error_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classification_data::{Action, ClassificationError, parse_and_validate};

    /// Wire spelling to typed kind, as the adapter's `error_kind()` does.
    fn kind(error_type: &str) -> OrderbookApiErrorType {
        OrderbookApiErrorType::from(error_type)
    }

    /// The generated table is non-empty.
    #[test]
    fn shipped_data_parses() {
        assert!(!table().is_empty());
    }

    /// Data-vs-code parity: the generated table agrees with an
    /// independent re-parse of the shipped file on every entry.
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
                Action::DropOnRepeat => RetryAction::DropOnRepeat,
                Action::Drop => RetryAction::Drop,
            };
            assert_eq!(
                classify(kind(&entry.error_type)),
                expected,
                "classify {}",
                entry.error_type,
            );
            assert_eq!(
                is_already_submitted(kind(&entry.error_type)),
                entry.already_submitted,
                "already-submitted {}",
                entry.error_type,
            );
        }
    }

    /// Spot check: the exemplar rows the table must carry.
    #[test]
    fn known_rows_classify_as_documented() {
        assert_eq!(classify(kind("InsufficientFee")), RetryAction::TryNextBlock);
        assert_eq!(
            classify(kind("TooManyLimitOrders")),
            RetryAction::Backoff { seconds: 30 },
        );
        assert_eq!(
            classify(kind("InvalidEip1271Signature")),
            RetryAction::DropOnRepeat,
        );
        assert_eq!(classify(kind("InvalidSignature")), RetryAction::Drop);
        assert!(is_already_submitted(kind("DuplicatedOrder")));
        assert!(is_already_submitted(kind("DuplicateOrder")));
    }

    /// Unlisted types are permanent.
    #[test]
    fn unlisted_type_drops() {
        let unknown = kind("NewlyMintedErrorType");
        assert!(matches!(unknown, OrderbookApiErrorType::Unknown(_)));
        assert_eq!(classify(unknown.clone()), RetryAction::Drop);
        assert!(!is_already_submitted(unknown));
    }

    /// Every retry arm is reachable from the table alone.
    #[test]
    fn table_reaches_every_arm() {
        assert_eq!(classify(kind("InsufficientFee")), RetryAction::TryNextBlock);
        assert!(matches!(
            classify(kind("TooManyLimitOrders")),
            RetryAction::Backoff { .. }
        ));
        assert_eq!(
            classify(kind("InvalidEip1271Signature")),
            RetryAction::DropOnRepeat,
        );
        assert_eq!(classify(kind("InvalidSignature")), RetryAction::Drop);
    }

    /// A denied detail re-enters the table by its `errorType` prefix
    /// and carries the row's action whole.
    #[test]
    fn denied_detail_refines_by_error_type_prefix() {
        assert_eq!(
            classify_denied("InvalidEip1271Signature: signature is not valid"),
            RetryAction::DropOnRepeat,
        );
        assert_eq!(
            classify_denied("InvalidSignature: bad sig"),
            RetryAction::Drop,
        );
        assert_eq!(
            classify_denied("InsufficientFee: too low"),
            RetryAction::TryNextBlock,
        );
        // Unparseable and unlisted details keep the permanent default.
        assert_eq!(classify_denied("policy refusal"), RetryAction::Drop);
        assert_eq!(classify_denied(""), RetryAction::Drop);
    }

    /// The narrowing this replaced turned every non-`drop-on-repeat`
    /// row into `Drop`, so a `backoff` row could not reach the retrier.
    /// These two are the rows that bug actually removed.
    #[test]
    fn a_clearable_refusal_backs_off_instead_of_removing() {
        for detail in [
            "InsufficientBalance: not enough sell token",
            "InsufficientAllowance: approve the vault relayer",
        ] {
            assert_eq!(
                classify_denied(detail),
                RetryAction::Backoff { seconds: 600 },
                "{detail} must stay in the rotation",
            );
        }
    }

    /// Both validTo rows were absent, so both took the implicit drop.
    #[test]
    fn both_valid_to_rows_are_classified() {
        assert_eq!(
            classify_denied("ExcessiveValidTo: too far out"),
            RetryAction::Drop,
        );
        assert_eq!(
            classify_denied("InsufficientValidTo: expires too soon"),
            RetryAction::TryNextBlock,
        );
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

    /// Every listed `error-type` names a real `errorType` in its exact
    /// wire spelling.
    #[test]
    fn every_row_names_a_real_error_type() {
        let entries = parse_and_validate(CLASSIFICATION_TOML).expect("shipped data is valid");
        for entry in &entries {
            let kind = cowprotocol::OrderbookApiErrorType::from(entry.error_type.as_str());
            assert!(
                !matches!(kind, cowprotocol::OrderbookApiErrorType::Unknown(_)),
                "phantom errorType {}",
                entry.error_type,
            );
            assert_eq!(kind.as_str(), entry.error_type, "wire spelling");
        }
    }

    /// The table's divergence from `retry_hint()` is exactly the
    /// ratified set; a change forces re-ratification.
    #[test]
    fn divergence_from_upstream_is_exactly_the_ratified_set() {
        // Balance and allowance left this set: adopting upstream's
        // 10-minute backoff is what fixed them.
        const RATIFIED: [&str; 3] = [
            "InvalidAppData",
            "InvalidEip1271Signature",
            "TooManyLimitOrders",
        ];
        let entries = parse_and_validate(CLASSIFICATION_TOML).expect("shipped data is valid");
        let mut divergent: Vec<&str> = Vec::new();
        for entry in &entries {
            let api = cowprotocol::ApiError {
                error_type: entry.error_type.clone(),
                description: String::new(),
                data: None,
            };
            // Project the upstream hint into the table's model; a hint
            // variant this projection does not know is a divergence.
            let upstream = match api.retry_hint() {
                cowprotocol::RetryHint::Retry => Some((RetryAction::TryNextBlock, false)),
                cowprotocol::RetryHint::Backoff { seconds } => {
                    Some((RetryAction::Backoff { seconds }, false))
                }
                cowprotocol::RetryHint::Drop => Some((RetryAction::Drop, false)),
                cowprotocol::RetryHint::AlreadySubmitted => Some((RetryAction::TryNextBlock, true)),
                _ => None,
            };
            let shepherd = (
                classify(kind(&entry.error_type)),
                is_already_submitted(kind(&entry.error_type)),
            );
            if upstream != Some(shepherd) {
                divergent.push(&entry.error_type);
            }
        }
        divergent.sort_unstable();
        assert_eq!(divergent, RATIFIED);
    }

    /// The shipped file parses with an untyped TOML model, so any TOML
    /// library reads it.
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
