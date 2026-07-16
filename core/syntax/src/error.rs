use std::error::Error;
use std::fmt;

/// Deterministic syntax budget that rejected an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxLimitKind {
    /// Complete contiguous parser input bytes.
    InputBytes,
    /// Bytes in one lexical token.
    TokenBytes,
    /// Bytes in one comment.
    CommentBytes,
    /// Decoded bytes in one name.
    NameBytes,
    /// Source bytes scanned for one string.
    StringSourceBytes,
    /// Decoded bytes retained for one string.
    StringDecodedBytes,
    /// Total decoded name, string, and real-number bytes.
    OwnedBytes,
    /// Total lexical tokens consumed by one parser attempt.
    Tokens,
    /// Total array items and dictionary entries.
    ContainerEntries,
    /// Allocator-reported array and dictionary vector capacity bytes.
    ContainerBytes,
    /// Combined allocator-reported owned scalar and container capacity bytes.
    RetainedBytes,
    /// Nested array and dictionary depth.
    ContainerDepth,
    /// Fallible allocation within an already bounded operation.
    Allocation,
}

/// Structured resource-limit context without source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxLimit {
    kind: SyntaxLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl SyntaxLimit {
    pub(crate) const fn new(
        kind: SyntaxLimitKind,
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
    pub const fn kind(self) -> SyntaxLimitKind {
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

/// Stable machine-readable PDF syntax failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxErrorCode {
    /// Configured syntax limits are zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// The input does not contain a supported PDF header at the requested position.
    InvalidHeader,
    /// A byte cannot begin or continue a valid token in the current context.
    UnexpectedByte,
    /// A valid token is not allowed by the current direct-object grammar.
    UnexpectedToken,
    /// Final input ended while a compound object was incomplete.
    UnexpectedEndOfInput,
    /// A numeric token violates the accepted integer/real grammar.
    InvalidNumber,
    /// An integer cannot be represented by the required semantic width.
    IntegerOutOfRange,
    /// A `#xx` name escape is malformed.
    InvalidNameEscape,
    /// A final literal string is missing its closing parenthesis.
    UnterminatedLiteralString,
    /// A hex string contains a non-hex byte or lacks its final delimiter.
    InvalidHexString,
    /// An array or dictionary closes with the wrong delimiter.
    MismatchedDelimiter,
    /// A candidate indirect reference has an invalid object or generation number.
    InvalidReference,
    /// The `stream` keyword is not followed by a strict line ending.
    InvalidStreamBoundary,
    /// A deterministic syntax budget was exceeded.
    ResourceLimit,
    /// Input source identity does not match the parser's bound identity.
    SourceMismatch,
    /// The owning runtime cancelled this parser operation.
    Cancelled,
    /// Internal checked state could not be maintained safely.
    InternalState,
}

/// Coarse syntax error policy category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxErrorCategory {
    /// Invalid caller configuration.
    Configuration,
    /// Malformed or contextually invalid PDF bytes.
    Syntax,
    /// Deterministic resource exhaustion.
    Resource,
    /// Immutable source identity mismatch.
    Integrity,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for a syntax failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxRecoverability {
    /// Correct the configured profile before retrying.
    CorrectConfiguration,
    /// Correct the PDF bytes or select an explicitly tolerant policy.
    CorrectInput,
    /// Reduce input work or use an approved larger budget.
    ReduceWorkload,
    /// Reopen against the correct immutable source snapshot.
    ReopenSource,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Source-redacted syntax error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    code: SyntaxErrorCode,
    category: SyntaxErrorCategory,
    recoverability: SyntaxRecoverability,
    diagnostic_id: &'static str,
    offset: Option<u64>,
    limit: Option<SyntaxLimit>,
}

impl SyntaxError {
    pub(crate) const fn for_code(code: SyntaxErrorCode, offset: Option<u64>) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            SyntaxErrorCode::InvalidLimits => (
                SyntaxErrorCategory::Configuration,
                SyntaxRecoverability::CorrectConfiguration,
                "RPE-SYNTAX-0001",
            ),
            SyntaxErrorCode::InvalidHeader => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0002",
            ),
            SyntaxErrorCode::UnexpectedByte => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0003",
            ),
            SyntaxErrorCode::UnexpectedToken => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0004",
            ),
            SyntaxErrorCode::UnexpectedEndOfInput => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0005",
            ),
            SyntaxErrorCode::InvalidNumber => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0006",
            ),
            SyntaxErrorCode::IntegerOutOfRange => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0007",
            ),
            SyntaxErrorCode::InvalidNameEscape => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0008",
            ),
            SyntaxErrorCode::UnterminatedLiteralString => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0009",
            ),
            SyntaxErrorCode::InvalidHexString => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0010",
            ),
            SyntaxErrorCode::MismatchedDelimiter => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0011",
            ),
            SyntaxErrorCode::InvalidReference => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0012",
            ),
            SyntaxErrorCode::InvalidStreamBoundary => (
                SyntaxErrorCategory::Syntax,
                SyntaxRecoverability::CorrectInput,
                "RPE-SYNTAX-0013",
            ),
            SyntaxErrorCode::ResourceLimit => (
                SyntaxErrorCategory::Resource,
                SyntaxRecoverability::ReduceWorkload,
                "RPE-SYNTAX-0014",
            ),
            SyntaxErrorCode::SourceMismatch => (
                SyntaxErrorCategory::Integrity,
                SyntaxRecoverability::ReopenSource,
                "RPE-SYNTAX-0015",
            ),
            SyntaxErrorCode::Cancelled => (
                SyntaxErrorCategory::Cancellation,
                SyntaxRecoverability::AbandonOperation,
                "RPE-SYNTAX-0017",
            ),
            SyntaxErrorCode::InternalState => (
                SyntaxErrorCategory::Internal,
                SyntaxRecoverability::DoNotRetry,
                "RPE-SYNTAX-0016",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            offset,
            limit: None,
        }
    }

    pub(crate) const fn resource(
        kind: SyntaxLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: SyntaxErrorCode::ResourceLimit,
            category: SyntaxErrorCategory::Resource,
            recoverability: SyntaxRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SYNTAX-0014",
            offset,
            limit: Some(SyntaxLimit::new(kind, limit, consumed, attempted)),
        }
    }

    /// Returns the exact machine-readable failure code.
    pub const fn code(self) -> SyntaxErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> SyntaxErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> SyntaxRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the absolute source byte offset, when one is known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns structured resource-limit context, when applicable.
    pub const fn limit(self) -> Option<SyntaxLimit> {
        self.limit
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(offset) = self.offset {
            write!(formatter, " at byte {offset}")?;
        }
        Ok(())
    }
}

impl Error for SyntaxError {}
