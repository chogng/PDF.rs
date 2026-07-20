use pdf_rs_skia::{
    BlendMode, ClipRect, Color, FillRule, Image, Paint, PathBuilder, Point, Rect, Scalar,
    SkiaErrorCode, Surface, SurfaceLimits, Transform,
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

#[test]
fn bezier_fill_uses_a_deterministic_fixed_flattening() {
    let mut path = PathBuilder::new(4).unwrap();
    path.move_to(point(0, 4)).unwrap();
    path.quad_to(point(2, 0), point(4, 4)).unwrap();
    path.close().unwrap();
    let path = path.finish().unwrap();

    let mut surface = Surface::new(5, 5, SurfaceLimits::default()).unwrap();
    let mut canvas = surface.canvas();
    canvas
        .fill_path(
            &path,
            FillRule::NonZero,
            Paint::new(Color::rgba(0, 255, 0, 255)),
        )
        .unwrap();
    drop(canvas);

    assert_eq!(pixel(&surface, 2, 3), [0, 255, 0, 255]);
    assert_eq!(pixel(&surface, 2, 0), [0, 0, 0, 0]);
}

#[test]
fn cubic_curves_and_sheared_rectangles_use_the_general_path_rasterizer() {
    let mut curve = PathBuilder::new(4).unwrap();
    curve.move_to(point(0, 4)).unwrap();
    curve
        .cubic_to(point(0, 0), point(4, 0), point(4, 4))
        .unwrap();
    curve.close().unwrap();
    let curve = curve.finish().unwrap();

    let mut surface = Surface::new(6, 5, SurfaceLimits::default()).unwrap();
    let mut canvas = surface.canvas();
    canvas
        .fill_path(
            &curve,
            FillRule::NonZero,
            Paint::new(Color::rgba(0, 0, 255, 255)),
        )
        .unwrap();
    canvas.set_transform(Transform::new(
        scalar(1),
        scalar(0),
        scalar(1),
        scalar(1),
        scalar(0),
        scalar(0),
    ));
    canvas
        .fill_rect(rect(0, 0, 2, 2), Paint::new(Color::rgba(255, 0, 0, 255)))
        .unwrap();
    drop(canvas);

    assert_eq!(pixel(&surface, 2, 3), [0, 0, 255, 255]);
    assert_eq!(pixel(&surface, 0, 0), [255, 0, 0, 255]);
}

#[test]
fn stroke_has_round_caps_and_joins_without_pdf_dependencies() {
    let mut path = PathBuilder::new(3).unwrap();
    path.move_to(point(1, 2)).unwrap();
    path.line_to(point(5, 2)).unwrap();
    let path = path.finish().unwrap();

    let mut surface = Surface::new(7, 4, SurfaceLimits::default()).unwrap();
    let mut canvas = surface.canvas();
    canvas
        .stroke_path(&path, scalar(2), Paint::new(Color::rgba(255, 0, 0, 255)))
        .unwrap();
    drop(canvas);

    assert_eq!(pixel(&surface, 0, 2), [255, 0, 0, 255]);
    assert_eq!(pixel(&surface, 5, 1), [255, 0, 0, 255]);
    assert_eq!(pixel(&surface, 3, 0), [0, 0, 0, 0]);
}

#[test]
fn concatenated_transforms_and_curve_order_fail_closed() {
    let transform = Transform::translate(scalar(1), scalar(1))
        .concat(Transform::scale(scalar(2), scalar(3)))
        .unwrap();
    assert_eq!(transform.map_point(point(1, 1)).unwrap(), point(4, 6));

    let mut path = PathBuilder::new(1).unwrap();
    assert_eq!(
        path.cubic_to(point(0, 0), point(1, 1), point(2, 2))
            .unwrap_err()
            .code(),
        SkiaErrorCode::InvalidPath
    );
}

#[test]
fn rgba_images_scale_nearest_neighbor_and_keep_source_color_under_opacity() {
    let image = Image::from_rgba8(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
    let mut surface = Surface::new(4, 2, SurfaceLimits::default()).unwrap();
    let mut canvas = surface.canvas();
    canvas
        .draw_image(&image, rect(0, 0, 4, 2), 128, BlendMode::SourceOver)
        .unwrap();
    drop(canvas);

    assert_eq!(pixel(&surface, 0, 0), [255, 0, 0, 128]);
    assert_eq!(pixel(&surface, 1, 1), [255, 0, 0, 128]);
    assert_eq!(pixel(&surface, 2, 0), [0, 0, 255, 128]);
    assert_eq!(pixel(&surface, 3, 1), [0, 0, 255, 128]);
    assert_eq!(
        Image::from_rgba8(2, 2, vec![0; 3]).unwrap_err().code(),
        SkiaErrorCode::InvalidImage
    );
}
