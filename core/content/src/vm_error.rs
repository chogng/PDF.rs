use std::error::Error;
use std::fmt;

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
    limit: Option<ContentVmLimit>,
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
            limit: None,
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
            limit: Some(limit),
        }
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
        self.limit
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
