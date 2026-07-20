use pdf_rs_skia_core::{
    ArcDirection, ArcStart, PathBuilder, PathVerb, Point, Rect, Scalar, SkiaErrorCode,
};

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

fn assert_cubic_end(verb: PathVerb, expected: Point) {
    match verb {
        PathVerb::CubicTo(_, _, end) => assert_eq!(end, expected),
        other => panic!("expected cubic arc segment, got {other:?}"),
    }
}
