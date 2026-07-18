use std::error::Error;
use std::fmt;

use pdf_rs_document::{
    DocumentError, DocumentErrorCode, FontResourceUnsupported, FormXObjectUnsupported,
    ImageXObjectUnsupported,
};

use crate::ContentOperatorSource;

/// Deterministic Content VM budget that rejected work or retained state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentVmLimitKind {
    /// Page-global operators admitted for interpretation.
    Operators,
    /// Deterministic VM work units.
    Fuel,
    /// Saved graphics-state nesting depth.
    GraphicsStateDepth,
    /// Active compatibility-section nesting depth.
    CompatibilityDepth,
    /// Active marked-content nesting depth.
    MarkedContentDepth,
    /// Marked-content property references retained by the interpreted result.
    PropertyUses,
    /// Allocator-reported capacity retained by VM-owned state.
    RetainedBytes,
    /// A fallible allocation failed inside an already validated bound.
    Allocation,
}

/// Deterministic graphics-profile budget that rejected path or dash state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentGraphicsLimitKind {
    /// Current-path construction segments.
    PathSegments,
    /// Allocator-reported current-path retained capacity.
    PathRetainedBytes,
    /// Entries in one line-dash array.
    DashEntries,
    /// Aggregate unique dash-array capacity retained by active graphics states.
    DashRetainedBytes,
}

/// Deterministic Image XObject budget that rejected aggregate work or cache state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentImageLimitKind {
    /// Executed `Do` operators retained by the interpreted result.
    ImageUses,
    /// Distinct proof-bound images acquired into the exact cache.
    UniqueImages,
    /// Aggregate decoded bytes copied into distinct Scene image resources.
    DecodedBytes,
    /// Operators structurally inspected by the one image-planning pass.
    PlanningOperators,
    /// Exact-cache key comparisons admitted during image planning.
    CacheProbes,
    /// Allocator-reported operator/proof planning capacity.
    PlanRetainedBytes,
    /// An operator/proof planning allocation failed inside an already validated bound.
    PlanAllocation,
    /// Allocator-reported exact-cache metadata capacity.
    CacheRetainedBytes,
    /// Calls admitted into the lower resumable Image XObject acquisition job.
    AcquisitionPolls,
    /// An exact-cache metadata allocation failed inside an already validated bound.
    CacheAllocation,
    /// A decoded Scene-resource copy allocation failed inside an already validated bound.
    DecodedAllocation,
}

/// Deterministic embedded-font and text-showing budget that rejected aggregate work or state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentFontLimitKind {
    /// Executed `Tf` selections retained by the semantic plan.
    FontUses,
    /// Distinct proof-bound embedded fonts acquired by the exact cache.
    UniqueFonts,
    /// Aggregate retained bytes of proof-bound acquired Font resources in the exact cache.
    ResourceRetainedBytes,
    /// Printable character codes expanded into positioned glyphs.
    Glyphs,
    /// Outline segments copied into project-owned Scene glyph resources.
    OutlineSegments,
    /// Allocator-reported positioned-glyph and outline capacity live before Scene handoff.
    GlyphRetainedBytes,
    /// Decoded printable string bytes retained by the immutable semantic plan.
    TextBytes,
    /// Numeric adjustments retained from `TJ` arrays.
    TextAdjustments,
    /// Operators inspected by the one font/text semantic-planning pass.
    PlanningOperators,
    /// Exact font-cache key comparisons.
    CacheProbes,
    /// Allocator-reported font/text plan capacity.
    PlanRetainedBytes,
    /// A font/text plan allocation failed inside its validated bound.
    PlanAllocation,
    /// Allocator-reported exact-font-cache metadata capacity.
    CacheRetainedBytes,
    /// An exact-font-cache allocation failed inside its validated bound.
    CacheAllocation,
    /// Calls admitted into lower resumable Font resource acquisition jobs.
    AcquisitionPolls,
    /// A glyph-outline or positioned-glyph allocation failed inside its retained-byte bound.
    GlyphAllocation,
}

/// Structured Content VM resource-limit context without content bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentVmLimit {
    kind: ContentVmLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ContentVmLimit {
    pub(crate) const fn new(
        kind: ContentVmLimitKind,
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

    /// Returns the rejected budget dimension.
    pub const fn kind(self) -> ContentVmLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount charged before the rejected work.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the additional amount or complete amount that was rejected.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Structured graphics-profile resource context without content bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentGraphicsLimit {
    kind: ContentGraphicsLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ContentGraphicsLimit {
    pub(crate) const fn new(
        kind: ContentGraphicsLimitKind,
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

    /// Returns the rejected graphics budget dimension.
    pub const fn kind(self) -> ContentGraphicsLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount charged before the rejected work.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the additional amount or complete amount that was rejected.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Structured Content Image XObject resource context without image or content bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentImageLimit {
    kind: ContentImageLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

/// Structured Content embedded-font resource context without font or content bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentFontLimit {
    kind: ContentFontLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ContentFontLimit {
    pub(crate) const fn new(
        kind: ContentFontLimitKind,
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

    /// Returns the rejected embedded-font budget dimension.
    pub const fn kind(self) -> ContentFontLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount committed before the rejected work.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the additional amount rejected.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

impl ContentImageLimit {
    pub(crate) const fn new(
        kind: ContentImageLimitKind,
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

    /// Returns the rejected Image XObject budget dimension.
    pub const fn kind(self) -> ContentImageLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount charged before the rejected work.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the additional amount or complete amount that was rejected.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentResourceLimit {
    Vm(ContentVmLimit),
    Graphics(ContentGraphicsLimit),
    Image(ContentImageLimit),
    Font(ContentFontLimit),
}

/// Stable machine-readable Content VM failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentVmErrorCode {
    /// Configured VM limits are zero or above fixed hard ceilings.
    InvalidLimits,
    /// A known operator has an invalid operand count.
    InvalidOperandCount,
    /// A known operator has an invalid operand type.
    InvalidOperandType,
    /// A PDF number lexeme has invalid syntax.
    InvalidNumber,
    /// A valid PDF number cannot be represented exactly at nine decimal places.
    NumericPrecision,
    /// Numeric conversion or checked VM arithmetic exceeded the signed fixed-point range.
    NumericOverflow,
    /// Graphics-state save/restore structure is invalid or unbalanced.
    InvalidGraphicsState,
    /// Text-object begin/end structure is invalid or unbalanced.
    InvalidTextObject,
    /// Compatibility-section begin/end structure is invalid or unbalanced.
    InvalidCompatibilityState,
    /// Marked-content begin/end structure is invalid or unbalanced.
    InvalidMarkedContentState,
    /// A registered graphics parameter is outside its admitted value domain.
    InvalidGraphicsParameter,
    /// Current-path construction or clipping sequencing is invalid.
    InvalidPathState,
    /// An operator is not admitted in the current structural context.
    InvalidOperatorContext,
    /// The supplied byte source no longer matches the acquired Page snapshot.
    SourceSnapshotMismatch,
    /// Cooperative cancellation was observed before atomic publication.
    Cancelled,
    /// A deterministic VM budget was exceeded.
    ResourceLimit,
    /// Checked internal VM state could not be maintained.
    InternalState,
}

/// Coarse Content VM failure policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentVmErrorCategory {
    /// Invalid caller-supplied VM configuration.
    Configuration,
    /// Malformed operator operands.
    Malformed,
    /// Invalid or unrepresentable numeric input.
    Numeric,
    /// Invalid operator-state sequencing.
    State,
    /// The immutable source generation changed before publication.
    Source,
    /// Cooperative cancellation.
    Cancellation,
    /// Deterministic resource exhaustion.
    Resource,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a Content VM failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentVmRecoverability {
    /// Correct the configured limits before retrying.
    CorrectConfiguration,
    /// Correct malformed content operands or operator sequencing.
    CorrectInput,
    /// Reopen the current source generation and reacquire the Page.
    ReopenSource,
    /// Retry only under a current generation if still useful.
    RetryCurrentGeneration,
    /// Reduce work or use an approved larger budget.
    ReduceWorkload,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Content-redacted structured Content VM error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentVmError {
    code: ContentVmErrorCode,
    category: ContentVmErrorCategory,
    recoverability: ContentVmRecoverability,
    diagnostic_id: &'static str,
    source: Option<ContentOperatorSource>,
    resource_limit: Option<ContentResourceLimit>,
}

impl ContentVmError {
    pub(crate) const fn new(
        code: ContentVmErrorCode,
        source: Option<ContentOperatorSource>,
    ) -> Self {
        let (category, recoverability, diagnostic_id) = error_policy(code);
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            source,
            resource_limit: None,
        }
    }

    pub(crate) const fn resource(
        limit: ContentVmLimit,
        source: Option<ContentOperatorSource>,
    ) -> Self {
        Self {
            code: ContentVmErrorCode::ResourceLimit,
            category: ContentVmErrorCategory::Resource,
            recoverability: ContentVmRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-CONTENT-VM-0012",
            source,
            resource_limit: Some(ContentResourceLimit::Vm(limit)),
        }
    }

    pub(crate) const fn graphics_resource(
        limit: ContentGraphicsLimit,
        source: Option<ContentOperatorSource>,
    ) -> Self {
        Self {
            code: ContentVmErrorCode::ResourceLimit,
            category: ContentVmErrorCategory::Resource,
            recoverability: ContentVmRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-CONTENT-VM-0012",
            source,
            resource_limit: Some(ContentResourceLimit::Graphics(limit)),
        }
    }

    pub(crate) const fn image_resource(
        limit: ContentImageLimit,
        source: Option<ContentOperatorSource>,
    ) -> Self {
        Self {
            code: ContentVmErrorCode::ResourceLimit,
            category: ContentVmErrorCategory::Resource,
            recoverability: ContentVmRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-CONTENT-VM-0012",
            source,
            resource_limit: Some(ContentResourceLimit::Image(limit)),
        }
    }

    pub(crate) const fn font_resource(
        limit: ContentFontLimit,
        source: Option<ContentOperatorSource>,
    ) -> Self {
        Self {
            code: ContentVmErrorCode::ResourceLimit,
            category: ContentVmErrorCategory::Resource,
            recoverability: ContentVmRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-CONTENT-VM-0012",
            source,
            resource_limit: Some(ContentResourceLimit::Font(limit)),
        }
    }

    pub(crate) const fn with_source(mut self, source: ContentOperatorSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Returns the stable machine-readable error code.
    pub const fn code(self) -> ContentVmErrorCode {
        self.code
    }

    /// Returns the coarse policy category.
    pub const fn category(self) -> ContentVmErrorCategory {
        self.category
    }

    /// Returns the stable recovery policy.
    pub const fn recoverability(self) -> ContentVmRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns exact operator-token provenance when the failure belongs to one operator.
    pub const fn source(self) -> Option<ContentOperatorSource> {
        self.source
    }

    /// Returns structured resource context for a budget failure.
    pub const fn limit(self) -> Option<ContentVmLimit> {
        match self.resource_limit {
            Some(ContentResourceLimit::Vm(limit)) => Some(limit),
            Some(
                ContentResourceLimit::Graphics(_)
                | ContentResourceLimit::Image(_)
                | ContentResourceLimit::Font(_),
            )
            | None => None,
        }
    }

    /// Returns structured graphics-profile resource context for a budget failure.
    pub const fn graphics_limit(self) -> Option<ContentGraphicsLimit> {
        match self.resource_limit {
            Some(ContentResourceLimit::Graphics(limit)) => Some(limit),
            Some(
                ContentResourceLimit::Vm(_)
                | ContentResourceLimit::Image(_)
                | ContentResourceLimit::Font(_),
            )
            | None => None,
        }
    }

    /// Returns Image XObject resource context when this is an image budget failure.
    pub const fn image_limit(self) -> Option<ContentImageLimit> {
        match self.resource_limit {
            Some(ContentResourceLimit::Image(limit)) => Some(limit),
            Some(
                ContentResourceLimit::Vm(_)
                | ContentResourceLimit::Graphics(_)
                | ContentResourceLimit::Font(_),
            )
            | None => None,
        }
    }

    /// Returns embedded-font resource context when this is a font/text budget failure.
    pub const fn font_limit(self) -> Option<ContentFontLimit> {
        match self.resource_limit {
            Some(ContentResourceLimit::Font(limit)) => Some(limit),
            Some(
                ContentResourceLimit::Vm(_)
                | ContentResourceLimit::Graphics(_)
                | ContentResourceLimit::Image(_),
            )
            | None => None,
        }
    }
}

const fn error_policy(
    code: ContentVmErrorCode,
) -> (
    ContentVmErrorCategory,
    ContentVmRecoverability,
    &'static str,
) {
    match code {
        ContentVmErrorCode::InvalidLimits => (
            ContentVmErrorCategory::Configuration,
            ContentVmRecoverability::CorrectConfiguration,
            "RPE-CONTENT-VM-0001",
        ),
        ContentVmErrorCode::InvalidOperandCount => (
            ContentVmErrorCategory::Malformed,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0002",
        ),
        ContentVmErrorCode::InvalidOperandType => (
            ContentVmErrorCategory::Malformed,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0003",
        ),
        ContentVmErrorCode::InvalidNumber => (
            ContentVmErrorCategory::Numeric,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0004",
        ),
        ContentVmErrorCode::NumericPrecision => (
            ContentVmErrorCategory::Numeric,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0005",
        ),
        ContentVmErrorCode::NumericOverflow => (
            ContentVmErrorCategory::Numeric,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0006",
        ),
        ContentVmErrorCode::InvalidGraphicsState => (
            ContentVmErrorCategory::State,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0007",
        ),
        ContentVmErrorCode::InvalidTextObject => (
            ContentVmErrorCategory::State,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0008",
        ),
        ContentVmErrorCode::InvalidCompatibilityState => (
            ContentVmErrorCategory::State,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0009",
        ),
        ContentVmErrorCode::InvalidMarkedContentState => (
            ContentVmErrorCategory::State,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0010",
        ),
        ContentVmErrorCode::InvalidGraphicsParameter => (
            ContentVmErrorCategory::Malformed,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0015",
        ),
        ContentVmErrorCode::InvalidPathState => (
            ContentVmErrorCategory::State,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0016",
        ),
        ContentVmErrorCode::InvalidOperatorContext => (
            ContentVmErrorCategory::State,
            ContentVmRecoverability::CorrectInput,
            "RPE-CONTENT-VM-0017",
        ),
        ContentVmErrorCode::SourceSnapshotMismatch => (
            ContentVmErrorCategory::Source,
            ContentVmRecoverability::ReopenSource,
            "RPE-CONTENT-VM-0014",
        ),
        ContentVmErrorCode::Cancelled => (
            ContentVmErrorCategory::Cancellation,
            ContentVmRecoverability::RetryCurrentGeneration,
            "RPE-CONTENT-VM-0011",
        ),
        ContentVmErrorCode::ResourceLimit => (
            ContentVmErrorCategory::Resource,
            ContentVmRecoverability::ReduceWorkload,
            "RPE-CONTENT-VM-0012",
        ),
        ContentVmErrorCode::InternalState => (
            ContentVmErrorCategory::Internal,
            ContentVmRecoverability::DoNotRetry,
            "RPE-CONTENT-VM-0013",
        ),
    }
}

impl fmt::Display for ContentVmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_id)
    }
}

impl Error for ContentVmError {}

/// Stable unsupported feature selected only after required operand validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentUnsupportedKind {
    /// A lexically valid operator is outside the sealed initial operator table.
    UnknownOperator,
    /// Marked-content point operator `MP` is outside the Scene-producing profile.
    MarkedContentPoint,
    /// Marked-content point operator `DP` is outside the Scene-producing profile.
    MarkedContentPointProperties,
    /// A `BDC` property operand is a direct content dictionary.
    DirectContentPropertyDictionary,
    /// The inherited Page `/Properties` dictionary is indirect.
    IndirectPageProperties,
    /// The selected Page property value is a direct dictionary.
    DirectPagePropertyDictionary,
    /// A registered graphics operator requires the explicit graphics-v2 profile.
    GraphicsV2Operator,
    /// A registered `gs` operator requires a proof-bound external graphics-state profile.
    ExtGStateProfileRequired,
    /// A selected external graphics-state name is absent from the proof-bound profile.
    ExtGStateResource,
    /// A registered `Do` operator requires an explicit proof-bound Content image profile.
    ImageProfileRequired,
    /// The selected Page XObject or Image XObject representation is outside the registered subset.
    ImageXObject,
    /// The selected Form XObject representation is outside the recursive registered subset.
    FormXObject,
    /// A registered text operator requires an explicit proof-bound Content font profile.
    FontProfileRequired,
    /// The selected Page Font or embedded program is outside the registered subset.
    FontResource,
    /// A decoded text string contains a byte outside printable WinAnsi ASCII.
    TextEncoding,
    /// A text rendering mode other than fill mode zero was selected.
    TextRenderMode,
}

/// Content-redacted structured unsupported outcome.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ContentUnsupported {
    kind: ContentUnsupportedKind,
    source: ContentOperatorSource,
    document_error: Option<DocumentError>,
    image_xobject: Option<ImageXObjectUnsupported>,
    form_xobject: Option<FormXObjectUnsupported>,
    font_resource: Option<FontResourceUnsupported>,
}

impl ContentUnsupported {
    pub(crate) const fn new(kind: ContentUnsupportedKind, source: ContentOperatorSource) -> Self {
        Self {
            kind,
            source,
            document_error: None,
            image_xobject: None,
            form_xobject: None,
            font_resource: None,
        }
    }

    pub(crate) fn from_document(
        error: DocumentError,
        source: ContentOperatorSource,
    ) -> Option<Self> {
        let kind = match error.code() {
            DocumentErrorCode::UnsupportedIndirectPageProperties => {
                ContentUnsupportedKind::IndirectPageProperties
            }
            DocumentErrorCode::UnsupportedDirectPagePropertyDictionary => {
                ContentUnsupportedKind::DirectPagePropertyDictionary
            }
            _ => return None,
        };
        Some(Self {
            kind,
            source,
            document_error: Some(error),
            image_xobject: None,
            form_xobject: None,
            font_resource: None,
        })
    }

    pub(crate) const fn from_image(
        unsupported: ImageXObjectUnsupported,
        source: ContentOperatorSource,
    ) -> Self {
        Self {
            kind: ContentUnsupportedKind::ImageXObject,
            source,
            document_error: None,
            image_xobject: Some(unsupported),
            form_xobject: None,
            font_resource: None,
        }
    }

    pub(crate) const fn from_form(
        unsupported: FormXObjectUnsupported,
        source: ContentOperatorSource,
    ) -> Self {
        Self {
            kind: ContentUnsupportedKind::FormXObject,
            source,
            document_error: None,
            image_xobject: None,
            form_xobject: Some(unsupported),
            font_resource: None,
        }
    }

    pub(crate) const fn from_font(
        unsupported: FontResourceUnsupported,
        source: ContentOperatorSource,
    ) -> Self {
        Self {
            kind: ContentUnsupportedKind::FontResource,
            source,
            document_error: None,
            image_xobject: None,
            form_xobject: None,
            font_resource: Some(unsupported),
        }
    }

    /// Returns the stable unsupported feature identity.
    pub const fn kind(self) -> ContentUnsupportedKind {
        self.kind
    }

    /// Returns the exact operator-token provenance.
    pub const fn source(self) -> ContentOperatorSource {
        self.source
    }

    /// Returns the preserved lower document error for an unsupported resource shape.
    pub const fn document_error(self) -> Option<DocumentError> {
        self.document_error
    }

    /// Returns the preserved lower Image XObject capability reason.
    pub const fn image_xobject(self) -> Option<ImageXObjectUnsupported> {
        self.image_xobject
    }

    /// Returns the preserved lower Form XObject capability reason.
    pub const fn form_xobject(self) -> Option<FormXObjectUnsupported> {
        self.form_xobject
    }

    /// Returns the preserved lower embedded-font capability reason.
    pub const fn font_resource(self) -> Option<FontResourceUnsupported> {
        self.font_resource
    }

    /// Returns the stable content-layer unsupported diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        match self.kind {
            ContentUnsupportedKind::UnknownOperator => "RPE-CONTENT-UNSUPPORTED-0001",
            ContentUnsupportedKind::MarkedContentPoint => "RPE-CONTENT-UNSUPPORTED-0002",
            ContentUnsupportedKind::MarkedContentPointProperties => "RPE-CONTENT-UNSUPPORTED-0003",
            ContentUnsupportedKind::DirectContentPropertyDictionary => {
                "RPE-CONTENT-UNSUPPORTED-0004"
            }
            ContentUnsupportedKind::IndirectPageProperties => "RPE-CONTENT-UNSUPPORTED-0005",
            ContentUnsupportedKind::DirectPagePropertyDictionary => "RPE-CONTENT-UNSUPPORTED-0006",
            ContentUnsupportedKind::GraphicsV2Operator => "RPE-CONTENT-UNSUPPORTED-0007",
            ContentUnsupportedKind::ExtGStateProfileRequired => "RPE-CONTENT-UNSUPPORTED-0014",
            ContentUnsupportedKind::ExtGStateResource => "RPE-CONTENT-UNSUPPORTED-0015",
            ContentUnsupportedKind::ImageProfileRequired => "RPE-CONTENT-UNSUPPORTED-0008",
            ContentUnsupportedKind::ImageXObject => "RPE-CONTENT-UNSUPPORTED-0009",
            ContentUnsupportedKind::FormXObject => "RPE-CONTENT-UNSUPPORTED-0016",
            ContentUnsupportedKind::FontProfileRequired => "RPE-CONTENT-UNSUPPORTED-0010",
            ContentUnsupportedKind::FontResource => "RPE-CONTENT-UNSUPPORTED-0011",
            ContentUnsupportedKind::TextEncoding => "RPE-CONTENT-UNSUPPORTED-0012",
            ContentUnsupportedKind::TextRenderMode => "RPE-CONTENT-UNSUPPORTED-0013",
        }
    }
}

impl fmt::Debug for ContentUnsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentUnsupported")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .field(
                "document_diagnostic_id",
                &self.document_error.map(DocumentError::diagnostic_id),
            )
            .field(
                "image_diagnostic_id",
                &self
                    .image_xobject
                    .map(ImageXObjectUnsupported::diagnostic_id),
            )
            .field(
                "form_diagnostic_id",
                &self.form_xobject.map(FormXObjectUnsupported::diagnostic_id),
            )
            .field(
                "font_diagnostic_id",
                &self
                    .font_resource
                    .map(FontResourceUnsupported::diagnostic_id),
            )
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ContentUnsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_id())
    }
}

impl Error for ContentUnsupported {}

/// Terminal lower-layer or VM failure preserved without lossy remapping.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ContentVmFailure {
    /// Ordered decoded-content scanning failed.
    Content(crate::ContentError),
    /// Page resource lookup or source validation failed.
    Document(DocumentError),
    /// Scene geometry, matrix arithmetic, or construction failed.
    Scene(pdf_rs_scene::SceneError),
    /// Content VM validation, state, cancellation, or budget failed.
    Vm(ContentVmError),
}

impl ContentVmFailure {
    /// Returns the exact stable diagnostic identifier of the preserved lower failure.
    pub const fn diagnostic_id(self) -> &'static str {
        match self {
            Self::Content(error) => error.diagnostic_id(),
            Self::Document(error) => error.diagnostic_id(),
            Self::Scene(error) => error.diagnostic_id(),
            Self::Vm(error) => error.diagnostic_id(),
        }
    }
}

impl fmt::Debug for ContentVmFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, diagnostic_id) = match self {
            Self::Content(error) => ("Content", error.diagnostic_id()),
            Self::Document(error) => ("Document", error.diagnostic_id()),
            Self::Scene(error) => ("Scene", error.diagnostic_id()),
            Self::Vm(error) => ("Vm", error.diagnostic_id()),
        };
        formatter
            .debug_struct("ContentVmFailure")
            .field("kind", &kind)
            .field("diagnostic_id", &diagnostic_id)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ContentVmFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Content(error) => fmt::Display::fmt(error, formatter),
            Self::Document(error) => fmt::Display::fmt(error, formatter),
            Self::Scene(error) => fmt::Display::fmt(error, formatter),
            Self::Vm(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl Error for ContentVmFailure {}
