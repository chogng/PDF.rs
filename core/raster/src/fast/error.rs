use core::fmt;

/// Stable Fast CPU failure category.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FastRasterErrorCategory {
    /// Input identity or renderer configuration is invalid for this backend.
    InvalidInput,
    /// Checked resource admission rejected the operation.
    ResourceLimit,
    /// Cooperative cancellation terminated private work.
    Cancelled,
    /// An internal checked-arithmetic or state invariant failed.
    Internal,
}

/// Stable Fast CPU failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FastRasterErrorCode {
    /// The RenderPlan selects a backend or configuration the Fast implementation cannot execute.
    InvalidRenderConfig,
    /// Scene and plan identities do not describe the same immutable input.
    IdentityMismatch,
    /// A graphics resource identifier is absent or has the wrong resource kind.
    InvalidResource,
    /// Graphics-state commands are not balanced during tile replay.
    InvalidCommandSequence,
    /// The operation exceeded one explicit resource dimension.
    ResourceLimit,
    /// A fallible allocation failed.
    Allocation,
    /// Cooperative cancellation was observed.
    Cancelled,
    /// Checked numeric arithmetic overflowed.
    NumericOverflow,
}

/// Independently bounded Fast CPU resource dimension.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FastRasterLimitKind {
    /// Published product pixels.
    Pixels,
    /// Scene commands considered by binning.
    Commands,
    /// Command references retained across all tile bins.
    BinEntries,
    /// Published tile bytes and durable bin metadata.
    RetainedBytes,
    /// Private working surfaces, masks, stacks, and geometry.
    IntermediateBytes,
    /// Deterministic scalar work units.
    Fuel,
    /// Maximum deterministic work permitted inside one atomic tile render.
    AtomicTileFuel,
    /// Maximum work units between cancellation probes.
    CancellationInterval,
}

/// Content-redacted resource-limit evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FastRasterLimit {
    kind: FastRasterLimitKind,
    limit: u64,
    observed: u64,
}

impl FastRasterLimit {
    pub(crate) const fn new(kind: FastRasterLimitKind, limit: u64, observed: u64) -> Self {
        Self {
            kind,
            limit,
            observed,
        }
    }

    /// Returns the independent resource dimension.
    pub const fn kind(self) -> FastRasterLimitKind {
        self.kind
    }

    /// Returns the configured maximum.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the attempted total.
    pub const fn observed(self) -> u64 {
        self.observed
    }
}

/// Structured content-redacted Fast CPU failure.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FastRasterError {
    code: FastRasterErrorCode,
    category: FastRasterErrorCategory,
    limit: Option<FastRasterLimit>,
}

impl FastRasterError {
    pub(crate) const fn for_code(code: FastRasterErrorCode) -> Self {
        let category = match code {
            FastRasterErrorCode::InvalidRenderConfig
            | FastRasterErrorCode::IdentityMismatch
            | FastRasterErrorCode::InvalidResource
            | FastRasterErrorCode::InvalidCommandSequence => FastRasterErrorCategory::InvalidInput,
            FastRasterErrorCode::ResourceLimit | FastRasterErrorCode::Allocation => {
                FastRasterErrorCategory::ResourceLimit
            }
            FastRasterErrorCode::Cancelled => FastRasterErrorCategory::Cancelled,
            FastRasterErrorCode::NumericOverflow => FastRasterErrorCategory::Internal,
        };
        Self {
            code,
            category,
            limit: None,
        }
    }

    pub(crate) const fn resource(kind: FastRasterLimitKind, limit: u64, observed: u64) -> Self {
        Self {
            code: FastRasterErrorCode::ResourceLimit,
            category: FastRasterErrorCategory::ResourceLimit,
            limit: Some(FastRasterLimit::new(kind, limit, observed)),
        }
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> FastRasterErrorCode {
        self.code
    }

    /// Returns the stable failure category.
    pub const fn category(self) -> FastRasterErrorCategory {
        self.category
    }

    /// Returns resource evidence when this is an explicit limit rejection.
    pub const fn limit(self) -> Option<FastRasterLimit> {
        self.limit
    }
}

impl fmt::Debug for FastRasterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FastRasterError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("limit", &self.limit)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for FastRasterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Fast CPU raster failed: {:?}", self.code)
    }
}

impl std::error::Error for FastRasterError {}
