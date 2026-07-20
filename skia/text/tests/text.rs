use pdf_rs_skia_text::{FontId, GlyphId, GlyphRun, PositionedGlyph, TextErrorCode, TextUnit};

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
    let run = GlyphRun::new(FontId::new(44), 12 << 16, vec![glyph()]).expect("valid run");

    assert_eq!(run.font(), FontId::new(44));
    assert_eq!(run.font_size_bits(), 12 << 16);
    assert_eq!(run.glyphs(), &[glyph()]);
}

#[test]
fn glyph_run_rejects_ambiguous_input() {
    assert_eq!(
        GlyphRun::new(FontId::new(1), 0, vec![glyph()])
            .expect_err("zero size must fail")
            .code(),
        TextErrorCode::InvalidFontSize
    );
    assert_eq!(
        GlyphRun::new(FontId::new(1), 12 << 16, Vec::new())
            .expect_err("empty run must fail")
            .code(),
        TextErrorCode::EmptyGlyphRun
    );
}
