use crate::{SceneError, SceneErrorCode};

const SCALE: i128 = 1_000_000_000;
const SCALE_U128: u128 = 1_000_000_000;
const FRACTION_DIGITS: usize = 9;

/// Signed fixed-point Scene number with nine decimal fractional digits.
///
/// Canonical output uses the raw scaled integer. This avoids platform floating-point formatting,
/// normalizes negative zero, and makes precision loss and overflow explicit.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SceneScalar(i64);

impl SceneScalar {
    /// Exact zero.
    pub const ZERO: Self = Self(0);

    /// Exact one.
    pub const ONE: Self = Self(1_000_000_000);

    /// Creates a value from its canonical nine-decimal scaled integer.
    pub const fn from_scaled(value: i64) -> Self {
        Self(value)
    }

    /// Parses one finite decimal without exponent notation.
    ///
    /// At most nine fractional digits are accepted. A leading sign is optional, integer or
    /// fractional digits may be omitted only when the other component is present, and no
    /// whitespace is ignored.
    pub fn from_decimal(input: &str) -> Result<Self, SceneError> {
        let bytes = input.as_bytes();
        if bytes.is_empty() {
            return Err(SceneError::for_code(SceneErrorCode::InvalidScalar, None));
        }
        let (negative, digits) = match bytes[0] {
            b'-' => (true, &bytes[1..]),
            b'+' => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        if digits.is_empty() {
            return Err(SceneError::for_code(SceneErrorCode::InvalidScalar, None));
        }

        let mut separator = None;
        for (index, byte) in digits.iter().copied().enumerate() {
            if byte == b'.' {
                if separator.replace(index).is_some() {
                    return Err(SceneError::for_code(SceneErrorCode::InvalidScalar, None));
                }
            } else if !byte.is_ascii_digit() {
                return Err(SceneError::for_code(SceneErrorCode::InvalidScalar, None));
            }
        }
        let (integer, fraction) = match separator {
            Some(index) => (&digits[..index], &digits[index + 1..]),
            None => (digits, &[][..]),
        };
        if integer.is_empty() && fraction.is_empty() {
            return Err(SceneError::for_code(SceneErrorCode::InvalidScalar, None));
        }
        if fraction.len() > FRACTION_DIGITS {
            return Err(SceneError::for_code(SceneErrorCode::ScalarPrecision, None));
        }

        let integer = parse_digits(integer)?;
        let fraction_value = parse_digits(fraction)?;
        let padding = FRACTION_DIGITS - fraction.len();
        let fraction_scale = 10_u128
            .checked_pow(
                u32::try_from(padding)
                    .map_err(|_| SceneError::for_code(SceneErrorCode::InternalState, None))?,
            )
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::InternalState, None))?;
        let magnitude = integer
            .checked_mul(SCALE_U128)
            .and_then(|value| value.checked_add(fraction_value.checked_mul(fraction_scale)?))
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
        if magnitude == 0 {
            return Ok(Self::ZERO);
        }

        let value = if negative {
            let minimum_magnitude = (i64::MAX as u128) + 1;
            if magnitude > minimum_magnitude {
                return Err(SceneError::for_code(SceneErrorCode::NumericOverflow, None));
            }
            if magnitude == minimum_magnitude {
                i64::MIN
            } else {
                -i64::try_from(magnitude)
                    .map_err(|_| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?
            }
        } else {
            i64::try_from(magnitude)
                .map_err(|_| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?
        };
        Ok(Self(value))
    }

    /// Returns the canonical scaled integer.
    pub const fn scaled(self) -> i64 {
        self.0
    }

    /// Adds two values with checked overflow.
    pub fn checked_add(self, other: Self) -> Result<Self, SceneError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
    }

    /// Subtracts two values with checked overflow.
    pub fn checked_sub(self, other: Self) -> Result<Self, SceneError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
    }

    /// Multiplies two values, rounding exact half units away from zero.
    pub fn checked_mul(self, other: Self) -> Result<Self, SceneError> {
        scaled_sum(&[(self.0, other.0)], 0)
    }
}

fn parse_digits(bytes: &[u8]) -> Result<u128, SceneError> {
    let mut value = 0_u128;
    for byte in bytes {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u128::from(*byte - b'0')))
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
    }
    Ok(value)
}

/// Six-component affine transform using checked Scene fixed-point values.
///
/// Components map a point as `x' = a*x + c*y + e` and `y' = b*x + d*y + f`.
/// Singular transforms are valid Scene semantics; this type deliberately exposes no inversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Matrix {
    components: [SceneScalar; 6],
}

impl Matrix {
    /// Exact identity transform.
    pub const IDENTITY: Self = Self {
        components: [
            SceneScalar::ONE,
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::ONE,
            SceneScalar::ZERO,
            SceneScalar::ZERO,
        ],
    };

    /// Creates a matrix from `[a, b, c, d, e, f]`.
    pub const fn new(components: [SceneScalar; 6]) -> Self {
        Self { components }
    }

    /// Returns `[a, b, c, d, e, f]`.
    pub const fn components(self) -> [SceneScalar; 6] {
        self.components
    }

    /// Returns checked matrix multiplication `self × other`.
    ///
    /// Each output component rounds once after its complete product sum, using exact-half-away-
    /// from-zero rounding.
    pub fn checked_multiply(self, other: Self) -> Result<Self, SceneError> {
        let [a1, b1, c1, d1, e1, f1] = self.components.map(SceneScalar::scaled);
        let [a2, b2, c2, d2, e2, f2] = other.components.map(SceneScalar::scaled);
        Ok(Self::new([
            scaled_sum(&[(a1, a2), (c1, b2)], 0)?,
            scaled_sum(&[(b1, a2), (d1, b2)], 0)?,
            scaled_sum(&[(a1, c2), (c1, d2)], 0)?,
            scaled_sum(&[(b1, c2), (d1, d2)], 0)?,
            scaled_sum(&[(a1, e2), (c1, f2)], e1)?,
            scaled_sum(&[(b1, e2), (d1, f2)], f1)?,
        ]))
    }

    /// Applies this affine transform to one exact user-space point.
    pub fn checked_transform_point(
        self,
        point: crate::ScenePoint,
    ) -> Result<crate::ScenePoint, SceneError> {
        let [a, b, c, d, e, f] = self.components.map(SceneScalar::scaled);
        Ok(crate::ScenePoint::new(
            scaled_sum(&[(a, point.x().scaled()), (c, point.y().scaled())], e)?,
            scaled_sum(&[(b, point.x().scaled()), (d, point.y().scaled())], f)?,
        ))
    }
}

fn scaled_sum(products: &[(i64, i64)], addend: i64) -> Result<SceneScalar, SceneError> {
    let mut numerator = i128::from(addend)
        .checked_mul(SCALE)
        .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
    for &(left, right) in products {
        numerator = numerator
            .checked_add(
                i128::from(left)
                    .checked_mul(i128::from(right))
                    .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?,
            )
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?;
    }
    let quotient = numerator / SCALE;
    let remainder = numerator % SCALE;
    let rounded = if remainder.abs() * 2 >= SCALE {
        quotient
            .checked_add(if numerator.is_negative() { -1 } else { 1 })
            .ok_or_else(|| SceneError::for_code(SceneErrorCode::NumericOverflow, None))?
    } else {
        quotient
    };
    i64::try_from(rounded)
        .map(SceneScalar)
        .map_err(|_| SceneError::for_code(SceneErrorCode::NumericOverflow, None))
}
