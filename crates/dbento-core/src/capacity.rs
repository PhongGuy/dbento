//! DynamoDB read and write capacity, as a statement's pages report it.

use std::fmt;

/// Capacity units a statement consumed. DynamoDB reports fractional units, so the total is
/// kept in thousandths of a unit, which makes sums exact and the value hashable.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapacityUnits(u64);

impl CapacityUnits {
    /// A value from thousandths of a unit, for shells and tests that build one directly.
    pub fn from_thousandths(thousandths: u64) -> Self {
        Self(thousandths)
    }

    /// The units as a float, for a shell that charts or compares them.
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / 1000.0
    }

    /// A reading rounded to the nearest thousandth. A negative or NaN reading is zero, and
    /// infinity saturates, since a float-to-int `as` cast clamps and maps NaN to zero.
    #[cfg_attr(not(test), allow(dead_code))] // the DynamoDB driver is the only caller
    pub(crate) fn from_units(units: f64) -> Self {
        Self((units * 1000.0).round().max(0.0) as u64)
    }

    /// The sum of two page totals, clamped at the largest value instead of wrapping.
    #[cfg_attr(not(test), allow(dead_code))] // the DynamoDB driver is the only caller
    pub(crate) fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl fmt::Display for CapacityUnits {
    /// A trimmed decimal: "12.5", "0.001", and "3" for a whole number.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (whole, frac) = (self.0 / 1000, self.0 % 1000);
        if frac == 0 {
            return write!(f, "{whole}");
        }
        let digits = format!("{frac:03}");
        write!(f, "{whole}.{}", digits.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests;
