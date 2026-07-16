use std::error::Error;
use std::fmt;

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
    /// Deterministic command-plus-pixel work units.
    Fuel,
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
