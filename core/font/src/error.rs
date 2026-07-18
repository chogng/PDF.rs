use std::error::Error;
use std::fmt;

/// Deterministic font budget that rejected an operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FontLimitKind {
    /// Bytes in the complete caller-supplied sfnt program.
    InputBytes,
    /// Records in the sfnt table directory.
    Tables,
    /// Glyphs declared by `maxp`.
    Glyphs,
    /// Segments in the selected format 4 character map.
    CmapSegments,
    /// Bytes addressed by the `glyf`/`loca` pair.
    GlyphDataBytes,
    /// Bytes in one glyph description.
    GlyphBytes,
    /// Contours in one simple glyph.
    GlyphContours,
    /// Source contours across all simple glyphs.
    TotalContours,
    /// Points in one simple glyph.
    GlyphPoints,
    /// Source points across all simple glyphs.
    TotalPoints,
    /// Direct component records across all compound glyphs.
    Components,
    /// Recursive compound-glyph expansion depth.
    ComponentDepth,
    /// Project-owned outline segments after compound expansion.
    PathSegments,
    /// Allocator-visible bytes retained at peak or by the published font.
    RetainedBytes,
    /// Deterministic parser work units.
    Fuel,
    /// A fallible allocation failed within an already validated capacity bound.
    Allocation,
}

/// Structured resource-limit context without font-program bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontLimit {
    kind: FontLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl FontLimit {
    pub(crate) const fn new(
        kind: FontLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> Self {
        Self {
            kind,
            limit,
            consumed,
            attempted,
        }
    }

    /// Returns the budget dimension that was exceeded.
    pub const fn kind(self) -> FontLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the charged amount before the rejected work.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the amount that the rejected work would have charged.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable font parsing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontErrorCode {
    /// Configured limits are zero, inconsistent, or above fixed implementation ceilings.
    InvalidLimits,
    /// The supplied input does not contain a complete sfnt header and directory.
    InvalidRequest,
    /// An sfnt table record or required table body is truncated or structurally malformed.
    MalformedSfnt,
    /// A table required by the registered profile is absent.
    MissingRequiredTable,
    /// A required table appears more than once.
    DuplicateRequiredTable,
    /// Required table ranges overlap or violate their declared geometry.
    InvalidTableGeometry,
    /// The `head` table is malformed.
    InvalidHead,
    /// The `hhea` table is malformed.
    InvalidHhea,
    /// The `maxp` table is malformed.
    InvalidMaxp,
    /// The `loca` table is malformed.
    InvalidLoca,
    /// The `hmtx` table is malformed.
    InvalidHmtx,
    /// The selected Windows Unicode `cmap` format 4 subtable is malformed.
    InvalidCmap,
    /// A simple or compound `glyf` description is malformed.
    InvalidGlyph,
    /// A standalone CFF1 header, INDEX, DICT, or charset is malformed.
    InvalidCff,
    /// A Type 2 charstring is malformed.
    InvalidCharString,
    /// Recursive compound-glyph references contain a cycle.
    CompoundCycle,
    /// Checked coordinate, size, or accounting arithmetic overflowed.
    NumericOverflow,
    /// A deterministic font budget was exceeded.
    ResourceLimit,
    /// The owning runtime cancelled parsing.
    Cancelled,
    /// Internal checked state could not be maintained safely.
    InternalState,
}

/// Coarse font error policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontErrorCategory {
    /// Invalid caller configuration or request shape.
    Configuration,
    /// Malformed font-program bytes.
    Syntax,
    /// Deterministic resource exhaustion.
    Resource,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a font failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontRecoverability {
    /// Correct the configured profile before retrying.
    CorrectConfiguration,
    /// Correct the embedded font program.
    CorrectInput,
    /// Reduce work or select an approved larger budget.
    ReduceWorkload,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Source-redacted font failure with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontError {
    code: FontErrorCode,
    category: FontErrorCategory,
    recoverability: FontRecoverability,
    diagnostic_id: &'static str,
    limit: Option<FontLimit>,
    glyph_id: Option<u16>,
}

impl FontError {
    pub(crate) const fn for_code(code: FontErrorCode, glyph_id: Option<u16>) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            FontErrorCode::InvalidLimits => (
                FontErrorCategory::Configuration,
                FontRecoverability::CorrectConfiguration,
                "RPE-FONT-0001",
            ),
            FontErrorCode::InvalidRequest => (
                FontErrorCategory::Configuration,
                FontRecoverability::CorrectConfiguration,
                "RPE-FONT-0002",
            ),
            FontErrorCode::MalformedSfnt => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0003",
            ),
            FontErrorCode::MissingRequiredTable => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0004",
            ),
            FontErrorCode::DuplicateRequiredTable => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0005",
            ),
            FontErrorCode::InvalidTableGeometry => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0006",
            ),
            FontErrorCode::InvalidHead => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0007",
            ),
            FontErrorCode::InvalidHhea => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0008",
            ),
            FontErrorCode::InvalidMaxp => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0009",
            ),
            FontErrorCode::InvalidLoca => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0010",
            ),
            FontErrorCode::InvalidHmtx => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0011",
            ),
            FontErrorCode::InvalidCmap => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0012",
            ),
            FontErrorCode::InvalidGlyph => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0013",
            ),
            FontErrorCode::InvalidCff => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0019",
            ),
            FontErrorCode::InvalidCharString => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0020",
            ),
            FontErrorCode::CompoundCycle => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0014",
            ),
            FontErrorCode::NumericOverflow => (
                FontErrorCategory::Syntax,
                FontRecoverability::CorrectInput,
                "RPE-FONT-0015",
            ),
            FontErrorCode::ResourceLimit => (
                FontErrorCategory::Resource,
                FontRecoverability::ReduceWorkload,
                "RPE-FONT-0016",
            ),
            FontErrorCode::Cancelled => (
                FontErrorCategory::Cancellation,
                FontRecoverability::AbandonOperation,
                "RPE-FONT-0017",
            ),
            FontErrorCode::InternalState => (
                FontErrorCategory::Internal,
                FontRecoverability::DoNotRetry,
                "RPE-FONT-0018",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            limit: None,
            glyph_id,
        }
    }

    pub(crate) const fn resource(limit: FontLimit) -> Self {
        let mut error = Self::for_code(FontErrorCode::ResourceLimit, None);
        error.limit = Some(limit);
        error
    }

    pub(crate) const fn resource_for_glyph(limit: FontLimit, glyph_id: u16) -> Self {
        let mut error = Self::for_code(FontErrorCode::ResourceLimit, Some(glyph_id));
        error.limit = Some(limit);
        error
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> FontErrorCode {
        self.code
    }

    /// Returns the coarse policy category.
    pub const fn category(self) -> FontErrorCategory {
        self.category
    }

    /// Returns the stable recovery policy.
    pub const fn recoverability(self) -> FontRecoverability {
        self.recoverability
    }

    /// Returns the source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns resource-limit context when the failure exhausted a budget.
    pub const fn limit(self) -> Option<FontLimit> {
        self.limit
    }

    /// Returns the implicated glyph identifier when one is known.
    pub const fn glyph_id(self) -> Option<u16> {
        self.glyph_id
    }
}

impl fmt::Display for FontError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "font parsing failed ({})", self.diagnostic_id)
    }
}

impl Error for FontError {}

/// Registered reason why a font cannot be consumed by the foundational profile.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FontUnsupportedKind {
    /// The sfnt scaler type is not TrueType outlines (`0x00010000`).
    SfntFlavor,
    /// The `maxp` version is not the TrueType outlines version 1.0.
    MaxpVersion,
    /// No Windows Unicode BMP (`platform 3, encoding 1`) character map is present.
    CmapPlatform,
    /// The selected Windows Unicode character map is not format 4.
    CmapFormat,
    /// A compound glyph attaches components by point number rather than XY translation.
    CompoundPointAttachment,
    /// A compound glyph requests scaling, anisotropic scaling, or a two-by-two transform.
    CompoundTransform,
    /// A parser profile was passed to the wrong font-program parser.
    ProfileMismatch,
    /// A CFF CID-keyed font is outside the foundational Type1C profile.
    CffCidFont,
    /// A CFF ExpertEncoding or custom Encoding is outside the foundational profile.
    CffEncoding,
    /// A non-default CFF FontMatrix is outside the foundational profile.
    CffFontMatrix,
    /// A Type 2 escaped operator is outside the foundational profile.
    CffCharStringOperator,
}

/// Typed capability outcome that contains no font-program bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontUnsupported {
    kind: FontUnsupportedKind,
    glyph_id: Option<u16>,
}

impl FontUnsupported {
    pub(crate) const fn new(kind: FontUnsupportedKind, glyph_id: Option<u16>) -> Self {
        Self { kind, glyph_id }
    }

    /// Returns the registered unsupported capability kind.
    pub const fn kind(self) -> FontUnsupportedKind {
        self.kind
    }

    /// Returns the implicated glyph identifier when one is known.
    pub const fn glyph_id(self) -> Option<u16> {
        self.glyph_id
    }

    /// Returns a stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            FontUnsupportedKind::SfntFlavor => "RPE-FONT-UNSUPPORTED-0001",
            FontUnsupportedKind::MaxpVersion => "RPE-FONT-UNSUPPORTED-0002",
            FontUnsupportedKind::CmapPlatform => "RPE-FONT-UNSUPPORTED-0004",
            FontUnsupportedKind::CmapFormat => "RPE-FONT-UNSUPPORTED-0005",
            FontUnsupportedKind::CompoundPointAttachment => "RPE-FONT-UNSUPPORTED-0006",
            FontUnsupportedKind::CompoundTransform => "RPE-FONT-UNSUPPORTED-0007",
            FontUnsupportedKind::ProfileMismatch => "RPE-FONT-UNSUPPORTED-0008",
            FontUnsupportedKind::CffCidFont => "RPE-FONT-UNSUPPORTED-0009",
            FontUnsupportedKind::CffEncoding => "RPE-FONT-UNSUPPORTED-0010",
            FontUnsupportedKind::CffFontMatrix => "RPE-FONT-UNSUPPORTED-0011",
            FontUnsupportedKind::CffCharStringOperator => "RPE-FONT-UNSUPPORTED-0012",
        }
    }
}
