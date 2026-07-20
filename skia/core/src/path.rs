use crate::{Point, Rect, Scalar, SkiaError, SkiaErrorCode};

const KAPPA_BITS: i32 = 36_195;

/// Cardinal start point for an ellipse arc.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArcStart {
    /// The right-most point of the ellipse.
    Right,
    /// The bottom-most point of the ellipse.
    Bottom,
    /// The left-most point of the ellipse.
    Left,
    /// The top-most point of the ellipse.
    Top,
}

/// Direction in which an ellipse arc is traced.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArcDirection {
    /// Traces toward increasing canvas angles: right, bottom, left, then top.
    Clockwise,
    /// Traces in the reverse direction.
    CounterClockwise,
}

/// Fill decision for closed path contours.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FillRule {
    /// A point is inside when it has odd crossing parity.
    EvenOdd,
    /// A point is inside when its signed winding number is non-zero.
    NonZero,
}

/// One immutable vector-path operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PathVerb {
    /// Starts a new contour.
    MoveTo(Point),
    /// Appends a straight segment to the active contour.
    LineTo(Point),
    /// Appends a quadratic Bézier segment to the active contour.
    QuadTo(Point, Point),
    /// Appends a cubic Bézier segment to the active contour.
    CubicTo(Point, Point, Point),
    /// Closes the active contour to its starting point.
    Close,
}

/// Immutable path containing line and Bézier contours.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path {
    verbs: Vec<PathVerb>,
}

impl Path {
    /// Borrows path operations in declaration order.
    pub fn verbs(&self) -> &[PathVerb] {
        &self.verbs
    }
}

/// Bounded, fallible builder for an immutable vector path.
#[derive(Debug)]
pub struct PathBuilder {
    verbs: Vec<PathVerb>,
    has_active_contour: bool,
    max_verbs: usize,
}

impl PathBuilder {
    /// Creates a builder with a positive maximum number of path operations.
    pub fn new(max_verbs: usize) -> Result<Self, SkiaError> {
        if max_verbs == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidLimits));
        }
        Ok(Self {
            verbs: Vec::new(),
            has_active_contour: false,
            max_verbs,
        })
    }

    /// Starts a new contour.
    pub fn move_to(&mut self, point: Point) -> Result<(), SkiaError> {
        self.push(PathVerb::MoveTo(point))?;
        self.has_active_contour = true;
        Ok(())
    }

    /// Appends a line to the active contour.
    pub fn line_to(&mut self, point: Point) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::LineTo(point))
    }

    /// Appends a quadratic Bézier segment to the active contour.
    pub fn quad_to(&mut self, control: Point, end: Point) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::QuadTo(control, end))
    }

    /// Appends a cubic Bézier segment to the active contour.
    pub fn cubic_to(
        &mut self,
        first_control: Point,
        second_control: Point,
        end: Point,
    ) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::CubicTo(first_control, second_control, end))
    }

    /// Appends one closed rectangular contour.
    pub fn add_rect(&mut self, rect: Rect) -> Result<(), SkiaError> {
        self.reserve_verbs(5)?;
        self.move_to(Point::new(rect.left(), rect.top()))?;
        self.line_to(Point::new(rect.right(), rect.top()))?;
        self.line_to(Point::new(rect.right(), rect.bottom()))?;
        self.line_to(Point::new(rect.left(), rect.bottom()))?;
        self.close()
    }

    /// Appends a closed ellipse approximated by four deterministic cubic Béziers.
    pub fn add_oval(&mut self, bounds: Rect) -> Result<(), SkiaError> {
        self.reserve_verbs(6)?;
        self.add_arc_unchecked(bounds, ArcStart::Right, ArcDirection::Clockwise, 4)?;
        self.close()
    }

    /// Appends a closed circle approximated by four deterministic cubic Béziers.
    pub fn add_circle(&mut self, center: Point, radius: Scalar) -> Result<(), SkiaError> {
        if radius.bits() <= 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        let bounds = Rect::new(
            subtract(center.x(), radius)?,
            subtract(center.y(), radius)?,
            add(center.x(), radius)?,
            add(center.y(), radius)?,
        )?;
        self.add_oval(bounds)
    }

    /// Appends a closed rounded rectangle.
    ///
    /// Negative radii are rejected. Positive radii are clamped independently
    /// to half the corresponding rectangle dimension.
    pub fn add_round_rect(
        &mut self,
        rect: Rect,
        radius_x: Scalar,
        radius_y: Scalar,
    ) -> Result<(), SkiaError> {
        if radius_x.bits() < 0 || radius_y.bits() < 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        if radius_x == Scalar::ZERO || radius_y == Scalar::ZERO {
            return self.add_rect(rect);
        }
        let half_width = half_extent(rect.left(), rect.right())?;
        let half_height = half_extent(rect.top(), rect.bottom())?;
        let radius_x = min_scalar(radius_x, half_width);
        let radius_y = min_scalar(radius_y, half_height);
        self.reserve_verbs(10)?;
        self.move_to(point_offset(
            rect.left(),
            radius_x,
            rect.top(),
            Scalar::ZERO,
        )?)?;
        self.line_to(point_offset(
            rect.right(),
            negate(radius_x)?,
            rect.top(),
            Scalar::ZERO,
        )?)?;
        append_clockwise_quarter_arc(
            self,
            point_offset(rect.right(), negate(radius_x)?, rect.top(), radius_y)?,
            radius_x,
            radius_y,
            ArcStart::Top,
        )?;
        self.line_to(point_offset(
            rect.right(),
            Scalar::ZERO,
            rect.bottom(),
            negate(radius_y)?,
        )?)?;
        append_clockwise_quarter_arc(
            self,
            point_offset(
                rect.right(),
                negate(radius_x)?,
                rect.bottom(),
                negate(radius_y)?,
            )?,
            radius_x,
            radius_y,
            ArcStart::Right,
        )?;
        self.line_to(point_offset(
            rect.left(),
            radius_x,
            rect.bottom(),
            Scalar::ZERO,
        )?)?;
        append_clockwise_quarter_arc(
            self,
            point_offset(rect.left(), radius_x, rect.bottom(), negate(radius_y)?)?,
            radius_x,
            radius_y,
            ArcStart::Bottom,
        )?;
        self.line_to(point_offset(
            rect.left(),
            Scalar::ZERO,
            rect.top(),
            radius_y,
        )?)?;
        append_clockwise_quarter_arc(
            self,
            point_offset(rect.left(), radius_x, rect.top(), radius_y)?,
            radius_x,
            radius_y,
            ArcStart::Left,
        )?;
        self.close()
    }

    /// Starts a contour with an elliptical arc of one to four quarter turns.
    ///
    /// The arc endpoints are the four exact cardinal points of `bounds`. This
    /// fixed-point contract avoids platform trigonometry while covering circle,
    /// oval, and rounded-rectangle construction deterministically.
    pub fn add_arc(
        &mut self,
        bounds: Rect,
        start: ArcStart,
        direction: ArcDirection,
        quarter_turns: u8,
    ) -> Result<(), SkiaError> {
        if !(1..=4).contains(&quarter_turns) {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        self.reserve_verbs(usize::from(quarter_turns) + 1)?;
        self.add_arc_unchecked(bounds, start, direction, quarter_turns)
    }

    fn add_arc_unchecked(
        &mut self,
        bounds: Rect,
        start: ArcStart,
        direction: ArcDirection,
        quarter_turns: u8,
    ) -> Result<(), SkiaError> {
        let center_x = midpoint(bounds.left(), bounds.right())?;
        let center_y = midpoint(bounds.top(), bounds.bottom())?;
        let radius_x = subtract(bounds.right(), center_x)?;
        let radius_y = subtract(bounds.bottom(), center_y)?;
        self.move_to(arc_point(center_x, center_y, radius_x, radius_y, start)?)?;
        let mut position = start;
        for _ in 0..quarter_turns {
            match direction {
                ArcDirection::Clockwise => {
                    append_clockwise_quarter_arc(
                        self,
                        Point::new(center_x, center_y),
                        radius_x,
                        radius_y,
                        position,
                    )?;
                    position = next(position);
                }
                ArcDirection::CounterClockwise => {
                    append_counterclockwise_quarter_arc(
                        self,
                        Point::new(center_x, center_y),
                        radius_x,
                        radius_y,
                        position,
                    )?;
                    position = previous(position);
                }
            }
        }
        Ok(())
    }

    /// Closes the active contour.
    pub fn close(&mut self) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::Close)?;
        self.has_active_contour = false;
        Ok(())
    }

    /// Publishes an immutable path. Open contours are implicitly closed by filling operations.
    pub fn finish(self) -> Result<Path, SkiaError> {
        if self.verbs.is_empty() {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        Ok(Path { verbs: self.verbs })
    }

    fn push(&mut self, verb: PathVerb) -> Result<(), SkiaError> {
        if self.verbs.len() == self.max_verbs {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        self.verbs
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.verbs.push(verb);
        Ok(())
    }

    fn reserve_verbs(&mut self, additional: usize) -> Result<(), SkiaError> {
        let required = self
            .verbs
            .len()
            .checked_add(additional)
            .ok_or(SkiaError::new(SkiaErrorCode::ResourceLimit))?;
        if required > self.max_verbs {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        self.verbs
            .try_reserve(additional)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))
    }
}

fn append_clockwise_quarter_arc(
    builder: &mut PathBuilder,
    center: Point,
    radius_x: Scalar,
    radius_y: Scalar,
    start: ArcStart,
) -> Result<(), SkiaError> {
    let control_x = scale_kappa(radius_x)?;
    let control_y = scale_kappa(radius_y)?;
    let (first, second, end) = match start {
        ArcStart::Right => (
            point_offset(center.x(), radius_x, center.y(), control_y)?,
            point_offset(center.x(), control_x, center.y(), radius_y)?,
            point_offset(center.x(), Scalar::ZERO, center.y(), radius_y)?,
        ),
        ArcStart::Bottom => (
            point_offset(center.x(), negate(control_x)?, center.y(), radius_y)?,
            point_offset(center.x(), negate(radius_x)?, center.y(), control_y)?,
            point_offset(center.x(), negate(radius_x)?, center.y(), Scalar::ZERO)?,
        ),
        ArcStart::Left => (
            point_offset(
                center.x(),
                negate(radius_x)?,
                center.y(),
                negate(control_y)?,
            )?,
            point_offset(
                center.x(),
                negate(control_x)?,
                center.y(),
                negate(radius_y)?,
            )?,
            point_offset(center.x(), Scalar::ZERO, center.y(), negate(radius_y)?)?,
        ),
        ArcStart::Top => (
            point_offset(center.x(), control_x, center.y(), negate(radius_y)?)?,
            point_offset(center.x(), radius_x, center.y(), negate(control_y)?)?,
            point_offset(center.x(), radius_x, center.y(), Scalar::ZERO)?,
        ),
    };
    builder.cubic_to(first, second, end)
}

fn append_counterclockwise_quarter_arc(
    builder: &mut PathBuilder,
    center: Point,
    radius_x: Scalar,
    radius_y: Scalar,
    start: ArcStart,
) -> Result<(), SkiaError> {
    let control_x = scale_kappa(radius_x)?;
    let control_y = scale_kappa(radius_y)?;
    let (first, second, end) = match start {
        ArcStart::Right => (
            point_offset(center.x(), radius_x, center.y(), negate(control_y)?)?,
            point_offset(center.x(), control_x, center.y(), negate(radius_y)?)?,
            point_offset(center.x(), Scalar::ZERO, center.y(), negate(radius_y)?)?,
        ),
        ArcStart::Top => (
            point_offset(
                center.x(),
                negate(control_x)?,
                center.y(),
                negate(radius_y)?,
            )?,
            point_offset(
                center.x(),
                negate(radius_x)?,
                center.y(),
                negate(control_y)?,
            )?,
            point_offset(center.x(), negate(radius_x)?, center.y(), Scalar::ZERO)?,
        ),
        ArcStart::Left => (
            point_offset(center.x(), negate(radius_x)?, center.y(), control_y)?,
            point_offset(center.x(), negate(control_x)?, center.y(), radius_y)?,
            point_offset(center.x(), Scalar::ZERO, center.y(), radius_y)?,
        ),
        ArcStart::Bottom => (
            point_offset(center.x(), control_x, center.y(), radius_y)?,
            point_offset(center.x(), radius_x, center.y(), control_y)?,
            point_offset(center.x(), radius_x, center.y(), Scalar::ZERO)?,
        ),
    };
    builder.cubic_to(first, second, end)
}

fn arc_point(
    center_x: Scalar,
    center_y: Scalar,
    radius_x: Scalar,
    radius_y: Scalar,
    position: ArcStart,
) -> Result<Point, SkiaError> {
    match position {
        ArcStart::Right => point_offset(center_x, radius_x, center_y, Scalar::ZERO),
        ArcStart::Bottom => point_offset(center_x, Scalar::ZERO, center_y, radius_y),
        ArcStart::Left => point_offset(center_x, negate(radius_x)?, center_y, Scalar::ZERO),
        ArcStart::Top => point_offset(center_x, Scalar::ZERO, center_y, negate(radius_y)?),
    }
}

fn next(position: ArcStart) -> ArcStart {
    match position {
        ArcStart::Right => ArcStart::Bottom,
        ArcStart::Bottom => ArcStart::Left,
        ArcStart::Left => ArcStart::Top,
        ArcStart::Top => ArcStart::Right,
    }
}

fn previous(position: ArcStart) -> ArcStart {
    match position {
        ArcStart::Right => ArcStart::Top,
        ArcStart::Top => ArcStart::Left,
        ArcStart::Left => ArcStart::Bottom,
        ArcStart::Bottom => ArcStart::Right,
    }
}

fn midpoint(first: Scalar, second: Scalar) -> Result<Scalar, SkiaError> {
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

fn subtract(left: Scalar, right: Scalar) -> Result<Scalar, SkiaError> {
    i32::try_from(i64::from(left.bits()) - i64::from(right.bits()))
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn half_extent(first: Scalar, second: Scalar) -> Result<Scalar, SkiaError> {
    let difference = i64::from(second.bits()) - i64::from(first.bits());
    let rounded = (difference + 1) / 2;
    i32::try_from(rounded)
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn negate(value: Scalar) -> Result<Scalar, SkiaError> {
    value
        .bits()
        .checked_neg()
        .map(Scalar::from_bits)
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn point_offset(
    x: Scalar,
    offset_x: Scalar,
    y: Scalar,
    offset_y: Scalar,
) -> Result<Point, SkiaError> {
    Ok(Point::new(add(x, offset_x)?, add(y, offset_y)?))
}

fn add(left: Scalar, right: Scalar) -> Result<Scalar, SkiaError> {
    left.bits()
        .checked_add(right.bits())
        .map(Scalar::from_bits)
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn scale_kappa(value: Scalar) -> Result<Scalar, SkiaError> {
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

fn min_scalar(left: Scalar, right: Scalar) -> Scalar {
    if left <= right { left } else { right }
}
