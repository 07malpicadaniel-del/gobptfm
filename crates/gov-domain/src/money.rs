use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::DomainError;

/// Exact integer monetary representation in cents (1 MXN = 100 cents).
/// Strictly forbids floating-point arithmetic (`f32`/`f64`) across the entire domain.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MoneyCents(i64);

impl MoneyCents {
    /// Constant representing `$0.00` (`0` cents).
    pub const ZERO: Self = Self(0);

    /// Basis points scale where `10_000 bps == 100.00%`.
    pub const BPS_SCALE: i128 = 10_000;

    /// Constructs a [`MoneyCents`] value from raw signed cents.
    #[must_use]
    pub const fn from_cents(cents: i64) -> Self {
        Self(cents)
    }

    /// Constructs a [`MoneyCents`] value ensuring it is greater than or equal to zero (`>= 0`).
    ///
    /// # Errors
    /// Returns [`DomainError::NegativeAmountNotAllowed`] if `cents < 0`.
    pub const fn from_non_negative(cents: i64) -> Result<Self, DomainError> {
        if cents < 0 {
            return Err(DomainError::NegativeAmountNotAllowed(cents));
        }
        Ok(Self(cents))
    }

    /// Constructs a [`MoneyCents`] value ensuring it is strictly positive (`> 0`).
    ///
    /// # Errors
    /// Returns [`DomainError::NegativeAmountNotAllowed`] if `cents <= 0`.
    pub const fn from_positive(cents: i64) -> Result<Self, DomainError> {
        if cents <= 0 {
            return Err(DomainError::NegativeAmountNotAllowed(cents));
        }
        Ok(Self(cents))
    }

    /// Returns the underlying integer value in cents.
    #[must_use]
    pub const fn as_cents(self) -> i64 {
        self.0
    }

    /// Returns `true` if the amount is strictly greater than zero.
    #[must_use]
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// Returns `true` if the amount is strictly less than zero.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Returns `true` if the amount is exactly zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Checked addition of two [`MoneyCents`] amounts.
    ///
    /// # Errors
    /// Returns [`DomainError::ArithmeticOverflow`] on integer overflow.
    pub const fn checked_add(self, other: Self) -> Result<Self, DomainError> {
        match self.0.checked_add(other.0) {
            Some(sum) => Ok(Self(sum)),
            None => Err(DomainError::ArithmeticOverflow),
        }
    }

    /// Checked subtraction of two [`MoneyCents`] amounts.
    ///
    /// # Errors
    /// Returns [`DomainError::ArithmeticOverflow`] on integer overflow.
    pub const fn checked_sub(self, other: Self) -> Result<Self, DomainError> {
        match self.0.checked_sub(other.0) {
            Some(diff) => Ok(Self(diff)),
            None => Err(DomainError::ArithmeticOverflow),
        }
    }

    /// Checked multiplication by an integer scalar factor.
    ///
    /// # Errors
    /// Returns [`DomainError::ArithmeticOverflow`] on integer overflow.
    pub const fn checked_mul_i64(self, factor: i64) -> Result<Self, DomainError> {
        match self.0.checked_mul(factor) {
            Some(product) => Ok(Self(product)),
            None => Err(DomainError::ArithmeticOverflow),
        }
    }

    /// Applies basis points (`10_000 bps = 100.00%`, e.g., `1500 bps = 15.00%`) using
    /// 128-bit integer arithmetic and Banker's Rounding (`MidpointNearestEven`).
    ///
    /// # Errors
    /// Returns [`DomainError::ArithmeticOverflow`] if the result overflows `i64`.
    pub fn apply_basis_points(self, bps: u32) -> Result<Self, DomainError> {
        let cents = i128::from(self.0);
        let bps_val = i128::from(bps);
        let numerator = cents
            .checked_mul(bps_val)
            .ok_or(DomainError::ArithmeticOverflow)?;

        let quotient = numerator / Self::BPS_SCALE;
        let remainder = numerator % Self::BPS_SCALE;
        let abs_remainder = remainder.abs();
        let half_scale = Self::BPS_SCALE / 2;

        let step: i128 = if numerator >= 0 { 1 } else { -1 };

        let rounded = if abs_remainder < half_scale {
            quotient
        } else if abs_remainder > half_scale {
            quotient
                .checked_add(step)
                .ok_or(DomainError::ArithmeticOverflow)?
        } else {
            // Exact midpoint (.5 cents): round half to nearest even integer (MidpointNearestEven)
            if quotient % 2 == 0 {
                quotient
            } else {
                quotient
                    .checked_add(step)
                    .ok_or(DomainError::ArithmeticOverflow)?
            }
        };

        let final_cents = i64::try_from(rounded).map_err(|_| DomainError::ArithmeticOverflow)?;
        Ok(Self(final_cents))
    }
}

impl fmt::Display for MoneyCents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = i128::from(self.0).abs();
        let whole = abs / 100;
        let frac = abs % 100;
        write!(f, "{sign}${whole}.{frac:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_non_negative_constructor() {
        assert_eq!(MoneyCents::from_non_negative(0), Ok(MoneyCents::ZERO));
        assert_eq!(
            MoneyCents::from_non_negative(15_000),
            Ok(MoneyCents::from_cents(15_000))
        );
        assert_eq!(
            MoneyCents::from_non_negative(-1),
            Err(DomainError::NegativeAmountNotAllowed(-1))
        );
    }

    #[test]
    fn test_checked_arithmetic_and_overflow() {
        let a = MoneyCents::from_cents(10_000);
        let b = MoneyCents::from_cents(2_550);
        assert_eq!(a.checked_add(b), Ok(MoneyCents::from_cents(12_550)));
        assert_eq!(a.checked_sub(b), Ok(MoneyCents::from_cents(7_450)));
        assert_eq!(b.checked_mul_i64(4), Ok(MoneyCents::from_cents(10_200)));

        let max = MoneyCents::from_cents(i64::MAX);
        assert_eq!(
            max.checked_add(MoneyCents::from_cents(1)),
            Err(DomainError::ArithmeticOverflow)
        );
        assert_eq!(
            max.checked_mul_i64(2),
            Err(DomainError::ArithmeticOverflow)
        );
    }

    #[test]
    fn test_apply_basis_points_discounts_and_bankers_rounding() {
        let base_tax = MoneyCents::from_cents(250_000); // $2,500.00 MXN

        // 20% Pronto Pago discount (2000 bps) -> $500.00 (50_000 cents)
        assert_eq!(
            base_tax.apply_basis_points(2_000),
            Ok(MoneyCents::from_cents(50_000))
        );

        // 50% INAPAM senior discount (5000 bps) -> $1,250.00 (125_000 cents)
        assert_eq!(
            base_tax.apply_basis_points(5_000),
            Ok(MoneyCents::from_cents(125_000))
        );

        // 15% surcharge (1500 bps) -> $375.00 (37_500 cents)
        assert_eq!(
            base_tax.apply_basis_points(1_500),
            Ok(MoneyCents::from_cents(37_500))
        );

        // Banker's rounding (MidpointNearestEven) on exact .5 cent midpoints:
        // 1 cent * 50% = 0.5 cents -> nearest even is 0
        assert_eq!(
            MoneyCents::from_cents(1).apply_basis_points(5_000),
            Ok(MoneyCents::from_cents(0))
        );
        // 3 cents * 50% = 1.5 cents -> nearest even is 2
        assert_eq!(
            MoneyCents::from_cents(3).apply_basis_points(5_000),
            Ok(MoneyCents::from_cents(2))
        );
        // 5 cents * 50% = 2.5 cents -> nearest even is 2
        assert_eq!(
            MoneyCents::from_cents(5).apply_basis_points(5_000),
            Ok(MoneyCents::from_cents(2))
        );
        // 7 cents * 50% = 3.5 cents -> nearest even is 4
        assert_eq!(
            MoneyCents::from_cents(7).apply_basis_points(5_000),
            Ok(MoneyCents::from_cents(4))
        );
    }
}
