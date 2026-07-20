use pdf_rs_skia_error::{SkiaError, SkiaErrorCode};
use pdf_rs_skia_geometry::{Point, Scalar};

pub(super) const KAPPA_BITS: i32 = 36_195;

pub(super) fn midpoint(first: Scalar, second: Scalar) -> Result<Scalar, SkiaError> {
    let sum = i64::from(first.bits())
        .checked_add(i64::from(second.bits()))
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
    let rounded = if sum >= 0 {
        (sum + 1) / 2
    } else {
        (sum - 1) / 2
    };
    i32::try_from(rounded)
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

pub(super) fn subtract(left: Scalar, right: Scalar) -> Result<Scalar, SkiaError> {
    i32::try_from(i64::from(left.bits()) - i64::from(right.bits()))
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

pub(super) fn half_extent(first: Scalar, second: Scalar) -> Result<Scalar, SkiaError> {
    let difference = i64::from(second.bits()) - i64::from(first.bits());
    let rounded = (difference + 1) / 2;
    i32::try_from(rounded)
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

pub(super) fn negate(value: Scalar) -> Result<Scalar, SkiaError> {
    value
        .bits()
        .checked_neg()
        .map(Scalar::from_bits)
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))
}

pub(super) fn point_offset(
    x: Scalar,
    offset_x: Scalar,
    y: Scalar,
    offset_y: Scalar,
) -> Result<Point, SkiaError> {
    Ok(Point::new(add(x, offset_x)?, add(y, offset_y)?))
}

pub(super) fn add(left: Scalar, right: Scalar) -> Result<Scalar, SkiaError> {
    left.bits()
        .checked_add(right.bits())
        .map(Scalar::from_bits)
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))
}

pub(super) fn scale_kappa(value: Scalar) -> Result<Scalar, SkiaError> {
    let product = i64::from(value.bits())
        .checked_mul(i64::from(KAPPA_BITS))
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
    let rounded = if product >= 0 {
        (product + (1_i64 << 15)) >> 16
    } else {
        -((-product + (1_i64 << 15)) >> 16)
    };
    i32::try_from(rounded)
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

pub(super) fn min_scalar(left: Scalar, right: Scalar) -> Scalar {
    if left <= right { left } else { right }
}

pub(super) fn max_scalar(left: Scalar, right: Scalar) -> Scalar {
    if left >= right { left } else { right }
}
