use pdf_rs_font::{FontCoordinate, FontPoint, OutlineSegment as PdfOutlineSegment};
use pdf_rs_skia::{FontId, GlyphId, OutlineSegment, TextUnit};
use pdf_rs_skia_pdf::convert_outline_segments;

fn point(x_half_units: i32, y_half_units: i32) -> FontPoint {
    FontPoint::new(
        FontCoordinate::from_half_units(x_half_units),
        FontCoordinate::from_half_units(y_half_units),
    )
}

#[test]
fn pdf_outline_adapter_converts_coordinate_orientation_and_precision() {
    let outline = convert_outline_segments(
        FontId::new(5),
        GlyphId::new(6),
        &[
            PdfOutlineSegment::MoveTo(point(1, 3)),
            PdfOutlineSegment::LineTo(point(5, -7)),
            PdfOutlineSegment::CloseContour,
        ],
    )
    .expect("valid PDF contour");

    assert_eq!(
        outline.segments(),
        &[
            OutlineSegment::MoveTo(pdf_rs_skia::OutlinePoint::new(
                TextUnit::from_bits(32),
                TextUnit::from_bits(-96),
            )),
            OutlineSegment::LineTo(pdf_rs_skia::OutlinePoint::new(
                TextUnit::from_bits(160),
                TextUnit::from_bits(224),
            )),
            OutlineSegment::Close,
        ]
    );
}
