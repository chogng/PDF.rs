use pdf_rs_skia::{
    ClipRect, Color, FillRule, Paint, PathBuilder, Point, Rect, Scalar, Surface, SurfaceLimits,
    Transform,
};

fn scalar(value: i32) -> Scalar {
    Scalar::from_i32(value).unwrap()
}

fn point(x: i32, y: i32) -> Point {
    Point::new(scalar(x), scalar(y))
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect::new(scalar(left), scalar(top), scalar(right), scalar(bottom)).unwrap()
}

fn pixel(surface: &Surface, x: usize, y: usize) -> [u8; 4] {
    let offset = (y * surface.width() as usize + x) * 4;
    surface.pixels()[offset..offset + 4].try_into().unwrap()
}

#[test]
fn clipped_source_over_rect_is_exact_and_save_restore_is_isolated() {
    let mut surface = Surface::new(4, 3, SurfaceLimits::default()).unwrap();
    let mut canvas = surface.canvas();
    canvas.clear(Color::WHITE);
    canvas.save().unwrap();
    canvas.clip_rect(ClipRect::new(rect(1, 1, 3, 3))).unwrap();
    canvas
        .fill_rect(rect(0, 0, 4, 3), Paint::new(Color::rgba(255, 0, 0, 128)))
        .unwrap();
    canvas.restore().unwrap();
    canvas
        .fill_rect(rect(0, 0, 1, 1), Paint::new(Color::BLACK))
        .unwrap();
    drop(canvas);

    assert_eq!(pixel(&surface, 0, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&surface, 1, 0), [255, 255, 255, 255]);
    assert_eq!(pixel(&surface, 1, 1), [255, 127, 127, 255]);
    assert_eq!(pixel(&surface, 2, 2), [255, 127, 127, 255]);
    assert_eq!(pixel(&surface, 3, 2), [255, 255, 255, 255]);
}

#[test]
fn even_odd_path_hole_and_translation_are_deterministic() {
    let mut path = PathBuilder::new(10).unwrap();
    path.move_to(point(0, 0)).unwrap();
    path.line_to(point(4, 0)).unwrap();
    path.line_to(point(4, 4)).unwrap();
    path.line_to(point(0, 4)).unwrap();
    path.close().unwrap();
    path.move_to(point(1, 1)).unwrap();
    path.line_to(point(3, 1)).unwrap();
    path.line_to(point(3, 3)).unwrap();
    path.line_to(point(1, 3)).unwrap();
    path.close().unwrap();
    let path = path.finish().unwrap();

    let mut surface = Surface::new(6, 5, SurfaceLimits::default()).unwrap();
    let mut canvas = surface.canvas();
    canvas.set_transform(Transform::translate(scalar(1), scalar(0)));
    canvas
        .fill_path(
            &path,
            FillRule::EvenOdd,
            Paint::new(Color::rgba(0, 0, 255, 255)),
        )
        .unwrap();
    drop(canvas);

    assert_eq!(pixel(&surface, 1, 0), [0, 0, 255, 255]);
    assert_eq!(pixel(&surface, 2, 1), [0, 0, 0, 0]);
    assert_eq!(pixel(&surface, 4, 3), [0, 0, 255, 255]);
    assert_eq!(pixel(&surface, 5, 4), [0, 0, 0, 0]);
}

#[test]
fn fixed_point_construction_and_surface_budgets_fail_closed() {
    assert!(Scalar::from_ratio(1, 0).is_err());
    assert!(Surface::new(3, 3, SurfaceLimits::new(8, 32, 1).unwrap()).is_err());
}
