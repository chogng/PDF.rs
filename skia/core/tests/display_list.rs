use pdf_rs_skia_core::{
    Color, DisplayListBuilder, DrawCommand, FontId, GlyphId, GlyphRun, Paint, PositionedGlyph,
    Scalar, SkiaErrorCode, TextUnit, Transform,
};

fn glyph_run() -> GlyphRun {
    GlyphRun::new(
        FontId::new(9),
        12 << 16,
        1_000,
        vec![PositionedGlyph::new(
            GlyphId::new(3),
            TextUnit::ZERO,
            TextUnit::ZERO,
            TextUnit::from_bits(64),
            TextUnit::ZERO,
        )],
    )
    .expect("valid glyph run")
}

#[test]
fn display_list_owns_and_draws_a_glyph_run() {
    let mut builder = DisplayListBuilder::new(8).expect("valid limits");
    let run = builder.add_glyph_run(glyph_run()).expect("store run");
    let paint = Paint::new(Color::rgba(10, 20, 30, 255));

    builder
        .draw_glyph_run(run, paint)
        .expect("record text command");
    let list = builder.finish();

    assert_eq!(
        list.glyph_run(run).expect("stored run").font(),
        FontId::new(9)
    );
    assert_eq!(list.commands(), &[DrawCommand::DrawGlyphRun { run, paint }]);
}

#[test]
fn stroke_command_rejects_non_positive_width() {
    let mut builder = DisplayListBuilder::new(2).expect("valid limits");
    let path = builder.add_path(empty_path()).expect("store path");
    let error = builder
        .stroke_path(
            path,
            pdf_rs_skia_core::Scalar::ZERO,
            Paint::new(Color::rgba(0, 0, 0, 255)),
        )
        .expect_err("zero stroke width must fail");

    assert_eq!(error.code(), SkiaErrorCode::InvalidGeometry);
}

#[test]
fn display_list_records_generic_transform_concatenation() {
    let mut builder = DisplayListBuilder::new(1).expect("valid limits");
    let transform = Transform::translate(
        Scalar::from_i32(3).expect("scalar"),
        Scalar::from_i32(-2).expect("scalar"),
    );
    builder
        .concat_transform(transform)
        .expect("record transform");

    assert_eq!(
        builder.finish().commands(),
        &[DrawCommand::ConcatTransform(transform)]
    );
}

fn empty_path() -> pdf_rs_skia_core::Path {
    let mut builder = pdf_rs_skia_core::PathBuilder::new(1).expect("valid path limits");
    builder
        .move_to(pdf_rs_skia_core::Point::new(
            pdf_rs_skia_core::Scalar::ZERO,
            pdf_rs_skia_core::Scalar::ZERO,
        ))
        .expect("start path");
    builder.finish().expect("valid one-point path")
}
