//! Portable text resources used by the Skia display-list layer.
//!
//! This crate deliberately contains neither a system-font dependency nor a
//! platform text API. It represents the stable output of shaping: identified
//! fonts and positioned glyphs. Font parsing, fallback, and Unicode shaping
//! can therefore evolve independently without changing raster backends.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

/// Stable machine-readable text-resource failure.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TextErrorCode {
    /// A coordinate or intermediate calculation overflowed.
    NumericOverflow,
    /// A glyph run has no glyphs.
    EmptyGlyphRun,
    /// A font size is zero or negative.
    InvalidFontSize,
    /// A font's units-per-em value is zero.
    InvalidUnitsPerEm,
    /// Glyph outline segments do not form valid contours.
    InvalidOutline,
    /// A resource ceiling was reached.
    ResourceLimit,
    /// A fallible allocation failed.
    AllocationFailed,
}

/// Source-redacted text-resource error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TextError {
    code: TextErrorCode,
}

impl TextError {
    /// Creates one stable text-resource failure.
    pub const fn new(code: TextErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> TextErrorCode {
        self.code
    }
}

impl fmt::Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for TextError {}

/// Opaque stable identifier of a font selected by a font resolver.
///
/// The identifier is intentionally not a platform handle. A display-list
/// consumer supplies the resolver that maps it to embedded, bundled, or
/// system font data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FontId(u64);

impl FontId {
    /// Creates an application-defined stable font identifier.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the application-defined font identifier.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Identifier of one glyph in a [`FontId`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GlyphId(u32);

impl GlyphId {
    /// Creates a glyph identifier from the font's glyph index.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the font-local glyph index.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Signed Q26.6 coordinate used by a shaped glyph run.
///
/// Q26.6 is the common fixed-point output unit for font shaping. The run's
/// font size converts these values into the canvas coordinate system.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextUnit(i32);

impl TextUnit {
    /// Exact zero.
    pub const ZERO: Self = Self(0);

    /// Creates an exact whole-number text coordinate.
    pub fn from_i32(value: i32) -> Result<Self, TextError> {
        value
            .checked_shl(6)
            .map(Self)
            .ok_or(TextError::new(TextErrorCode::ResourceLimit))
    }

    /// Creates a text coordinate from exact Q26.6 storage.
    pub const fn from_bits(bits: i32) -> Self {
        Self(bits)
    }

    /// Returns the exact Q26.6 storage value.
    pub const fn bits(self) -> i32 {
        self.0
    }
}

/// One positioned glyph produced by a text shaper.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PositionedGlyph {
    glyph: GlyphId,
    x: TextUnit,
    y: TextUnit,
    advance_x: TextUnit,
    advance_y: TextUnit,
}

impl PositionedGlyph {
    /// Creates one positioned glyph and its pen advance.
    pub const fn new(
        glyph: GlyphId,
        x: TextUnit,
        y: TextUnit,
        advance_x: TextUnit,
        advance_y: TextUnit,
    ) -> Self {
        Self {
            glyph,
            x,
            y,
            advance_x,
            advance_y,
        }
    }

    /// Returns the font-local glyph index.
    pub const fn glyph(self) -> GlyphId {
        self.glyph
    }

    /// Returns the glyph's shaped horizontal position.
    pub const fn x(self) -> TextUnit {
        self.x
    }

    /// Returns the glyph's shaped vertical position.
    pub const fn y(self) -> TextUnit {
        self.y
    }

    /// Returns the shaped horizontal pen advance.
    pub const fn advance_x(self) -> TextUnit {
        self.advance_x
    }

    /// Returns the shaped vertical pen advance.
    pub const fn advance_y(self) -> TextUnit {
        self.advance_y
    }
}

/// Immutable shaped glyph run ready for a rendering backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlyphRun {
    font: FontId,
    /// The positive Q16.16 point size used by this run.
    font_size_bits: i32,
    units_per_em: u16,
    glyphs: Vec<PositionedGlyph>,
}

impl GlyphRun {
    /// Creates a non-empty run with a positive Q16.16 font size.
    pub fn new(
        font: FontId,
        font_size_bits: i32,
        units_per_em: u16,
        glyphs: Vec<PositionedGlyph>,
    ) -> Result<Self, TextError> {
        if font_size_bits <= 0 {
            return Err(TextError::new(TextErrorCode::InvalidFontSize));
        }
        if units_per_em == 0 {
            return Err(TextError::new(TextErrorCode::InvalidUnitsPerEm));
        }
        if glyphs.is_empty() {
            return Err(TextError::new(TextErrorCode::EmptyGlyphRun));
        }
        Ok(Self {
            font,
            font_size_bits,
            units_per_em,
            glyphs,
        })
    }

    /// Returns the font selected for this run.
    pub const fn font(&self) -> FontId {
        self.font
    }

    /// Returns the positive Q16.16 point size used by this run.
    pub const fn font_size_bits(&self) -> i32 {
        self.font_size_bits
    }

    /// Returns the font design-unit scale used by glyph positions and outlines.
    pub const fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Borrows shaped glyphs in visual drawing order.
    pub fn glyphs(&self) -> &[PositionedGlyph] {
        &self.glyphs
    }
}

/// One exact point in canvas-oriented font design coordinates.
///
/// Positive Y points downward, matching Skia canvas coordinates. A font parser
/// whose source coordinates point upward performs that inversion in its
/// adapter, not in a renderer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OutlinePoint {
    x: TextUnit,
    y: TextUnit,
}

impl OutlinePoint {
    /// Creates one exact outline point.
    pub const fn new(x: TextUnit, y: TextUnit) -> Self {
        Self { x, y }
    }

    /// Returns the horizontal design coordinate.
    pub const fn x(self) -> TextUnit {
        self.x
    }

    /// Returns the vertical design coordinate.
    pub const fn y(self) -> TextUnit {
        self.y
    }
}

/// One Bézier operation in a glyph outline.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OutlineSegment {
    /// Starts one contour.
    MoveTo(OutlinePoint),
    /// Adds a straight segment.
    LineTo(OutlinePoint),
    /// Adds a quadratic Bézier segment.
    QuadTo {
        /// Quadratic control point.
        control: OutlinePoint,
        /// Segment endpoint.
        end: OutlinePoint,
    },
    /// Adds a cubic Bézier segment.
    CubicTo {
        /// First cubic control point.
        first_control: OutlinePoint,
        /// Second cubic control point.
        second_control: OutlinePoint,
        /// Segment endpoint.
        end: OutlinePoint,
    },
    /// Closes the active contour.
    Close,
}

/// Immutable, validated outline for one font-local glyph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlyphOutline {
    font: FontId,
    glyph: GlyphId,
    segments: Vec<OutlineSegment>,
}

impl GlyphOutline {
    /// Creates an empty or fully closed glyph outline.
    pub fn new(
        font: FontId,
        glyph: GlyphId,
        segments: Vec<OutlineSegment>,
    ) -> Result<Self, TextError> {
        let mut contour_active = false;
        for segment in &segments {
            match segment {
                OutlineSegment::MoveTo(_) => {
                    if contour_active {
                        return Err(TextError::new(TextErrorCode::InvalidOutline));
                    }
                    contour_active = true;
                }
                OutlineSegment::LineTo(_)
                | OutlineSegment::QuadTo { .. }
                | OutlineSegment::CubicTo { .. } => {
                    if !contour_active {
                        return Err(TextError::new(TextErrorCode::InvalidOutline));
                    }
                }
                OutlineSegment::Close => {
                    if !contour_active {
                        return Err(TextError::new(TextErrorCode::InvalidOutline));
                    }
                    contour_active = false;
                }
            }
        }
        if contour_active {
            return Err(TextError::new(TextErrorCode::InvalidOutline));
        }
        Ok(Self {
            font,
            glyph,
            segments,
        })
    }

    /// Returns the selected font.
    pub const fn font(&self) -> FontId {
        self.font
    }

    /// Returns the font-local glyph index.
    pub const fn glyph(&self) -> GlyphId {
        self.glyph
    }

    /// Borrows closed outline segments in drawing order.
    pub fn segments(&self) -> &[OutlineSegment] {
        &self.segments
    }
}

/// Resolves font-local glyphs into portable Bézier outlines.
///
/// Implementations may use embedded font data, a bundled font collection, or
/// a system font service. Missing glyphs return `Ok(None)` so callers can use
/// deterministic fallback without treating ordinary fallback as an error.
pub trait GlyphOutlineProvider {
    /// Resolves a selected font-local glyph.
    fn glyph_outline(
        &self,
        font: FontId,
        glyph: GlyphId,
    ) -> Result<Option<GlyphOutline>, TextError>;
}
