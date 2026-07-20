use pdf_rs_scene::{DashPattern, LineCap, LineJoin, LineStyle};

use super::coverage::{CoverageMask, SAMPLE_GRID_WIDTH, SAMPLES_PER_PIXEL, sample_point};
use super::geometry::{
    Affine, Fixed, FixedPoint, FlattenedPath, GeometryFailure, GeometryWork, flatten_path,
    integer_sqrt, point_distance_squared, rounded_divide_i128,
};

const CURVE_TOLERANCE_RAW: i64 = Fixed::ONE.raw() / 256;
const MAX_CURVE_RECURSION: u8 = 16;

#[derive(Clone, Debug, Eq, PartialEq)]
struct StrokeRun {
    points: Vec<FixedPoint>,
    closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum StrokePrimitive {
    Polygon(Vec<FixedPoint>),
    Circle {
        center: FixedPoint,
        radius: Fixed,
    },
    RoundSector {
        center: FixedPoint,
        radius: Fixed,
        incoming: FixedPoint,
        outgoing: FixedPoint,
        turn: i8,
    },
}

#[derive(Clone, Copy)]
struct StrokeJoinStyle {
    join: LineJoin,
    miter_limit: pdf_rs_scene::SceneScalar,
    half_width: Fixed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StrokeOutline {
    primitives: Vec<StrokePrimitive>,
    comparison_work: u64,
}

#[derive(Default)]
struct StrokeOutlineBuilder {
    primitives: Vec<StrokePrimitive>,
    comparison_work: u64,
}

impl StrokeOutline {
    pub(crate) fn from_flattened(
        path: &FlattenedPath,
        style: &LineStyle,
        work: &mut GeometryWork<'_>,
    ) -> Result<Self, GeometryFailure> {
        let width = Fixed::from_scene(style.width())?;
        let half_width = width.checked_half()?;
        Self::from_flattened_with_width(path, style, half_width, work)
    }

    fn from_flattened_with_width(
        path: &FlattenedPath,
        style: &LineStyle,
        half_width: Fixed,
        work: &mut GeometryWork<'_>,
    ) -> Result<Self, GeometryFailure> {
        Self::from_runs(
            dashed_runs(path, style.dash(), work)?,
            style,
            half_width,
            work,
        )
    }

    fn from_runs(
        runs: Vec<StrokeRun>,
        style: &LineStyle,
        half_width: Fixed,
        work: &mut GeometryWork<'_>,
    ) -> Result<Self, GeometryFailure> {
        let mut builder = StrokeOutlineBuilder::default();
        for run in runs {
            append_run_primitives(&mut builder, &run, style, half_width, work)?;
        }
        Ok(Self {
            primitives: builder.primitives,
            comparison_work: builder.comparison_work,
        })
    }

    pub(crate) fn contains(
        &self,
        point: FixedPoint,
        work: &mut GeometryWork<'_>,
    ) -> Result<bool, GeometryFailure> {
        for primitive in &self.primitives {
            work.charge_fuel(1)?;
            if primitive_contains(primitive, point, work)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    const fn comparison_bound(&self) -> u64 {
        self.comparison_work
    }

    fn is_empty(&self) -> bool {
        self.primitives.is_empty()
    }

    #[cfg(test)]
    fn primitives(&self) -> &[StrokePrimitive] {
        &self.primitives
    }
}

pub(crate) fn rasterize_stroke(
    path: &pdf_rs_scene::PathResource,
    path_to_page: Affine,
    page_to_device: Affine,
    style: &LineStyle,
    width: u32,
    height: u32,
    work: &mut GeometryWork<'_>,
) -> Result<CoverageMask, GeometryFailure> {
    let stroke_to_page = Affine::from_scene(style.stroke_transform())?;
    let page_to_stroke = stroke_to_page
        .inverse()?
        .ok_or(GeometryFailure::InvalidGeometry)?;
    let path_to_stroke = page_to_stroke.checked_concat(path_to_page)?;
    let path_to_device = page_to_device.checked_concat(path_to_page)?;
    let stroke_to_device = page_to_device.checked_concat(stroke_to_page)?;
    let flattened_stroke = flatten_path(
        path,
        path_to_stroke,
        path_to_device,
        Fixed::from_raw(CURVE_TOLERANCE_RAW),
        MAX_CURVE_RECURSION.min(work.limits().max_curve_recursion),
        work,
    )?;
    let (outline, device_to_outline) = if style.width() == pdf_rs_scene::SceneScalar::ZERO {
        let mut runs = dashed_runs(&flattened_stroke, style.dash(), work)?;
        for run in &mut runs {
            for point in &mut run.points {
                work.charge_fuel(1)?;
                *point = stroke_to_device.apply(*point)?;
            }
        }
        (
            StrokeOutline::from_runs(runs, style, Fixed::HALF, work)?,
            Some(Affine::IDENTITY),
        )
    } else {
        (
            StrokeOutline::from_flattened(&flattened_stroke, style, work)?,
            stroke_to_device.inverse()?,
        )
    };
    let device_to_outline = device_to_outline.ok_or(GeometryFailure::InvalidGeometry)?;
    if outline.is_empty() {
        return CoverageMask::empty(width, height, work);
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(GeometryFailure::NumericOverflow)?;
    let samples = pixels
        .checked_mul(u64::from(SAMPLES_PER_PIXEL))
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.preflight_samples(samples)?;
    let comparison_fuel = samples
        .checked_mul(outline.comparison_bound())
        .ok_or(GeometryFailure::NumericOverflow)?;
    let initialization_fuel = CoverageMask::initialization_fuel(width, height)?;
    let total_fuel = initialization_fuel
        .checked_add(samples)
        .and_then(|value| value.checked_add(comparison_fuel))
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.preflight_fuel(total_fuel)?;
    let mut mask = CoverageMask::empty(width, height, work)?;
    if width == 0 || height == 0 {
        return Ok(mask);
    }
    for y in 0..height {
        for x in 0..width {
            work.charge_samples(u64::from(SAMPLES_PER_PIXEL))?;
            let mut pixel_mask = 0_u64;
            for sample_y in 0..SAMPLE_GRID_WIDTH {
                for sample_x in 0..SAMPLE_GRID_WIDTH {
                    let device = sample_point(x, y, sample_x, sample_y)?;
                    let outline_point = device_to_outline.apply(device)?;
                    if outline.contains(outline_point, work)? {
                        let bit = sample_y
                            .checked_mul(SAMPLE_GRID_WIDTH)
                            .and_then(|value| value.checked_add(sample_x))
                            .ok_or(GeometryFailure::NumericOverflow)?;
                        pixel_mask |= 1_u64
                            .checked_shl(bit)
                            .ok_or(GeometryFailure::NumericOverflow)?;
                    }
                }
            }
            mask.set_sample_mask(x, y, pixel_mask)?;
        }
    }
    Ok(mask)
}

fn append_run_primitives(
    builder: &mut StrokeOutlineBuilder,
    run: &StrokeRun,
    style: &LineStyle,
    half_width: Fixed,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    if half_width <= Fixed::ZERO {
        return Ok(());
    }
    let mut points = remove_consecutive_duplicates(&run.points, work)?;
    if run.closed && points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    if points.len() == 1 {
        if run.closed {
            if style.cap() == LineCap::Round {
                push_primitive(
                    builder,
                    StrokePrimitive::Circle {
                        center: points[0],
                        radius: half_width,
                    },
                    work,
                )?;
            }
        } else {
            append_degenerate(builder, points[0], style.cap(), half_width, work)?;
        }
        return Ok(());
    }
    if points.len() < 2 {
        return Ok(());
    }

    let segment_count = if run.closed {
        points.len()
    } else {
        points.len() - 1
    };
    let join_style = StrokeJoinStyle {
        join: style.join(),
        miter_limit: style.miter_limit(),
        half_width,
    };
    for index in 0..segment_count {
        work.charge_fuel(1)?;
        let start = points[index];
        let end = points[(index + 1) % points.len()];
        if start == end {
            continue;
        }
        let extend_start = !run.closed && index == 0 && style.cap() == LineCap::Square;
        let extend_end =
            !run.closed && index + 1 == segment_count && style.cap() == LineCap::Square;
        let polygon = segment_polygon(start, end, half_width, extend_start, extend_end, work)?;
        push_primitive(builder, StrokePrimitive::Polygon(polygon), work)?;
    }

    if run.closed {
        for index in 0..points.len() {
            work.charge_fuel(1)?;
            let previous = points[(index + points.len() - 1) % points.len()];
            let vertex = points[index];
            let next = points[(index + 1) % points.len()];
            append_join(builder, previous, vertex, next, join_style, work)?;
        }
    } else {
        for index in 1..points.len() - 1 {
            work.charge_fuel(1)?;
            append_join(
                builder,
                points[index - 1],
                points[index],
                points[index + 1],
                join_style,
                work,
            )?;
        }
        if style.cap() == LineCap::Round {
            push_primitive(
                builder,
                StrokePrimitive::Circle {
                    center: points[0],
                    radius: half_width,
                },
                work,
            )?;
            push_primitive(
                builder,
                StrokePrimitive::Circle {
                    center: *points.last().ok_or(GeometryFailure::InvalidGeometry)?,
                    radius: half_width,
                },
                work,
            )?;
        }
    }
    Ok(())
}

fn remove_consecutive_duplicates(
    points: &[FixedPoint],
    work: &mut GeometryWork<'_>,
) -> Result<Vec<FixedPoint>, GeometryFailure> {
    let mut output = Vec::new();
    for point in points {
        work.charge_fuel(1)?;
        if output.last() != Some(point) {
            work.try_push_geometry(&mut output, *point)?;
        }
    }
    Ok(output)
}

fn append_degenerate(
    builder: &mut StrokeOutlineBuilder,
    point: FixedPoint,
    cap: LineCap,
    half_width: Fixed,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    match cap {
        LineCap::Butt => {}
        LineCap::Round => push_primitive(
            builder,
            StrokePrimitive::Circle {
                center: point,
                radius: half_width,
            },
            work,
        )?,
        LineCap::Square => {
            let minimum = FixedPoint::new(
                point.x.checked_sub(half_width)?,
                point.y.checked_sub(half_width)?,
            );
            let maximum = FixedPoint::new(
                point.x.checked_add(half_width)?,
                point.y.checked_add(half_width)?,
            );
            let points = polygon(
                &[
                    minimum,
                    FixedPoint::new(maximum.x, minimum.y),
                    maximum,
                    FixedPoint::new(minimum.x, maximum.y),
                ],
                work,
            )?;
            push_primitive(builder, StrokePrimitive::Polygon(points), work)?;
        }
    }
    Ok(())
}

fn segment_polygon(
    start: FixedPoint,
    end: FixedPoint,
    half_width: Fixed,
    extend_start: bool,
    extend_end: bool,
    work: &mut GeometryWork<'_>,
) -> Result<Vec<FixedPoint>, GeometryFailure> {
    let (tangent, normal) = tangent_and_left_normal(start, end, half_width)?;
    let start_center = if extend_start {
        subtract_point(start, tangent)?
    } else {
        start
    };
    let end_center = if extend_end {
        add_point(end, tangent)?
    } else {
        end
    };
    polygon(
        &[
            add_point(start_center, normal)?,
            add_point(end_center, normal)?,
            subtract_point(end_center, normal)?,
            subtract_point(start_center, normal)?,
        ],
        work,
    )
}

fn append_join(
    builder: &mut StrokeOutlineBuilder,
    previous: FixedPoint,
    vertex: FixedPoint,
    next: FixedPoint,
    style: StrokeJoinStyle,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    if previous == vertex || vertex == next {
        return Ok(());
    }
    let incoming = subtract_point(vertex, previous)?;
    let outgoing = subtract_point(next, vertex)?;
    let turn = cross(incoming, outgoing)?;
    if turn == 0 {
        if dot(incoming, outgoing)? < 0 && style.join == LineJoin::Round {
            push_primitive(
                builder,
                StrokePrimitive::Circle {
                    center: vertex,
                    radius: style.half_width,
                },
                work,
            )?;
        }
        return Ok(());
    }
    let (_, incoming_left) = tangent_and_left_normal(previous, vertex, style.half_width)?;
    let (_, outgoing_left) = tangent_and_left_normal(vertex, next, style.half_width)?;
    let (incoming_outer, outgoing_outer, turn_sign) = if turn > 0 {
        (
            negate_point(incoming_left)?,
            negate_point(outgoing_left)?,
            1,
        )
    } else {
        (incoming_left, outgoing_left, -1)
    };
    let first = add_point(vertex, incoming_outer)?;
    let second = add_point(vertex, outgoing_outer)?;
    match style.join {
        LineJoin::Bevel => {
            let points = polygon(&[vertex, first, second], work)?;
            push_primitive(builder, StrokePrimitive::Polygon(points), work)?;
        }
        LineJoin::Round => push_primitive(
            builder,
            StrokePrimitive::RoundSector {
                center: vertex,
                radius: style.half_width,
                incoming,
                outgoing,
                turn: turn_sign,
            },
            work,
        )?,
        LineJoin::Miter => {
            let miter = line_intersection(first, incoming, second, outgoing)?;
            let limit = Fixed::from_scene(style.miter_limit)?.checked_mul(style.half_width)?;
            let within_limit = miter
                .map(|point| {
                    point_distance_squared(point, vertex).and_then(|distance| {
                        let limit_squared = i128::from(limit.raw())
                            .checked_mul(i128::from(limit.raw()))
                            .ok_or(GeometryFailure::NumericOverflow)?;
                        Ok(distance <= limit_squared)
                    })
                })
                .transpose()?
                .unwrap_or(false);
            if within_limit {
                let points = polygon(
                    &[
                        vertex,
                        first,
                        miter.ok_or(GeometryFailure::InvalidGeometry)?,
                        second,
                    ],
                    work,
                )?;
                push_primitive(builder, StrokePrimitive::Polygon(points), work)?;
            } else {
                let points = polygon(&[vertex, first, second], work)?;
                push_primitive(builder, StrokePrimitive::Polygon(points), work)?;
            }
        }
    }
    Ok(())
}

fn push_primitive(
    builder: &mut StrokeOutlineBuilder,
    primitive: StrokePrimitive,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    let comparison_work = match &primitive {
        StrokePrimitive::Polygon(vertices) => u64::try_from(vertices.len())
            .map_err(|_| GeometryFailure::NumericOverflow)?
            .checked_add(1)
            .ok_or(GeometryFailure::NumericOverflow)?,
        StrokePrimitive::Circle { .. } | StrokePrimitive::RoundSector { .. } => 1,
    };
    let next_comparison_work = builder
        .comparison_work
        .checked_add(comparison_work)
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.charge_stroke_primitives(1)?;
    work.try_push_geometry(&mut builder.primitives, primitive)?;
    builder.comparison_work = next_comparison_work;
    Ok(())
}

fn tangent_and_left_normal(
    start: FixedPoint,
    end: FixedPoint,
    magnitude: Fixed,
) -> Result<(FixedPoint, FixedPoint), GeometryFailure> {
    let delta = subtract_point(end, start)?;
    let length = vector_length(delta)?;
    if length == Fixed::ZERO {
        return Err(GeometryFailure::InvalidGeometry);
    }
    let tangent = FixedPoint::new(
        delta.x.checked_mul_ratio(magnitude, length)?,
        delta.y.checked_mul_ratio(magnitude, length)?,
    );
    let normal = FixedPoint::new(tangent.y.checked_neg()?, tangent.x);
    Ok((tangent, normal))
}

fn vector_length(vector: FixedPoint) -> Result<Fixed, GeometryFailure> {
    let x = i128::from(vector.x.raw());
    let y = i128::from(vector.y.raw());
    let squared = x
        .checked_mul(x)
        .and_then(|x_squared| {
            y.checked_mul(y)
                .and_then(|y_squared| x_squared.checked_add(y_squared))
        })
        .ok_or(GeometryFailure::NumericOverflow)?;
    let squared = u128::try_from(squared).map_err(|_| GeometryFailure::NumericOverflow)?;
    let length = integer_sqrt(squared);
    i64::try_from(length)
        .map(Fixed::from_raw)
        .map_err(|_| GeometryFailure::NumericOverflow)
}

fn line_intersection(
    first: FixedPoint,
    first_direction: FixedPoint,
    second: FixedPoint,
    second_direction: FixedPoint,
) -> Result<Option<FixedPoint>, GeometryFailure> {
    let denominator = cross(first_direction, second_direction)?;
    if denominator == 0 {
        return Ok(None);
    }
    let separation = subtract_point(second, first)?;
    let numerator = cross(separation, second_direction)?;
    let x_delta = i128::from(first_direction.x.raw())
        .checked_mul(numerator)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let y_delta = i128::from(first_direction.y.raw())
        .checked_mul(numerator)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let x_delta = rounded_divide_i128(x_delta, denominator)?;
    let y_delta = rounded_divide_i128(y_delta, denominator)?;
    let x_delta = i64::try_from(x_delta).map_err(|_| GeometryFailure::NumericOverflow)?;
    let y_delta = i64::try_from(y_delta).map_err(|_| GeometryFailure::NumericOverflow)?;
    Ok(Some(FixedPoint::new(
        first.x.checked_add(Fixed::from_raw(x_delta))?,
        first.y.checked_add(Fixed::from_raw(y_delta))?,
    )))
}

fn primitive_contains(
    primitive: &StrokePrimitive,
    point: FixedPoint,
    work: &mut GeometryWork<'_>,
) -> Result<bool, GeometryFailure> {
    match primitive {
        StrokePrimitive::Polygon(vertices) => point_in_convex_polygon(vertices, point, work),
        StrokePrimitive::Circle { center, radius } => {
            let distance = point_distance_squared(*center, point)?;
            let radius_squared = i128::from(radius.raw())
                .checked_mul(i128::from(radius.raw()))
                .ok_or(GeometryFailure::NumericOverflow)?;
            Ok(distance <= radius_squared)
        }
        StrokePrimitive::RoundSector {
            center,
            radius,
            incoming,
            outgoing,
            turn,
        } => {
            let vector = subtract_point(point, *center)?;
            let distance = point_distance_squared(*center, point)?;
            let radius_squared = i128::from(radius.raw())
                .checked_mul(i128::from(radius.raw()))
                .ok_or(GeometryFailure::NumericOverflow)?;
            if distance > radius_squared {
                return Ok(false);
            }
            let incoming_outer = if *turn > 0 {
                FixedPoint::new(incoming.y, incoming.x.checked_neg()?)
            } else {
                FixedPoint::new(incoming.y.checked_neg()?, incoming.x)
            };
            let outgoing_outer = if *turn > 0 {
                FixedPoint::new(outgoing.y, outgoing.x.checked_neg()?)
            } else {
                FixedPoint::new(outgoing.y.checked_neg()?, outgoing.x)
            };
            let from_incoming = cross(incoming_outer, vector)?;
            let to_outgoing = cross(vector, outgoing_outer)?;
            Ok(if *turn > 0 {
                from_incoming >= 0 && to_outgoing >= 0
            } else {
                from_incoming <= 0 && to_outgoing <= 0
            })
        }
    }
}

fn point_in_convex_polygon(
    vertices: &[FixedPoint],
    point: FixedPoint,
    work: &mut GeometryWork<'_>,
) -> Result<bool, GeometryFailure> {
    if vertices.len() < 3 {
        return Ok(false);
    }
    let mut positive = false;
    let mut negative = false;
    for index in 0..vertices.len() {
        work.charge_fuel(1)?;
        let edge = subtract_point(vertices[(index + 1) % vertices.len()], vertices[index])?;
        let relative = subtract_point(point, vertices[index])?;
        let side = cross(edge, relative)?;
        positive |= side > 0;
        negative |= side < 0;
        if positive && negative {
            return Ok(false);
        }
    }
    Ok(true)
}

fn polygon(
    points: &[FixedPoint],
    work: &mut GeometryWork<'_>,
) -> Result<Vec<FixedPoint>, GeometryFailure> {
    let mut output = Vec::new();
    let copy_fuel = u64::try_from(points.len()).map_err(|_| GeometryFailure::NumericOverflow)?;
    work.charge_fuel(copy_fuel)?;
    work.try_reserve_geometry(&mut output, points.len())?;
    output.extend_from_slice(points);
    Ok(output)
}

fn dashed_runs(
    path: &FlattenedPath,
    dash: &DashPattern,
    work: &mut GeometryWork<'_>,
) -> Result<Vec<StrokeRun>, GeometryFailure> {
    if dash.array().is_empty() {
        let mut active_subpaths = 0_usize;
        let mut copy_fuel = 0_u64;
        for subpath in path.subpaths() {
            work.charge_fuel(1)?;
            if subpath.has_segments() || subpath.closed() {
                active_subpaths = active_subpaths
                    .checked_add(1)
                    .ok_or(GeometryFailure::NumericOverflow)?;
                copy_fuel = copy_fuel
                    .checked_add(
                        u64::try_from(subpath.points().len())
                            .map_err(|_| GeometryFailure::NumericOverflow)?,
                    )
                    .and_then(|value| value.checked_add(1))
                    .ok_or(GeometryFailure::NumericOverflow)?;
            }
        }
        work.preflight_fuel(copy_fuel)?;
        work.preflight_stroke_runs(
            u64::try_from(active_subpaths).map_err(|_| GeometryFailure::NumericOverflow)?,
        )?;
        let mut runs = Vec::new();
        for subpath in path.subpaths() {
            if !subpath.has_segments() && !subpath.closed() {
                continue;
            }
            let mut points = Vec::new();
            let point_count = u64::try_from(subpath.points().len())
                .map_err(|_| GeometryFailure::NumericOverflow)?;
            work.charge_fuel(point_count)?;
            work.try_reserve_geometry(&mut points, subpath.points().len())?;
            points.extend_from_slice(subpath.points());
            push_run(
                &mut runs,
                StrokeRun {
                    points,
                    closed: subpath.closed(),
                },
                work,
            )?;
        }
        return Ok(runs);
    }
    let input_pattern_len =
        u64::try_from(dash.array().len()).map_err(|_| GeometryFailure::NumericOverflow)?;
    let duplicated_pattern_len = if dash.array().len() % 2 == 1 {
        input_pattern_len
    } else {
        0
    };
    let canonical_pattern_len = input_pattern_len
        .checked_add(duplicated_pattern_len)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let parse_fuel = input_pattern_len
        .checked_add(duplicated_pattern_len)
        .and_then(|value| value.checked_add(canonical_pattern_len))
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.preflight_fuel(parse_fuel)?;
    let mut pattern = Vec::new();
    for value in dash.array() {
        work.charge_fuel(1)?;
        work.try_push_geometry(&mut pattern, Fixed::from_scene(*value)?)?;
    }
    if pattern.len() % 2 == 1 {
        let initial_length = pattern.len();
        for index in 0..initial_length {
            work.charge_fuel(1)?;
            let value = pattern[index];
            work.try_push_geometry(&mut pattern, value)?;
        }
    }
    let mut total = Fixed::ZERO;
    for value in &pattern {
        work.charge_fuel(1)?;
        total = total.checked_add(*value)?;
    }
    if total <= Fixed::ZERO {
        return Err(GeometryFailure::InvalidGeometry);
    }
    let phase = Fixed::from_scene(dash.phase())?;
    let mut runs = Vec::new();
    for subpath in path.subpaths() {
        work.charge_fuel(1)?;
        if !subpath.has_segments() && !subpath.closed() {
            continue;
        }
        let subpath_runs = dash_subpath(
            subpath.points(),
            subpath.closed(),
            &pattern,
            total,
            phase,
            work,
        )?;
        let run_count =
            u64::try_from(subpath_runs.len()).map_err(|_| GeometryFailure::NumericOverflow)?;
        work.charge_fuel(run_count)?;
        work.try_reserve_geometry(&mut runs, subpath_runs.len())?;
        runs.extend(subpath_runs);
    }
    Ok(runs)
}

fn dash_subpath(
    points: &[FixedPoint],
    closed: bool,
    pattern: &[Fixed],
    total: Fixed,
    phase: Fixed,
    work: &mut GeometryWork<'_>,
) -> Result<Vec<StrokeRun>, GeometryFailure> {
    if points.is_empty() {
        return Ok(Vec::new());
    }
    let (mut dash_index, mut dash_remaining) = initial_dash_state(pattern, total, phase, work)?;
    let starts_on = dash_index % 2 == 0;
    if points.len() == 1 {
        return Ok(if starts_on {
            let mut runs = Vec::new();
            let mut run_points = Vec::new();
            work.try_push_geometry(&mut run_points, points[0])?;
            push_run(
                &mut runs,
                StrokeRun {
                    points: run_points,
                    closed,
                },
                work,
            )?;
            runs
        } else {
            Vec::new()
        });
    }

    let segment_count = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    let mut runs = Vec::new();
    let mut current: Option<Vec<FixedPoint>> = None;
    let mut saw_nonzero_segment = false;
    for segment_index in 0..segment_count {
        work.charge_fuel(1)?;
        let start = points[segment_index];
        let end = points[(segment_index + 1) % points.len()];
        let length = vector_length(subtract_point(end, start)?)?;
        if length == Fixed::ZERO {
            continue;
        }
        saw_nonzero_segment = true;
        let segment_length =
            u64::try_from(length.raw()).map_err(|_| GeometryFailure::InvalidGeometry)?;
        let mut consumed = 0_u64;
        while consumed < segment_length {
            if dash_remaining == Fixed::ZERO {
                advance_dash(pattern, &mut dash_index, &mut dash_remaining, work)?;
            }
            work.charge_dash_chunks(1)?;
            let remaining_segment = segment_length
                .checked_sub(consumed)
                .ok_or(GeometryFailure::NumericOverflow)?;
            let remaining_dash = u64::try_from(dash_remaining.raw())
                .map_err(|_| GeometryFailure::InvalidGeometry)?;
            let amount = remaining_segment.min(remaining_dash);
            if amount == 0 {
                return Err(GeometryFailure::InvalidGeometry);
            }
            let chunk_start = if consumed == 0 {
                start
            } else {
                start.checked_lerp(end, consumed, segment_length)?
            };
            let next_consumed = consumed
                .checked_add(amount)
                .ok_or(GeometryFailure::NumericOverflow)?;
            let chunk_end = if next_consumed == segment_length {
                end
            } else {
                start.checked_lerp(end, next_consumed, segment_length)?
            };
            if dash_index % 2 == 0 {
                if current.is_none() {
                    let mut points = Vec::new();
                    work.try_push_geometry(&mut points, chunk_start)?;
                    current = Some(points);
                }
                let run = current.as_mut().ok_or(GeometryFailure::InvalidGeometry)?;
                if run.last() != Some(&chunk_end) {
                    work.try_push_geometry(run, chunk_end)?;
                }
            }
            consumed = next_consumed;
            let amount = i64::try_from(amount).map_err(|_| GeometryFailure::NumericOverflow)?;
            dash_remaining = Fixed::from_raw(
                dash_remaining
                    .raw()
                    .checked_sub(amount)
                    .ok_or(GeometryFailure::NumericOverflow)?,
            );
            if dash_remaining == Fixed::ZERO {
                let was_on = dash_index % 2 == 0;
                advance_dash(pattern, &mut dash_index, &mut dash_remaining, work)?;
                let is_on = dash_index % 2 == 0;
                if was_on
                    && !is_on
                    && let Some(points) = current.take()
                {
                    push_run(
                        &mut runs,
                        StrokeRun {
                            points,
                            closed: false,
                        },
                        work,
                    )?;
                }
            }
        }
    }
    if !saw_nonzero_segment {
        if starts_on {
            let mut run_points = Vec::new();
            work.try_push_geometry(&mut run_points, points[0])?;
            push_run(
                &mut runs,
                StrokeRun {
                    points: run_points,
                    closed,
                },
                work,
            )?;
        }
        return Ok(runs);
    }
    let ends_on_without_boundary = current.is_some();
    if let Some(points) = current.take() {
        push_run(
            &mut runs,
            StrokeRun {
                points,
                closed: false,
            },
            work,
        )?;
    }
    if closed && starts_on && ends_on_without_boundary {
        merge_closed_seam(&mut runs, points[0], work)?;
    }
    Ok(runs)
}

fn initial_dash_state(
    pattern: &[Fixed],
    total: Fixed,
    phase: Fixed,
    work: &mut GeometryWork<'_>,
) -> Result<(usize, Fixed), GeometryFailure> {
    work.charge_fuel(1)?;
    let mut phase = phase.raw() % total.raw();
    let mut index = 0_usize;
    let mut remaining = pattern[0];
    let mut visits = 0_usize;
    while remaining == Fixed::ZERO || phase >= remaining.raw() {
        work.charge_fuel(1)?;
        if remaining != Fixed::ZERO {
            phase -= remaining.raw();
        }
        index = (index + 1) % pattern.len();
        remaining = pattern[index];
        visits = visits
            .checked_add(1)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let maximum_visits = pattern
            .len()
            .checked_mul(2)
            .ok_or(GeometryFailure::NumericOverflow)?;
        if visits > maximum_visits {
            return Err(GeometryFailure::InvalidGeometry);
        }
    }
    Ok((
        index,
        Fixed::from_raw(
            remaining
                .raw()
                .checked_sub(phase)
                .ok_or(GeometryFailure::NumericOverflow)?,
        ),
    ))
}

fn advance_dash(
    pattern: &[Fixed],
    index: &mut usize,
    remaining: &mut Fixed,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    for _ in 0..pattern.len() {
        work.charge_fuel(1)?;
        *index = (*index + 1) % pattern.len();
        *remaining = pattern[*index];
        if *remaining > Fixed::ZERO {
            return Ok(());
        }
    }
    Err(GeometryFailure::InvalidGeometry)
}

fn merge_closed_seam(
    runs: &mut Vec<StrokeRun>,
    start: FixedPoint,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    work.charge_fuel(1)?;
    let first_starts_at_seam = runs
        .first()
        .and_then(|run| run.points.first())
        .is_some_and(|point| *point == start);
    let last_ends_at_seam = runs
        .last()
        .and_then(|run| run.points.last())
        .is_some_and(|point| *point == start);
    if !first_starts_at_seam || !last_ends_at_seam {
        return Ok(());
    }
    if runs.len() == 1 {
        work.charge_fuel(1)?;
        if let Some(run) = runs.first_mut() {
            if run.points.len() > 1 && run.points.last() == run.points.first() {
                run.points.pop();
            }
            run.closed = true;
        }
        return Ok(());
    }
    let shift_fuel = u64::try_from(runs.len())
        .map_err(|_| GeometryFailure::NumericOverflow)?
        .checked_mul(2)
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.charge_fuel(shift_fuel)?;
    let first = runs.remove(0);
    let mut last = runs.pop().ok_or(GeometryFailure::InvalidGeometry)?;
    let appended = first.points.len().saturating_sub(1);
    work.charge_fuel(u64::try_from(appended).map_err(|_| GeometryFailure::NumericOverflow)?)?;
    work.try_reserve_geometry(&mut last.points, appended)?;
    last.points.extend(first.points.into_iter().skip(1));
    runs.insert(0, last);
    Ok(())
}

fn push_run(
    runs: &mut Vec<StrokeRun>,
    run: StrokeRun,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    work.charge_stroke_runs(1)?;
    work.try_push_geometry(runs, run)
}

fn add_point(first: FixedPoint, second: FixedPoint) -> Result<FixedPoint, GeometryFailure> {
    Ok(FixedPoint::new(
        first.x.checked_add(second.x)?,
        first.y.checked_add(second.y)?,
    ))
}

fn subtract_point(first: FixedPoint, second: FixedPoint) -> Result<FixedPoint, GeometryFailure> {
    Ok(FixedPoint::new(
        first.x.checked_sub(second.x)?,
        first.y.checked_sub(second.y)?,
    ))
}

fn negate_point(point: FixedPoint) -> Result<FixedPoint, GeometryFailure> {
    Ok(FixedPoint::new(
        point.x.checked_neg()?,
        point.y.checked_neg()?,
    ))
}

fn cross(first: FixedPoint, second: FixedPoint) -> Result<i128, GeometryFailure> {
    i128::from(first.x.raw())
        .checked_mul(i128::from(second.y.raw()))
        .and_then(|left| {
            i128::from(first.y.raw())
                .checked_mul(i128::from(second.x.raw()))
                .and_then(|right| left.checked_sub(right))
        })
        .ok_or(GeometryFailure::NumericOverflow)
}

fn dot(first: FixedPoint, second: FixedPoint) -> Result<i128, GeometryFailure> {
    i128::from(first.x.raw())
        .checked_mul(i128::from(second.x.raw()))
        .and_then(|left| {
            i128::from(first.y.raw())
                .checked_mul(i128::from(second.y.raw()))
                .and_then(|right| left.checked_add(right))
        })
        .ok_or(GeometryFailure::NumericOverflow)
}

#[cfg(test)]
mod tests {
    use pdf_rs_scene::{
        DashPattern, LineCap, LineJoin, LineStyle, Matrix, PathResource, PathSegment, ScenePoint,
        SceneScalar,
    };

    use super::{
        StrokeOutline, StrokePrimitive, dashed_runs, primitive_contains, rasterize_stroke,
    };
    use crate::reference::geometry::{
        Affine, Fixed, FixedPoint, GeometryCancellation, GeometryFailure, GeometryLimitKind,
        GeometryLimits, GeometryWork, flatten_path,
    };

    struct NeverCancel;

    impl GeometryCancellation for NeverCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn scalar(value: &str) -> SceneScalar {
        SceneScalar::from_decimal(value).unwrap()
    }

    fn point(x: &str, y: &str) -> ScenePoint {
        ScenePoint::new(scalar(x), scalar(y))
    }

    fn matrix(components: [&str; 6]) -> Matrix {
        Matrix::new(components.map(scalar))
    }

    fn style(
        width: &str,
        cap: LineCap,
        join: LineJoin,
        miter: &str,
        dash: &[&str],
        phase: &str,
        transform: Matrix,
    ) -> LineStyle {
        LineStyle::new(
            scalar(width),
            cap,
            join,
            scalar(miter),
            DashPattern::new(
                dash.iter().map(|value| scalar(value)).collect(),
                scalar(phase),
            )
            .unwrap(),
            transform,
        )
        .unwrap()
    }

    fn line(start: ScenePoint, end: ScenePoint) -> PathResource {
        PathResource::new(vec![PathSegment::MoveTo(start), PathSegment::LineTo(end)]).unwrap()
    }

    #[test]
    fn butt_line_has_exact_half_pixel_rows_on_small_grid() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mask = rasterize_stroke(
            &line(point("0", "1"), point("2", "1")),
            Affine::IDENTITY,
            Affine::IDENTITY,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Miter,
                "10",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            2,
            2,
            &mut work,
        )
        .unwrap();
        assert_eq!(mask.coverage(0, 0), Some(32));
        assert_eq!(mask.coverage(1, 0), Some(32));
        assert_eq!(mask.coverage(0, 1), Some(32));
        assert_eq!(mask.coverage(1, 1), Some(32));
    }

    #[test]
    fn zero_width_hairline_is_exactly_one_device_pixel_wide() {
        let transforms = [
            Affine::IDENTITY,
            Affine::from_scene(Matrix::new([
                scalar("2"),
                SceneScalar::ZERO,
                SceneScalar::ZERO,
                scalar("3"),
                SceneScalar::ZERO,
                scalar("-1"),
            ]))
            .unwrap(),
            Affine::from_scene(Matrix::new([
                scalar("-1"),
                SceneScalar::ZERO,
                SceneScalar::ZERO,
                scalar("1"),
                scalar("1"),
                SceneScalar::ZERO,
            ]))
            .unwrap(),
        ];
        for page_to_device in transforms {
            let cancellation = NeverCancel;
            let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            let mask = rasterize_stroke(
                &line(point("0", "0.5"), point("1", "0.5")),
                Affine::IDENTITY,
                page_to_device,
                &style(
                    "0",
                    LineCap::Butt,
                    LineJoin::Miter,
                    "10",
                    &[],
                    "0",
                    Matrix::IDENTITY,
                ),
                1,
                1,
                &mut work,
            )
            .unwrap();
            assert_eq!(mask.coverage(0, 0), Some(64));
        }
    }

    #[test]
    fn dash_phase_and_repetition_are_exact_in_user_space() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mask = rasterize_stroke(
            &line(point("0", "0.5"), point("4", "0.5")),
            Affine::IDENTITY,
            Affine::IDENTITY,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Miter,
                "10",
                &["1", "1"],
                "0",
                Matrix::IDENTITY,
            ),
            4,
            1,
            &mut work,
        )
        .unwrap();
        assert_eq!(
            (0..4)
                .map(|x| mask.coverage(x, 0).unwrap())
                .collect::<Vec<_>>(),
            vec![64, 0, 64, 0]
        );
    }

    #[test]
    fn zero_length_dash_gaps_do_not_split_a_continuous_on_run() {
        let path = line(point("0", "0"), point("2", "0"));
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let flattened = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut work,
        )
        .unwrap();
        let dash =
            DashPattern::new(vec![scalar("1"), SceneScalar::ZERO], SceneScalar::ZERO).unwrap();
        let runs = dashed_runs(&flattened, &dash, &mut work).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].points.len(), 3);
    }

    #[test]
    fn round_and_square_caps_paint_degenerate_subpaths_but_butt_does_not() {
        let path = line(point("1", "1"), point("1", "1"));
        let cancellation = NeverCancel;
        let render = |cap| {
            let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            rasterize_stroke(
                &path,
                Affine::IDENTITY,
                Affine::IDENTITY,
                &style("1", cap, LineJoin::Miter, "10", &[], "0", Matrix::IDENTITY),
                2,
                2,
                &mut work,
            )
            .unwrap()
        };
        let butt = render(LineCap::Butt);
        let round = render(LineCap::Round);
        let square = render(LineCap::Square);
        assert_eq!(butt.samples().iter().copied().sum::<u64>(), 0);
        assert_eq!(round.coverage(0, 0), Some(13));
        assert_eq!(square.coverage(0, 0), Some(16));
    }

    #[test]
    fn lone_move_does_not_paint_but_dashed_zero_length_segment_obeys_phase() {
        let lone_move = PathResource::new(vec![PathSegment::MoveTo(point("1", "1"))]).unwrap();
        let closed_lone_move = PathResource::new(vec![
            PathSegment::MoveTo(point("1", "1")),
            PathSegment::ClosePath,
        ])
        .unwrap();
        let degenerate = line(point("1", "1"), point("1", "1"));
        let cancellation = NeverCancel;
        let render = |path: &PathResource, dash: &[&str], phase: &str, cap| {
            let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            rasterize_stroke(
                path,
                Affine::IDENTITY,
                Affine::IDENTITY,
                &style(
                    "1",
                    cap,
                    LineJoin::Miter,
                    "10",
                    dash,
                    phase,
                    Matrix::IDENTITY,
                ),
                2,
                2,
                &mut work,
            )
            .unwrap()
        };
        assert_eq!(
            render(&lone_move, &[], "0", LineCap::Round)
                .samples()
                .iter()
                .copied()
                .sum::<u64>(),
            0
        );
        assert_eq!(
            render(&closed_lone_move, &[], "0", LineCap::Round).coverage(0, 0),
            Some(13)
        );
        assert_eq!(
            render(&closed_lone_move, &[], "0", LineCap::Square)
                .samples()
                .iter()
                .copied()
                .sum::<u64>(),
            0
        );
        assert_eq!(
            render(&degenerate, &["1", "1"], "0", LineCap::Round).coverage(0, 0),
            Some(13)
        );
        assert_eq!(
            render(&degenerate, &["1", "1"], "1", LineCap::Round)
                .samples()
                .iter()
                .copied()
                .sum::<u64>(),
            0
        );
    }

    #[test]
    fn inverse_sampling_preserves_nonuniform_affine_stroke_width() {
        let transform = Affine::new(
            Fixed::from_i64(2).unwrap(),
            Fixed::ZERO,
            Fixed::ZERO,
            Fixed::from_raw(Fixed::ONE.raw() / 2),
            Fixed::ZERO,
            Fixed::from_raw(Fixed::ONE.raw() / 2),
        );
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mask = rasterize_stroke(
            &line(point("0", "0"), point("1", "0")),
            Affine::IDENTITY,
            transform,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Miter,
                "10",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            2,
            1,
            &mut work,
        )
        .unwrap();
        assert_eq!(mask.coverage(0, 0), Some(32));
        assert_eq!(mask.coverage(1, 0), Some(32));
    }

    #[test]
    fn singular_paint_time_stroke_transform_fails_closed_for_all_widths() {
        let path = line(point("0", "0.5"), point("1", "0.5"));
        let singular = Matrix::new([
            scalar("1"),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            scalar("2"),
            scalar("3"),
        ]);
        let constructed_under = Affine::from_scene(Matrix::new([
            scalar("2"),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            scalar("3"),
            scalar("5"),
            scalar("7"),
        ]))
        .unwrap();

        for width in ["0", "1"] {
            let cancellation = NeverCancel;
            let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            assert_eq!(
                rasterize_stroke(
                    &path,
                    constructed_under,
                    Affine::IDENTITY,
                    &style(
                        width,
                        LineCap::Butt,
                        LineJoin::Miter,
                        "10",
                        &[],
                        "0",
                        singular,
                    ),
                    1,
                    1,
                    &mut work,
                ),
                Err(GeometryFailure::InvalidGeometry)
            );
            assert_eq!(work.samples(), 0);
        }
    }

    #[test]
    fn normalized_page_path_matches_local_path_under_noncommuting_stroke_transform() {
        let stroke_to_page = matrix(["2", "0.5", "0.25", "1", "1", "0.5"]);
        let page_to_device = matrix(["1", "0", "0.5", "1", "0.25", "0.25"]);
        assert_ne!(
            stroke_to_page.checked_multiply(page_to_device).unwrap(),
            page_to_device.checked_multiply(stroke_to_page).unwrap()
        );
        let local_start = point("0", "0");
        let local_end = point("1", "0");
        let normalized_start = stroke_to_page.checked_transform_point(local_start).unwrap();
        let normalized_end = stroke_to_page.checked_transform_point(local_end).unwrap();
        let style = style(
            "0.75",
            LineCap::Round,
            LineJoin::Miter,
            "10",
            &["0.6", "0.4"],
            "0.1",
            stroke_to_page,
        );
        let cancellation = NeverCancel;
        let mut local_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let local = rasterize_stroke(
            &line(local_start, local_end),
            Affine::from_scene(stroke_to_page).unwrap(),
            Affine::from_scene(page_to_device).unwrap(),
            &style,
            5,
            3,
            &mut local_work,
        )
        .unwrap();
        let mut normalized_work =
            GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let normalized = rasterize_stroke(
            &line(normalized_start, normalized_end),
            Affine::IDENTITY,
            Affine::from_scene(page_to_device).unwrap(),
            &style,
            5,
            3,
            &mut normalized_work,
        )
        .unwrap();
        assert_eq!(local, normalized);
        assert!(local.samples().iter().any(|samples| *samples != 0));
    }

    #[test]
    fn miter_limit_falls_back_to_bevel_and_round_join_uses_sector() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::LineTo(point("1.1", "1")),
        ])
        .unwrap();
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let flattened = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut work,
        )
        .unwrap();
        let bevel = StrokeOutline::from_flattened(
            &flattened,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Miter,
                "1",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            &mut work,
        )
        .unwrap();
        assert!(matches!(
            bevel.primitives().last(),
            Some(StrokePrimitive::Polygon(points)) if points.len() == 3
        ));
        let miter = StrokeOutline::from_flattened(
            &flattened,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Miter,
                "20",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            &mut work,
        )
        .unwrap();
        assert!(matches!(
            miter.primitives().last(),
            Some(StrokePrimitive::Polygon(points)) if points.len() == 4
        ));
        let round = StrokeOutline::from_flattened(
            &flattened,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Round,
                "10",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            &mut work,
        )
        .unwrap();
        assert!(matches!(
            round.primitives().last(),
            Some(StrokePrimitive::RoundSector { .. })
        ));

        let broad_turn_path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::LineTo(point("0.015", "0.174")),
        ])
        .unwrap();
        let broad_turn = flatten_path(
            &broad_turn_path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut work,
        )
        .unwrap();
        let broad_round = StrokeOutline::from_flattened(
            &broad_turn,
            &style(
                "1",
                LineCap::Butt,
                LineJoin::Round,
                "10",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            &mut work,
        )
        .unwrap();
        let sector = broad_round
            .primitives()
            .last()
            .expect("broad turn has one round sector");
        assert!(
            primitive_contains(
                sector,
                FixedPoint::new(
                    Fixed::from_scene(scalar("1.4")).unwrap(),
                    Fixed::from_i64(0).unwrap()
                ),
                &mut work,
            )
            .unwrap()
        );
        assert!(
            !primitive_contains(
                sector,
                FixedPoint::new(
                    Fixed::from_i64(1).unwrap(),
                    Fixed::from_scene(scalar("0.4")).unwrap()
                ),
                &mut work,
            )
            .unwrap()
        );
    }

    #[test]
    fn exact_reversal_round_join_adds_half_disk_while_bevel_and_miter_do_not_extend() {
        let reversal = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0.5")),
            PathSegment::LineTo(point("1", "0.5")),
            PathSegment::LineTo(point("0", "0.5")),
        ])
        .unwrap();
        let cancellation = NeverCancel;
        let render = |join| {
            let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            rasterize_stroke(
                &reversal,
                Affine::IDENTITY,
                Affine::IDENTITY,
                &style("1", LineCap::Butt, join, "10", &[], "0", Matrix::IDENTITY),
                2,
                1,
                &mut work,
            )
            .unwrap()
        };
        let round = render(LineJoin::Round);
        let bevel = render(LineJoin::Bevel);
        let miter = render(LineJoin::Miter);
        assert_eq!(round.coverage(0, 0), Some(64));
        assert_eq!(round.coverage(1, 0), Some(26));
        assert_eq!(bevel.coverage(1, 0), Some(0));
        assert_eq!(miter.coverage(1, 0), Some(0));
    }

    #[test]
    fn closed_path_uses_joins_without_endpoint_caps() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::LineTo(point("0", "1")),
            PathSegment::ClosePath,
        ])
        .unwrap();
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let flattened = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut work,
        )
        .unwrap();
        let outline = StrokeOutline::from_flattened(
            &flattened,
            &style(
                "1",
                LineCap::Round,
                LineJoin::Bevel,
                "10",
                &[],
                "0",
                Matrix::IDENTITY,
            ),
            &mut work,
        )
        .unwrap();
        assert_eq!(
            outline
                .primitives()
                .iter()
                .filter(|primitive| matches!(primitive, StrokePrimitive::Circle { .. }))
                .count(),
            0
        );
    }

    #[test]
    fn odd_dash_pattern_merges_the_closed_seam_into_one_run() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::LineTo(point("1", "1")),
            PathSegment::LineTo(point("0", "1")),
            PathSegment::ClosePath,
        ])
        .unwrap();
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let flattened = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut work,
        )
        .unwrap();
        let dash = DashPattern::new(vec![scalar("1")], scalar("0.5")).unwrap();
        let runs = dashed_runs(&flattened, &dash, &mut work).unwrap();
        let seam = FixedPoint::from_scene(point("0", "0")).unwrap();

        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| !run.closed));
        assert!(
            runs[0].points.windows(3).any(|points| points[1] == seam),
            "the last and first on-runs must be joined through the close-path seam"
        );
    }

    #[test]
    fn dash_run_primitive_and_geometry_budgets_fail_independently() {
        let cancellation = NeverCancel;
        let path = line(point("0", "0"), point("2", "0"));
        let mut build_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let flattened = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut build_work,
        )
        .unwrap();
        let dash =
            DashPattern::new(vec![scalar("0.25"), scalar("0.25")], SceneScalar::ZERO).unwrap();

        let mut parse_work = GeometryWork::new(
            GeometryLimits {
                max_fuel: 3,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            dashed_runs(&flattened, &dash, &mut parse_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Fuel,
                limit: 3,
                consumed: 0,
                attempted: 4
            })
        ));
        assert_eq!(parse_work.geometry_bytes(), 0);

        let mut chunk_work = GeometryWork::new(
            GeometryLimits {
                max_dash_chunks: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            dashed_runs(&flattened, &dash, &mut chunk_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::DashChunks,
                limit: 1,
                consumed: 1,
                attempted: 1
            })
        ));
        assert_eq!(chunk_work.dash_chunks(), 1);

        let mut run_work = GeometryWork::new(
            GeometryLimits {
                max_stroke_runs: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            dashed_runs(&flattened, &dash, &mut run_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::StrokeRuns,
                limit: 1,
                consumed: 1,
                attempted: 1
            })
        ));
        assert_eq!(run_work.stroke_runs(), 1);

        let solid = DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap();
        let multiple_path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::MoveTo(point("0", "1")),
            PathSegment::LineTo(point("1", "1")),
        ])
        .unwrap();
        let multiple = flatten_path(
            &multiple_path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut build_work,
        )
        .unwrap();
        let mut solid_run_work = GeometryWork::new(
            GeometryLimits {
                max_stroke_runs: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            dashed_runs(&multiple, &solid, &mut solid_run_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::StrokeRuns,
                limit: 1,
                consumed: 0,
                attempted: 2
            })
        ));
        assert_eq!(solid_run_work.geometry_bytes(), 0);

        let mut geometry_work = GeometryWork::new(
            GeometryLimits {
                max_geometry_bytes: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            dashed_runs(&flattened, &solid, &mut geometry_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::GeometryBytes,
                limit: 1,
                consumed: 0,
                ..
            })
        ));
        assert_eq!(geometry_work.geometry_bytes(), 0);

        let mut primitive_work = GeometryWork::new(
            GeometryLimits {
                max_stroke_primitives: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            StrokeOutline::from_flattened(
                &flattened,
                &style(
                    "1",
                    LineCap::Round,
                    LineJoin::Miter,
                    "10",
                    &[],
                    "0",
                    Matrix::IDENTITY,
                ),
                &mut primitive_work,
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::StrokePrimitives,
                limit: 1,
                consumed: 1,
                attempted: 1
            })
        ));
        assert_eq!(primitive_work.stroke_primitives(), 1);
    }

    #[test]
    fn primitive_and_polygon_comparisons_consume_fuel() {
        let cancellation = NeverCancel;
        let unit = Fixed::ONE;
        let outline = StrokeOutline {
            primitives: vec![StrokePrimitive::Polygon(vec![
                FixedPoint::new(unit.checked_neg().unwrap(), unit.checked_neg().unwrap()),
                FixedPoint::new(unit, unit.checked_neg().unwrap()),
                FixedPoint::new(unit, unit),
                FixedPoint::new(unit.checked_neg().unwrap(), unit),
            ])],
            comparison_work: 5,
        };
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_fuel: 4,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            outline.contains(FixedPoint::default(), &mut work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Fuel,
                limit: 4,
                consumed: 4,
                attempted: 1
            })
        ));
    }

    #[test]
    fn coverage_budget_rejects_stroke_mask_before_samples_commit() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_coverage_bytes: 7,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            rasterize_stroke(
                &line(point("0", "0.5"), point("1", "0.5")),
                Affine::IDENTITY,
                Affine::IDENTITY,
                &style(
                    "1",
                    LineCap::Butt,
                    LineJoin::Miter,
                    "10",
                    &[],
                    "0",
                    Matrix::IDENTITY,
                ),
                1,
                1,
                &mut work
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::CoverageBytes,
                limit: 7,
                consumed: 0,
                attempted: 8
            })
        ));
        assert_eq!(work.samples(), 0);
    }

    #[test]
    fn one_less_sample_budget_rejects_stroke_before_first_pixel() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_samples: 63,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            rasterize_stroke(
                &line(point("0", "0.5"), point("1", "0.5")),
                Affine::IDENTITY,
                Affine::IDENTITY,
                &style(
                    "1",
                    LineCap::Butt,
                    LineJoin::Miter,
                    "10",
                    &[],
                    "0",
                    Matrix::IDENTITY,
                ),
                1,
                1,
                &mut work
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Samples,
                limit: 63,
                consumed: 0,
                attempted: 64
            })
        ));
    }
}
