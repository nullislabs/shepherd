//! Helpers for parsing the `Vec<(String, String)>` config entries a
//! module's `on_event` receives.
//!
//! Each entry is a `(key, value)` pair the runtime read from the
//! module's `[config]` table. Modules need three operations
//! repeatedly: required-key lookup, optional-key lookup, and decimal
//! parsing for thresholds / amounts. Hoisting these here keeps the
//! example modules consuming the SDK rather than re-implementing the
//! same loops around it (each copy in price-alert + stop-loss had
//! started to drift in error wording).

use alloy_primitives::{I256, U256};
use thiserror::Error;

/// Why a config lookup or parse failed.
///
/// Modules wrap this into their own domain-specific `HostError`
/// (`HostErrorKind::InvalidInput`, domain string of the module) at
/// the boundary. The SDK type stays host-neutral so the same parser
/// can be unit-tested without `wasm32-wasip2`.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The key was not present in the `entries` slice.
    #[error("missing key {key:?}")]
    MissingKey {
        /// Config-table key the lookup was for.
        key: String,
    },
    /// The value at `key` did not parse as the expected shape.
    #[error("parse {key:?}: {detail}")]
    Parse {
        /// Config-table key whose value failed to parse.
        key: String,
        /// Free-text parser detail.
        detail: String,
    },
    /// The value parsed but did not fit the target type's range.
    #[error("range {key:?}: {detail}")]
    Range {
        /// Config-table key whose value overflowed.
        key: String,
        /// Free-text range detail.
        detail: String,
    },
}

/// Look up a required `(key, value)` entry in a config table.
///
/// Returns `Err(MissingKey)` if the key is absent. The returned
/// `&str` borrows from `entries`.
pub fn get_required<'a>(
    entries: &'a [(String, String)],
    key: &str,
) -> Result<&'a str, ConfigError> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| ConfigError::MissingKey {
            key: key.to_owned(),
        })
}

/// Look up an optional `(key, value)` entry. Returns `None` when
/// absent; never errors.
pub fn get_optional<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Parse a signed fixed-point decimal string into an `I256` scaled by
/// `10**decimals`.
///
/// - Short fractional parts are right-padded with zeros.
/// - Long fractional parts are truncated.
/// - A leading `-` is honoured.
/// - Empty input is rejected as a parse error.
/// - Non-digit characters (other than the leading sign and a single
///   `.`) are rejected.
///
/// `key` is the config-table key the value came from; it is embedded
/// in the returned error so the caller can surface a useful message
/// without re-passing context.
pub fn scale_decimal(value: &str, decimals: u32, key: &str) -> Result<I256, ConfigError> {
    let (sign, body) = if let Some(rest) = value.strip_prefix('-') {
        (-1i32, rest)
    } else {
        (1, value)
    };
    let (whole, frac) = match body.split_once('.') {
        Some((w, f)) => (w, f),
        None => (body, ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return Err(ConfigError::Parse {
            key: key.to_owned(),
            detail: "empty".to_owned(),
        });
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(ConfigError::Parse {
            key: key.to_owned(),
            detail: format!("non-digit character in {value:?}"),
        });
    }
    let frac_len = frac.len() as u32;
    let composed: String = if frac_len <= decimals {
        let mut s = String::with_capacity(whole.len() + decimals as usize);
        s.push_str(whole);
        s.push_str(frac);
        for _ in 0..(decimals - frac_len) {
            s.push('0');
        }
        s
    } else {
        let mut s = String::with_capacity(whole.len() + decimals as usize);
        s.push_str(whole);
        s.push_str(&frac[..decimals as usize]);
        s
    };
    let raw = if composed.is_empty() { "0" } else { &composed };
    let unsigned: U256 = raw.parse().map_err(|e| ConfigError::Parse {
        key: key.to_owned(),
        detail: format!("{e}"),
    })?;
    let signed = I256::try_from(unsigned).map_err(|e| ConfigError::Range {
        key: key.to_owned(),
        detail: format!("{e}"),
    })?;
    Ok(if sign < 0 { -signed } else { signed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn get_required_finds_value() {
        let cfg = entries(&[("a", "1"), ("b", "2")]);
        assert_eq!(get_required(&cfg, "a").unwrap(), "1");
        assert_eq!(get_required(&cfg, "b").unwrap(), "2");
    }

    #[test]
    fn get_required_missing_is_typed_error() {
        let cfg = entries(&[("a", "1")]);
        let err = get_required(&cfg, "b").unwrap_err();
        assert!(matches!(err, ConfigError::MissingKey { ref key } if key == "b"));
    }

    #[test]
    fn get_optional_returns_none_for_missing() {
        let cfg = entries(&[("a", "1")]);
        assert_eq!(get_optional(&cfg, "missing"), None);
        assert_eq!(get_optional(&cfg, "a"), Some("1"));
    }

    #[test]
    fn scale_decimal_pads_short_fractional() {
        // "2500.00" with 8 decimals -> 2500 * 1e8 = 250_000_000_000
        let v = scale_decimal("2500.00", 8, "trigger").unwrap();
        assert_eq!(v, I256::try_from(250_000_000_000_i128).unwrap());
    }

    #[test]
    fn scale_decimal_truncates_long_fractional() {
        // "1.123456789" with 4 decimals -> "11234"
        let v = scale_decimal("1.123456789", 4, "trigger").unwrap();
        assert_eq!(v, I256::try_from(11234_i128).unwrap());
    }

    #[test]
    fn scale_decimal_handles_no_decimal_point() {
        let v = scale_decimal("42", 4, "x").unwrap();
        assert_eq!(v, I256::try_from(420_000_i128).unwrap());
    }

    #[test]
    fn scale_decimal_handles_negative() {
        let v = scale_decimal("-2.5", 2, "x").unwrap();
        assert_eq!(v, I256::try_from(-250_i128).unwrap());
    }

    #[test]
    fn scale_decimal_rejects_empty() {
        let err = scale_decimal("", 2, "x").unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { ref key, .. } if key == "x"),
            "got {err:?}"
        );
    }

    #[test]
    fn scale_decimal_rejects_garbage() {
        let err = scale_decimal("not-a-number", 2, "x").unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { ref key, .. } if key == "x"),
            "got {err:?}"
        );
    }
}
