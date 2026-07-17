use std::error::Error;
use std::fmt;

use pdf_rs_scene::{
    CapabilityContext, CapabilityRequirement, CapabilityStatus, GraphicsCapability, GraphicsCommand,
};

/// Content-redacted graphics command family used by renderer capability decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceGraphicsCommandKind {
    /// Graphics-state save.
    Save,
    /// Graphics-state restore.
    Restore,
    /// Clip intersection.
    Clip,
    /// Path fill.
    Fill,
    /// Path stroke.
    Stroke,
    /// Fill followed by stroke.
    FillStroke,
    /// Basic image paint.
    DrawImage,
    /// Embedded glyph-run paint.
    DrawGlyphRun,
    /// Isolated transparency-group begin.
    BeginIsolatedGroup,
    /// Isolated transparency-group end.
    EndIsolatedGroup,
}

impl From<&GraphicsCommand> for ReferenceGraphicsCommandKind {
    fn from(command: &GraphicsCommand) -> Self {
        match command {
            GraphicsCommand::Save => Self::Save,
            GraphicsCommand::Restore => Self::Restore,
            GraphicsCommand::Clip { .. } => Self::Clip,
            GraphicsCommand::Fill { .. } => Self::Fill,
            GraphicsCommand::Stroke { .. } => Self::Stroke,
            GraphicsCommand::FillStroke { .. } => Self::FillStroke,
            GraphicsCommand::DrawImage { .. } => Self::DrawImage,
            GraphicsCommand::DrawGlyphRun(_) => Self::DrawGlyphRun,
            GraphicsCommand::BeginIsolatedGroup { .. } => Self::BeginIsolatedGroup,
            GraphicsCommand::EndIsolatedGroup => Self::EndIsolatedGroup,
        }
    }
}

/// Visible Scene capability outside the current non-painting Reference profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceRenderUnsupportedKind {
    /// One declared visible graphics requirement is not implemented by this renderer.
    VisibleGraphicsRequirement,
    /// One visible graphics command is not implemented by this renderer.
    VisibleGraphicsCommand,
}

/// Content-redacted structured unsupported Reference-rendering outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRenderUnsupported {
    kind: ReferenceRenderUnsupportedKind,
    index: u32,
    requirement_id: Option<u32>,
    capability: Option<GraphicsCapability>,
    parameter: Option<u64>,
    context: Option<CapabilityContext>,
    producer_status: Option<CapabilityStatus>,
    command: Option<ReferenceGraphicsCommandKind>,
    diagnostic_id: &'static str,
}

impl ReferenceRenderUnsupported {
    pub(crate) fn requirement(index: u32, requirement: &CapabilityRequirement) -> Self {
        Self {
            kind: ReferenceRenderUnsupportedKind::VisibleGraphicsRequirement,
            index,
            requirement_id: Some(requirement.id().value()),
            capability: Some(requirement.capability()),
            parameter: Some(requirement.parameter()),
            context: Some(requirement.context()),
            producer_status: Some(requirement.status()),
            command: None,
            diagnostic_id: "RPE-RASTER-0007",
        }
    }

    pub(crate) fn command(index: u32, command: &GraphicsCommand) -> Self {
        Self {
            kind: ReferenceRenderUnsupportedKind::VisibleGraphicsCommand,
            index,
            requirement_id: None,
            capability: None,
            parameter: None,
            context: None,
            producer_status: None,
            command: Some(command.into()),
            diagnostic_id: "RPE-RASTER-0008",
        }
    }

    /// Returns the unsupported visible Scene surface.
    pub const fn kind(self) -> ReferenceRenderUnsupportedKind {
        self.kind
    }

    /// Returns the zero-based requirement or command index.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the canonical requirement identifier when a graph node was rejected.
    pub const fn requirement_id(self) -> Option<u32> {
        self.requirement_id
    }

    /// Returns the exact rejected capability.
    pub const fn capability(self) -> Option<GraphicsCapability> {
        self.capability
    }

    /// Returns the exact capability-specific parameter.
    pub const fn parameter(self) -> Option<u64> {
        self.parameter
    }

    /// Returns the exact Scene context for a rejected requirement.
    pub const fn context(self) -> Option<CapabilityContext> {
        self.context
    }

    /// Returns the producing-profile status that was independently evaluated.
    pub const fn producer_status(self) -> Option<CapabilityStatus> {
        self.producer_status
    }

    /// Returns the rejected graphics command family.
    pub const fn command_kind(self) -> Option<ReferenceGraphicsCommandKind> {
        self.command
    }

    /// Returns the stable content-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }
}

/// Deterministic Reference raster budget that rejected work or publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceRenderLimitKind {
    /// Output width in device pixels.
    Width,
    /// Output height in device pixels.
    Height,
    /// Total device pixel count.
    Pixels,
    /// Bytes in one top-down RGBA row.
    StrideBytes,
    /// Semantic RGBA bytes in the complete output.
    OutputBytes,
    /// Scene commands traversed by the Reference profile.
    Commands,
    /// Scene graphics resources admitted by the Reference profile.
    Resources,
    /// Scene capability requirements traversed before dispatch.
    Requirements,
    /// Scene capability dependency edges traversed before dispatch.
    Dependencies,
    /// Flattened path and glyph segments.
    GeometrySegments,
    /// Fill and glyph edges.
    GeometryEdges,
    /// Scalar geometry coverage samples.
    GeometrySamples,
    /// One live coverage-mask allocation.
    CoverageBytes,
    /// Generated dash chunks.
    DashChunks,
    /// Generated stroke runs.
    StrokeRuns,
    /// Generated stroke primitives.
    StrokePrimitives,
    /// Live transient flattened and stroke geometry.
    GeometryBytes,
    /// Saved graphics clip depth.
    ClipDepth,
    /// Live current and saved clip masks.
    ClipBytes,
    /// Decoded source image pixels.
    ImageSourcePixels,
    /// Decoded image row stride.
    ImageStrideBytes,
    /// Decoded image bytes.
    ImageDecodedBytes,
    /// Image sample positions.
    ImageSamples,
    /// Sampled image color conversions.
    ImageConversions,
    /// Positioned glyphs.
    Glyphs,
    /// Glyph resource lookups.
    GlyphResourceLookups,
    /// Source glyph outline segments.
    GlyphOutlineSegments,
    /// Glyph coverage samples.
    GlyphSamples,
    /// Covered glyph samples composited.
    GlyphComposites,
    /// Adaptive curve recursion depth.
    CurveRecursion,
    /// Deterministic requirement-plus-command-plus-pixel work units.
    Fuel,
    /// Allocator-reported private Q16 surface capacity.
    SurfaceBytes,
    /// Simultaneous private surface, masks, geometry, and output bytes.
    PeakWorkingBytes,
    /// Allocator-reported pixel-vector capacity.
    RetainedBytes,
    /// A fallible allocation failed inside validated semantic bounds.
    Allocation,
}

/// Structured Reference raster resource-limit context without pixel content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRenderLimit {
    kind: ReferenceRenderLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ReferenceRenderLimit {
    pub(crate) const fn new(
        kind: ReferenceRenderLimitKind,
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

    /// Returns the rejected deterministic dimension.
    pub const fn kind(self) -> ReferenceRenderLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns work or bytes committed before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the additional or complete amount that was rejected.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable Reference raster failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceRenderErrorCode {
    /// Configured limits are zero or above fixed hard ceilings.
    InvalidLimits,
    /// Output dimensions are zero or otherwise invalid.
    InvalidConfig,
    /// Checked dimension, byte, or fuel arithmetic overflowed.
    NumericOverflow,
    /// Cooperative cancellation was observed before atomic publication.
    Cancelled,
    /// A deterministic work or memory budget was exceeded.
    ResourceLimit,
    /// An immutable Scene violated a renderer input invariant.
    InvalidScene,
    /// Checked internal state could not be maintained.
    InternalState,
}

/// Coarse Reference raster error-policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceRenderErrorCategory {
    /// Invalid caller-supplied configuration.
    Configuration,
    /// Invalid or unrepresentable numeric geometry.
    Numeric,
    /// Cooperative cancellation.
    Cancellation,
    /// Deterministic resource exhaustion.
    Resource,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a Reference raster failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceRenderRecoverability {
    /// Correct the configuration before retrying.
    CorrectConfiguration,
    /// Correct the numeric input before retrying.
    CorrectInput,
    /// Retry only while the result remains useful.
    RetryIfUseful,
    /// Reduce work or use an approved larger budget.
    ReduceWorkload,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Pixel-redacted structured Reference raster error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceRenderError {
    code: ReferenceRenderErrorCode,
    category: ReferenceRenderErrorCategory,
    recoverability: ReferenceRenderRecoverability,
    diagnostic_id: &'static str,
    limit: Option<ReferenceRenderLimit>,
}

impl ReferenceRenderError {
    pub(crate) const fn for_code(code: ReferenceRenderErrorCode) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            ReferenceRenderErrorCode::InvalidLimits => (
                ReferenceRenderErrorCategory::Configuration,
                ReferenceRenderRecoverability::CorrectConfiguration,
                "RPE-RASTER-0001",
            ),
            ReferenceRenderErrorCode::InvalidConfig => (
                ReferenceRenderErrorCategory::Configuration,
                ReferenceRenderRecoverability::CorrectConfiguration,
                "RPE-RASTER-0002",
            ),
            ReferenceRenderErrorCode::NumericOverflow => (
                ReferenceRenderErrorCategory::Numeric,
                ReferenceRenderRecoverability::CorrectInput,
                "RPE-RASTER-0003",
            ),
            ReferenceRenderErrorCode::Cancelled => (
                ReferenceRenderErrorCategory::Cancellation,
                ReferenceRenderRecoverability::RetryIfUseful,
                "RPE-RASTER-0004",
            ),
            ReferenceRenderErrorCode::ResourceLimit => (
                ReferenceRenderErrorCategory::Resource,
                ReferenceRenderRecoverability::ReduceWorkload,
                "RPE-RASTER-0005",
            ),
            ReferenceRenderErrorCode::InvalidScene => (
                ReferenceRenderErrorCategory::Numeric,
                ReferenceRenderRecoverability::CorrectInput,
                "RPE-RASTER-0009",
            ),
            ReferenceRenderErrorCode::InternalState => (
                ReferenceRenderErrorCategory::Internal,
                ReferenceRenderRecoverability::DoNotRetry,
                "RPE-RASTER-0006",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            limit: None,
        }
    }

    pub(crate) const fn resource(
        kind: ReferenceRenderLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> Self {
        Self {
            code: ReferenceRenderErrorCode::ResourceLimit,
            category: ReferenceRenderErrorCategory::Resource,
            recoverability: ReferenceRenderRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-RASTER-0005",
            limit: Some(ReferenceRenderLimit::new(kind, limit, consumed, attempted)),
        }
    }

    /// Returns the stable machine-readable error code.
    pub const fn code(self) -> ReferenceRenderErrorCode {
        self.code
    }

    /// Returns the coarse error-policy category.
    pub const fn category(self) -> ReferenceRenderErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> ReferenceRenderRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns structured resource evidence for a budget failure.
    pub const fn limit(self) -> Option<ReferenceRenderLimit> {
        self.limit
    }
}

impl fmt::Display for ReferenceRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_id)
    }
}

impl Error for ReferenceRenderError {}
