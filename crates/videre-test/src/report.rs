//! The conformance verdict: every check in this crate either passes or
//! returns a [`ConformanceReport`] naming each vector that failed.

use std::error::Error;
use std::fmt;

/// One vector or golden the subject failed, with the detail to fix it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Violation {
    /// The `name` of the failing vector or golden.
    pub vector: String,
    /// What diverged: the expected and observed outcome.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.vector, self.detail)
    }
}

/// Every violation a conformance check found, one per failing vector;
/// checks never stop at the first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceReport {
    /// The violations, in vector order.
    pub violations: Vec<Violation>,
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} conformance violation(s):", self.violations.len())?;
        for violation in &self.violations {
            writeln!(f, "  {violation}")?;
        }
        Ok(())
    }
}

impl Error for ConformanceReport {}

/// Fold collected violations into the check's verdict.
pub(crate) fn settle(violations: Vec<Violation>) -> Result<(), ConformanceReport> {
    if violations.is_empty() {
        Ok(())
    } else {
        Err(ConformanceReport { violations })
    }
}
