//! Deterministic scalar stroke construction.

use core::mem::size_of;

use pdf_rs_scene::{
    DashPattern, LineCap, LineJoin, LineStyle, Matrix, PathResource, PathSegment, ScenePoint,
    SceneScalar,
};

use crate::fast::kernels::{Coverage, FIXED_ONE, KernelWork, PageMap, Point, WorkRect, coverage};
use crate::fast::{FastRasterError, FastRasterErrorCode};

const SCENE_SCALE: i128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Affine {
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
}

impl Affine {
    const IDENTITY: Self = Self {
        a: FIXED_ONE,
        b: 0,
        c: 0,
        d: FIXED_ONE,
        e: 0,
        f: 0,
    };

    fn from_scene(matrix: Matrix) -> Result<Self, FastRasterError> {
        let [a, b, c, d, e, f] = matrix.components();
        Ok(Self {
            a: scalar_to_fixed(a)?,
            b: scalar_to_fixed(b)?,
            c: scalar_to_fixed(c)?,
            d: scalar_to_fixed(d)?,
            e: scalar_to_fixed(e)?,
            f: scalar_to_fixed(f)?,
        })
    }

    fn from_page_map(map: PageMap) -> Result<Self, FastRasterError> {
        let [a, b, c, d, e, f] = map.device_affine()?;
        Ok(Self { a, b, c, d, e, f })
    }

    fn apply(self, point: Point) -> Result<Point, FastRasterError> {
        Ok(Point {
            x: fixed_sum(&[(self.a, point.x), (self.c, point.y)], self.e)?,
            y: fixed_sum(&[(self.b, point.x), (self.d, point.y)], self.f)?,
        })
    }

    fn concat(self, other: Self) -> Result<Self, FastRasterError> {
        Ok(Self {
            a: fixed_sum(&[(self.a, other.a), (self.c, other.b)], 0)?,
            b: fixed_sum(&[(self.b, other.a), (self.d, other.b)], 0)?,
            c: fixed_sum(&[(self.a, other.c), (self.c, other.d)], 0)?,
            d: fixed_sum(&[(self.b, other.c), (self.d, other.d)], 0)?,
            e: fixed_sum(&[(self.a, other.e), (self.c, other.f)], self.e)?,
            f: fixed_sum(&[(self.b, other.e), (self.d, other.f)], self.f)?,
        })
    }

    fn inverse(self) -> Result<Self, FastRasterError> {
        let determinant = i128::from(self.a)
            .checked_mul(i128::from(self.d))
            .and_then(|value| {
                i128::from(self.b)
                    .checked_mul(i128::from(self.c))
                    .and_then(|other| value.checked_sub(other))
            })
            .ok_or_else(numeric)?;
        if determinant == 0 {
            return Err(invalid_resource());
        }
        let scale_squared = i128::from(FIXED_ONE)
            .checked_mul(i128::from(FIXED_ONE))
            .ok_or_else(numeric)?;
        let coefficient = |value: i64, negate: bool| -> Result<i64, FastRasterError> {
            let value = if negate {
                i128::from(value).checked_neg().ok_or_else(numeric)?
            } else {
                i128::from(value)
            };
            round_to_i64(
                value.checked_mul(scale_squared).ok_or_else(numeric)?,
                determinant,
            )
        };
        let a = coefficient(self.d, false)?;
        let b = coefficient(self.b, true)?;
        let c = coefficient(self.c, true)?;
        let d = coefficient(self.a, false)?;
        let e = fixed_sum(&[(a, self.e), (c, self.f)], 0)?
            .checked_neg()
            .ok_or_else(numeric)?;
        let f = fixed_sum(&[(b, self.e), (d, self.f)], 0)?
            .checked_neg()
            .ok_or_else(numeric)?;
        Ok(Self { a, b, c, d, e, f })
    }
}

#[derive(Debug)]
struct StrokeSubpath {
    points: Vec<Point>,
    closed: bool,
}

#[derive(Debug)]
struct StrokePath {
    subpaths: Vec<StrokeSubpath>,
    retained_bytes: u64,
}

#[derive(Debug)]
struct StrokeRun {
    points: Vec<Point>,
    closed: bool,
}

#[derive(Debug)]
enum StrokePrimitive {
    Polygon(Vec<Point>),
    Circle {
        center: Point,
        radius: i64,
    },
    RoundSector {
        center: Point,
        radius: i64,
        incoming: Point,
        outgoing: Point,
        turn: i8,
    },
}

#[derive(Debug)]
struct StrokeOutline {
    primitives: Vec<StrokePrimitive>,
    retained_bytes: u64,
}

impl StrokeOutline {
    fn contains(&self, point: Point, work: &mut dyn KernelWork) -> Result<bool, FastRasterError> {
        for primitive in &self.primitives {
            work.step()?;
            if primitive_contains(primitive, point, work)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn stroke_coverage(
    path: &PathResource,
    path_transform: Matrix,
    style: &LineStyle,
    map: PageMap,
    rect: WorkRect,
    flatness_denominator: u32,
    recursion_limit: u8,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<Coverage, FastRasterError> {
    let stroke_to_page = Affine::from_scene(style.stroke_transform())?;
    let page_to_stroke = stroke_to_page.inverse()?;
    let path_to_page = Affine::from_scene(path_transform)?;
    let page_to_device = Affine::from_page_map(map)?;
    let path_to_stroke = page_to_stroke.concat(path_to_page)?;
    let path_to_device = page_to_device.concat(path_to_page)?;
    let stroke_to_device = page_to_device.concat(stroke_to_page)?;
    let flattened = flatten_stroke_path(
        path,
        path_to_stroke,
        path_to_device,
        flatness_denominator,
        recursion_limit,
        base_intermediate,
        work,
    )?;
    let flattened_bytes = flattened.retained_bytes;
    let (mut runs, runs_bytes) = dashed_runs(
        &flattened,
        style.dash(),
        add(base_intermediate, flattened_bytes)?,
        work,
    )?;
    drop(flattened);

    let (half_width, device_to_outline) = if style.width() == SceneScalar::ZERO {
        for run in &mut runs {
            for point in &mut run.points {
                *point = stroke_to_device.apply(*point)?;
                work.step()?;
            }
        }
        (FIXED_ONE / 2, Affine::IDENTITY)
    } else {
        (
            round_to_i64(i128::from(scalar_to_fixed(style.width())?), 2)?,
            stroke_to_device.inverse()?,
        )
    };
    let outline = build_outline(runs, runs_bytes, style, half_width, base_intermediate, work)?;
    let coverage_base = add(base_intermediate, outline.retained_bytes)?;
    coverage(rect, coverage_base, work, |device, work| {
        outline.contains(device_to_outline.apply(device)?, work)
    })
}

#[allow(clippy::too_many_arguments)]
fn flatten_stroke_path(
    path: &PathResource,
    output_transform: Affine,
    measure_transform: Affine,
    flatness_denominator: u32,
    recursion_limit: u8,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<StrokePath, FastRasterError> {
    let tolerance = (FIXED_ONE / i64::from(flatness_denominator)).max(1);
    let mut subpaths = Vec::new();
    let mut retained = 0_u64;
    let mut current = None;
    let mut output_last = None;
    let mut measure_last = None;
    for segment in path.segments() {
        work.step()?;
        match *segment {
            PathSegment::MoveTo(point) => {
                let output = output_transform.apply(point_from_scene(point)?)?;
                let measure = measure_transform.apply(point_from_scene(point)?)?;
                let mut points = Vec::new();
                reserve_tracked(&mut points, 1, base_intermediate, &mut retained, work)?;
                points.push(output);
                reserve_tracked(&mut subpaths, 1, base_intermediate, &mut retained, work)?;
                subpaths.push(StrokeSubpath {
                    points,
                    closed: false,
                });
                current = Some(subpaths.len() - 1);
                output_last = Some(output);
                measure_last = Some(measure);
            }
            PathSegment::LineTo(point) => {
                let output = output_transform.apply(point_from_scene(point)?)?;
                push_flattened_point(
                    &mut subpaths,
                    current,
                    output,
                    base_intermediate,
                    &mut retained,
                    work,
                )?;
                output_last = Some(output);
                measure_last = Some(measure_transform.apply(point_from_scene(point)?)?);
            }
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                let output = [
                    output_last.ok_or_else(command_sequence)?,
                    output_transform.apply(point_from_scene(control_1)?)?,
                    output_transform.apply(point_from_scene(control_2)?)?,
                    output_transform.apply(point_from_scene(end)?)?,
                ];
                let measure = [
                    measure_last.ok_or_else(command_sequence)?,
                    measure_transform.apply(point_from_scene(control_1)?)?,
                    measure_transform.apply(point_from_scene(control_2)?)?,
                    measure_transform.apply(point_from_scene(end)?)?,
                ];
                flatten_stroke_cubic(
                    output,
                    measure,
                    tolerance,
                    recursion_limit,
                    0,
                    &mut subpaths,
                    current,
                    base_intermediate,
                    &mut retained,
                    work,
                )?;
                output_last = Some(output[3]);
                measure_last = Some(measure[3]);
            }
            PathSegment::ClosePath => {
                let index = current.ok_or_else(command_sequence)?;
                subpaths[index].closed = true;
                current = None;
                output_last = None;
                measure_last = None;
            }
        }
    }
    Ok(StrokePath {
        subpaths,
        retained_bytes: retained,
    })
}

#[allow(clippy::too_many_arguments)]
fn flatten_stroke_cubic(
    output: [Point; 4],
    measure: [Point; 4],
    tolerance: i64,
    recursion_limit: u8,
    depth: u8,
    subpaths: &mut [StrokeSubpath],
    current: Option<usize>,
    base_intermediate: u64,
    retained: &mut u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    work.step()?;
    if cubic_is_flat(measure, tolerance)? {
        return push_flattened_point(
            subpaths,
            current,
            output[3],
            base_intermediate,
            retained,
            work,
        );
    }
    if depth >= recursion_limit {
        return Err(invalid_resource());
    }
    let (left_output, right_output) = split_cubic(output)?;
    let (left_measure, right_measure) = split_cubic(measure)?;
    flatten_stroke_cubic(
        left_output,
        left_measure,
        tolerance,
        recursion_limit,
        depth + 1,
        subpaths,
        current,
        base_intermediate,
        retained,
        work,
    )?;
    flatten_stroke_cubic(
        right_output,
        right_measure,
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

fn push_flattened_point(
    subpaths: &mut [StrokeSubpath],
    current: Option<usize>,
    point: Point,
    base_intermediate: u64,
    retained: &mut u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    let points = &mut subpaths[current.ok_or_else(command_sequence)?].points;
    reserve_tracked(points, 1, base_intermediate, retained, work)?;
    points.push(point);
    Ok(())
}

fn split_cubic(points: [Point; 4]) -> Result<([Point; 4], [Point; 4]), FastRasterError> {
    let p01 = midpoint(points[0], points[1])?;
    let p12 = midpoint(points[1], points[2])?;
    let p23 = midpoint(points[2], points[3])?;
    let p012 = midpoint(p01, p12)?;
    let p123 = midpoint(p12, p23)?;
    let middle = midpoint(p012, p123)?;
    Ok((
        [points[0], p01, p012, middle],
        [middle, p123, p23, points[3]],
    ))
}

fn cubic_is_flat(points: [Point; 4], tolerance: i64) -> Result<bool, FastRasterError> {
    let vx = i128::from(points[3].x) - i128::from(points[0].x);
    let vy = i128::from(points[3].y) - i128::from(points[0].y);
    let length_squared = squared_sum(vx, vy)?.max(1);
    let tolerance_squared = i128::from(tolerance)
        .checked_mul(i128::from(tolerance))
        .ok_or_else(numeric)?;
    for point in [points[1], points[2]] {
        let wx = i128::from(point.x) - i128::from(points[0].x);
        let wy = i128::from(point.y) - i128::from(points[0].y);
        let distance = vx
            .checked_mul(wy)
            .and_then(|value| value.checked_sub(vy.checked_mul(wx)?))
            .ok_or_else(numeric)?
            .abs();
        if distance.checked_mul(distance).ok_or_else(numeric)?
            > tolerance_squared
                .checked_mul(length_squared)
                .ok_or_else(numeric)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

struct RunBuilder<'a> {
    runs: Vec<StrokeRun>,
    retained: u64,
    base: u64,
    work: &'a mut dyn KernelWork,
}

impl<'a> RunBuilder<'a> {
    fn new(base: u64, work: &'a mut dyn KernelWork) -> Self {
        Self {
            runs: Vec::new(),
            retained: 0,
            base,
            work,
        }
    }

    fn points(&mut self, first: Point) -> Result<Vec<Point>, FastRasterError> {
        let mut points = Vec::new();
        reserve_tracked(&mut points, 1, self.base, &mut self.retained, self.work)?;
        points.push(first);
        self.work.step()?;
        Ok(points)
    }

    fn push_point(&mut self, points: &mut Vec<Point>, point: Point) -> Result<(), FastRasterError> {
        if points.last() == Some(&point) {
            return Ok(());
        }
        reserve_tracked(points, 1, self.base, &mut self.retained, self.work)?;
        points.push(point);
        self.work.step()
    }

    fn discard_points(&mut self, points: Vec<Point>) -> Result<(), FastRasterError> {
        self.retained = self
            .retained
            .checked_sub(capacity_bytes(&points)?)
            .ok_or_else(numeric)?;
        drop(points);
        Ok(())
    }

    fn push_run(&mut self, points: Vec<Point>, closed: bool) -> Result<(), FastRasterError> {
        reserve_tracked(&mut self.runs, 1, self.base, &mut self.retained, self.work)?;
        self.runs.push(StrokeRun { points, closed });
        self.work.step()
    }

    fn merge_closed_seam(
        &mut self,
        first_index: usize,
        seam: Point,
    ) -> Result<(), FastRasterError> {
        let count = self.runs.len().saturating_sub(first_index);
        if count == 0 {
            return Ok(());
        }
        let first_matches = self.runs[first_index].points.first() == Some(&seam);
        let last_matches = self.runs.last().and_then(|run| run.points.last()) == Some(&seam);
        if !first_matches || !last_matches {
            return Ok(());
        }
        if count == 1 {
            let run = &mut self.runs[first_index];
            if run.points.len() > 1 && run.points.first() == run.points.last() {
                run.points.pop();
            }
            run.closed = true;
            self.work.step()?;
            return Ok(());
        }
        for _ in 0..count {
            self.work.step()?;
        }
        let first = self.runs.remove(first_index);
        let mut last = self.runs.pop().ok_or_else(command_sequence)?;
        for point in first.points.iter().copied().skip(1) {
            self.push_point(&mut last.points, point)?;
        }
        self.discard_points(first.points)?;
        if self.runs.len() == self.runs.capacity() {
            return Err(command_sequence());
        }
        self.runs.insert(first_index, last);
        Ok(())
    }

    fn finish(self) -> (Vec<StrokeRun>, u64) {
        (self.runs, self.retained)
    }
}

fn dashed_runs(
    path: &StrokePath,
    dash: &DashPattern,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<(Vec<StrokeRun>, u64), FastRasterError> {
    if dash.array().is_empty() {
        let mut builder = RunBuilder::new(base_intermediate, work);
        for subpath in &path.subpaths {
            builder.work.step()?;
            if subpath.points.len() < 2 && !subpath.closed {
                continue;
            }
            let first = *subpath.points.first().ok_or_else(command_sequence)?;
            let mut points = builder.points(first)?;
            for &point in subpath.points.iter().skip(1) {
                builder.push_point(&mut points, point)?;
            }
            builder.push_run(points, subpath.closed)?;
        }
        return Ok(builder.finish());
    }

    let mut pattern = Vec::new();
    let mut pattern_bytes = 0_u64;
    for value in dash.array() {
        reserve_tracked(&mut pattern, 1, base_intermediate, &mut pattern_bytes, work)?;
        pattern.push(scalar_to_fixed(*value)?);
        work.step()?;
    }
    if pattern.len() % 2 == 1 {
        let initial = pattern.len();
        for index in 0..initial {
            let value = pattern[index];
            reserve_tracked(&mut pattern, 1, base_intermediate, &mut pattern_bytes, work)?;
            pattern.push(value);
            work.step()?;
        }
    }
    let mut total = 0_i64;
    for &value in &pattern {
        total = total.checked_add(value).ok_or_else(numeric)?;
        work.step()?;
    }
    if total <= 0 {
        return Err(invalid_resource());
    }
    let phase = scalar_to_fixed(dash.phase())?;
    let builder_base = add(base_intermediate, pattern_bytes)?;
    let mut builder = RunBuilder::new(builder_base, work);
    for subpath in &path.subpaths {
        builder.work.step()?;
        if subpath.points.len() < 2 && !subpath.closed {
            continue;
        }
        dash_subpath(subpath, &pattern, total, phase, &mut builder)?;
    }
    Ok(builder.finish())
}

fn dash_subpath(
    subpath: &StrokeSubpath,
    pattern: &[i64],
    total: i64,
    phase: i64,
    builder: &mut RunBuilder<'_>,
) -> Result<(), FastRasterError> {
    let first = *subpath.points.first().ok_or_else(command_sequence)?;
    let (mut dash_index, mut dash_remaining) =
        initial_dash_state(pattern, total, phase, builder.work)?;
    let starts_on = dash_index % 2 == 0;
    let first_run = builder.runs.len();
    if subpath.points.len() == 1 {
        if starts_on {
            let points = builder.points(first)?;
            builder.push_run(points, subpath.closed)?;
        }
        return Ok(());
    }
    let segment_count = if subpath.closed {
        subpath.points.len()
    } else {
        subpath.points.len() - 1
    };
    let mut current = None;
    let mut saw_nonzero = false;
    for segment_index in 0..segment_count {
        builder.work.step()?;
        let start = subpath.points[segment_index];
        let end = subpath.points[(segment_index + 1) % subpath.points.len()];
        let length = vector_length(subtract(end, start)?)?;
        if length == 0 {
            continue;
        }
        saw_nonzero = true;
        let segment_length = u64::try_from(length).map_err(|_| invalid_resource())?;
        let mut consumed = 0_u64;
        while consumed < segment_length {
            builder.work.step()?;
            if dash_remaining == 0 {
                advance_dash(pattern, &mut dash_index, &mut dash_remaining, builder.work)?;
            }
            let remaining_dash = u64::try_from(dash_remaining).map_err(|_| invalid_resource())?;
            let amount = (segment_length - consumed).min(remaining_dash);
            if amount == 0 {
                return Err(invalid_resource());
            }
            let chunk_start = if consumed == 0 {
                start
            } else {
                lerp(start, end, consumed, segment_length)?
            };
            let next_consumed = consumed.checked_add(amount).ok_or_else(numeric)?;
            let chunk_end = if next_consumed == segment_length {
                end
            } else {
                lerp(start, end, next_consumed, segment_length)?
            };
            if dash_index % 2 == 0 {
                if current.is_none() {
                    current = Some(builder.points(chunk_start)?);
                }
                builder.push_point(current.as_mut().ok_or_else(command_sequence)?, chunk_end)?;
            }
            consumed = next_consumed;
            dash_remaining = dash_remaining
                .checked_sub(i64::try_from(amount).map_err(|_| numeric())?)
                .ok_or_else(numeric)?;
            if dash_remaining == 0 {
                let was_on = dash_index % 2 == 0;
                advance_dash(pattern, &mut dash_index, &mut dash_remaining, builder.work)?;
                if was_on
                    && dash_index % 2 == 1
                    && let Some(points) = current.take()
                {
                    builder.push_run(points, false)?;
                }
            }
        }
    }
    if !saw_nonzero {
        if starts_on {
            let points = builder.points(first)?;
            builder.push_run(points, subpath.closed)?;
        }
        return Ok(());
    }
    let ends_on_without_boundary = current.is_some();
    if let Some(points) = current {
        builder.push_run(points, false)?;
    }
    if subpath.closed && starts_on && ends_on_without_boundary {
        builder.merge_closed_seam(first_run, first)?;
    }
    Ok(())
}

fn initial_dash_state(
    pattern: &[i64],
    total: i64,
    phase: i64,
    work: &mut dyn KernelWork,
) -> Result<(usize, i64), FastRasterError> {
    work.step()?;
    let mut phase = phase.checked_rem(total).ok_or_else(numeric)?;
    let mut index = 0_usize;
    let mut remaining = pattern[0];
    let maximum_visits = pattern.len().checked_mul(2).ok_or_else(numeric)?;
    for _ in 0..=maximum_visits {
        if remaining != 0 && phase < remaining {
            return Ok((index, remaining.checked_sub(phase).ok_or_else(numeric)?));
        }
        work.step()?;
        if remaining != 0 {
            phase = phase.checked_sub(remaining).ok_or_else(numeric)?;
        }
        index = (index + 1) % pattern.len();
        remaining = pattern[index];
    }
    Err(invalid_resource())
}

fn advance_dash(
    pattern: &[i64],
    index: &mut usize,
    remaining: &mut i64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    for _ in 0..pattern.len() {
        work.step()?;
        *index = (*index + 1) % pattern.len();
        *remaining = pattern[*index];
        if *remaining > 0 {
            return Ok(());
        }
    }
    Err(invalid_resource())
}

fn build_outline(
    runs: Vec<StrokeRun>,
    runs_bytes: u64,
    style: &LineStyle,
    half_width: i64,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<StrokeOutline, FastRasterError> {
    let mut primitives = Vec::new();
    let mut outline_bytes = 0_u64;
    let mut active_runs_bytes = runs_bytes;
    for mut run in runs {
        compact_points(&mut run.points, work)?;
        if run.closed && run.points.len() > 1 && run.points.first() == run.points.last() {
            run.points.pop();
        }
        append_run_primitives(
            &mut primitives,
            &mut outline_bytes,
            active_runs_bytes,
            &run,
            style,
            half_width,
            base_intermediate,
            work,
        )?;
        active_runs_bytes = active_runs_bytes
            .checked_sub(capacity_bytes(&run.points)?)
            .ok_or_else(numeric)?;
    }
    Ok(StrokeOutline {
        primitives,
        retained_bytes: outline_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_run_primitives(
    primitives: &mut Vec<StrokePrimitive>,
    outline_bytes: &mut u64,
    runs_bytes: u64,
    run: &StrokeRun,
    style: &LineStyle,
    half_width: i64,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    if half_width <= 0 || run.points.is_empty() {
        return Ok(());
    }
    if run.points.len() == 1 {
        if run.closed {
            if style.cap() == LineCap::Round {
                push_primitive(
                    primitives,
                    outline_bytes,
                    runs_bytes,
                    StrokePrimitive::Circle {
                        center: run.points[0],
                        radius: half_width,
                    },
                    base_intermediate,
                    work,
                )?;
            }
        } else {
            append_degenerate(
                primitives,
                outline_bytes,
                runs_bytes,
                run.points[0],
                style.cap(),
                half_width,
                base_intermediate,
                work,
            )?;
        }
        return Ok(());
    }

    let segment_count = if run.closed {
        run.points.len()
    } else {
        run.points.len() - 1
    };
    for index in 0..segment_count {
        work.step()?;
        let start = run.points[index];
        let end = run.points[(index + 1) % run.points.len()];
        if start == end {
            continue;
        }
        let polygon = segment_polygon(
            start,
            end,
            half_width,
            !run.closed && index == 0 && style.cap() == LineCap::Square,
            !run.closed && index + 1 == segment_count && style.cap() == LineCap::Square,
            add_many(base_intermediate, runs_bytes, *outline_bytes)?,
            work,
        )?;
        *outline_bytes = outline_bytes
            .checked_add(capacity_bytes(&polygon)?)
            .ok_or_else(numeric)?;
        push_primitive(
            primitives,
            outline_bytes,
            runs_bytes,
            StrokePrimitive::Polygon(polygon),
            base_intermediate,
            work,
        )?;
    }
    if run.closed {
        for index in 0..run.points.len() {
            append_join(
                primitives,
                outline_bytes,
                runs_bytes,
                run.points[(index + run.points.len() - 1) % run.points.len()],
                run.points[index],
                run.points[(index + 1) % run.points.len()],
                style,
                half_width,
                base_intermediate,
                work,
            )?;
        }
    } else {
        for index in 1..run.points.len() - 1 {
            append_join(
                primitives,
                outline_bytes,
                runs_bytes,
                run.points[index - 1],
                run.points[index],
                run.points[index + 1],
                style,
                half_width,
                base_intermediate,
                work,
            )?;
        }
        if style.cap() == LineCap::Round {
            for center in [
                run.points[0],
                *run.points.last().ok_or_else(command_sequence)?,
            ] {
                push_primitive(
                    primitives,
                    outline_bytes,
                    runs_bytes,
                    StrokePrimitive::Circle {
                        center,
                        radius: half_width,
                    },
                    base_intermediate,
                    work,
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_degenerate(
    primitives: &mut Vec<StrokePrimitive>,
    outline_bytes: &mut u64,
    runs_bytes: u64,
    point: Point,
    cap: LineCap,
    half_width: i64,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    let primitive = match cap {
        LineCap::Butt => return Ok(()),
        LineCap::Round => StrokePrimitive::Circle {
            center: point,
            radius: half_width,
        },
        LineCap::Square => {
            let minimum = Point {
                x: point.x.checked_sub(half_width).ok_or_else(numeric)?,
                y: point.y.checked_sub(half_width).ok_or_else(numeric)?,
            };
            let maximum = Point {
                x: point.x.checked_add(half_width).ok_or_else(numeric)?,
                y: point.y.checked_add(half_width).ok_or_else(numeric)?,
            };
            let polygon = polygon(
                &[
                    minimum,
                    Point {
                        x: maximum.x,
                        y: minimum.y,
                    },
                    maximum,
                    Point {
                        x: minimum.x,
                        y: maximum.y,
                    },
                ],
                add_many(base_intermediate, runs_bytes, *outline_bytes)?,
                work,
            )?;
            *outline_bytes = outline_bytes
                .checked_add(capacity_bytes(&polygon)?)
                .ok_or_else(numeric)?;
            StrokePrimitive::Polygon(polygon)
        }
    };
    push_primitive(
        primitives,
        outline_bytes,
        runs_bytes,
        primitive,
        base_intermediate,
        work,
    )
}

#[allow(clippy::too_many_arguments)]
fn append_join(
    primitives: &mut Vec<StrokePrimitive>,
    outline_bytes: &mut u64,
    runs_bytes: u64,
    previous: Point,
    vertex: Point,
    next: Point,
    style: &LineStyle,
    half_width: i64,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    work.step()?;
    if previous == vertex || vertex == next {
        return Ok(());
    }
    let incoming = subtract(vertex, previous)?;
    let outgoing = subtract(next, vertex)?;
    let turn = cross(incoming, outgoing)?;
    if turn == 0 {
        if dot(incoming, outgoing)? < 0 && style.join() == LineJoin::Round {
            push_primitive(
                primitives,
                outline_bytes,
                runs_bytes,
                StrokePrimitive::Circle {
                    center: vertex,
                    radius: half_width,
                },
                base_intermediate,
                work,
            )?;
        }
        return Ok(());
    }
    let (_, incoming_left) = tangent_and_left_normal(previous, vertex, half_width)?;
    let (_, outgoing_left) = tangent_and_left_normal(vertex, next, half_width)?;
    let (incoming_outer, outgoing_outer, turn_sign) = if turn > 0 {
        (negate(incoming_left)?, negate(outgoing_left)?, 1)
    } else {
        (incoming_left, outgoing_left, -1)
    };
    let first = add_point(vertex, incoming_outer)?;
    let second = add_point(vertex, outgoing_outer)?;
    let primitive = match style.join() {
        LineJoin::Round => StrokePrimitive::RoundSector {
            center: vertex,
            radius: half_width,
            incoming,
            outgoing,
            turn: turn_sign,
        },
        LineJoin::Bevel => {
            let value = polygon(
                &[vertex, first, second],
                add_many(base_intermediate, runs_bytes, *outline_bytes)?,
                work,
            )?;
            *outline_bytes = outline_bytes
                .checked_add(capacity_bytes(&value)?)
                .ok_or_else(numeric)?;
            StrokePrimitive::Polygon(value)
        }
        LineJoin::Miter => {
            let miter = line_intersection(first, incoming, second, outgoing)?;
            let limit = fixed_mul(scalar_to_fixed(style.miter_limit())?, half_width)?;
            let within_limit = miter
                .map(|point| {
                    point_distance_squared(point, vertex).and_then(|distance| {
                        let squared = i128::from(limit)
                            .checked_mul(i128::from(limit))
                            .ok_or_else(numeric)?;
                        Ok(distance <= squared)
                    })
                })
                .transpose()?
                .unwrap_or(false);
            let value = if within_limit {
                polygon(
                    &[vertex, first, miter.ok_or_else(invalid_resource)?, second],
                    add_many(base_intermediate, runs_bytes, *outline_bytes)?,
                    work,
                )?
            } else {
                polygon(
                    &[vertex, first, second],
                    add_many(base_intermediate, runs_bytes, *outline_bytes)?,
                    work,
                )?
            };
            *outline_bytes = outline_bytes
                .checked_add(capacity_bytes(&value)?)
                .ok_or_else(numeric)?;
            StrokePrimitive::Polygon(value)
        }
    };
    push_primitive(
        primitives,
        outline_bytes,
        runs_bytes,
        primitive,
        base_intermediate,
        work,
    )
}

fn push_primitive(
    primitives: &mut Vec<StrokePrimitive>,
    outline_bytes: &mut u64,
    runs_bytes: u64,
    primitive: StrokePrimitive,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    reserve_tracked(
        primitives,
        1,
        add(base_intermediate, runs_bytes)?,
        outline_bytes,
        work,
    )?;
    primitives.push(primitive);
    work.step()
}

fn segment_polygon(
    start: Point,
    end: Point,
    half_width: i64,
    extend_start: bool,
    extend_end: bool,
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<Vec<Point>, FastRasterError> {
    let (tangent, normal) = tangent_and_left_normal(start, end, half_width)?;
    let start = if extend_start {
        subtract(start, tangent)?
    } else {
        start
    };
    let end = if extend_end {
        add_point(end, tangent)?
    } else {
        end
    };
    polygon(
        &[
            add_point(start, normal)?,
            add_point(end, normal)?,
            subtract(end, normal)?,
            subtract(start, normal)?,
        ],
        base_intermediate,
        work,
    )
}

fn polygon(
    points: &[Point],
    base_intermediate: u64,
    work: &mut dyn KernelWork,
) -> Result<Vec<Point>, FastRasterError> {
    let mut output = Vec::new();
    let mut retained = 0;
    reserve_tracked(
        &mut output,
        points.len(),
        base_intermediate,
        &mut retained,
        work,
    )?;
    for &point in points {
        output.push(point);
        work.step()?;
    }
    Ok(output)
}

fn compact_points(
    points: &mut Vec<Point>,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    let mut write = 0_usize;
    for read in 0..points.len() {
        work.step()?;
        if write == 0 || points[write - 1] != points[read] {
            points[write] = points[read];
            write += 1;
        }
    }
    points.truncate(write);
    Ok(())
}

fn primitive_contains(
    primitive: &StrokePrimitive,
    point: Point,
    work: &mut dyn KernelWork,
) -> Result<bool, FastRasterError> {
    match primitive {
        StrokePrimitive::Polygon(vertices) => point_in_convex_polygon(vertices, point, work),
        StrokePrimitive::Circle { center, radius } => {
            let distance = point_distance_squared(*center, point)?;
            let radius_squared = i128::from(*radius)
                .checked_mul(i128::from(*radius))
                .ok_or_else(numeric)?;
            Ok(distance <= radius_squared)
        }
        StrokePrimitive::RoundSector {
            center,
            radius,
            incoming,
            outgoing,
            turn,
        } => {
            let vector = subtract(point, *center)?;
            let distance = point_distance_squared(*center, point)?;
            let radius_squared = i128::from(*radius)
                .checked_mul(i128::from(*radius))
                .ok_or_else(numeric)?;
            if distance > radius_squared {
                return Ok(false);
            }
            let incoming_outer = if *turn > 0 {
                Point {
                    x: incoming.y,
                    y: incoming.x.checked_neg().ok_or_else(numeric)?,
                }
            } else {
                Point {
                    x: incoming.y.checked_neg().ok_or_else(numeric)?,
                    y: incoming.x,
                }
            };
            let outgoing_outer = if *turn > 0 {
                Point {
                    x: outgoing.y,
                    y: outgoing.x.checked_neg().ok_or_else(numeric)?,
                }
            } else {
                Point {
                    x: outgoing.y.checked_neg().ok_or_else(numeric)?,
                    y: outgoing.x,
                }
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
    vertices: &[Point],
    point: Point,
    work: &mut dyn KernelWork,
) -> Result<bool, FastRasterError> {
    if vertices.len() < 3 {
        return Ok(false);
    }
    let mut positive = false;
    let mut negative = false;
    for index in 0..vertices.len() {
        work.step()?;
        let edge = subtract(vertices[(index + 1) % vertices.len()], vertices[index])?;
        let relative = subtract(point, vertices[index])?;
        let side = cross(edge, relative)?;
        positive |= side > 0;
        negative |= side < 0;
        if positive && negative {
            return Ok(false);
        }
    }
    Ok(true)
}

fn tangent_and_left_normal(
    start: Point,
    end: Point,
    magnitude: i64,
) -> Result<(Point, Point), FastRasterError> {
    let delta = subtract(end, start)?;
    let length = vector_length(delta)?;
    if length == 0 {
        return Err(invalid_resource());
    }
    let tangent = Point {
        x: mul_ratio(delta.x, magnitude, length)?,
        y: mul_ratio(delta.y, magnitude, length)?,
    };
    Ok((
        tangent,
        Point {
            x: tangent.y.checked_neg().ok_or_else(numeric)?,
            y: tangent.x,
        },
    ))
}

fn vector_length(vector: Point) -> Result<i64, FastRasterError> {
    let squared = squared_sum(i128::from(vector.x), i128::from(vector.y))?;
    let squared = u128::try_from(squared).map_err(|_| numeric())?;
    i64::try_from(integer_sqrt(squared)).map_err(|_| numeric())
}

fn line_intersection(
    first: Point,
    first_direction: Point,
    second: Point,
    second_direction: Point,
) -> Result<Option<Point>, FastRasterError> {
    let denominator = cross(first_direction, second_direction)?;
    if denominator == 0 {
        return Ok(None);
    }
    let separation = subtract(second, first)?;
    let numerator = cross(separation, second_direction)?;
    let x_delta = round_to_i64(
        i128::from(first_direction.x)
            .checked_mul(numerator)
            .ok_or_else(numeric)?,
        denominator,
    )?;
    let y_delta = round_to_i64(
        i128::from(first_direction.y)
            .checked_mul(numerator)
            .ok_or_else(numeric)?,
        denominator,
    )?;
    Ok(Some(Point {
        x: first.x.checked_add(x_delta).ok_or_else(numeric)?,
        y: first.y.checked_add(y_delta).ok_or_else(numeric)?,
    }))
}

fn point_from_scene(point: ScenePoint) -> Result<Point, FastRasterError> {
    Ok(Point {
        x: scalar_to_fixed(point.x())?,
        y: scalar_to_fixed(point.y())?,
    })
}

fn scalar_to_fixed(value: SceneScalar) -> Result<i64, FastRasterError> {
    round_to_i64(
        i128::from(value.scaled())
            .checked_mul(i128::from(FIXED_ONE))
            .ok_or_else(numeric)?,
        SCENE_SCALE,
    )
}

fn fixed_sum(products: &[(i64, i64)], addend: i64) -> Result<i64, FastRasterError> {
    let mut numerator = i128::from(addend)
        .checked_mul(i128::from(FIXED_ONE))
        .ok_or_else(numeric)?;
    for &(left, right) in products {
        numerator = numerator
            .checked_add(
                i128::from(left)
                    .checked_mul(i128::from(right))
                    .ok_or_else(numeric)?,
            )
            .ok_or_else(numeric)?;
    }
    round_to_i64(numerator, i128::from(FIXED_ONE))
}

fn fixed_mul(left: i64, right: i64) -> Result<i64, FastRasterError> {
    round_to_i64(
        i128::from(left)
            .checked_mul(i128::from(right))
            .ok_or_else(numeric)?,
        i128::from(FIXED_ONE),
    )
}

fn mul_ratio(value: i64, numerator: i64, denominator: i64) -> Result<i64, FastRasterError> {
    round_to_i64(
        i128::from(value)
            .checked_mul(i128::from(numerator))
            .ok_or_else(numeric)?,
        i128::from(denominator),
    )
}

fn lerp(
    start: Point,
    end: Point,
    numerator: u64,
    denominator: u64,
) -> Result<Point, FastRasterError> {
    if denominator == 0 || numerator > denominator {
        return Err(invalid_resource());
    }
    let coordinate = |start: i64, end: i64| -> Result<i64, FastRasterError> {
        let delta = i128::from(end)
            .checked_sub(i128::from(start))
            .ok_or_else(numeric)?;
        let offset = round_i128(
            delta
                .checked_mul(i128::from(numerator))
                .ok_or_else(numeric)?,
            i128::from(denominator),
        )?;
        i64::try_from(i128::from(start).checked_add(offset).ok_or_else(numeric)?)
            .map_err(|_| numeric())
    };
    Ok(Point {
        x: coordinate(start.x, end.x)?,
        y: coordinate(start.y, end.y)?,
    })
}

fn midpoint(left: Point, right: Point) -> Result<Point, FastRasterError> {
    Ok(Point {
        x: round_to_i64(i128::from(left.x) + i128::from(right.x), 2)?,
        y: round_to_i64(i128::from(left.y) + i128::from(right.y), 2)?,
    })
}

fn add_point(left: Point, right: Point) -> Result<Point, FastRasterError> {
    Ok(Point {
        x: left.x.checked_add(right.x).ok_or_else(numeric)?,
        y: left.y.checked_add(right.y).ok_or_else(numeric)?,
    })
}

fn subtract(left: Point, right: Point) -> Result<Point, FastRasterError> {
    Ok(Point {
        x: left.x.checked_sub(right.x).ok_or_else(numeric)?,
        y: left.y.checked_sub(right.y).ok_or_else(numeric)?,
    })
}

fn negate(point: Point) -> Result<Point, FastRasterError> {
    Ok(Point {
        x: point.x.checked_neg().ok_or_else(numeric)?,
        y: point.y.checked_neg().ok_or_else(numeric)?,
    })
}

fn cross(left: Point, right: Point) -> Result<i128, FastRasterError> {
    i128::from(left.x)
        .checked_mul(i128::from(right.y))
        .and_then(|value| {
            i128::from(left.y)
                .checked_mul(i128::from(right.x))
                .and_then(|other| value.checked_sub(other))
        })
        .ok_or_else(numeric)
}

fn dot(left: Point, right: Point) -> Result<i128, FastRasterError> {
    i128::from(left.x)
        .checked_mul(i128::from(right.x))
        .and_then(|value| {
            i128::from(left.y)
                .checked_mul(i128::from(right.y))
                .and_then(|other| value.checked_add(other))
        })
        .ok_or_else(numeric)
}

fn point_distance_squared(left: Point, right: Point) -> Result<i128, FastRasterError> {
    let delta = subtract(left, right)?;
    squared_sum(i128::from(delta.x), i128::from(delta.y))
}

fn squared_sum(x: i128, y: i128) -> Result<i128, FastRasterError> {
    x.checked_mul(x)
        .and_then(|x_squared| {
            y.checked_mul(y)
                .and_then(|y_squared| x_squared.checked_add(y_squared))
        })
        .ok_or_else(numeric)
}

fn integer_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let bit_width = 128 - value.leading_zeros();
    let mut current = 1_u128 << bit_width.div_ceil(2);
    loop {
        let next = (current + value / current) / 2;
        if next >= current {
            return current;
        }
        current = next;
    }
}

fn round_to_i64(numerator: i128, denominator: i128) -> Result<i64, FastRasterError> {
    i64::try_from(round_i128(numerator, denominator)?).map_err(|_| numeric())
}

fn round_i128(numerator: i128, denominator: i128) -> Result<i128, FastRasterError> {
    if denominator == 0 {
        return Err(invalid_resource());
    }
    let quotient = numerator.checked_div(denominator).ok_or_else(numeric)?;
    let remainder = numerator.checked_rem(denominator).ok_or_else(numeric)?;
    if remainder
        .unsigned_abs()
        .checked_mul(2)
        .ok_or_else(numeric)?
        >= denominator.unsigned_abs()
    {
        quotient
            .checked_add(if numerator.is_negative() == denominator.is_negative() {
                1
            } else {
                -1
            })
            .ok_or_else(numeric)
    } else {
        Ok(quotient)
    }
}

fn reserve_tracked<T>(
    values: &mut Vec<T>,
    additional: usize,
    base_intermediate: u64,
    retained: &mut u64,
    work: &mut dyn KernelWork,
) -> Result<(), FastRasterError> {
    if additional == 0
        || values.len().checked_add(additional).ok_or_else(numeric)? <= values.capacity()
    {
        return Ok(());
    }
    let old_bytes = capacity_bytes(values)?;
    let retained_without_old = retained.checked_sub(old_bytes).ok_or_else(numeric)?;
    let required = values.len().checked_add(additional).ok_or_else(numeric)?;
    let minimum = bytes_for_items::<T>(required)?;
    work.admit_intermediate(add_many(base_intermediate, *retained, minimum)?)?;
    values
        .try_reserve_exact(additional)
        .map_err(|_| FastRasterError::for_code(FastRasterErrorCode::Allocation))?;
    let actual = capacity_bytes(values)?;
    work.admit_intermediate(add_many(base_intermediate, *retained, actual)?)?;
    *retained = retained_without_old
        .checked_add(actual)
        .ok_or_else(numeric)?;
    work.admit_intermediate(add(base_intermediate, *retained)?)
}

fn capacity_bytes<T>(values: &Vec<T>) -> Result<u64, FastRasterError> {
    bytes_for_items::<T>(values.capacity())
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

fn add(left: u64, right: u64) -> Result<u64, FastRasterError> {
    left.checked_add(right).ok_or_else(numeric)
}

fn add_many(first: u64, second: u64, third: u64) -> Result<u64, FastRasterError> {
    first
        .checked_add(second)
        .and_then(|value| value.checked_add(third))
        .ok_or_else(numeric)
}

fn numeric() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::NumericOverflow)
}

fn invalid_resource() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::InvalidResource)
}

fn command_sequence() -> FastRasterError {
    FastRasterError::for_code(FastRasterErrorCode::InvalidCommandSequence)
}
