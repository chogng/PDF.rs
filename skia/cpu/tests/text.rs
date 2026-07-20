use pdf_rs_skia_core::{
    Color, DisplayListBuilder, FontId, GlyphId, GlyphOutline, GlyphOutlineProvider, GlyphRun,
    OutlinePoint, OutlineSegment, Paint, PositionedGlyph, TextError, TextUnit,
};
use pdf_rs_skia_cpu::{Surface, SurfaceLimits};

struct SingleGlyphProvider(GlyphOutline);

impl GlyphOutlineProvider for SingleGlyphProvider {
    fn glyph_outline(
        &self,
        font: FontId,
        glyph: GlyphId,
    ) -> Result<Option<GlyphOutline>, TextError> {
        Ok((self.0.font() == font && self.0.glyph() == glyph).then(|| self.0.clone()))
    }
}

fn coordinate(value: i32) -> TextUnit {
    TextUnit::from_i32(value).expect("small exact coordinate")
}

fn glyph_run() -> GlyphRun {
    GlyphRun::new(
        FontId::new(7),
        10 << 16,
        10,
        vec![PositionedGlyph::new(
            GlyphId::new(3),
            TextUnit::ZERO,
            TextUnit::ZERO,
            TextUnit::ZERO,
            TextUnit::ZERO,
        )],
    )
    .expect("valid run")
}

fn glyph_outline() -> GlyphOutline {
    let point = |x, y| OutlinePoint::new(coordinate(x), coordinate(y));
    GlyphOutline::new(
        FontId::new(7),
        GlyphId::new(3),
        vec![
            OutlineSegment::MoveTo(point(0, 0)),
            OutlineSegment::LineTo(point(10, 0)),
            OutlineSegment::LineTo(point(10, 10)),
            OutlineSegment::LineTo(point(0, 10)),
            OutlineSegment::Close,
        ],
    )
    .expect("valid closed square")
}

#[test]
fn display_list_glyph_run_reuses_core_path_fill_rasterization() {
    let mut surface = Surface::new(12, 12, SurfaceLimits::default()).expect("surface");
    let mut builder = DisplayListBuilder::new(2).expect("display-list limits");
    let run = builder.add_glyph_run(glyph_run()).expect("store glyph run");
    builder
        .draw_glyph_run(run, Paint::new(Color::rgba(20, 40, 60, 255)))
        .expect("record glyph draw");
    let list = builder.finish();

    surface
        .execute_display_list(&list, &SingleGlyphProvider(glyph_outline()))
        .expect("execute glyph command");

    assert_eq!(
        &surface.pixels()[4 * (5 * 12 + 5)..][..4],
        &[20, 40, 60, 255]
    );
    assert_eq!(&surface.pixels()[4 * (11 * 12 + 11)..][..4], &[0, 0, 0, 0]);
}
