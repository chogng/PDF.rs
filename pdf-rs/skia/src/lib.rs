//! PDF font-outline adapter for the reusable Skia text boundary.
//!
//! This crate is deliberately one-way: it depends on PDF parsing and Skia,
//! while `skia/*` never depends on the PDF implementation. The adapter uses
//! only the stable `pdf-rs-skia` public API and converts parsed TrueType and
//! CFF outlines into canvas-oriented glyph outlines.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use pdf_rs_font::{
    FontCoordinate, FontPoint, FontProgram, GlyphId as PdfGlyphId,
    OutlineSegment as PdfOutlineSegment,
};
use pdf_rs_skia::{
    FontId, GlyphId, GlyphOutline, GlyphOutlineProvider, OutlinePoint, OutlineSegment, TextError,
    TextErrorCode, TextUnit,
};

/// Adapts one parsed embedded PDF font program to Skia glyph-outline lookup.
#[derive(Debug)]
pub struct PdfFontOutlineProvider<'a> {
    font_id: FontId,
    program: &'a FontProgram,
}

impl<'a> PdfFontOutlineProvider<'a> {
    /// Binds a stable Skia font identifier to one parsed PDF font program.
    pub const fn new(font_id: FontId, program: &'a FontProgram) -> Self {
        Self { font_id, program }
    }

    /// Returns the parsed program's design units per em.
    pub const fn units_per_em(&self) -> u16 {
        self.program.units_per_em()
    }
}

impl GlyphOutlineProvider for PdfFontOutlineProvider<'_> {
    fn glyph_outline(
        &self,
        font: FontId,
        glyph: GlyphId,
    ) -> Result<Option<GlyphOutline>, TextError> {
        if font != self.font_id {
            return Ok(None);
        }
        let Ok(glyph_id) = u16::try_from(glyph.value()) else {
            return Ok(None);
        };
        let Some(outline) = self.program.glyph_outline(PdfGlyphId::new(glyph_id)) else {
            return Ok(None);
        };
        convert_outline_segments(font, glyph, outline.segments()).map(Some)
    }
}

/// Converts PDF's Y-up half-unit outline coordinates into Skia's Y-down Q26.6 coordinates.
pub fn convert_outline_segments(
    font: FontId,
    glyph: GlyphId,
    segments: &[PdfOutlineSegment],
) -> Result<GlyphOutline, TextError> {
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(segments.len())
        .map_err(|_| TextError::new(TextErrorCode::AllocationFailed))?;
    for segment in segments {
        converted.push(match *segment {
            PdfOutlineSegment::MoveTo(point) => OutlineSegment::MoveTo(convert_point(point)?),
            PdfOutlineSegment::LineTo(point) => OutlineSegment::LineTo(convert_point(point)?),
            PdfOutlineSegment::QuadTo { control, end } => OutlineSegment::QuadTo {
                control: convert_point(control)?,
                end: convert_point(end)?,
            },
            PdfOutlineSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => OutlineSegment::CubicTo {
                first_control: convert_point(control_1)?,
                second_control: convert_point(control_2)?,
                end: convert_point(end)?,
            },
            PdfOutlineSegment::CloseContour => OutlineSegment::Close,
        });
    }
    GlyphOutline::new(font, glyph, converted)
}

fn convert_point(point: FontPoint) -> Result<OutlinePoint, TextError> {
    Ok(OutlinePoint::new(
        convert_coordinate(point.x(), false)?,
        convert_coordinate(point.y(), true)?,
    ))
}

fn convert_coordinate(coordinate: FontCoordinate, invert: bool) -> Result<TextUnit, TextError> {
    let half_units = i64::from(coordinate.half_units());
    let signed = if invert { -half_units } else { half_units };
    let bits = signed
        .checked_mul(32)
        .ok_or(TextError::new(TextErrorCode::NumericOverflow))?;
    i32::try_from(bits)
        .map(TextUnit::from_bits)
        .map_err(|_| TextError::new(TextErrorCode::NumericOverflow))
}
