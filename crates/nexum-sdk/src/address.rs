//! EVM address parsing helpers.
//!
//! Parses `[config]` values such as `addresses = "0xabc..., 0xdef..."`
//! and single `0x...` strings into typed [`Address`] values. The list
//! parser is permissive about whitespace and empty segments, so a
//! trailing comma is not an error.

use alloy_primitives::Address;

/// Typed errors from [`parse_address_list`] and [`parse_address`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AddressParse {
    /// An entry failed to parse as an EVM address; `index` is `0` for a
    /// single-address parse.
    #[error("address #{index} ({raw:?}): {message}")]
    InvalidAddress {
        /// Zero-based position in the list (counts skipped empties);
        /// `0` for single-address parses.
        index: usize,
        /// The trimmed source string that failed to parse.
        raw: String,
        /// Parse-error detail.
        message: String,
    },
    /// The list held no non-whitespace segment. Only from
    /// [`parse_address_list`].
    #[error("expected at least one address")]
    Empty,
}

/// Parse a comma-separated address list, trimming whitespace and
/// skipping empty segments. [`AddressParse::Empty`] on no segment,
/// [`AddressParse::InvalidAddress`] on the first bad entry (`index`
/// counts skipped empties).
pub fn parse_address_list(raw: &str) -> Result<Vec<Address>, AddressParse> {
    let mut out = Vec::new();
    for (i, part) in raw.split(',').enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let addr = trimmed
            .parse::<Address>()
            .map_err(|e| AddressParse::InvalidAddress {
                index: i,
                raw: trimmed.to_owned(),
                message: e.to_string(),
            })?;
        out.push(addr);
    }
    if out.is_empty() {
        return Err(AddressParse::Empty);
    }
    Ok(out)
}

/// Parse a single `0x...` (or bare-hex) address string, trimming
/// whitespace. Failures surface as [`AddressParse::InvalidAddress`]
/// with `index = 0`.
pub fn parse_address(raw: &str) -> Result<Address, AddressParse> {
    let trimmed = raw.trim();
    trimmed
        .parse::<Address>()
        .map_err(|e| AddressParse::InvalidAddress {
            index: 0,
            raw: trimmed.to_owned(),
            message: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn handles_whitespace_and_multiple() {
        let raw = "  0x70997970C51812dc3A010C7d01b50e0d17dc79C8 ,\
                   0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
        let parsed = parse_address_list(raw).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0],
            address!("70997970C51812dc3A010C7d01b50e0d17dc79C8"),
        );
    }

    #[test]
    fn skips_empty_segments() {
        let parsed = parse_address_list("0x70997970C51812dc3A010C7d01b50e0d17dc79C8,,").unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn rejects_empty_list() {
        assert!(matches!(parse_address_list(""), Err(AddressParse::Empty)));
        assert!(matches!(
            parse_address_list(", ,"),
            Err(AddressParse::Empty)
        ));
    }

    #[test]
    fn rejects_malformed_entry() {
        match parse_address_list("not-an-address") {
            Err(AddressParse::InvalidAddress { index, raw, .. }) => {
                assert_eq!(index, 0);
                assert_eq!(raw, "not-an-address");
            }
            other => panic!("expected InvalidAddress, got {other:?}"),
        }
    }

    #[test]
    fn parse_address_accepts_canonical() {
        let parsed = parse_address("  0x70997970C51812dc3A010C7d01b50e0d17dc79C8  ").unwrap();
        assert_eq!(parsed, address!("70997970C51812dc3A010C7d01b50e0d17dc79C8"));
    }

    #[test]
    fn parse_address_rejects_wrong_length() {
        match parse_address("0xdeadbeef") {
            Err(AddressParse::InvalidAddress { index, raw, .. }) => {
                assert_eq!(index, 0);
                assert_eq!(raw, "0xdeadbeef");
            }
            other => panic!("expected InvalidAddress, got {other:?}"),
        }
    }
}
