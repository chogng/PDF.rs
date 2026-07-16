use std::error::Error;
use std::fmt;

/// Deterministic Scene budget that rejected construction or serialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneLimitKind {
    /// Semantic commands retained by one Scene.
    Commands,
    /// Stable resources retained by one Scene.
    Resources,
    /// Active marked-content nesting depth.
    MarkedContentDepth,
    /// Graphics-capable Scene v2 commands.
    GraphicsCommands,
    /// Graphics-capable Scene v2 resources.
    GraphicsResources,
    /// Graphics-capable Scene v2 capability requirements.
    GraphicsRequirements,
    /// Capability-graph dependency identifiers.
    GraphicsDependencies,
    /// Aggregate path segments retained by graphics resources.
    PathSegments,
    /// Aggregate decoded image bytes retained by graphics resources.
    ImageBytes,
    /// Aggregate positioned glyph uses.
    Glyphs,
    /// Saved graphics-state nesting depth.
    GraphicsStateDepth,
    /// Isolated transparency-group nesting depth.
    GraphicsGroupDepth,
    /// Decoded bytes retained by one marked-content tag.
    NameBytes,
    /// Allocator-reported element and scalar-buffer capacity retained by one Scene.
    RetainedBytes,
    /// Resource-index comparison bounds and insertion shifts during Scene construction.
    ResourceIndexWork,
    /// Bytes emitted by canonical Scene JSON.
    CanonicalBytes,
    /// Semantic difference records retained by one Scene comparison.
    Differences,
    /// Fixed-size difference-record capacity retained by one Scene comparison.
    DiffRetainedBytes,
    /// Deterministic semantic comparisons performed by one Scene diff.
    DiffCompareWork,
    /// Bytes emitted by canonical Scene semantic-diff JSON.
    DiffCanonicalBytes,
    /// A fallible allocation failed within an already validated bound.
    Allocation,
}

/// Structured resource-limit context without PDF bytes or document text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneLimit {
    kind: SceneLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl SceneLimit {
    pub(crate) const fn new(
        kind: SceneLimitKind,
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
    pub const fn kind(self) -> SceneLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount charged before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the amount the rejected operation would add or require.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable Scene failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneErrorCode {
    /// Configured Scene limits are zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// A decimal Scene scalar contains invalid syntax.
    InvalidScalar,
    /// A decimal Scene scalar exceeds the nine-digit fractional profile.
    ScalarPrecision,
    /// Fixed-point conversion or arithmetic exceeded the Scene scalar range.
    NumericOverflow,
    /// Page box coordinates do not define a positive-area rectangle.
    InvalidGeometry,
    /// Commands violate marked-content balance or another Scene sequence invariant.
    InvalidCommandSequence,
    /// Commands and source provenance are not paired one-to-one.
    InvalidProvenance,
    /// A deterministic Scene budget was exceeded.
    ResourceLimit,
    /// Internal checked state could not be maintained safely.
    InternalState,
}

/// Coarse Scene error policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Invalid or unrepresentable numeric input.
    Numeric,
    /// Invalid page geometry or command/provenance structure.
    Structure,
    /// Deterministic resource exhaustion.
    Resource,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a Scene failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneRecoverability {
    /// Correct the configured limit profile before retrying.
    CorrectConfiguration,
    /// Correct the numeric or semantic input.
    CorrectInput,
    /// Reduce Scene work or use an approved larger budget.
    ReduceWorkload,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Content-redacted Scene error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneError {
    code: SceneErrorCode,
    category: SceneErrorCategory,
    recoverability: SceneRecoverability,
    diagnostic_id: &'static str,
    command_index: Option<u32>,
    limit: Option<SceneLimit>,
}

impl SceneError {
    pub(crate) const fn for_code(code: SceneErrorCode, command_index: Option<u32>) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            SceneErrorCode::InvalidLimits => (
                SceneErrorCategory::Configuration,
                SceneRecoverability::CorrectConfiguration,
                "RPE-SCENE-0001",
            ),
            SceneErrorCode::InvalidScalar => (
                SceneErrorCategory::Numeric,
                SceneRecoverability::CorrectInput,
                "RPE-SCENE-0002",
            ),
            SceneErrorCode::ScalarPrecision => (
                SceneErrorCategory::Numeric,
                SceneRecoverability::CorrectInput,
                "RPE-SCENE-0003",
            ),
            SceneErrorCode::NumericOverflow => (
                SceneErrorCategory::Numeric,
                SceneRecoverability::CorrectInput,
                "RPE-SCENE-0004",
            ),
            SceneErrorCode::InvalidGeometry => (
                SceneErrorCategory::Structure,
                SceneRecoverability::CorrectInput,
                "RPE-SCENE-0005",
            ),
            SceneErrorCode::InvalidCommandSequence => (
                SceneErrorCategory::Structure,
                SceneRecoverability::CorrectInput,
                "RPE-SCENE-0006",
            ),
            SceneErrorCode::InvalidProvenance => (
                SceneErrorCategory::Structure,
                SceneRecoverability::CorrectInput,
                "RPE-SCENE-0007",
            ),
            SceneErrorCode::ResourceLimit => (
                SceneErrorCategory::Resource,
                SceneRecoverability::ReduceWorkload,
                "RPE-SCENE-0008",
            ),
            SceneErrorCode::InternalState => (
                SceneErrorCategory::Internal,
                SceneRecoverability::DoNotRetry,
                "RPE-SCENE-0009",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            command_index,
            limit: None,
        }
    }

    pub(crate) const fn resource(
        kind: SceneLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        command_index: Option<u32>,
    ) -> Self {
        Self {
            code: SceneErrorCode::ResourceLimit,
            category: SceneErrorCategory::Resource,
            recoverability: SceneRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SCENE-0008",
            command_index,
            limit: Some(SceneLimit::new(kind, limit, consumed, attempted)),
        }
    }

    /// Returns the stable Scene error code.
    pub const fn code(self) -> SceneErrorCode {
        self.code
    }

    /// Returns the coarse policy category.
    pub const fn category(self) -> SceneErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> SceneRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the zero-based command index associated with the failure, when known.
    pub const fn command_index(self) -> Option<u32> {
        self.command_index
    }

    /// Returns resource-limit evidence, when this is a budget failure.
    pub const fn limit(self) -> Option<SceneLimit> {
        self.limit
    }
}

impl fmt::Display for SceneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.diagnostic_id)?;
        if let Some(command_index) = self.command_index {
            write!(formatter, " command_index={command_index}")?;
        }
        Ok(())
    }
}

impl Error for SceneError {}
