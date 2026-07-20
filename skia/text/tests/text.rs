use pdf_rs_skia_text::{
    FontId, GlyphId, GlyphRun, OutlinePoint, OutlineSegment, PositionedGlyph, TextErrorCode,
    TextUnit,
};

fn glyph() -> PositionedGlyph {
    PositionedGlyph::new(
        GlyphId::new(17),
        TextUnit::from_bits(64),
        TextUnit::ZERO,
        TextUnit::from_bits(96),
        TextUnit::ZERO,
    )
}

#[test]
fn glyph_run_preserves_shaper_output() {
    let run = GlyphRun::new(FontId::new(44), 12 << 16, 1_000, vec![glyph()]).expect("valid run");

    assert_eq!(run.font(), FontId::new(44));
    assert_eq!(run.font_size_bits(), 12 << 16);
    assert_eq!(run.units_per_em(), 1_000);
    assert_eq!(run.glyphs(), &[glyph()]);
}

#[test]
fn glyph_run_rejects_ambiguous_input() {
    assert_eq!(
        GlyphRun::new(FontId::new(1), 0, 1_000, vec![glyph()])
            .expect_err("zero size must fail")
            .code(),
        TextErrorCode::InvalidFontSize
    );
    assert_eq!(
        GlyphRun::new(FontId::new(1), 12 << 16, 1_000, Vec::new())
            .expect_err("empty run must fail")
            .code(),
        TextErrorCode::EmptyGlyphRun
    );
}

#[test]
fn outline_rejects_open_or_out_of_order_contours() {
    let point = OutlinePoint::new(TextUnit::ZERO, TextUnit::ZERO);
    assert_eq!(
        pdf_rs_skia_text::GlyphOutline::new(
            FontId::new(1),
            GlyphId::new(1),
            vec![OutlineSegment::LineTo(point)],
        )
        .expect_err("line requires a move")
        .code(),
        TextErrorCode::InvalidOutline
    );
    assert_eq!(
        pdf_rs_skia_text::GlyphOutline::new(
            FontId::new(1),
            GlyphId::new(1),
            vec![OutlineSegment::MoveTo(point)],
        )
        .expect_err("open contour is ambiguous")
        .code(),
        TextErrorCode::InvalidOutline
    );
}
