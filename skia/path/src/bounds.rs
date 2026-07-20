use pdf_rs_skia_geometry::{Point, Scalar};

use super::{PathBounds, max_scalar, min_scalar};

const Q32_ONE: i64 = 1_i64 << 32;

pub(super) fn extend_bounds(bounds: &mut Option<PathBounds>, point: Point) {
    match bounds {
        Some(bounds) => {
            bounds.left = min_scalar(bounds.left, point.x());
            bounds.top = min_scalar(bounds.top, point.y());
            bounds.right = max_scalar(bounds.right, point.x());
            bounds.bottom = max_scalar(bounds.bottom, point.y());
        }
        None => {
            *bounds = Some(PathBounds {
                left: point.x(),
                top: point.y(),
                right: point.x(),
                bottom: point.y(),
            });
        }
    }
}

pub(super) fn extend_quad_tight_bounds(
    bounds: &mut Option<PathBounds>,
    start: Point,
    control: Point,
    end: Point,
) {
    extend_bounds(bounds, end);
    for t in quadratic_extrema(start.x(), control.x(), end.x())
        .into_iter()
        .chain(quadratic_extrema(start.y(), control.y(), end.y()))
        .flatten()
    {
        extend_bounds(bounds, evaluate_quad(start, control, end, t));
    }
}

pub(super) fn extend_cubic_tight_bounds(
    bounds: &mut Option<PathBounds>,
    start: Point,
    first_control: Point,
    second_control: Point,
    end: Point,
) {
    extend_bounds(bounds, end);
    for t in cubic_extrema(start.x(), first_control.x(), second_control.x(), end.x())
        .into_iter()
        .chain(cubic_extrema(
            start.y(),
            first_control.y(),
            second_control.y(),
            end.y(),
        ))
        .flatten()
    {
        extend_bounds(
            bounds,
            evaluate_cubic(start, first_control, second_control, end, t),
        );
    }
}

pub(super) fn pad_bounds(mut bounds: PathBounds) -> PathBounds {
    bounds.left = Scalar::from_bits(bounds.left.bits().saturating_sub(2));
    bounds.top = Scalar::from_bits(bounds.top.bits().saturating_sub(2));
    bounds.right = Scalar::from_bits(bounds.right.bits().saturating_add(2));
    bounds.bottom = Scalar::from_bits(bounds.bottom.bits().saturating_add(2));
    bounds
}

fn quadratic_extrema(start: Scalar, control: Scalar, end: Scalar) -> [Option<i64>; 1] {
    let start = i128::from(start.bits());
    let control = i128::from(control.bits());
    let end = i128::from(end.bits());
    [unit_root(start - control, start - control * 2 + end)]
}

fn cubic_extrema(
    start: Scalar,
    first_control: Scalar,
    second_control: Scalar,
    end: Scalar,
) -> [Option<i64>; 2] {
    let start = i128::from(start.bits());
    let first = i128::from(first_control.bits());
    let second = i128::from(second_control.bits());
    let end = i128::from(end.bits());
    let a = -start + first * 3 - second * 3 + end;
    let b = (start - first * 2 + second) * 2;
    let c = first - start;
    if a == 0 {
        return [unit_root(-c, b), None];
    }
    let discriminant = b * b - 4 * a * c;
    if discriminant < 0 {
        return [None, None];
    }
    let root = integer_sqrt(discriminant as u128) as i128;
    [unit_root(-b + root, a * 2), unit_root(-b - root, a * 2)]
}

fn unit_root(numerator: i128, denominator: i128) -> Option<i64> {
    if denominator == 0 {
        return None;
    }
    let value = numerator.checked_shl(32)? / denominator;
    let value = i64::try_from(value).ok()?;
    (0 < value && value < Q32_ONE).then_some(value)
}

fn integer_sqrt(value: u128) -> u128 {
    let mut low = 0_u128;
    let mut high = 1_u128 << 64;
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        if middle <= value / middle {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

fn evaluate_quad(start: Point, control: Point, end: Point, t: i64) -> Point {
    Point::new(
        lerp_q32(
            lerp_q32(start.x(), control.x(), t),
            lerp_q32(control.x(), end.x(), t),
            t,
        ),
        lerp_q32(
            lerp_q32(start.y(), control.y(), t),
            lerp_q32(control.y(), end.y(), t),
            t,
        ),
    )
}

fn evaluate_cubic(
    start: Point,
    first_control: Point,
    second_control: Point,
    end: Point,
    t: i64,
) -> Point {
    let first_x = lerp_q32(start.x(), first_control.x(), t);
    let second_x = lerp_q32(first_control.x(), second_control.x(), t);
    let third_x = lerp_q32(second_control.x(), end.x(), t);
    let first_y = lerp_q32(start.y(), first_control.y(), t);
    let second_y = lerp_q32(first_control.y(), second_control.y(), t);
    let third_y = lerp_q32(second_control.y(), end.y(), t);
    Point::new(
        lerp_q32(
            lerp_q32(first_x, second_x, t),
            lerp_q32(second_x, third_x, t),
            t,
        ),
        lerp_q32(
            lerp_q32(first_y, second_y, t),
            lerp_q32(second_y, third_y, t),
            t,
        ),
    )
}

fn lerp_q32(first: Scalar, second: Scalar, t: i64) -> Scalar {
    let delta = i128::from(second.bits()) - i128::from(first.bits());
    let scaled = delta * i128::from(t);
    let offset = if scaled >= 0 {
        (scaled + i128::from(Q32_ONE / 2)) / i128::from(Q32_ONE)
    } else {
        -((-scaled + i128::from(Q32_ONE / 2)) / i128::from(Q32_ONE))
    };
    let value = i128::from(first.bits()) + offset;
    Scalar::from_bits(value.clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32)
}
