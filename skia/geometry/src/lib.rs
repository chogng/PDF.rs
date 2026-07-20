use pdf_rs_skia_error::{SkiaError, SkiaErrorCode};

const FRACTION_BITS: i32 = 16;
const SCALE: i64 = 1_i64 << FRACTION_BITS;

/// Signed Q16.16 coordinate used by the reusable canvas API.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Scalar(i32);

impl Scalar {
    /// Exact zero.
    pub const ZERO: Self = Self(0);

    /// Creates an exact whole-number coordinate.
    pub fn from_i32(value: i32) -> Result<Self, SkiaError> {
        value
            .checked_shl(FRACTION_BITS as u32)
            .map(Self)
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))
    }

    /// Creates a checked coordinate from a rational value, rounding ties away from zero.
    pub fn from_ratio(numerator: i64, denominator: i64) -> Result<Self, SkiaError> {
        if denominator == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        let scaled = i128::from(numerator)
            .checked_mul(i128::from(SCALE))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        let denominator = i128::from(denominator);
        let magnitude = scaled.unsigned_abs();
        let divisor = denominator.unsigned_abs();
        let rounded = magnitude
            .checked_add(divisor / 2)
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?
            / divisor;
        let signed = if (scaled < 0) == (denominator < 0) {
            i128::try_from(rounded).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))?
        } else {
            -i128::try_from(rounded).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))?
        };
        i32::try_from(signed)
            .map(Self)
            .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
    }

    /// Returns the exact Q16.16 storage value.
    pub const fn bits(self) -> i32 {
        self.0
    }

    /// Creates a scalar from exact Q16.16 storage for backend implementations.
    pub const fn from_bits(bits: i32) -> Self {
        Self(bits)
    }
}

/// One Q16.16 canvas point.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Point {
    x: Scalar,
    y: Scalar,
}

impl Point {
    /// Creates a point from checked fixed-point coordinates.
    pub const fn new(x: Scalar, y: Scalar) -> Self {
        Self { x, y }
    }

    /// Returns the horizontal coordinate.
    pub const fn x(self) -> Scalar {
        self.x
    }

    /// Returns the vertical coordinate.
    pub const fn y(self) -> Scalar {
        self.y
    }
}

/// Positive-area axis-aligned canvas rectangle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Rect {
    left: Scalar,
    top: Scalar,
    right: Scalar,
    bottom: Scalar,
}

impl Rect {
    /// Creates a positive-area rectangle in top-left canvas coordinates.
    pub fn new(
        left: Scalar,
        top: Scalar,
        right: Scalar,
        bottom: Scalar,
    ) -> Result<Self, SkiaError> {
        if left >= right || top >= bottom {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }

    /// Returns the left edge.
    pub const fn left(self) -> Scalar {
        self.left
    }

    /// Returns the top edge.
    pub const fn top(self) -> Scalar {
        self.top
    }

    /// Returns the right edge.
    pub const fn right(self) -> Scalar {
        self.right
    }

    /// Returns the bottom edge.
    pub const fn bottom(self) -> Scalar {
        self.bottom
    }
}

/// Checked affine transform in Q16.16 coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Transform {
    a: Scalar,
    b: Scalar,
    c: Scalar,
    d: Scalar,
    e: Scalar,
    f: Scalar,
}

impl Transform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        a: Scalar::from_bits(1 << FRACTION_BITS),
        b: Scalar::ZERO,
        c: Scalar::ZERO,
        d: Scalar::from_bits(1 << FRACTION_BITS),
        e: Scalar::ZERO,
        f: Scalar::ZERO,
    };

    /// Creates one affine transform with PDF/Canvas coefficient order `(a, b, c, d, e, f)`.
    pub const fn new(a: Scalar, b: Scalar, c: Scalar, d: Scalar, e: Scalar, f: Scalar) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Creates an exact translation transform.
    pub const fn translate(x: Scalar, y: Scalar) -> Self {
        Self::new(
            Scalar::from_bits(1 << FRACTION_BITS),
            Scalar::ZERO,
            Scalar::ZERO,
            Scalar::from_bits(1 << FRACTION_BITS),
            x,
            y,
        )
    }

    /// Creates an axis-aligned scale transform.
    pub const fn scale(x: Scalar, y: Scalar) -> Self {
        Self::new(x, Scalar::ZERO, Scalar::ZERO, y, Scalar::ZERO, Scalar::ZERO)
    }

    /// Returns the affine transform produced by applying `self` and then `next`.
    ///
    /// This order matches canvas state concatenation: `current.concat(next)`
    /// maps a point through `current` before mapping it through `next`.
    pub fn concat(self, next: Self) -> Result<Self, SkiaError> {
        Ok(Self::new(
            multiply_add(next.a, self.a, next.c, self.b)?,
            multiply_add(next.b, self.a, next.d, self.b)?,
            multiply_add(next.a, self.c, next.c, self.d)?,
            multiply_add(next.b, self.c, next.d, self.d)?,
            checked_affine(next.a, self.e, next.c, self.f, next.e).map(Scalar::from_bits)?,
            checked_affine(next.b, self.e, next.d, self.f, next.f).map(Scalar::from_bits)?,
        ))
    }

    /// Maps a point with checked Q16.16 arithmetic.
    pub fn map_point(self, point: Point) -> Result<Point, SkiaError> {
        Ok(Point::new(
            Scalar::from_bits(checked_affine(self.a, point.x, self.c, point.y, self.e)?),
            Scalar::from_bits(checked_affine(self.b, point.x, self.d, point.y, self.f)?),
        ))
    }

    /// Returns whether this transform preserves axis-aligned rectangles.
    pub const fn is_axis_aligned(self) -> bool {
        self.b.bits() == 0 && self.c.bits() == 0
    }
}

fn multiply_add(
    first_coefficient: Scalar,
    first_value: Scalar,
    second_coefficient: Scalar,
    second_value: Scalar,
) -> Result<Scalar, SkiaError> {
    checked_affine(
        first_coefficient,
        first_value,
        second_coefficient,
        second_value,
        Scalar::ZERO,
    )
    .map(Scalar::from_bits)
}

fn checked_affine(
    first_coefficient: Scalar,
    first_value: Scalar,
    second_coefficient: Scalar,
    second_value: Scalar,
    offset: Scalar,
) -> Result<i32, SkiaError> {
    let product = i128::from(first_coefficient.bits())
        .checked_mul(i128::from(first_value.bits()))
        .and_then(|value| {
            i128::from(second_coefficient.bits())
                .checked_mul(i128::from(second_value.bits()))
                .and_then(|other| value.checked_add(other))
        })
        .and_then(|value| value.checked_add(i128::from(offset.bits()) << FRACTION_BITS))
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
    let rounded = if product >= 0 {
        product
            .checked_add(i128::from(SCALE / 2))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?
            / i128::from(SCALE)
    } else {
        -((-product
            .checked_add(i128::from(SCALE / 2))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?)
            / i128::from(SCALE))
    };
    i32::try_from(rounded).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}
