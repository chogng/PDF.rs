use std::sync::Arc;

use crate::{FontError, FontLimits, FontUnsupported};

/// Registered foundational font profile.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FontProfile {
    /// TrueType outlines, Windows Unicode format 4, and printable WinAnsi ASCII mapping.
    #[default]
    SimpleTrueTypeWinAnsiAsciiV1,
    /// TrueType outlines, Windows Unicode format 4, and the complete PDF WinAnsi mapping.
    SimpleTrueTypeWinAnsiV1,
    /// Standalone CFF1 Type 2 outlines with the standard non-CID encoding model.
    SimpleType1CStandardV1,
}

impl FontProfile {
    /// Returns the stable registered profile identifier.
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::SimpleTrueTypeWinAnsiAsciiV1 => "m3.simple-truetype-winansi-ascii.v1",
            Self::SimpleTrueTypeWinAnsiV1 => "m4.simple-truetype-winansi.v1",
            Self::SimpleType1CStandardV1 => "m4.simple-type1c-standard.v1",
        }
    }
}

/// Cooperative cancellation authority for pure font parsing.
pub trait FontCancellation: Send + Sync {
    /// Returns whether the owning operation has been cancelled.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation authority that never cancels.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelled;

impl FontCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Stable glyph identifier within one TrueType font.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GlyphId(u16);

impl GlyphId {
    /// Creates an identifier from its exact TrueType glyph index.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }
    /// Returns the exact TrueType glyph index.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Exact coordinate measured in half font units.
///
/// TrueType on-curve and off-curve points use integral font units. Implied on-curve midpoints can
/// fall on half units, so this representation retains those midpoints without floating point.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FontCoordinate(i32);

impl FontCoordinate {
    /// Creates a coordinate from an exact number of half font units.
    pub const fn from_half_units(value: i32) -> Self {
        Self(value)
    }
    /// Returns the exact number of half font units.
    pub const fn half_units(self) -> i32 {
        self.0
    }
}

/// One exact point in a glyph's coordinate space.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FontPoint {
    x: FontCoordinate,
    y: FontCoordinate,
}

impl FontPoint {
    /// Creates a point from exact half-font-unit coordinates.
    pub const fn new(x: FontCoordinate, y: FontCoordinate) -> Self {
        Self { x, y }
    }
    /// Returns the horizontal coordinate.
    pub const fn x(self) -> FontCoordinate {
        self.x
    }
    /// Returns the vertical coordinate.
    pub const fn y(self) -> FontCoordinate {
        self.y
    }
}

/// Integral TrueType glyph bounds in font units.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FontBounds {
    x_min: i16,
    y_min: i16,
    x_max: i16,
    y_max: i16,
}

impl FontBounds {
    pub(crate) const fn new(x_min: i16, y_min: i16, x_max: i16, y_max: i16) -> Self {
        Self {
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }
    /// Returns the minimum horizontal font-unit coordinate.
    pub const fn x_min(self) -> i16 {
        self.x_min
    }
    /// Returns the minimum vertical font-unit coordinate.
    pub const fn y_min(self) -> i16 {
        self.y_min
    }
    /// Returns the maximum horizontal font-unit coordinate.
    pub const fn x_max(self) -> i16 {
        self.x_max
    }
    /// Returns the maximum vertical font-unit coordinate.
    pub const fn y_max(self) -> i16 {
        self.y_max
    }
}

/// One project-owned exact glyph outline segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutlineSegment {
    /// Starts one contour.
    MoveTo(FontPoint),
    /// Appends one straight segment.
    LineTo(FontPoint),
    /// Appends one quadratic Bézier segment.
    QuadTo {
        /// Exact quadratic control point.
        control: FontPoint,
        /// Exact segment endpoint.
        end: FontPoint,
    },
    /// Appends one cubic Bézier segment.
    CubicTo {
        /// Exact first cubic control point.
        control_1: FontPoint,
        /// Exact second cubic control point.
        control_2: FontPoint,
        /// Exact segment endpoint.
        end: FontPoint,
    },
    /// Closes the active contour.
    CloseContour,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GlyphRecord {
    pub(crate) advance_width: u16,
    pub(crate) bounds: Option<FontBounds>,
    pub(crate) segment_start: u32,
    pub(crate) segment_len: u32,
}

/// Borrowed immutable outline and metric for one glyph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlyphOutline<'a> {
    glyph_id: GlyphId,
    advance_width: u16,
    bounds: Option<FontBounds>,
    segments: &'a [OutlineSegment],
}

impl<'a> GlyphOutline<'a> {
    /// Returns the glyph identifier.
    pub const fn glyph_id(self) -> GlyphId {
        self.glyph_id
    }
    /// Returns the unscaled horizontal advance in font units.
    pub const fn advance_width(self) -> u16 {
        self.advance_width
    }
    /// Returns declared integral glyph bounds, or `None` for an empty description.
    pub const fn bounds(self) -> Option<FontBounds> {
        self.bounds
    }
    /// Borrows exact project-owned outline segments.
    pub const fn segments(self) -> &'a [OutlineSegment] {
        self.segments
    }
}

/// Deterministic parsing work and retained-state accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FontStats {
    pub(crate) input_bytes: u64,
    pub(crate) tables_visited: u64,
    pub(crate) glyphs: u64,
    pub(crate) cmap_segments: u64,
    pub(crate) glyph_data_bytes: u64,
    pub(crate) source_contours: u64,
    pub(crate) source_points: u64,
    pub(crate) components: u64,
    pub(crate) path_segments: u64,
    pub(crate) fuel: u64,
    pub(crate) retained_bytes: u64,
    pub(crate) peak_retained_bytes: u64,
}

impl FontStats {
    /// Returns exact input bytes supplied to the parser.
    pub const fn input_bytes(self) -> u64 {
        self.input_bytes
    }
    /// Returns sfnt table records inspected.
    pub const fn tables_visited(self) -> u64 {
        self.tables_visited
    }
    /// Returns glyph descriptions measured for publication.
    pub const fn glyphs(self) -> u64 {
        self.glyphs
    }
    /// Returns selected format 4 character-map segments.
    pub const fn cmap_segments(self) -> u64 {
        self.cmap_segments
    }
    /// Returns bytes addressed by the validated `glyf`/`loca` pair.
    pub const fn glyph_data_bytes(self) -> u64 {
        self.glyph_data_bytes
    }
    /// Returns source contours across simple glyph descriptions.
    pub const fn source_contours(self) -> u64 {
        self.source_contours
    }
    /// Returns source points across simple glyph descriptions.
    pub const fn source_points(self) -> u64 {
        self.source_points
    }
    /// Returns direct component records across compound glyph descriptions.
    pub const fn components(self) -> u64 {
        self.components
    }
    /// Returns published outline segments after compound expansion.
    pub const fn path_segments(self) -> u64 {
        self.path_segments
    }
    /// Returns deterministic parser work units consumed.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }
    /// Returns allocator-visible bytes retained by a published font.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }
    /// Returns peak allocator-visible bytes retained during parsing.
    pub const fn peak_retained_bytes(self) -> u64 {
        self.peak_retained_bytes
    }
}

/// Immutable parsed TrueType font under one registered profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrueTypeFont {
    pub(crate) profile: FontProfile,
    pub(crate) limits: FontLimits,
    pub(crate) stats: FontStats,
    pub(crate) units_per_em: u16,
    pub(crate) winansi_glyphs: [GlyphId; 224],
    pub(crate) glyphs: Arc<Vec<GlyphRecord>>,
    pub(crate) segments: Arc<Vec<OutlineSegment>>,
}

impl TrueTypeFont {
    /// Returns the registered parser profile.
    pub const fn profile(&self) -> FontProfile {
        self.profile
    }
    /// Returns the validated budget profile.
    pub const fn limits(&self) -> FontLimits {
        self.limits
    }
    /// Returns deterministic committed parsing statistics.
    pub const fn stats(&self) -> FontStats {
        self.stats
    }
    /// Returns the TrueType units-per-em scaling denominator.
    pub const fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
    /// Returns the exact number of addressable glyphs.
    pub fn glyph_count(&self) -> u16 {
        self.glyphs.len() as u16
    }

    /// Maps one byte admitted by the selected WinAnsi profile to its TrueType glyph.
    ///
    /// Both profiles reject control bytes below `0x20`; the legacy ASCII profile also rejects
    /// bytes above `0x7e`. A missing format 4 mapping follows TrueType semantics and returns glyph
    /// zero.
    pub fn glyph_id_for_winansi(&self, byte: u8) -> Option<GlyphId> {
        if self.profile == FontProfile::SimpleTrueTypeWinAnsiAsciiV1 && byte > 0x7e {
            return None;
        }
        let index = byte.checked_sub(0x20)?;
        self.winansi_glyphs.get(usize::from(index)).copied()
    }

    /// Returns the unscaled horizontal advance for one valid glyph identifier.
    pub fn advance_width(&self, glyph_id: GlyphId) -> Option<u16> {
        self.glyphs
            .get(usize::from(glyph_id.get()))
            .map(|glyph| glyph.advance_width)
    }

    /// Borrows one immutable project-owned glyph outline.
    pub fn glyph_outline(&self, glyph_id: GlyphId) -> Option<GlyphOutline<'_>> {
        let glyph = self.glyphs.get(usize::from(glyph_id.get()))?;
        let start = usize::try_from(glyph.segment_start).ok()?;
        let len = usize::try_from(glyph.segment_len).ok()?;
        let end = start.checked_add(len)?;
        let segments = self.segments.get(start..end)?;
        Some(GlyphOutline {
            glyph_id,
            advance_width: glyph.advance_width,
            bounds: glyph.bounds,
            segments,
        })
    }
}

/// Immutable parsed standalone CFF1 font under the registered Type1C profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CffFont {
    pub(crate) profile: FontProfile,
    pub(crate) limits: FontLimits,
    pub(crate) stats: FontStats,
    pub(crate) units_per_em: u16,
    pub(crate) glyph_names: Arc<Vec<Option<Box<str>>>>,
    pub(crate) glyphs: Arc<Vec<GlyphRecord>>,
    pub(crate) segments: Arc<Vec<OutlineSegment>>,
}

impl CffFont {
    /// Returns the registered parser profile.
    pub const fn profile(&self) -> FontProfile {
        self.profile
    }
    /// Returns the validated budget profile.
    pub const fn limits(&self) -> FontLimits {
        self.limits
    }
    /// Returns deterministic committed parsing statistics.
    pub const fn stats(&self) -> FontStats {
        self.stats
    }
    /// Returns the CFF coordinate scaling denominator.
    pub const fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
    /// Returns the exact number of addressable glyphs.
    pub fn glyph_count(&self) -> u16 {
        self.glyphs.len() as u16
    }
    /// Resolves one CFF charset glyph name.
    pub fn glyph_id_for_name(&self, name: &str) -> Option<GlyphId> {
        self.glyph_names
            .iter()
            .position(|candidate| candidate.as_deref() == Some(name))
            .and_then(|index| u16::try_from(index).ok())
            .map(GlyphId::new)
    }
    /// Resolves one byte through CFF StandardEncoding.
    pub fn glyph_id_for_standard_code(&self, byte: u8) -> Option<GlyphId> {
        self.glyph_id_for_name(standard_encoding_name(byte)?)
    }
    /// Returns the unscaled horizontal advance for one valid glyph identifier.
    pub fn advance_width(&self, glyph_id: GlyphId) -> Option<u16> {
        self.glyphs
            .get(usize::from(glyph_id.get()))
            .map(|glyph| glyph.advance_width)
    }
    /// Borrows one immutable project-owned glyph outline.
    pub fn glyph_outline(&self, glyph_id: GlyphId) -> Option<GlyphOutline<'_>> {
        let glyph = self.glyphs.get(usize::from(glyph_id.get()))?;
        let start = usize::try_from(glyph.segment_start).ok()?;
        let len = usize::try_from(glyph.segment_len).ok()?;
        let end = start.checked_add(len)?;
        Some(GlyphOutline {
            glyph_id,
            advance_width: glyph.advance_width,
            bounds: glyph.bounds,
            segments: self.segments.get(start..end)?,
        })
    }
}

/// One immutable project-owned embedded font program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FontProgram {
    /// A TrueType `glyf` program.
    TrueType(Box<TrueTypeFont>),
    /// A standalone CFF1 Type 2 program embedded as Type1C.
    Type1C(Box<CffFont>),
}

impl FontProgram {
    /// Returns the registered parser profile.
    pub const fn profile(&self) -> FontProfile {
        match self {
            Self::TrueType(font) => font.profile(),
            Self::Type1C(font) => font.profile(),
        }
    }
    /// Returns the validated budget profile.
    pub const fn limits(&self) -> FontLimits {
        match self {
            Self::TrueType(font) => font.limits(),
            Self::Type1C(font) => font.limits(),
        }
    }
    /// Returns deterministic committed parsing statistics.
    pub const fn stats(&self) -> FontStats {
        match self {
            Self::TrueType(font) => font.stats(),
            Self::Type1C(font) => font.stats(),
        }
    }
    /// Returns the font coordinate scaling denominator.
    pub const fn units_per_em(&self) -> u16 {
        match self {
            Self::TrueType(font) => font.units_per_em(),
            Self::Type1C(font) => font.units_per_em(),
        }
    }
    /// Returns the exact number of addressable glyphs.
    pub fn glyph_count(&self) -> u16 {
        match self {
            Self::TrueType(font) => font.glyph_count(),
            Self::Type1C(font) => font.glyph_count(),
        }
    }
    /// Maps one byte through the program's simple encoding.
    pub fn glyph_id_for_winansi(&self, byte: u8) -> Option<GlyphId> {
        match self {
            Self::TrueType(font) => font.glyph_id_for_winansi(byte),
            Self::Type1C(font) => font.glyph_id_for_standard_code(byte),
        }
    }
    /// Returns the unscaled horizontal advance for one glyph.
    pub fn advance_width(&self, glyph_id: GlyphId) -> Option<u16> {
        match self {
            Self::TrueType(font) => font.advance_width(glyph_id),
            Self::Type1C(font) => font.advance_width(glyph_id),
        }
    }
    /// Borrows one immutable project-owned glyph outline.
    pub fn glyph_outline(&self, glyph_id: GlyphId) -> Option<GlyphOutline<'_>> {
        match self {
            Self::TrueType(font) => font.glyph_outline(glyph_id),
            Self::Type1C(font) => font.glyph_outline(glyph_id),
        }
    }
}

impl From<TrueTypeFont> for FontProgram {
    fn from(font: TrueTypeFont) -> Self {
        Self::TrueType(Box::new(font))
    }
}

impl From<CffFont> for FontProgram {
    fn from(font: CffFont) -> Self {
        Self::Type1C(Box::new(font))
    }
}

fn standard_encoding_name(byte: u8) -> Option<&'static str> {
    let sid = u16::from(byte.checked_sub(0x20)?) + 1;
    standard_name_for_sid(sid)
}

pub(crate) fn standard_name_for_sid(sid: u16) -> Option<&'static str> {
    const NAMES: [&str; 95] = [
        "space",
        "exclam",
        "quotedbl",
        "numbersign",
        "dollar",
        "percent",
        "ampersand",
        "quoteright",
        "parenleft",
        "parenright",
        "asterisk",
        "plus",
        "comma",
        "hyphen",
        "period",
        "slash",
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "colon",
        "semicolon",
        "less",
        "equal",
        "greater",
        "question",
        "at",
        "A",
        "B",
        "C",
        "D",
        "E",
        "F",
        "G",
        "H",
        "I",
        "J",
        "K",
        "L",
        "M",
        "N",
        "O",
        "P",
        "Q",
        "R",
        "S",
        "T",
        "U",
        "V",
        "W",
        "X",
        "Y",
        "Z",
        "bracketleft",
        "backslash",
        "bracketright",
        "asciicircum",
        "underscore",
        "quoteleft",
        "a",
        "b",
        "c",
        "d",
        "e",
        "f",
        "g",
        "h",
        "i",
        "j",
        "k",
        "l",
        "m",
        "n",
        "o",
        "p",
        "q",
        "r",
        "s",
        "t",
        "u",
        "v",
        "w",
        "x",
        "y",
        "z",
        "braceleft",
        "bar",
        "braceright",
        "asciitilde",
    ];
    match sid {
        0 => Some(".notdef"),
        1..=95 => NAMES.get(usize::from(sid - 1)).copied(),
        200 => Some("aacute"),
        228 => Some("zcaron"),
        _ => None,
    }
}

/// Terminal outcome of one atomic CFF1 parse attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CffParseOutcome {
    /// A complete immutable CFF font was published.
    Ready(CffFont),
    /// Valid input selected a capability outside the registered profile.
    Unsupported(FontUnsupported),
    /// Malformed input, configuration, or resource exhaustion prevented publication.
    Failed(FontError),
    /// Cooperative cancellation prevented publication.
    Cancelled(FontError),
}

/// Terminal CFF parse outcome paired with deterministic work statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CffParseReport {
    pub(crate) outcome: CffParseOutcome,
    pub(crate) stats: FontStats,
}

impl CffParseReport {
    /// Borrows the terminal outcome.
    pub const fn outcome(&self) -> &CffParseOutcome {
        &self.outcome
    }
    /// Returns deterministic statistics up to the terminal outcome.
    pub const fn stats(&self) -> FontStats {
        self.stats
    }
    /// Consumes the report and returns its terminal outcome.
    pub fn into_outcome(self) -> CffParseOutcome {
        self.outcome
    }
}

/// Terminal outcome of one atomic parse attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum FontParseOutcome {
    /// A complete immutable font was published.
    Ready(TrueTypeFont),
    /// Valid input selected a capability outside the registered profile.
    Unsupported(FontUnsupported),
    /// Malformed input, configuration, or resource exhaustion prevented publication.
    Failed(FontError),
    /// Cooperative cancellation prevented publication.
    Cancelled(FontError),
}

/// Terminal parse outcome paired with deterministic work statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FontParseReport {
    pub(crate) outcome: FontParseOutcome,
    pub(crate) stats: FontStats,
}

impl FontParseReport {
    /// Borrows the terminal outcome.
    pub const fn outcome(&self) -> &FontParseOutcome {
        &self.outcome
    }
    /// Returns deterministic statistics up to the terminal outcome.
    pub const fn stats(&self) -> FontStats {
        self.stats
    }
    /// Consumes the report and returns its terminal outcome.
    pub fn into_outcome(self) -> FontParseOutcome {
        self.outcome
    }
}
