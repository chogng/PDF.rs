use std::fmt;

use crate::{ContentVmError, ContentVmErrorCode};

const MAX_SCALED_DIGITS: i128 = 19;

/// Exact signed PDF content number represented with nine decimal fractional digits.
///
/// Construction never uses floating-point arithmetic. Negative zero is normalized to zero, and
/// values that would require discarding nonzero decimal digits are rejected explicitly.
#[derive(Clone, Copy, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentNumber(i64);

impl ContentNumber {
    /// Exact zero.
    pub const ZERO: Self = Self(0);

    /// Exact one.
    pub const ONE: Self = Self(1_000_000_000);

    /// Parses one complete PDF decimal number lexeme, including optional exponent notation.
    ///
    /// Integer, fractional, and exponent forms share the same exact conversion. Syntax errors,
    /// precision loss beyond nine decimal places, and signed fixed-point overflow have distinct
    /// stable [`ContentVmErrorCode`] values.
    pub fn parse(raw: &[u8]) -> Result<Self, ContentVmError> {
        parse_content_number(raw)
    }

    /// Converts a scanner-owned PDF integer to the same exact nine-decimal representation.
    pub fn from_integer(value: i64) -> Result<Self, ContentVmError> {
        value
            .checked_mul(1_000_000_000)
            .map(Self)
            .ok_or_else(numeric_overflow)
    }

    /// Creates a value from its canonical nine-decimal scaled integer.
    pub const fn from_scaled(value: i64) -> Self {
        Self(value)
    }

    /// Returns the canonical nine-decimal scaled integer.
    pub const fn scaled(self) -> i64 {
        self.0
    }
}

impl fmt::Debug for ContentNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentNumber")
            .field("scaled", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Copy)]
enum ParsedExponent {
    Finite(i128),
    PositiveOverflow,
    NegativeOverflow,
}

fn parse_content_number(raw: &[u8]) -> Result<ContentNumber, ContentVmError> {
    if raw.is_empty() {
        return Err(invalid_number());
    }
    let (negative, unsigned) = match raw[0] {
        b'-' => (true, &raw[1..]),
        b'+' => (false, &raw[1..]),
        _ => (false, raw),
    };
    if unsigned.is_empty() {
        return Err(invalid_number());
    }

    let mut mantissa_end = unsigned.len();
    let mut seen_decimal = false;
    let mut total_digits = 0_usize;
    let mut fractional_digits = 0_usize;
    let mut first_nonzero = None;
    let mut last_nonzero = 0_usize;

    for (index, byte) in unsigned.iter().copied().enumerate() {
        match byte {
            b'0'..=b'9' => {
                let ordinal = total_digits;
                total_digits = total_digits.checked_add(1).ok_or_else(numeric_overflow)?;
                if seen_decimal {
                    fractional_digits = fractional_digits
                        .checked_add(1)
                        .ok_or_else(numeric_overflow)?;
                }
                if byte != b'0' {
                    first_nonzero.get_or_insert(ordinal);
                    last_nonzero = ordinal;
                }
            }
            b'.' if !seen_decimal => seen_decimal = true,
            b'e' | b'E' => {
                mantissa_end = index;
                break;
            }
            _ => return Err(invalid_number()),
        }
    }
    if total_digits == 0 {
        return Err(invalid_number());
    }

    let exponent = if mantissa_end == unsigned.len() {
        ParsedExponent::Finite(0)
    } else {
        parse_exponent(&unsigned[mantissa_end + 1..])?
    };
    let Some(first_nonzero) = first_nonzero else {
        return Ok(ContentNumber::ZERO);
    };
    let trailing_zeros = total_digits
        .checked_sub(last_nonzero)
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(numeric_overflow)?;
    let significant_digits = last_nonzero
        .checked_sub(first_nonzero)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(numeric_overflow)?;

    let exponent = match exponent {
        ParsedExponent::Finite(value) => value,
        ParsedExponent::PositiveOverflow => return Err(numeric_overflow()),
        ParsedExponent::NegativeOverflow => return Err(numeric_precision()),
    };
    let fractional_digits = i128::try_from(fractional_digits).map_err(|_| numeric_precision())?;
    let trailing_zeros = i128::try_from(trailing_zeros).map_err(|_| numeric_overflow())?;
    let decimal_shift = exponent
        .checked_sub(fractional_digits)
        .ok_or_else(numeric_precision)?
        .checked_add(9)
        .ok_or_else(|| {
            if exponent.is_negative() {
                numeric_precision()
            } else {
                numeric_overflow()
            }
        })?
        .checked_add(trailing_zeros)
        .ok_or_else(numeric_overflow)?;
    if decimal_shift < 0 {
        return Err(numeric_precision());
    }

    let significant_digits = i128::try_from(significant_digits).map_err(|_| numeric_overflow())?;
    let scaled_digits = significant_digits
        .checked_add(decimal_shift)
        .ok_or_else(numeric_overflow)?;
    if scaled_digits > MAX_SCALED_DIGITS {
        return Err(numeric_overflow());
    }

    let mut ordinal = 0_usize;
    let mut magnitude = 0_u64;
    for byte in unsigned[..mantissa_end].iter().copied() {
        if !byte.is_ascii_digit() {
            continue;
        }
        if ordinal >= first_nonzero && ordinal <= last_nonzero {
            magnitude = magnitude
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .ok_or_else(numeric_overflow)?;
        }
        ordinal = ordinal.checked_add(1).ok_or_else(numeric_overflow)?;
    }
    let shift = u32::try_from(decimal_shift).map_err(|_| numeric_overflow())?;
    let scaled_magnitude = magnitude
        .checked_mul(10_u64.checked_pow(shift).ok_or_else(numeric_overflow)?)
        .ok_or_else(numeric_overflow)?;
    signed_scaled(scaled_magnitude, negative)
}

fn parse_exponent(raw: &[u8]) -> Result<ParsedExponent, ContentVmError> {
    if raw.is_empty() {
        return Err(invalid_number());
    }
    let (negative, digits) = match raw[0] {
        b'-' => (true, &raw[1..]),
        b'+' => (false, &raw[1..]),
        _ => (false, raw),
    };
    if digits.is_empty() {
        return Err(invalid_number());
    }

    let mut magnitude = Some(0_i128);
    for byte in digits {
        if !byte.is_ascii_digit() {
            return Err(invalid_number());
        }
        if let Some(value) = magnitude {
            magnitude = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(i128::from(*byte - b'0')));
        }
    }
    match magnitude {
        Some(value) if negative => value
            .checked_neg()
            .map(ParsedExponent::Finite)
            .ok_or_else(numeric_precision),
        Some(value) => Ok(ParsedExponent::Finite(value)),
        None if negative => Ok(ParsedExponent::NegativeOverflow),
        None => Ok(ParsedExponent::PositiveOverflow),
    }
}

fn signed_scaled(magnitude: u64, negative: bool) -> Result<ContentNumber, ContentVmError> {
    if magnitude == 0 {
        return Ok(ContentNumber::ZERO);
    }
    if !negative {
        return i64::try_from(magnitude)
            .map(ContentNumber)
            .map_err(|_| numeric_overflow());
    }
    let minimum_magnitude = (i64::MAX as u64) + 1;
    if magnitude > minimum_magnitude {
        return Err(numeric_overflow());
    }
    if magnitude == minimum_magnitude {
        Ok(ContentNumber(i64::MIN))
    } else {
        let value = i64::try_from(magnitude).map_err(|_| numeric_overflow())?;
        Ok(ContentNumber(-value))
    }
}

fn invalid_number() -> ContentVmError {
    ContentVmError::new(ContentVmErrorCode::InvalidNumber, None)
}

fn numeric_precision() -> ContentVmError {
    ContentVmError::new(ContentVmErrorCode::NumericPrecision, None)
}

fn numeric_overflow() -> ContentVmError {
    ContentVmError::new(ContentVmErrorCode::NumericOverflow, None)
}
