use std::error::Error;
use std::fmt;

use crate::ContentPosition;

/// Deterministic scanner budget that rejected work or publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentLimitKind {
    /// Ordered decoded streams.
    Streams,
    /// Aggregate decoded bytes.
    TotalDecodedBytes,
    /// Lexical tokens.
    Tokens,
    /// Raw bytes in one lexical token.
    TokenBytes,
    /// Top-level operands preceding one operator.
    OperandsPerOperator,
    /// Nested array/dictionary depth.
    NestingDepth,
    /// Published operators.
    Operators,
    /// Deterministic scanner work units.
    Fuel,
    /// Allocator-reported owned capacity.
    RetainedBytes,
    /// A fallible allocation failed inside validated limits.
    Allocation,
}

/// Structured limit context that never retains document bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentLimit {
    kind: ContentLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ContentLimit {
    pub(crate) const fn new(
        kind: ContentLimitKind,
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
    pub const fn kind(self) -> ContentLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the charged amount before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the additional amount or complete amount that was rejected.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable content-scanner failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentErrorCode {
    /// Configured limits are zero or exceed fixed hard ceilings.
    InvalidLimits,
    /// Stream ordinals are not the exact zero-based input order.
    InvalidStreamOrder,
    /// A lexical token or direct operand has invalid syntax.
    MalformedToken,
    /// A literal or hexadecimal string reaches its stream boundary unterminated.
    UnterminatedString,
    /// Array or dictionary delimiters are mismatched or unclosed.
    MismatchedDelimiter,
    /// A number is invalid or outside the supported integer range.
    InvalidNumber,
    /// A name contains an incomplete or non-hexadecimal escape.
    InvalidNameEscape,
    /// A hexadecimal string contains a non-hexadecimal byte.
    InvalidHexString,
    /// A dictionary key is not a PDF name.
    InvalidDictionaryKey,
    /// Top-level operands remain after the final stream without an operator.
    DanglingOperands,
    /// Cooperative cancellation was observed before atomic publication.
    Cancelled,
    /// A deterministic resource budget was exceeded.
    ResourceLimit,
    /// Checked internal scanner state could not be maintained.
    InternalState,
}

/// Coarse content-scanner error policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Malformed ordered input metadata.
    Input,
    /// Malformed PDF content syntax.
    Malformed,
    /// Cooperative cancellation.
    Cancellation,
    /// Deterministic resource exhaustion.
    Resource,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a scanner failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentRecoverability {
    /// Correct the configured limits before retrying.
    CorrectConfiguration,
    /// Correct stream metadata or malformed decoded bytes.
    CorrectInput,
    /// Retry only under a current generation if still useful.
    RetryCurrentGeneration,
    /// Reduce work or use an approved larger budget.
    ReduceWorkload,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Content-redacted structured scanner error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentError {
    code: ContentErrorCode,
    category: ContentErrorCategory,
    recoverability: ContentRecoverability,
    diagnostic_id: &'static str,
    position: Option<ContentPosition>,
    limit: Option<ContentLimit>,
}

impl ContentError {
    pub(crate) const fn for_code(
        code: ContentErrorCode,
        position: Option<ContentPosition>,
    ) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            ContentErrorCode::InvalidLimits => (
                ContentErrorCategory::Configuration,
                ContentRecoverability::CorrectConfiguration,
                "RPE-CONTENT-0001",
            ),
            ContentErrorCode::InvalidStreamOrder => (
                ContentErrorCategory::Input,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0002",
            ),
            ContentErrorCode::MalformedToken => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0003",
            ),
            ContentErrorCode::UnterminatedString => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0004",
            ),
            ContentErrorCode::MismatchedDelimiter => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0005",
            ),
            ContentErrorCode::InvalidNumber => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0006",
            ),
            ContentErrorCode::InvalidNameEscape => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0007",
            ),
            ContentErrorCode::InvalidHexString => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0008",
            ),
            ContentErrorCode::InvalidDictionaryKey => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0009",
            ),
            ContentErrorCode::DanglingOperands => (
                ContentErrorCategory::Malformed,
                ContentRecoverability::CorrectInput,
                "RPE-CONTENT-0010",
            ),
            ContentErrorCode::Cancelled => (
                ContentErrorCategory::Cancellation,
                ContentRecoverability::RetryCurrentGeneration,
                "RPE-CONTENT-0011",
            ),
            ContentErrorCode::ResourceLimit => (
                ContentErrorCategory::Resource,
                ContentRecoverability::ReduceWorkload,
                "RPE-CONTENT-0012",
            ),
            ContentErrorCode::InternalState => (
                ContentErrorCategory::Internal,
                ContentRecoverability::DoNotRetry,
                "RPE-CONTENT-0013",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            position,
            limit: None,
        }
    }

    pub(crate) const fn resource(
        kind: ContentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        position: Option<ContentPosition>,
    ) -> Self {
        Self {
            code: ContentErrorCode::ResourceLimit,
            category: ContentErrorCategory::Resource,
            recoverability: ContentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-CONTENT-0012",
            position,
            limit: Some(ContentLimit::new(kind, limit, consumed, attempted)),
        }
    }

    /// Returns the stable machine-readable error code.
    pub const fn code(self) -> ContentErrorCode {
        self.code
    }

    /// Returns the coarse policy category.
    pub const fn category(self) -> ContentErrorCategory {
        self.category
    }

    /// Returns the stable recovery policy.
    pub const fn recoverability(self) -> ContentRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the decoded coordinate where failure was detected.
    pub const fn position(self) -> Option<ContentPosition> {
        self.position
    }

    /// Returns structured resource context for a budget failure.
    pub const fn limit(self) -> Option<ContentLimit> {
        self.limit
    }
}

impl fmt::Display for ContentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.diagnostic_id)
    }
}

impl Error for ContentError {}
