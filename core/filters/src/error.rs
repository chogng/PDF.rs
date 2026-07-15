use std::error::Error;
use std::fmt;

/// Deterministic stream-decoding budget that rejected an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeLimitKind {
    /// Bytes in the exact physical encoded input slice.
    InputBytes,
    /// Number of filters in the canonical plan.
    FilterCount,
    /// Bytes emitted by one filter layer.
    LayerOutputBytes,
    /// Bytes emitted cumulatively by every filter layer.
    TotalOutputBytes,
    /// Bytes in the final decoded result.
    FinalOutputBytes,
    /// Allocator-reported capacity simultaneously retained by filter outputs.
    RetainedCapacityBytes,
    /// Deterministic work units under the bound fuel schedule.
    Fuel,
    /// A fallible allocation failed within an already validated capacity bound.
    Allocation,
}

/// Structured resource-limit context without document bytes or byte offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeLimit {
    kind: DecodeLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl DecodeLimit {
    pub(crate) const fn new(
        kind: DecodeLimitKind,
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
    pub const fn kind(self) -> DecodeLimitKind {
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

/// Stable machine-readable stream-decoding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeErrorCode {
    /// Configured limits are zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// Request spans do not exactly describe the supplied physical bytes.
    InvalidRequest,
    /// A PDF filter name is not implemented by this product slice.
    UnsupportedFilter,
    /// A strict filter input ended without its required end marker.
    MissingEndMarker,
    /// Non-permitted bytes follow a filter end marker.
    TrailingData,
    /// ASCIIHex input contains an invalid byte or state transition.
    InvalidAsciiHex,
    /// ASCII85 input contains an invalid byte, group, or state transition.
    InvalidAscii85,
    /// RunLength input contains a truncated or otherwise invalid run.
    InvalidRunLength,
    /// A zlib wrapper or its Deflate payload is malformed.
    InvalidFlate,
    /// A zlib preset dictionary was declared without an approved dictionary profile.
    UnsupportedFlateDictionary,
    /// A deterministic decoding budget was exceeded.
    ResourceLimit,
    /// The physical bytes belong to a different immutable source revision.
    SourceChanged,
    /// The owning runtime cancelled the decode operation.
    Cancelled,
    /// Internal checked state could not be maintained safely.
    InternalState,
}

/// Coarse stream-decoding error policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeErrorCategory {
    /// Invalid caller configuration or request geometry.
    Configuration,
    /// Malformed filter bytes.
    Syntax,
    /// A recognized but unimplemented filter.
    Unsupported,
    /// Deterministic resource exhaustion.
    Resource,
    /// Immutable source identity mismatch.
    Integrity,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a decoding failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeRecoverability {
    /// Correct the configured profile or request geometry before retrying.
    CorrectConfiguration,
    /// Correct the PDF bytes or select an explicitly different policy.
    CorrectInput,
    /// Report the capability as unsupported without external-engine fallback.
    ReportUnsupported,
    /// Reduce work or use an approved larger budget.
    ReduceWorkload,
    /// Reopen against the correct immutable source snapshot.
    ReopenSource,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Source-redacted decoding error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodeError {
    code: DecodeErrorCode,
    category: DecodeErrorCategory,
    recoverability: DecodeRecoverability,
    diagnostic_id: &'static str,
    filter_index: Option<u16>,
    limit: Option<DecodeLimit>,
}

impl DecodeError {
    pub(crate) const fn for_code(code: DecodeErrorCode, filter_index: Option<u16>) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            DecodeErrorCode::InvalidLimits => (
                DecodeErrorCategory::Configuration,
                DecodeRecoverability::CorrectConfiguration,
                "RPE-FILTERS-0001",
            ),
            DecodeErrorCode::InvalidRequest => (
                DecodeErrorCategory::Configuration,
                DecodeRecoverability::CorrectConfiguration,
                "RPE-FILTERS-0002",
            ),
            DecodeErrorCode::UnsupportedFilter => (
                DecodeErrorCategory::Unsupported,
                DecodeRecoverability::ReportUnsupported,
                "RPE-FILTERS-0003",
            ),
            DecodeErrorCode::MissingEndMarker => (
                DecodeErrorCategory::Syntax,
                DecodeRecoverability::CorrectInput,
                "RPE-FILTERS-0004",
            ),
            DecodeErrorCode::TrailingData => (
                DecodeErrorCategory::Syntax,
                DecodeRecoverability::CorrectInput,
                "RPE-FILTERS-0005",
            ),
            DecodeErrorCode::InvalidAsciiHex => (
                DecodeErrorCategory::Syntax,
                DecodeRecoverability::CorrectInput,
                "RPE-FILTERS-0006",
            ),
            DecodeErrorCode::InvalidAscii85 => (
                DecodeErrorCategory::Syntax,
                DecodeRecoverability::CorrectInput,
                "RPE-FILTERS-0007",
            ),
            DecodeErrorCode::InvalidRunLength => (
                DecodeErrorCategory::Syntax,
                DecodeRecoverability::CorrectInput,
                "RPE-FILTERS-0008",
            ),
            DecodeErrorCode::InvalidFlate => (
                DecodeErrorCategory::Syntax,
                DecodeRecoverability::CorrectInput,
                "RPE-FILTERS-0013",
            ),
            DecodeErrorCode::UnsupportedFlateDictionary => (
                DecodeErrorCategory::Unsupported,
                DecodeRecoverability::ReportUnsupported,
                "RPE-FILTERS-0014",
            ),
            DecodeErrorCode::ResourceLimit => (
                DecodeErrorCategory::Resource,
                DecodeRecoverability::ReduceWorkload,
                "RPE-FILTERS-0009",
            ),
            DecodeErrorCode::SourceChanged => (
                DecodeErrorCategory::Integrity,
                DecodeRecoverability::ReopenSource,
                "RPE-FILTERS-0010",
            ),
            DecodeErrorCode::Cancelled => (
                DecodeErrorCategory::Cancellation,
                DecodeRecoverability::AbandonOperation,
                "RPE-FILTERS-0011",
            ),
            DecodeErrorCode::InternalState => (
                DecodeErrorCategory::Internal,
                DecodeRecoverability::DoNotRetry,
                "RPE-FILTERS-0012",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            filter_index,
            limit: None,
        }
    }

    pub(crate) const fn resource(
        kind: DecodeLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        filter_index: Option<u16>,
    ) -> Self {
        Self {
            code: DecodeErrorCode::ResourceLimit,
            category: DecodeErrorCategory::Resource,
            recoverability: DecodeRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-FILTERS-0009",
            filter_index,
            limit: Some(DecodeLimit::new(kind, limit, consumed, attempted)),
        }
    }

    /// Returns the exact machine-readable failure code.
    pub const fn code(self) -> DecodeErrorCode {
        self.code
    }

    /// Returns the coarse policy category.
    pub const fn category(self) -> DecodeErrorCategory {
        self.category
    }

    /// Returns the stable recovery policy.
    pub const fn recoverability(self) -> DecodeRecoverability {
        self.recoverability
    }

    /// Returns the stable source-redacted diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the zero-based filter-layer index, when one layer owns the failure.
    pub const fn filter_index(self) -> Option<u16> {
        self.filter_index
    }

    /// Returns deterministic limit context for resource failures.
    pub const fn limit(self) -> Option<DecodeLimit> {
        self.limit
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_id)
    }
}

impl Error for DecodeError {}
