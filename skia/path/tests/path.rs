use pdf_rs_skia_error::SkiaErrorCode;
use pdf_rs_skia_geometry::{Point, Rect, Scalar, Transform};
use pdf_rs_skia_path::{Angle, ArcDirection, ArcStart, ConicWeight, PathBuilder, PathVerb};

fn scalar(value: i32) -> Scalar {
    Scalar::from_i32(value).expect("whole scalar")
}

fn point(x: i32, y: i32) -> Point {
    Point::new(scalar(x), scalar(y))
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect::new(scalar(left), scalar(top), scalar(right), scalar(bottom)).expect("positive rectangle")
}

#[test]
fn rect_oval_and_round_rect_expand_to_closed_deterministic_paths() {
    let mut rectangle = PathBuilder::new(5).expect("valid limit");
    rectangle
        .add_rect(rect(1, 2, 5, 7))
        .expect("rectangle path");
    assert_eq!(
        rectangle.finish().expect("finished rectangle").verbs(),
        &[
            PathVerb::MoveTo(point(1, 2)),
            PathVerb::LineTo(point(5, 2)),
            PathVerb::LineTo(point(5, 7)),
            PathVerb::LineTo(point(1, 7)),
            PathVerb::Close,
        ]
    );

    let mut oval = PathBuilder::new(6).expect("valid limit");
    oval.add_oval(rect(0, 0, 8, 4)).expect("oval path");
    let oval = oval.finish().expect("finished oval");
    assert_eq!(oval.verbs().len(), 6);
    assert_eq!(oval.verbs()[0], PathVerb::MoveTo(point(8, 2)));
    assert_eq!(oval.verbs()[5], PathVerb::Close);
    assert_cubic_end(oval.verbs()[1], point(4, 4));

    let mut circle = PathBuilder::new(6).expect("valid limit");
    circle
        .add_circle(point(4, 4), scalar(3))
        .expect("circle path");
    assert_eq!(
        circle.finish().expect("finished circle").verbs()[0],
        PathVerb::MoveTo(point(7, 4))
    );

    let mut rounded = PathBuilder::new(10).expect("valid limit");
    rounded
        .add_round_rect(rect(0, 0, 8, 6), scalar(2), scalar(2))
        .expect("rounded rectangle path");
    let rounded = rounded.finish().expect("finished rounded rectangle");
    assert_eq!(rounded.verbs().len(), 10);
    assert_eq!(rounded.verbs()[0], PathVerb::MoveTo(point(2, 0)));
    assert_eq!(rounded.verbs()[9], PathVerb::Close);
}

#[test]
fn cardinal_arcs_keep_their_declared_direction_and_validate_bounds() {
    let mut clockwise = PathBuilder::new(2).expect("valid limit");
    clockwise
        .add_arc(
            rect(0, 0, 8, 4),
            ArcStart::Right,
            ArcDirection::Clockwise,
            1,
        )
        .expect("clockwise arc");
    let clockwise = clockwise.finish().expect("finished clockwise arc");
    assert_eq!(clockwise.verbs()[0], PathVerb::MoveTo(point(8, 2)));
    assert_cubic_end(clockwise.verbs()[1], point(4, 4));

    let mut counterclockwise = PathBuilder::new(2).expect("valid limit");
    counterclockwise
        .add_arc(
            rect(0, 0, 8, 4),
            ArcStart::Right,
            ArcDirection::CounterClockwise,
            1,
        )
        .expect("counterclockwise arc");
    let counterclockwise = counterclockwise
        .finish()
        .expect("finished counterclockwise arc");
    assert_cubic_end(counterclockwise.verbs()[1], point(4, 0));

    let mut invalid = PathBuilder::new(5).expect("valid limit");
    assert_eq!(
        invalid
            .add_arc(
                rect(0, 0, 8, 4),
                ArcStart::Right,
                ArcDirection::Clockwise,
                0
            )
            .expect_err("zero quarter turns are invalid")
            .code(),
        SkiaErrorCode::InvalidGeometry
    );
    assert!(
        invalid.finish().is_err(),
        "invalid arc must not mutate the path"
    );
}

#[test]
fn arbitrary_angle_arcs_are_bounded_and_can_continue_a_contour() {
    let mut arc = PathBuilder::new(2).expect("valid limit");
    arc.add_arc_degrees(
        rect(0, 0, 8, 8),
        Angle::ZERO,
        Angle::from_degrees(45).expect("angle"),
    )
    .expect("arbitrary arc");
    let arc = arc.finish().expect("finished arc");
    assert_eq!(arc.verbs()[0], PathVerb::MoveTo(point(8, 4)));
    let end = cubic_end(arc.verbs()[1]);
    assert!(end.x() > scalar(4) && end.x() < scalar(8));
    assert!(end.y() > scalar(4) && end.y() < scalar(8));

    let mut continued = PathBuilder::new(3).expect("valid limit");
    continued.move_to(point(0, 4)).expect("start contour");
    continued
        .arc_to(
            rect(0, 0, 8, 8),
            Angle::ZERO,
            Angle::from_degrees(90).expect("angle"),
        )
        .expect("continue with arc");
    let continued = continued.finish().expect("finished continued arc");
    assert_eq!(continued.verbs()[1], PathVerb::LineTo(point(8, 4)));
    assert_near(cubic_end(continued.verbs()[2]), point(4, 8), 2);

    let mut reversed = PathBuilder::new(2).expect("valid limit");
    reversed
        .add_arc_degrees(
            rect(0, 0, 8, 8),
            Angle::ZERO,
            Angle::from_degrees(-90).expect("angle"),
        )
        .expect("reverse arc");
    assert_near(
        cubic_end(reversed.finish().expect("finished reverse arc").verbs()[1]),
        point(4, 0),
        2,
    );

    let mut rotated = PathBuilder::new(2).expect("valid limit");
    rotated
        .add_rotated_arc_degrees(
            rect(0, 0, 8, 4),
            Angle::from_degrees(90).expect("rotation"),
            Angle::ZERO,
            Angle::from_degrees(90).expect("sweep"),
        )
        .expect("rotated arc");
    let rotated = rotated.finish().expect("finished rotated arc");
    assert_near(
        match rotated.verbs()[0] {
            PathVerb::MoveTo(point) => point,
            other => panic!("expected arc start, got {other:?}"),
        },
        point(4, 6),
        2,
    );
    assert_near(cubic_end(rotated.verbs()[1]), point(2, 2), 2);

    let mut invalid = PathBuilder::new(5).expect("valid limit");
    assert_eq!(
        invalid
            .add_arc_degrees(
                rect(0, 0, 8, 8),
                Angle::ZERO,
                Angle::from_degrees(361).expect("angle"),
            )
            .expect_err("more than one full turn is invalid")
            .code(),
        SkiaErrorCode::InvalidGeometry
    );
}

#[test]
fn paths_transform_append_and_report_conservative_bounds() {
    let mut source = PathBuilder::new(5).expect("valid limit");
    source.add_rect(rect(0, 0, 2, 3)).expect("rectangle");
    let source = source.finish().expect("finished path");

    let transformed = source
        .transformed(Transform::translate(scalar(3), scalar(4)))
        .expect("transformed path");
    let bounds = transformed.bounds().expect("path bounds");
    assert_eq!(bounds.left(), scalar(3));
    assert_eq!(bounds.top(), scalar(4));
    assert_eq!(bounds.right(), scalar(5));
    assert_eq!(bounds.bottom(), scalar(7));

    let mut joined = PathBuilder::new(6).expect("valid limit");
    joined.move_to(point(9, 9)).expect("initial contour");
    joined.append_path(&source).expect("append path");
    let joined = joined.finish().expect("finished joined path");
    assert_eq!(joined.verbs().len(), 6);
    assert_eq!(joined.verbs()[1], PathVerb::MoveTo(point(0, 0)));

    let mut contours = PathBuilder::new(10).expect("valid limit");
    contours.add_rect(rect(0, 0, 2, 2)).expect("first contour");
    contours.add_rect(rect(5, 6, 8, 9)).expect("second contour");
    let bounds = contours
        .finish()
        .expect("finished contours")
        .bounds()
        .expect("multi-contour bounds");
    assert_eq!(bounds.left(), scalar(0));
    assert_eq!(bounds.top(), scalar(0));
    assert_eq!(bounds.right(), scalar(8));
    assert_eq!(bounds.bottom(), scalar(9));

    let mut extreme = PathBuilder::new(1).expect("valid limit");
    extreme
        .move_to(Point::new(Scalar::from_bits(i32::MAX), Scalar::ZERO))
        .expect("extreme point");
    assert_eq!(
        extreme
            .finish()
            .expect("finished extreme path")
            .transformed(Transform::translate(Scalar::from_bits(1), Scalar::ZERO))
            .expect_err("overflow must fail")
            .code(),
        SkiaErrorCode::NumericOverflow
    );
}

#[test]
fn degenerate_shape_arguments_fail_before_mutating_the_builder() {
    let mut builder = PathBuilder::new(6).expect("valid limit");
    assert_eq!(
        builder
            .add_circle(point(3, 3), Scalar::ZERO)
            .expect_err("zero-radius circle is invalid")
            .code(),
        SkiaErrorCode::InvalidGeometry
    );
    assert_eq!(
        builder
            .add_round_rect(rect(0, 0, 4, 4), Scalar::from_bits(-1), scalar(1))
            .expect_err("negative radius is invalid")
            .code(),
        SkiaErrorCode::InvalidGeometry
    );
    assert!(
        builder.finish().is_err(),
        "failed shapes must not add verbs"
    );
}

#[test]
fn polygons_and_reversed_contours_preserve_drawable_geometry() {
    let polygon = [point(0, 0), point(4, 0), point(2, 3)];
    let mut builder = PathBuilder::new(4).expect("valid limit");
    builder.add_polygon(&polygon, true).expect("closed polygon");
    let polygon = builder.finish().expect("finished polygon");
    assert_eq!(polygon.verbs().len(), 4);
    assert_eq!(polygon.verbs()[3], PathVerb::Close);

    let mut curve = PathBuilder::new(3).expect("valid limit");
    curve.move_to(point(0, 0)).expect("move");
    curve.quad_to(point(2, 3), point(4, 0)).expect("quad");
    curve
        .cubic_to(point(5, 0), point(6, 1), point(8, 2))
        .expect("cubic");
    let reversed = curve
        .finish()
        .expect("finished curve")
        .reversed()
        .expect("reverse");
    assert_eq!(reversed.verbs()[0], PathVerb::MoveTo(point(8, 2)));
    assert_eq!(
        reversed.verbs()[1],
        PathVerb::CubicTo(point(6, 1), point(5, 0), point(4, 0))
    );
    assert_eq!(
        reversed.verbs()[2],
        PathVerb::QuadTo(point(2, 3), point(0, 0))
    );

    let mut invalid = PathBuilder::new(3).expect("valid limit");
    assert_eq!(
        invalid
            .add_polygon(&[point(0, 0), point(1, 1)], true)
            .expect_err("closed polygon needs three points")
            .code(),
        SkiaErrorCode::InvalidGeometry
    );
}

#[test]
fn tight_bounds_follow_bezier_extrema_not_control_hulls() {
    let mut builder = PathBuilder::new(2).expect("valid limit");
    builder.move_to(point(0, 0)).expect("move");
    builder.quad_to(point(5, 10), point(10, 0)).expect("quad");
    let path = builder.finish().expect("finished path");
    assert_eq!(path.bounds().expect("control bounds").bottom(), scalar(10));
    let tight = path.tight_bounds().expect("tight bounds");
    assert!(tight.bottom() > scalar(4) && tight.bottom() < scalar(6));
    assert!(tight.top() <= Scalar::ZERO);
    assert!(tight.left() <= Scalar::ZERO);
    assert!(tight.right() >= scalar(10));

    let mut cubic = PathBuilder::new(2).expect("valid limit");
    cubic.move_to(point(0, 0)).expect("move");
    cubic
        .cubic_to(point(0, 12), point(12, 12), point(12, 0))
        .expect("cubic");
    let tight = cubic
        .finish()
        .expect("finished cubic")
        .tight_bounds()
        .expect("tight cubic bounds");
    assert!(tight.bottom() > scalar(8) && tight.bottom() < scalar(10));
}

#[test]
fn rational_quadratics_are_first_class_path_segments() {
    let weight = ConicWeight::from_ratio(1, 2).expect("positive weight");
    let mut builder = PathBuilder::new(2).expect("valid limit");
    builder.move_to(point(0, 0)).expect("move");
    builder
        .conic_to(point(4, 8), point(8, 0), weight)
        .expect("conic");
    let path = builder.finish().expect("finished conic");
    assert_eq!(
        path.verbs()[1],
        PathVerb::ConicTo(point(4, 8), point(8, 0), weight)
    );
    assert_eq!(
        path.reversed().expect("reverse conic").verbs()[1],
        PathVerb::ConicTo(point(4, 8), point(0, 0), weight)
    );
    assert_eq!(
        ConicWeight::from_ratio(0, 1)
            .expect_err("zero weight is invalid")
            .code(),
        SkiaErrorCode::InvalidGeometry
    );
}

fn assert_cubic_end(verb: PathVerb, expected: Point) {
    assert_eq!(cubic_end(verb), expected);
}

fn cubic_end(verb: PathVerb) -> Point {
    match verb {
        PathVerb::CubicTo(_, _, end) => end,
        other => panic!("expected cubic arc segment, got {other:?}"),
    }
}

fn assert_near(actual: Point, expected: Point, tolerance_bits: i32) {
    assert!(
        (actual.x().bits() - expected.x().bits()).abs() <= tolerance_bits
            && (actual.y().bits() - expected.y().bits()).abs() <= tolerance_bits,
        "actual {actual:?} differs from {expected:?} by more than {tolerance_bits} bits"
    );
}
