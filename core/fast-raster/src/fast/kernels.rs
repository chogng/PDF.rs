//! Scalar coverage, sampling, and compositing kernels.

use core::mem::size_of;

use pdf_rs_policy::{DeviceRect, RenderPlan};
use pdf_rs_scene::{
    BlendMode, DeviceColor, FillRule, GraphicsResource, GraphicsScene, ImageResource, Matrix,
    Paint, PathResource, PathSegment, SceneBounds, ScenePoint, SceneScalar, SceneUnit,
};

use crate::fast::{FastRasterError, FastRasterErrorCode, FastRasterLimitKind};

const SCENE_SCALE: i128 = 1_000_000_000;
pub(crate) const FIXED_ONE: i64 = 1 << 16;
const Q16_ONE: u32 = 1 << 16;
const SAMPLE_SIDE: i64 = 4;
const SAMPLE_COUNT: u32 = 16;

pub(crate) trait KernelWork {
    fn step(&mut self) -> Result<(), FastRasterError>;
    fn admit_intermediate(&mut self, bytes: u64) -> Result<(), FastRasterError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkRect {
    pub(crate) x: i64,
    pub(crate) y: i64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl WorkRect {
    pub(crate) fn expanded(tile: DeviceRect, halo: u16) -> Result<Self, FastRasterError> {
        let halo = i64::from(halo);
        let width = u64::from(tile.width())
            .checked_add(u64::try_from(halo * 2).map_err(|_| numeric())?)
            .ok_or_else(numeric)?;
        let height = u64::from(tile.height())
            .checked_add(u64::try_from(halo * 2).map_err(|_| numeric())?)
            .ok_or_else(numeric)?;
        Ok(Self {
            x: i64::from(tile.x()).checked_sub(halo).ok_or_else(numeric)?,
            y: i64::from(tile.y()).checked_sub(halo).ok_or_else(numeric)?,
            width: u32::try_from(width).map_err(|_| numeric())?,
            height: u32::try_from(height).map_err(|_| numeric())?,
        })
    }

    pub(crate) fn pixels(self) -> Result<u64, FastRasterError> {
        u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or_else(numeric)
    }

    fn right_fixed(self) -> Result<i64, FastRasterError> {
        self.x
            .checked_add(i64::from(self.width))
            .and_then(|value| value.checked_mul(FIXED_ONE))
            .ok_or_else(numeric)
    }

    fn bottom_fixed(self) -> Result<i64, FastRasterError> {
        self.y
            .checked_add(i64::from(self.height))
            .and_then(|value| value.checked_mul(FIXED_ONE))
            .ok_or_else(numeric)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Pixel {
    red: u32,
    green: u32,
    blue: u32,
    alpha: u32,
}

impl Pixel {
    pub(crate) const WHITE: Self = Self {
        red: Q16_ONE,
        green: Q16_ONE,
        blue: Q16_ONE,
        alpha: Q16_ONE,
    };

    pub(crate) fn from_paint(paint: Paint, coverage: u32) -> Self {
        let (red, green, blue) = color(paint.color());
        let alpha = q16(SceneUnit::from_u16(paint.alpha().get()));
        let coverage = ((u64::from(coverage) * u64::from(Q16_ONE) + u64::from(SAMPLE_COUNT / 2))
            / u64::from(SAMPLE_COUNT)) as u32;
        let alpha = multiply(alpha, coverage);
        Self {
            red: multiply(red, alpha),
            green: multiply(green, alpha),
            blue: multiply(blue, alpha),
            alpha,
        }
    }

    pub(crate) fn source_over(self, backdrop: Self, mode: BlendMode) -> Self {
        let alpha = round_q16(
            u64::from(self.alpha) * u64::from(Q16_ONE)
                + u64::from(backdrop.alpha) * u64::from(Q16_ONE - self.alpha),
        );
        Self {
            red: composite_channel(mode, self.red, self.alpha, backdrop.red, backdrop.alpha),
            green: composite_channel(mode, self.green, self.alpha, backdrop.green, backdrop.alpha),
            blue: composite_channel(mode, self.blue, self.alpha, backdrop.blue, backdrop.alpha),
            alpha,
        }
    }

    pub(crate) fn to_rgba8(self) -> [u8; 4] {
        if self.alpha == 0 {
            return [0, 0, 0, 0];
        }
        [
            to_u8(unpremultiply(self.red, self.alpha)),
            to_u8(unpremultiply(self.green, self.alpha)),
            to_u8(unpremultiply(self.blue, self.alpha)),
            to_u8(self.alpha),
        ]
    }
}

fn q16(value: SceneUnit) -> u32 {
    ((u64::from(value.get()) * u64::from(Q16_ONE) + u64::from(u16::MAX / 2)) / u64::from(u16::MAX))
        as u32
}

fn color(value: DeviceColor) -> (u32, u32, u32) {
    match value {
        DeviceColor::Gray(gray) => {
            let value = q16(gray);
            (value, value, value)
        }
        DeviceColor::Rgb { red, green, blue } => (q16(red), q16(green), q16(blue)),
        DeviceColor::Cmyk {
            cyan,
            magenta,
            yellow,
            black,
        } => {
            let black = q16(black);
            let remove = |component: SceneUnit| {
                Q16_ONE.saturating_sub(q16(component).saturating_add(black).min(Q16_ONE))
            };
            (remove(cyan), remove(magenta), remove(yellow))
        }
    }
}

fn multiply(left: u32, right: u32) -> u32 {
    round_q16(u64::from(left) * u64::from(right))
}

fn round_q16(numerator: u64) -> u32 {
    ((numerator + u64::from(Q16_ONE / 2)) / u64::from(Q16_ONE)) as u32
}

fn composite_channel(mode: BlendMode, source: u32, sa: u32, backdrop: u32, ba: u32) -> u32 {
    let source = u64::from(source);
    let backdrop = u64::from(backdrop);
    let numerator = match mode {
        BlendMode::Normal => source * u64::from(Q16_ONE) + backdrop * u64::from(Q16_ONE - sa),
        BlendMode::Multiply => {
            backdrop * u64::from(Q16_ONE - sa)
                + source * u64::from(Q16_ONE - ba)
                + source * backdrop
        }
        BlendMode::Screen => {
            source * u64::from(Q16_ONE) + backdrop * u64::from(Q16_ONE) - source * backdrop
        }
    };
    round_q16(numerator)
}

fn unpremultiply(channel: u32, alpha: u32) -> u32 {
    ((u64::from(channel) * u64::from(Q16_ONE) + u64::from(alpha / 2)) / u64::from(alpha))
        .min(u64::from(Q16_ONE)) as u32
}

fn to_u8(value: u32) -> u8 {
    ((u64::from(value) * u64::from(u8::MAX) + u64::from(Q16_ONE / 2)) / u64::from(Q16_ONE)) as u8
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Point {
    pub(crate) x: i64,
    pub(crate) y: i64,
}

#[derive(Debug)]
pub(crate) struct Subpath {
    pub(crate) points: Vec<Point>,
    pub(crate) closed: bool,
}

#[derive(Debug)]
pub(crate) struct FlatPath {
    pub(crate) subpaths: Vec<Subpath>,
    retained_bytes: u64,
}

impl FlatPath {
    pub(crate) const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    pub(crate) fn coverage_window(
        &self,
        rect: WorkRect,
        work: &mut dyn KernelWork,
    ) -> Result<Option<CoverageWindow>, FastRasterError> {
        let mut minimum_x = i64::MAX;
        let mut minimum_y = i64::MAX;
        let mut maximum_x = i64::MIN;
        let mut maximum_y = i64::MIN;
        let mut has_point = false;
        for subpath in &self.subpaths {
            work.step()?;
            for point in &subpath.points {
                work.step()?;
                minimum_x = minimum_x.min(point.x);
                minimum_y = minimum_y.min(point.y);
                maximum_x = maximum_x.max(point.x);
                maximum_y = maximum_y.max(point.y);
                has_point = true;
            }
        }
        if !has_point {
            return Ok(None);
        }
        let Some((column_start, column_end)) =
            coverage_axis(minimum_x, maximum_x, rect.x, rect.width)?
        else {
            return Ok(None);
        };
        let Some((row_start, row_end)) = coverage_axis(minimum_y, maximum_y, rect.y, rect.height)?
        else {
            return Ok(None);
        };
        Ok(Some(CoverageWindow {
            column_start,
            column_end,
            row_start,
            row_end,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CoverageWindow {
    column_start: u32,
    column_end: u32,
    row_start: u32,
    row_end: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PageMap {
    crop: [i64; 4],
    scale_numerator: i128,
    scale_denominator: i128,
    rotation: u16,
    width: i64,
    height: i64,
}

impl PageMap {
    pub(crate) fn new(
        scene: &pdf_rs_scene::Scene,
        plan: &RenderPlan,
    ) -> Result<Self, FastRasterError> {
        let crop = scene
            .geometry()
            .crop_box()
            .coordinates()
            .map(SceneScalar::scaled);
        let zoom = plan.viewport().zoom();
        let scale_numerator = i128::from(zoom.numerator())
            .checked_mul(i128::from(plan.viewport().device_scale_milli()))
            .and_then(|value| value.checked_mul(i128::from(FIXED_ONE)))
            .ok_or_else(numeric)?;
        let scale_denominator = i128::from(zoom.denominator())
            .checked_mul(1_000)
            .and_then(|value| value.checked_mul(SCENE_SCALE))
            .ok_or_else(numeric)?;
        let width = scale_delta(
            i128::from(crop[2]) - i128::from(crop[0]),
            scale_numerator,
            scale_denominator,
        )?;
        let height = scale_delta(
            i128::from(crop[3]) - i128::from(crop[1]),
            scale_numerator,
            scale_denominator,
        )?;
        let rotation =
            (scene.geometry().rotation().degrees() + plan.viewport().rotation().degrees()) % 360;
        Ok(Self {
            crop,
            scale_numerator,
            scale_denominator,
            rotation,
            width,
            height,
        })
    }

    pub(crate) fn map(
        self,
        point: ScenePoint,
        transform: Matrix,
        divisor: u16,
    ) -> Result<Point, FastRasterError> {
        let point = if divisor == 1 {
            point
        } else {
            ScenePoint::new(
                SceneScalar::from_scaled(round_div(
                    i128::from(point.x().scaled()),
                    i128::from(divisor),
                )?),
                SceneScalar::from_scaled(round_div(
                    i128::from(point.y().scaled()),
                    i128::from(divisor),
                )?),
            )
        };
        let point = transform
            .checked_transform_point(point)
            .map_err(|_| numeric())?;
        self.map_page(point)
    }

    pub(crate) fn map_page(self, point: ScenePoint) -> Result<Point, FastRasterError> {
        let x = scale_delta(
            i128::from(point.x().scaled()) - i128::from(self.crop[0]),
            self.scale_numerator,
            self.scale_denominator,
        )?;
        let y = scale_delta(
            i128::from(self.crop[3]) - i128::from(point.y().scaled()),
            self.scale_numerator,
            self.scale_denominator,
        )?;
        let (x, y) = match self.rotation {
            0 => (x, y),
            90 => (self.height.checked_sub(y).ok_or_else(numeric)?, x),
            180 => (
                self.width.checked_sub(x).ok_or_else(numeric)?,
                self.height.checked_sub(y).ok_or_else(numeric)?,
            ),
            270 => (y, self.width.checked_sub(x).ok_or_else(numeric)?),
            _ => {
                return Err(FastRasterError::for_code(
                    FastRasterErrorCode::InvalidRenderConfig,
                ));
            }
        };
        Ok(Point { x, y })
    }

    pub(crate) fn bounds_intersect(
        self,
        bounds: SceneBounds,
        rect: WorkRect,
    ) -> Result<bool, FastRasterError> {
        let SceneBounds::Finite { minimum, maximum } = bounds else {
            return Ok(matches!(bounds, SceneBounds::Page));
        };
        let corners = [
            minimum,
            maximum,
            ScenePoint::new(minimum.x(), maximum.y()),
            ScenePoint::new(maximum.x(), minimum.y()),
        ];
        let mut min_x = i64::MAX;
        let mut min_y = i64::MAX;
        let mut max_x = i64::MIN;
        let mut max_y = i64::MIN;
        for corner in corners {
            let point = self.map_page(corner)?;
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
        let left = rect.x.checked_mul(FIXED_ONE).ok_or_else(numeric)?;
        let top = rect.y.checked_mul(FIXED_ONE).ok_or_else(numeric)?;
        Ok(max_x >= left
            && max_y >= top
            && min_x <= rect.right_fixed()?
            && min_y <= rect.bottom_fixed()?)
    }

    pub(crate) fn device_affine(self) -> Result<[i64; 6], FastRasterError> {
        let zero = SceneScalar::ZERO;
        let one = SceneScalar::ONE;
        let origin = self.map_page(ScenePoint::new(zero, zero))?;
        let x_basis = self.map_page(ScenePoint::new(one, zero))?;
        let y_basis = self.map_page(ScenePoint::new(zero, one))?;
        Ok([
            x_basis.x.checked_sub(origin.x).ok_or_else(numeric)?,
            x_basis.y.checked_sub(origin.y).ok_or_else(numeric)?,
            y_basis.x.checked_sub(origin.x).ok_or_else(numeric)?,
            y_basis.y.checked_sub(origin.y).ok_or_else(numeric)?,
            origin.x,
            origin.y,
        ])
    }
}

fn scale_delta(delta: i128, numerator: i128, denominator: i128) -> Result<i64, FastRasterError> {
    let scaled = delta.checked_mul(numerator).ok_or_else(numeric)?;
    round_div(scaled, denominator)
}

fn round_div(numerator: i128, denominator: i128) -> Result<i64, FastRasterError> {
    if denominator <= 0 {
        return Err(numeric());
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = if remainder.abs().checked_mul(2).ok_or_else(numeric)? >= denominator {
        quotient
            .checked_add(if numerator.is_negative() { -1 } else { 1 })
            .ok_or_else(numeric)?
    } else {
        quotient
    };
    i64::try_from(rounded).map_err(|_| numeric())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the scalar flattener receives every policy-bound transform and work limit explicitly"
)]
pub(crate) fn flatten_path(
    path: &PathResource,
    map: PageMap,
    transform: Matrix,
    divisor: u16,
    flatness_denominator: u32,
    recursion_limit: u8,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<FlatPath, FastRasterError> {
    let tolerance = (FIXED_ONE / i64::from(flatness_denominator)).max(1);
    let mut subpaths = Vec::new();
    let mut retained = 0;
    let mut current: Option<usize> = None;
    let mut last = None;
    for segment in path.segments() {
        work.step()?;
        match *segment {
            PathSegment::MoveTo(point) => {
                let mapped = map.map(point, transform, divisor)?;
                let mut points = Vec::new();
                let point_bytes =
                    reserve_intermediate(&mut points, 1, add(base_intermediate, retained)?, work)?;
                points.push(mapped);
                retained = retained.checked_add(point_bytes).ok_or_else(numeric)?;
                grow_retained_vector(&mut subpaths, 1, base_intermediate, &mut retained, work)?;
                subpaths.push(Subpath {
                    points,
                    closed: false,
                });
                work.admit_intermediate(add(base_intermediate, retained)?)?;
                current = Some(subpaths.len() - 1);
                last = Some(mapped);
            }
            PathSegment::LineTo(point) => {
                let mapped = map.map(point, transform, divisor)?;
                push_point(
                    &mut subpaths,
                    current,
                    mapped,
                    base_intermediate,
                    &mut retained,
                    work,
                )?;
                last = Some(mapped);
            }
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                let start = last.ok_or_else(|| {
                    FastRasterError::for_code(FastRasterErrorCode::InvalidCommandSequence)
                })?;
                let control_1 = map.map(control_1, transform, divisor)?;
                let control_2 = map.map(control_2, transform, divisor)?;
                let end = map.map(end, transform, divisor)?;
                flatten_cubic(
                    start,
                    control_1,
                    control_2,
                    end,
                    tolerance,
                    recursion_limit,
                    0,
                    &mut subpaths,
                    current,
                    base_intermediate,
                    &mut retained,
                    work,
                )?;
                last = Some(end);
            }
            PathSegment::ClosePath => {
                let index = current.ok_or_else(|| {
                    FastRasterError::for_code(FastRasterErrorCode::InvalidCommandSequence)
                })?;
                subpaths[index].closed = true;
                current = None;
                last = None;
            }
        }
    }
    Ok(FlatPath {
        subpaths,
        retained_bytes: retained,
    })
}

#[allow(clippy::too_many_arguments)]
fn flatten_cubic(
    start: Point,
    c1: Point,
    c2: Point,
    end: Point,
    tolerance: i64,
    recursion_limit: u8,
    depth: u8,
    subpaths: &mut [Subpath],
    current: Option<usize>,
    base_intermediate: u64,
    retained: &mut u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    work.step()?;
    if cubic_flat(start, c1, c2, end, tolerance)? {
        return push_point(subpaths, current, end, base_intermediate, retained, work);
    }
    if depth >= recursion_limit {
        return Err(FastRasterError::resource(
            FastRasterLimitKind::Fuel,
            u64::from(recursion_limit),
            u64::from(depth) + 1,
        ));
    }
    let p01 = midpoint(start, c1)?;
    let p12 = midpoint(c1, c2)?;
    let p23 = midpoint(c2, end)?;
    let p012 = midpoint(p01, p12)?;
    let p123 = midpoint(p12, p23)?;
    let middle = midpoint(p012, p123)?;
    flatten_cubic(
        start,
        p01,
        p012,
        middle,
        tolerance,
        recursion_limit,
        depth + 1,
        subpaths,
        current,
        base_intermediate,
        retained,
        work,
    )?;
    flatten_cubic(
        middle,
        p123,
        p23,
        end,
        tolerance,
        recursion_limit,
        depth + 1,
        subpaths,
        current,
        base_intermediate,
        retained,
        work,
    )
}

fn cubic_flat(
    start: Point,
    c1: Point,
    c2: Point,
    end: Point,
    tolerance: i64,
) -> Result<bool, FastRasterError> {
    let vx = i128::from(end.x) - i128::from(start.x);
    let vy = i128::from(end.y) - i128::from(start.y);
    let length_sq = vx
        .checked_mul(vx)
        .and_then(|value| value.checked_add(vy.checked_mul(vy)?))
        .ok_or_else(numeric)?
        .max(1);
    for point in [c1, c2] {
        let wx = i128::from(point.x) - i128::from(start.x);
        let wy = i128::from(point.y) - i128::from(start.y);
        let cross = vx
            .checked_mul(wy)
            .and_then(|value| value.checked_sub(vy.checked_mul(wx)?))
            .ok_or_else(numeric)?
            .abs();
        let left = cross.checked_mul(cross).ok_or_else(numeric)?;
        let tolerance_sq = i128::from(tolerance)
            .checked_mul(i128::from(tolerance))
            .ok_or_else(numeric)?;
        if left > tolerance_sq.checked_mul(length_sq).ok_or_else(numeric)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn midpoint(left: Point, right: Point) -> Result<Point, FastRasterError> {
    Ok(Point {
        x: average(left.x, right.x)?,
        y: average(left.y, right.y)?,
    })
}

fn average(left: i64, right: i64) -> Result<i64, FastRasterError> {
    let total = i128::from(left)
        .checked_add(i128::from(right))
        .ok_or_else(numeric)?;
    round_div(total, 2)
}

fn push_point(
    subpaths: &mut [Subpath],
    current: Option<usize>,
    point: Point,
    base_intermediate: u64,
    retained: &mut u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    let index = current
        .ok_or_else(|| FastRasterError::for_code(FastRasterErrorCode::InvalidCommandSequence))?;
    let points = &mut subpaths[index].points;
    if points.len() == points.capacity() {
        grow_retained_vector(points, 1, base_intermediate, retained, work)?;
    }
    points.push(point);
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Coverage {
    masks: Vec<u16>,
    retained_bytes: u64,
}

impl Coverage {
    pub(crate) fn full(
        rect: WorkRect,
        base_intermediate: u64,
        work: &mut dyn KernelWork,
    ) -> Result<Self, FastRasterError> {
        let length = usize::try_from(rect.pixels()?).map_err(|_| numeric())?;
        let mut masks = Vec::new();
        let retained_bytes = reserve_intermediate(&mut masks, length, base_intermediate, work)?;
        for _ in 0..length {
            masks.push(u16::MAX);
            work.step()?;
        }
        Ok(Self {
            masks,
            retained_bytes,
        })
    }

    pub(crate) fn empty(
        rect: WorkRect,
        base_intermediate: u64,
        work: &mut dyn KernelWork,
    ) -> Result<Self, FastRasterError> {
        let length = usize::try_from(rect.pixels()?).map_err(|_| numeric())?;
        let mut masks = Vec::new();
        let retained_bytes = reserve_intermediate(&mut masks, length, base_intermediate, work)?;
        for _ in 0..length {
            masks.push(0);
            work.step()?;
        }
        Ok(Self {
            masks,
            retained_bytes,
        })
    }

    pub(crate) fn copy_from(
        source: &Self,
        base_intermediate: u64,
        work: &mut dyn KernelWork,
    ) -> Result<Self, FastRasterError> {
        let mut masks = Vec::new();
        let retained_bytes =
            reserve_intermediate(&mut masks, source.masks.len(), base_intermediate, work)?;
        for &mask in &source.masks {
            masks.push(mask);
            work.step()?;
        }
        Ok(Self {
            masks,
            retained_bytes,
        })
    }

    pub(crate) fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }

    pub(crate) fn intersect(
        &mut self,
        other: &Self,
        work: &mut dyn KernelWork,
    ) -> Result<(), FastRasterError> {
        if self.masks.len() != other.masks.len() {
            return Err(FastRasterError::for_code(
                FastRasterErrorCode::IdentityMismatch,
            ));
        }
        for (target, incoming) in self.masks.iter_mut().zip(&other.masks) {
            *target &= *incoming;
            work.step()?;
        }
        Ok(())
    }

    pub(crate) fn union(
        &mut self,
        other: &Self,
        rect: WorkRect,
        window: CoverageWindow,
        work: &mut dyn KernelWork,
    ) -> Result<(), FastRasterError> {
        if self.masks.len() != other.masks.len() {
            return Err(FastRasterError::for_code(
                FastRasterErrorCode::IdentityMismatch,
            ));
        }
        for row in window.row_start..window.row_end {
            for column in window.column_start..window.column_end {
                let index = coverage_index(rect, row, column)?;
                self.masks[index] |= other.masks[index];
                work.step()?;
            }
        }
        Ok(())
    }
}

pub(crate) fn fill_coverage(
    path: &FlatPath,
    rect: WorkRect,
    rule: FillRule,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<Coverage, FastRasterError> {
    Ok(fill_coverage_bounded(path, rect, rule, base_intermediate, work)?.0)
}

pub(crate) fn fill_coverage_bounded(
    path: &FlatPath,
    rect: WorkRect,
    rule: FillRule,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<(Coverage, Option<CoverageWindow>), FastRasterError> {
    let window = path.coverage_window(rect, work)?;
    let operation = bounded_coverage(rect, window, base_intermediate, work, |point, work| {
        point_in_path(path, point, rule, work)
    })?;
    Ok((operation, window))
}

pub(crate) fn coverage(
    rect: WorkRect,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
    mut contains: impl FnMut(Point, &mut dyn KernelWork) -> Result<bool, FastRasterError>,
) -> Result<Coverage, FastRasterError> {
    let length = usize::try_from(rect.pixels()?).map_err(|_| numeric())?;
    let mut masks = Vec::new();
    let retained_bytes = reserve_intermediate(&mut masks, length, base_intermediate, work)?;
    for row in 0..rect.height {
        for column in 0..rect.width {
            let mut mask = 0_u16;
            for sample_y in 0..SAMPLE_SIDE {
                for sample_x in 0..SAMPLE_SIDE {
                    let x = sample_coordinate(rect.x, column, sample_x)?;
                    let y = sample_coordinate(rect.y, row, sample_y)?;
                    if contains(Point { x, y }, work)? {
                        let bit = u32::try_from(sample_y * SAMPLE_SIDE + sample_x)
                            .map_err(|_| numeric())?;
                        mask |= 1_u16.checked_shl(bit).ok_or_else(numeric)?;
                    }
                    work.step()?;
                }
            }
            masks.push(mask);
        }
    }
    Ok(Coverage {
        masks,
        retained_bytes,
    })
}

fn bounded_coverage(
    rect: WorkRect,
    window: Option<CoverageWindow>,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
    mut contains: impl FnMut(Point, &mut dyn KernelWork) -> Result<bool, FastRasterError>,
) -> Result<Coverage, FastRasterError> {
    let mut coverage = Coverage::empty(rect, base_intermediate, work)?;
    let Some(window) = window else {
        return Ok(coverage);
    };
    for row in window.row_start..window.row_end {
        for column in window.column_start..window.column_end {
            let mut mask = 0_u16;
            for sample_y in 0..SAMPLE_SIDE {
                for sample_x in 0..SAMPLE_SIDE {
                    let x = sample_coordinate(rect.x, column, sample_x)?;
                    let y = sample_coordinate(rect.y, row, sample_y)?;
                    if contains(Point { x, y }, work)? {
                        let bit = u32::try_from(sample_y * SAMPLE_SIDE + sample_x)
                            .map_err(|_| numeric())?;
                        mask |= 1_u16.checked_shl(bit).ok_or_else(numeric)?;
                    }
                    work.step()?;
                }
            }
            let index = coverage_index(rect, row, column)?;
            coverage.masks[index] = mask;
        }
    }
    Ok(coverage)
}

fn coverage_axis(
    minimum: i64,
    maximum: i64,
    origin: i64,
    length: u32,
) -> Result<Option<(u32, u32)>, FastRasterError> {
    let rect_end = origin.checked_add(i64::from(length)).ok_or_else(numeric)?;
    let candidate_start = minimum
        .div_euclid(FIXED_ONE)
        .checked_sub(1)
        .ok_or_else(numeric)?;
    let candidate_end = maximum
        .div_euclid(FIXED_ONE)
        .checked_add(2)
        .ok_or_else(numeric)?;
    let start = candidate_start.max(origin);
    let end = candidate_end.min(rect_end);
    if start >= end {
        return Ok(None);
    }
    Ok(Some((
        u32::try_from(start.checked_sub(origin).ok_or_else(numeric)?).map_err(|_| numeric())?,
        u32::try_from(end.checked_sub(origin).ok_or_else(numeric)?).map_err(|_| numeric())?,
    )))
}

fn coverage_index(rect: WorkRect, row: u32, column: u32) -> Result<usize, FastRasterError> {
    u64::from(row)
        .checked_mul(u64::from(rect.width))
        .and_then(|value| value.checked_add(u64::from(column)))
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(numeric)
}

fn sample_coordinate(origin: i64, pixel: u32, sample: i64) -> Result<i64, FastRasterError> {
    origin
        .checked_add(i64::from(pixel))
        .and_then(|value| value.checked_mul(FIXED_ONE))
        .and_then(|value| {
            value.checked_add(
                (sample * 2 + 1)
                    .checked_mul(FIXED_ONE)
                    .and_then(|value| value.checked_div(SAMPLE_SIDE * 2))?,
            )
        })
        .ok_or_else(numeric)
}

fn point_in_path(
    path: &FlatPath,
    point: Point,
    rule: FillRule,
    work: &mut dyn KernelWork,
) -> Result<bool, FastRasterError> {
    let mut winding = 0_i64;
    let mut parity = false;
    for subpath in &path.subpaths {
        work.step()?;
        if subpath.points.len() < 2 {
            continue;
        }
        for index in 0..subpath.points.len() {
            work.step()?;
            let next = if index + 1 == subpath.points.len() {
                0
            } else {
                index + 1
            };
            let start = subpath.points[index];
            let end = subpath.points[next];
            if start.y <= point.y && end.y > point.y {
                if cross(start, end, point)? > 0 {
                    winding = winding.checked_add(1).ok_or_else(numeric)?;
                    parity = !parity;
                }
            } else if end.y <= point.y && start.y > point.y && cross(start, end, point)? < 0 {
                winding = winding.checked_sub(1).ok_or_else(numeric)?;
                parity = !parity;
            }
        }
    }
    Ok(match rule {
        FillRule::Nonzero => winding != 0,
        FillRule::EvenOdd => parity,
    })
}

fn cross(start: Point, end: Point, point: Point) -> Result<i128, FastRasterError> {
    let ax = i128::from(end.x) - i128::from(start.x);
    let ay = i128::from(end.y) - i128::from(start.y);
    let bx = i128::from(point.x) - i128::from(start.x);
    let by = i128::from(point.y) - i128::from(start.y);
    ax.checked_mul(by)
        .and_then(|value| value.checked_sub(ay.checked_mul(bx)?))
        .ok_or_else(numeric)
}

pub(crate) fn composite_coverage(
    surface: &mut [Pixel],
    operation: &Coverage,
    clip: &Coverage,
    paint: Paint,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    if surface.len() != operation.masks.len() || surface.len() != clip.masks.len() {
        return Err(FastRasterError::for_code(
            FastRasterErrorCode::IdentityMismatch,
        ));
    }
    for ((pixel, operation), clip) in surface.iter_mut().zip(&operation.masks).zip(&clip.masks) {
        let coverage = (operation & clip).count_ones();
        if coverage != 0 {
            *pixel = Pixel::from_paint(paint, coverage).source_over(*pixel, paint.blend_mode());
        }
        work.step()?;
    }
    Ok(())
}

pub(crate) fn lookup_path(
    graphics: &GraphicsScene,
    id: pdf_rs_scene::GraphicsResourceId,
) -> Result<&PathResource, FastRasterError> {
    let index = usize::try_from(id.value()).map_err(|_| numeric())?;
    let entry = graphics
        .resources()
        .get(index)
        .ok_or_else(invalid_resource)?;
    if entry.id() != id {
        return Err(invalid_resource());
    }
    match entry.resource() {
        GraphicsResource::Path(path) => Ok(path),
        GraphicsResource::Image(_) | GraphicsResource::GlyphOutline(_) => Err(invalid_resource()),
    }
}

pub(crate) fn lookup_image(
    graphics: &GraphicsScene,
    id: pdf_rs_scene::GraphicsResourceId,
) -> Result<&ImageResource, FastRasterError> {
    let index = usize::try_from(id.value()).map_err(|_| numeric())?;
    let entry = graphics
        .resources()
        .get(index)
        .ok_or_else(invalid_resource)?;
    if entry.id() != id {
        return Err(invalid_resource());
    }
    match entry.resource() {
        GraphicsResource::Image(image) => Ok(image),
        GraphicsResource::Path(_) | GraphicsResource::GlyphOutline(_) => Err(invalid_resource()),
    }
}

pub(crate) fn lookup_glyph(
    graphics: &GraphicsScene,
    id: pdf_rs_scene::GraphicsResourceId,
) -> Result<&pdf_rs_scene::GlyphOutline, FastRasterError> {
    let index = usize::try_from(id.value()).map_err(|_| numeric())?;
    let entry = graphics
        .resources()
        .get(index)
        .ok_or_else(invalid_resource)?;
    if entry.id() != id {
        return Err(invalid_resource());
    }
    match entry.resource() {
        GraphicsResource::GlyphOutline(glyph) => Ok(glyph),
        GraphicsResource::Path(_) | GraphicsResource::Image(_) => Err(invalid_resource()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the image kernel receives immutable source, geometry, clip, alpha, blend, and work state independently"
)]
pub(crate) fn draw_image(
    surface: &mut [Pixel],
    clip: &Coverage,
    rect: WorkRect,
    image: &ImageResource,
    transform: Matrix,
    alpha: SceneUnit,
    blend: BlendMode,
    map: PageMap,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    let origin = map.map(
        ScenePoint::new(SceneScalar::ZERO, SceneScalar::ZERO),
        transform,
        1,
    )?;
    let x_axis = map.map(
        ScenePoint::new(SceneScalar::ONE, SceneScalar::ZERO),
        transform,
        1,
    )?;
    let y_axis = map.map(
        ScenePoint::new(SceneScalar::ZERO, SceneScalar::ONE),
        transform,
        1,
    )?;
    let vx = Point {
        x: x_axis.x.checked_sub(origin.x).ok_or_else(numeric)?,
        y: x_axis.y.checked_sub(origin.y).ok_or_else(numeric)?,
    };
    let vy = Point {
        x: y_axis.x.checked_sub(origin.x).ok_or_else(numeric)?,
        y: y_axis.y.checked_sub(origin.y).ok_or_else(numeric)?,
    };
    let determinant = i128::from(vx.x)
        .checked_mul(i128::from(vy.y))
        .and_then(|value| value.checked_sub(i128::from(vx.y).checked_mul(i128::from(vy.x))?))
        .ok_or_else(numeric)?;
    if determinant == 0 {
        return Ok(());
    }
    for row in 0..rect.height {
        for column in 0..rect.width {
            let index = usize::try_from(
                u64::from(row)
                    .checked_mul(u64::from(rect.width))
                    .and_then(|value| value.checked_add(u64::from(column)))
                    .ok_or_else(numeric)?,
            )
            .map_err(|_| numeric())?;
            let mut accumulated = [0_u64; 4];
            for sample_y in 0..SAMPLE_SIDE {
                for sample_x in 0..SAMPLE_SIDE {
                    let bit =
                        u32::try_from(sample_y * SAMPLE_SIDE + sample_x).map_err(|_| numeric())?;
                    if clip.masks[index] & (1_u16 << bit) == 0 {
                        work.step()?;
                        continue;
                    }
                    let point = Point {
                        x: sample_coordinate(rect.x, column, sample_x)?,
                        y: sample_coordinate(rect.y, row, sample_y)?,
                    };
                    if let Some((x, y)) =
                        image_coordinates(origin, vx, vy, determinant, point, image)?
                    {
                        let (red, green, blue) = image_color(image, x, y)?;
                        let alpha = q16(alpha);
                        accumulated[0] += u64::from(multiply(red, alpha));
                        accumulated[1] += u64::from(multiply(green, alpha));
                        accumulated[2] += u64::from(multiply(blue, alpha));
                        accumulated[3] += u64::from(alpha);
                    }
                    work.step()?;
                }
            }
            let source = Pixel {
                red: average_samples(accumulated[0]),
                green: average_samples(accumulated[1]),
                blue: average_samples(accumulated[2]),
                alpha: average_samples(accumulated[3]),
            };
            if source.alpha != 0 {
                surface[index] = source.source_over(surface[index], blend);
            }
        }
    }
    Ok(())
}

fn image_coordinates(
    origin: Point,
    vx: Point,
    vy: Point,
    determinant: i128,
    point: Point,
    image: &ImageResource,
) -> Result<Option<(u32, u32)>, FastRasterError> {
    let dx = i128::from(point.x) - i128::from(origin.x);
    let dy = i128::from(point.y) - i128::from(origin.y);
    let u = dx
        .checked_mul(i128::from(vy.y))
        .and_then(|value| value.checked_sub(dy.checked_mul(i128::from(vy.x))?))
        .ok_or_else(numeric)?;
    let v = i128::from(vx.x)
        .checked_mul(dy)
        .and_then(|value| value.checked_sub(i128::from(vx.y).checked_mul(dx)?))
        .ok_or_else(numeric)?;
    let (u, v, denominator) = if determinant < 0 {
        (-u, -v, -determinant)
    } else {
        (u, v, determinant)
    };
    if u < 0 || v < 0 || u >= denominator || v >= denominator {
        return Ok(None);
    }
    let x = u
        .checked_mul(i128::from(image.width()))
        .ok_or_else(numeric)?
        / denominator;
    let y = v
        .checked_mul(i128::from(image.height()))
        .ok_or_else(numeric)?
        / denominator;
    Ok(Some((
        u32::try_from(x).map_err(|_| numeric())?,
        u32::try_from(y).map_err(|_| numeric())?,
    )))
}

fn image_color(image: &ImageResource, x: u32, y: u32) -> Result<(u32, u32, u32), FastRasterError> {
    let components = u64::from(image.color_space().components());
    let index = u64::from(y)
        .checked_mul(u64::from(image.width()))
        .and_then(|value| value.checked_add(u64::from(x)))
        .and_then(|value| value.checked_mul(components))
        .ok_or_else(numeric)?;
    let index = usize::try_from(index).map_err(|_| numeric())?;
    let values = image.decoded().get(index..).ok_or_else(invalid_resource)?;
    let unit = |value: u8| SceneUnit::from_u16(u16::from(value) * 257);
    let device_color = match image.color_space() {
        pdf_rs_scene::ImageColorSpace::DeviceGray => DeviceColor::Gray(unit(values[0])),
        pdf_rs_scene::ImageColorSpace::DeviceRgb => DeviceColor::Rgb {
            red: unit(values[0]),
            green: unit(values[1]),
            blue: unit(values[2]),
        },
        pdf_rs_scene::ImageColorSpace::DeviceCmyk => DeviceColor::Cmyk {
            cyan: unit(values[0]),
            magenta: unit(values[1]),
            yellow: unit(values[2]),
            black: unit(values[3]),
        },
    };
    Ok(color(device_color))
}

fn average_samples(total: u64) -> u32 {
    ((total + u64::from(SAMPLE_COUNT / 2)) / u64::from(SAMPLE_COUNT)) as u32
}

pub(crate) fn vector_bytes<T>(values: &Vec<T>) -> Result<u64, FastRasterError> {
    capacity_bytes(values)
}

fn reserve<T>(values: &mut Vec<T>, additional: usize) -> Result<(), FastRasterError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| FastRasterError::for_code(FastRasterErrorCode::Allocation))
}

fn reserve_intermediate<T>(
    values: &mut Vec<T>,
    additional: usize,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<u64, FastRasterError> {
    let required = values.len().checked_add(additional).ok_or_else(numeric)?;
    let minimum_bytes = bytes_for_items::<T>(required)?;
    work.admit_intermediate(add(base_intermediate, minimum_bytes)?)?;
    reserve(values, additional)?;
    let retained_bytes = capacity_bytes(values)?;
    work.admit_intermediate(add(base_intermediate, retained_bytes)?)?;
    Ok(retained_bytes)
}

fn grow_retained_vector<T>(
    values: &mut Vec<T>,
    additional: usize,
    base_intermediate: u64,
    retained: &mut u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    let required = values.len().checked_add(additional).ok_or_else(numeric)?;
    if required <= values.capacity() {
        return Ok(());
    }
    let old_bytes = capacity_bytes(values)?;
    let retained_without_old = retained.checked_sub(old_bytes).ok_or_else(numeric)?;
    let minimum_bytes = bytes_for_items::<T>(required)?;
    work.admit_intermediate(add(add(base_intermediate, *retained)?, minimum_bytes)?)?;
    reserve(values, additional)?;
    let new_bytes = capacity_bytes(values)?;
    work.admit_intermediate(add(add(base_intermediate, *retained)?, new_bytes)?)?;
    *retained = retained_without_old
        .checked_add(new_bytes)
        .ok_or_else(numeric)?;
    work.admit_intermediate(add(base_intermediate, *retained)?)?;
    Ok(())
}

fn bytes_for_items<T>(items: usize) -> Result<u64, FastRasterError> {
    u64::try_from(items)
        .ok()
        .and_then(|count| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| count.checked_mul(width))
        })
        .ok_or_else(numeric)
}

fn capacity_bytes<T>(values: &Vec<T>) -> Result<u64, FastRasterError> {
    u64::try_from(values.capacity())
        .ok()
        .and_then(|capacity| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|width| capacity.checked_mul(width))
        })
        .ok_or_else(numeric)
}

fn add(left: u64, right: u64) -> Result<u64, FastRasterError> {
    left.checked_add(right).ok_or_else(numeric)
}

fn numeric() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::NumericOverflow)
}

fn invalid_resource() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::InvalidResource)
}
