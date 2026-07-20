use pdf_rs_skia_error::{SkiaError, SkiaErrorCode};
use pdf_rs_skia_geometry::{Point, Rect, Scalar, Transform};

mod arc;
mod bounds;
mod math;
mod reverse;

use bounds::{extend_bounds, extend_cubic_tight_bounds, extend_quad_tight_bounds, pad_bounds};
use math::{
    add, half_extent, max_scalar, midpoint, min_scalar, negate, point_offset, scale_kappa, subtract,
};
use reverse::{append_reversed_contour, split_contours};

const DEGREE_BITS: i64 = 1_i64 << 16;
const FULL_TURN_BITS: i64 = 360 * DEGREE_BITS;
const QUARTER_TURN_BITS: i64 = 90 * DEGREE_BITS;

/// Signed Q16.16 angle measured in clockwise canvas degrees.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Angle(i32);

impl Angle {
    /// Exact zero degrees.
    pub const ZERO: Self = Self(0);

    /// Creates an exact whole-degree angle.
    pub fn from_degrees(value: i32) -> Result<Self, SkiaError> {
        value
            .checked_shl(16)
            .map(Self)
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))
    }

    /// Creates an angle from a rational degree value, rounding ties away from zero.
    pub fn from_degrees_ratio(numerator: i64, denominator: i64) -> Result<Self, SkiaError> {
        Scalar::from_ratio(numerator, denominator).map(|value| Self(value.bits()))
    }

    /// Returns the exact Q16.16 degree storage value.
    pub const fn bits(self) -> i32 {
        self.0
    }
}

/// Positive Q16.16 weight for a rational quadratic Bézier segment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConicWeight(Scalar);

impl ConicWeight {
    /// Unit weight, equivalent to an ordinary quadratic Bézier.
    pub const ONE: Self = Self(Scalar::from_bits(1 << 16));

    /// Creates a positive rational weight, rounding ties away from zero.
    pub fn from_ratio(numerator: i64, denominator: i64) -> Result<Self, SkiaError> {
        let value = Scalar::from_ratio(numerator, denominator)?;
        if value.bits() <= 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        Ok(Self(value))
    }

    /// Returns the exact Q16.16 weight storage value.
    pub const fn bits(self) -> i32 {
        self.0.bits()
    }
}

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
    /// Appends a rational quadratic Bézier segment to the active contour.
    ConicTo(Point, Point, ConicWeight),
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

    /// Returns a new path with every stored point mapped by `transform`.
    pub fn transformed(&self, transform: Transform) -> Result<Self, SkiaError> {
        let mut verbs = Vec::new();
        verbs
            .try_reserve_exact(self.verbs.len())
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        for verb in &self.verbs {
            verbs.push(match *verb {
                PathVerb::MoveTo(point) => PathVerb::MoveTo(transform.map_point(point)?),
                PathVerb::LineTo(point) => PathVerb::LineTo(transform.map_point(point)?),
                PathVerb::QuadTo(control, end) => {
                    PathVerb::QuadTo(transform.map_point(control)?, transform.map_point(end)?)
                }
                PathVerb::ConicTo(control, end, weight) => PathVerb::ConicTo(
                    transform.map_point(control)?,
                    transform.map_point(end)?,
                    weight,
                ),
                PathVerb::CubicTo(first_control, second_control, end) => PathVerb::CubicTo(
                    transform.map_point(first_control)?,
                    transform.map_point(second_control)?,
                    transform.map_point(end)?,
                ),
                PathVerb::Close => PathVerb::Close,
            });
        }
        Ok(Self { verbs })
    }

    /// Returns bounds enclosing every endpoint and Bézier control point.
    ///
    /// The result is conservative for curves: it is guaranteed to contain the
    /// rendered path, but may be larger than the curve's mathematically tight
    /// bounds. A line or point may therefore have a zero-width or zero-height
    /// result.
    pub fn bounds(&self) -> Option<PathBounds> {
        let mut bounds = None;
        for verb in &self.verbs {
            match *verb {
                PathVerb::MoveTo(point) | PathVerb::LineTo(point) => {
                    extend_bounds(&mut bounds, point)
                }
                PathVerb::QuadTo(control, end) => {
                    extend_bounds(&mut bounds, control);
                    extend_bounds(&mut bounds, end);
                }
                PathVerb::ConicTo(control, end, _) => {
                    extend_bounds(&mut bounds, control);
                    extend_bounds(&mut bounds, end);
                }
                PathVerb::CubicTo(first_control, second_control, end) => {
                    extend_bounds(&mut bounds, first_control);
                    extend_bounds(&mut bounds, second_control);
                    extend_bounds(&mut bounds, end);
                }
                PathVerb::Close => {}
            }
        }
        bounds
    }

    /// Returns a curve-extrema-aware conservative bounds box.
    ///
    /// Polynomial Bézier derivative roots are evaluated in fixed-point space.
    /// Rational quadratic segments conservatively retain their control-point
    /// hull. The result is padded by two Q16.16 units on each non-saturated
    /// edge to remain conservative across deterministic rounding.
    pub fn tight_bounds(&self) -> Option<PathBounds> {
        let mut bounds = None;
        let mut current = None;
        let mut contour_start = None;
        let mut contains_curve = false;
        for verb in &self.verbs {
            match *verb {
                PathVerb::MoveTo(point) => {
                    extend_bounds(&mut bounds, point);
                    current = Some(point);
                    contour_start = Some(point);
                }
                PathVerb::LineTo(end) => {
                    extend_bounds(&mut bounds, end);
                    current = Some(end);
                }
                PathVerb::QuadTo(control, end) => {
                    if let Some(start) = current {
                        extend_quad_tight_bounds(&mut bounds, start, control, end);
                        contains_curve = true;
                    }
                    current = Some(end);
                }
                PathVerb::ConicTo(control, end, _) => {
                    extend_bounds(&mut bounds, control);
                    extend_bounds(&mut bounds, end);
                    contains_curve = true;
                    current = Some(end);
                }
                PathVerb::CubicTo(first_control, second_control, end) => {
                    if let Some(start) = current {
                        extend_cubic_tight_bounds(
                            &mut bounds,
                            start,
                            first_control,
                            second_control,
                            end,
                        );
                        contains_curve = true;
                    }
                    current = Some(end);
                }
                PathVerb::Close => current = contour_start,
            }
        }
        if contains_curve {
            bounds = bounds.map(pad_bounds);
        }
        bounds
    }

    /// Returns a path with every contour traversed in the opposite direction.
    pub fn reversed(&self) -> Result<Self, SkiaError> {
        let contours = split_contours(self)?;
        let extra_closing_edges = contours.iter().filter(|contour| contour.closed).count();
        let capacity = self
            .verbs
            .len()
            .checked_add(extra_closing_edges)
            .ok_or(SkiaError::new(SkiaErrorCode::ResourceLimit))?;
        let mut builder = PathBuilder::new(capacity)?;
        for contour in contours {
            append_reversed_contour(&mut builder, contour)?;
        }
        builder.finish()
    }
}

/// Axis-aligned bounds that can represent degenerate paths.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PathBounds {
    left: Scalar,
    top: Scalar,
    right: Scalar,
    bottom: Scalar,
}

impl PathBounds {
    /// Returns the smallest horizontal coordinate.
    pub const fn left(self) -> Scalar {
        self.left
    }

    /// Returns the smallest vertical coordinate.
    pub const fn top(self) -> Scalar {
        self.top
    }

    /// Returns the largest horizontal coordinate.
    pub const fn right(self) -> Scalar {
        self.right
    }

    /// Returns the largest vertical coordinate.
    pub const fn bottom(self) -> Scalar {
        self.bottom
    }
}

/// Bounded, fallible builder for an immutable vector path.
#[derive(Debug)]
pub struct PathBuilder {
    verbs: Vec<PathVerb>,
    has_active_contour: bool,
    current_point: Option<Point>,
    contour_start: Option<Point>,
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
            current_point: None,
            contour_start: None,
            max_verbs,
        })
    }

    /// Starts a new contour.
    pub fn move_to(&mut self, point: Point) -> Result<(), SkiaError> {
        self.push(PathVerb::MoveTo(point))?;
        self.has_active_contour = true;
        self.current_point = Some(point);
        self.contour_start = Some(point);
        Ok(())
    }

    /// Appends a line to the active contour.
    pub fn line_to(&mut self, point: Point) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::LineTo(point))?;
        self.current_point = Some(point);
        Ok(())
    }

    /// Appends a quadratic Bézier segment to the active contour.
    pub fn quad_to(&mut self, control: Point, end: Point) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::QuadTo(control, end))?;
        self.current_point = Some(end);
        Ok(())
    }

    /// Appends a rational quadratic Bézier segment to the active contour.
    pub fn conic_to(
        &mut self,
        control: Point,
        end: Point,
        weight: ConicWeight,
    ) -> Result<(), SkiaError> {
        if !self.has_active_contour {
            return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
        }
        self.push(PathVerb::ConicTo(control, end, weight))?;
        self.current_point = Some(end);
        Ok(())
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
        self.push(PathVerb::CubicTo(first_control, second_control, end))?;
        self.current_point = Some(end);
        Ok(())
    }

    /// Appends a contour through `points`, optionally closing it.
    ///
    /// Open polygons require at least two points; closed polygons require at
    /// least three. The input is copied into ordinary move/line/close verbs.
    pub fn add_polygon(&mut self, points: &[Point], close: bool) -> Result<(), SkiaError> {
        let minimum = if close { 3 } else { 2 };
        if points.len() < minimum {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        let extra = points
            .len()
            .checked_add(usize::from(close))
            .ok_or(SkiaError::new(SkiaErrorCode::ResourceLimit))?;
        self.reserve_verbs(extra)?;
        self.move_to(points[0])?;
        for &point in &points[1..] {
            self.line_to(point)?;
        }
        if close {
            self.close()?;
        }
        Ok(())
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

    /// Appends every contour in `path` while preserving its verbs exactly.
    pub fn append_path(&mut self, path: &Path) -> Result<(), SkiaError> {
        self.reserve_verbs(path.verbs.len())?;
        for verb in path.verbs() {
            match *verb {
                PathVerb::MoveTo(point) => self.move_to(point)?,
                PathVerb::LineTo(point) => self.line_to(point)?,
                PathVerb::QuadTo(control, end) => self.quad_to(control, end)?,
                PathVerb::ConicTo(control, end, weight) => self.conic_to(control, end, weight)?,
                PathVerb::CubicTo(first_control, second_control, end) => {
                    self.cubic_to(first_control, second_control, end)?
                }
                PathVerb::Close => self.close()?,
            }
        }
        Ok(())
    }

    pub(crate) fn add_arc_unchecked(
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
        self.current_point = self.contour_start;
        self.contour_start = None;
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

#[derive(Clone, Copy)]
pub(crate) struct ArcSegment {
    pub(crate) first_control: Point,
    pub(crate) second_control: Point,
    pub(crate) end: Point,
}

pub(crate) fn build_ellipse_arc(
    bounds: Rect,
    start: Angle,
    sweep: Angle,
) -> Result<(Point, Vec<ArcSegment>), SkiaError> {
    let sweep_bits = i64::from(sweep.bits());
    if sweep_bits == 0 || sweep_bits.unsigned_abs() > FULL_TURN_BITS as u64 {
        return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
    }
    let center_x = midpoint(bounds.left(), bounds.right())?;
    let center_y = midpoint(bounds.top(), bounds.bottom())?;
    let radius_x = subtract(bounds.right(), center_x)?;
    let radius_y = subtract(bounds.bottom(), center_y)?;
    let segment_count = usize::try_from(
        (sweep_bits.unsigned_abs() + QUARTER_TURN_BITS as u64 - 1) / QUARTER_TURN_BITS as u64,
    )
    .map_err(|_| SkiaError::new(SkiaErrorCode::ResourceLimit))?;
    let mut segments = Vec::new();
    segments
        .try_reserve_exact(segment_count)
        .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
    let mut angle = i64::from(start.bits());
    let mut remaining = sweep_bits;
    let (start_cosine, start_sine) = sin_cos(angle);
    let start_point = ellipse_point(
        center_x,
        center_y,
        radius_x,
        radius_y,
        start_cosine,
        start_sine,
    )?;
    while remaining != 0 {
        let step = if remaining > 0 {
            remaining.min(QUARTER_TURN_BITS)
        } else {
            remaining.max(-QUARTER_TURN_BITS)
        };
        let end_angle = angle
            .checked_add(step)
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        let (start_cosine, start_sine) = sin_cos(angle);
        let (end_cosine, end_sine) = sin_cos(end_angle);
        let start_point = ellipse_point(
            center_x,
            center_y,
            radius_x,
            radius_y,
            start_cosine,
            start_sine,
        )?;
        let end = ellipse_point(center_x, center_y, radius_x, radius_y, end_cosine, end_sine)?;
        let (quarter_cosine, quarter_sine) = sin_cos(step / 4);
        let tangent = q30_ratio(quarter_sine, quarter_cosine)?;
        let kappa = q30_multiply_ratio(tangent, 4, 3)?;
        let (start_x, start_y) = ellipse_tangent(radius_x, radius_y, start_cosine, start_sine)?;
        let (end_x, end_y) = ellipse_tangent(radius_x, radius_y, end_cosine, end_sine)?;
        let first_control = point_offset(
            start_point.x(),
            scale_q30(start_x, kappa)?,
            start_point.y(),
            scale_q30(start_y, kappa)?,
        )?;
        let second_control = point_offset(
            end.x(),
            negate(scale_q30(end_x, kappa)?)?,
            end.y(),
            negate(scale_q30(end_y, kappa)?)?,
        )?;
        segments.push(ArcSegment {
            first_control,
            second_control,
            end,
        });
        angle = end_angle;
        remaining -= step;
    }
    Ok((start_point, segments))
}

pub(crate) fn build_rotated_ellipse_arc(
    bounds: Rect,
    rotation: Angle,
    start: Angle,
    sweep: Angle,
) -> Result<(Point, Vec<ArcSegment>), SkiaError> {
    let center = Point::new(
        midpoint(bounds.left(), bounds.right())?,
        midpoint(bounds.top(), bounds.bottom())?,
    );
    let (start_point, segments) = build_ellipse_arc(bounds, start, sweep)?;
    let (cosine, sine) = sin_cos(i64::from(rotation.bits()));
    let start_point = rotate_point(start_point, center, cosine, sine)?;
    let mut rotated = Vec::new();
    rotated
        .try_reserve_exact(segments.len())
        .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
    for segment in segments {
        rotated.push(ArcSegment {
            first_control: rotate_point(segment.first_control, center, cosine, sine)?,
            second_control: rotate_point(segment.second_control, center, cosine, sine)?,
            end: rotate_point(segment.end, center, cosine, sine)?,
        });
    }
    Ok((start_point, rotated))
}

fn rotate_point(point: Point, center: Point, cosine: i32, sine: i32) -> Result<Point, SkiaError> {
    let x = subtract(point.x(), center.x())?;
    let y = subtract(point.y(), center.y())?;
    let rotated_x = subtract(scale_q30(x, cosine)?, scale_q30(y, sine)?)?;
    let rotated_y = add(scale_q30(x, sine)?, scale_q30(y, cosine)?)?;
    point_offset(center.x(), rotated_x, center.y(), rotated_y)
}

fn ellipse_point(
    center_x: Scalar,
    center_y: Scalar,
    radius_x: Scalar,
    radius_y: Scalar,
    cosine: i32,
    sine: i32,
) -> Result<Point, SkiaError> {
    point_offset(
        center_x,
        scale_q30(radius_x, cosine)?,
        center_y,
        scale_q30(radius_y, sine)?,
    )
}

fn ellipse_tangent(
    radius_x: Scalar,
    radius_y: Scalar,
    cosine: i32,
    sine: i32,
) -> Result<(Scalar, Scalar), SkiaError> {
    Ok((
        negate(scale_q30(radius_x, sine)?)?,
        scale_q30(radius_y, cosine)?,
    ))
}

const PI_Q30: i64 = 3_373_259_426;
const HALF_PI_Q30: i64 = PI_Q30 / 2;
const CORDIC_GAIN_INVERSE_Q30: i64 = 652_032_874;
const CORDIC_ANGLES_Q30: [i64; 30] = [
    843_314_857,
    497_837_829,
    263_043_837,
    133_525_159,
    67_021_688,
    33_543_516,
    16_775_851,
    8_388_437,
    4_194_283,
    2_097_149,
    1_048_575,
    524_288,
    262_144,
    131_072,
    65_536,
    32_768,
    16_384,
    8_192,
    4_096,
    2_048,
    1_024,
    512,
    256,
    128,
    64,
    32,
    16,
    8,
    4,
    2,
];

fn sin_cos(degrees: i64) -> (i32, i32) {
    let mut degrees = degrees % FULL_TURN_BITS;
    if degrees > FULL_TURN_BITS / 2 {
        degrees -= FULL_TURN_BITS;
    } else if degrees < -(FULL_TURN_BITS / 2) {
        degrees += FULL_TURN_BITS;
    }
    let mut angle = degrees * PI_Q30 / (180 * DEGREE_BITS);
    let negate_result = if angle > HALF_PI_Q30 {
        angle -= PI_Q30;
        true
    } else if angle < -HALF_PI_Q30 {
        angle += PI_Q30;
        true
    } else {
        false
    };
    let mut x = CORDIC_GAIN_INVERSE_Q30;
    let mut y = 0_i64;
    for (index, adjustment) in CORDIC_ANGLES_Q30.iter().enumerate() {
        let shifted_x = x >> index;
        let shifted_y = y >> index;
        if angle >= 0 {
            x -= shifted_y;
            y += shifted_x;
            angle -= adjustment;
        } else {
            x += shifted_y;
            y -= shifted_x;
            angle += adjustment;
        }
    }
    let (x, y) = if negate_result { (-x, -y) } else { (x, y) };
    (
        i32::try_from(x).unwrap_or(if x < 0 { i32::MIN } else { i32::MAX }),
        i32::try_from(y).unwrap_or(if y < 0 { i32::MIN } else { i32::MAX }),
    )
}

fn q30_ratio(numerator: i32, denominator: i32) -> Result<i32, SkiaError> {
    if denominator == 0 {
        return Err(SkiaError::new(SkiaErrorCode::NumericOverflow));
    }
    let scaled = i128::from(numerator) << 30;
    let quotient = rounded_divide(scaled, i128::from(denominator))?;
    i32::try_from(quotient).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn q30_multiply_ratio(value: i32, numerator: i32, denominator: i32) -> Result<i32, SkiaError> {
    let product = i128::from(value)
        .checked_mul(i128::from(numerator))
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
    let quotient = rounded_divide(product, i128::from(denominator))?;
    i32::try_from(quotient).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn scale_q30(value: Scalar, multiplier: i32) -> Result<Scalar, SkiaError> {
    let product = i128::from(value.bits())
        .checked_mul(i128::from(multiplier))
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
    let quotient = rounded_divide(product, i128::from(1_i64 << 30))?;
    i32::try_from(quotient)
        .map(Scalar::from_bits)
        .map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))
}

fn rounded_divide(numerator: i128, denominator: i128) -> Result<i128, SkiaError> {
    if denominator == 0 {
        return Err(SkiaError::new(SkiaErrorCode::NumericOverflow));
    }
    let negative = (numerator < 0) != (denominator < 0);
    let magnitude = numerator.unsigned_abs();
    let divisor = denominator.unsigned_abs();
    let rounded = magnitude
        .checked_add(divisor / 2)
        .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?
        / divisor;
    let rounded =
        i128::try_from(rounded).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))?;
    Ok(if negative { -rounded } else { rounded })
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
