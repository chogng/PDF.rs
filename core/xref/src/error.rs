use std::error::Error;
use std::fmt;

use pdf_rs_bytes::{SourceError, SourceErrorCategory, SourceRecoverability};
use pdf_rs_syntax::{SyntaxError, SyntaxErrorCategory};

/// Deterministic xref budget that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefLimitKind {
    /// Immutable source bytes addressable by this bootstrap profile.
    SourceBytes,
    /// Bytes in the bounded tail search window.
    TailBytes,
    /// Bytes in one traditional xref section window.
    SectionBytes,
    /// Cumulative exact byte ranges requested by one open job.
    TotalReadBytes,
    /// Cumulative complete windows scanned across retries.
    TotalParseBytes,
    /// Traditional xref subsections.
    Subsections,
    /// Declared objects or parsed xref entries.
    Entries,
    /// Fallible bounded metadata allocation.
    Allocation,
}

/// Structured resource-limit context without document bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefLimit {
    kind: XrefLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl XrefLimit {
    pub(crate) const fn new(
        kind: XrefLimitKind,
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
    pub const fn kind(self) -> XrefLimitKind {
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

    /// Returns the amount the rejected operation would add or require.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable xref failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefErrorCode {
    /// Xref limit configuration is zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// Job identity or phase checkpoints are invalid.
    InvalidJobContext,
    /// The bootstrap cannot seek a tail without a proven source length.
    UnknownSourceLength,
    /// A known-empty source cannot contain a PDF xref.
    EmptySource,
    /// A polled source no longer matches the job's immutable snapshot.
    SnapshotMismatch,
    /// The lower byte source failed.
    SourceFailure,
    /// A supposedly in-range exact read crossed the immutable source end.
    UnexpectedEndOfSource,
    /// No final `startxref` was found within the complete bounded search area.
    StartXrefNotFound,
    /// The final `startxref` or `%%EOF` syntax is malformed.
    InvalidStartXref,
    /// The declared xref offset is outside the immutable source.
    StartXrefOutOfBounds,
    /// The declared offset does not begin a traditional `xref` section.
    InvalidXrefKeyword,
    /// A traditional subsection header is malformed, overlapping, or unordered.
    InvalidSubsection,
    /// A traditional fixed-width entry is malformed or semantically invalid.
    InvalidEntry,
    /// The trailer is missing or has invalid required fields.
    InvalidTrailer,
    /// The declared offset identifies an xref stream, which this slice does not support.
    UnsupportedXrefStream,
    /// A trailer requests a hybrid `/XRefStm` revision.
    UnsupportedHybridXref,
    /// A trailer requests an incremental `/Prev` revision chain.
    UnsupportedIncrementalRevision,
    /// The lower direct-object syntax parser rejected the trailer.
    SyntaxFailure,
    /// A deterministic xref budget was exhausted.
    ResourceLimit,
    /// The owning runtime cancelled this xref job.
    Cancelled,
    /// An internal checked state invariant could not be maintained.
    InternalState,
    /// A completed one-shot xref job was polled again.
    JobAlreadyComplete,
}

/// Coarse xref failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefErrorCategory {
    /// Invalid caller configuration or job identity.
    Configuration,
    /// Immutable byte-source or snapshot failure.
    Source,
    /// Malformed PDF xref or trailer bytes.
    Syntax,
    /// Valid syntax that requires a deliberately unsupported capability.
    Unsupported,
    /// Deterministic work or allocation exhaustion.
    Resource,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal implementation invariant failure.
    Internal,
}

/// Stable recovery policy for an xref failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XrefRecoverability {
    /// Correct configuration or job/checkpoint identity before retrying.
    CorrectConfiguration,
    /// Correct the PDF bytes or use an explicitly approved recovery policy.
    CorrectInput,
    /// Reopen against a newly bound immutable source snapshot.
    ReopenSource,
    /// Retry the host source operation while preserving snapshot identity.
    RetrySource,
    /// Reduce work or select an approved larger deterministic budget.
    ReduceWorkload,
    /// Select an implementation profile that supports the requested feature.
    UseSupportedFeature,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum XrefErrorDetail {
    None,
    Limit(XrefLimit),
    Source(SourceError),
    Syntax(SyntaxError),
}

/// Source-redacted xref error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XrefError {
    code: XrefErrorCode,
    category: XrefErrorCategory,
    recoverability: XrefRecoverability,
    diagnostic_id: &'static str,
    offset: Option<u64>,
    detail: XrefErrorDetail,
}

impl XrefError {
    pub(crate) const fn for_code(code: XrefErrorCode, offset: Option<u64>) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            XrefErrorCode::InvalidLimits => (
                XrefErrorCategory::Configuration,
                XrefRecoverability::CorrectConfiguration,
                "RPE-XREF-0001",
            ),
            XrefErrorCode::InvalidJobContext => (
                XrefErrorCategory::Configuration,
                XrefRecoverability::CorrectConfiguration,
                "RPE-XREF-0002",
            ),
            XrefErrorCode::UnknownSourceLength => (
                XrefErrorCategory::Configuration,
                XrefRecoverability::CorrectConfiguration,
                "RPE-XREF-0003",
            ),
            XrefErrorCode::EmptySource => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0004",
            ),
            XrefErrorCode::SnapshotMismatch => (
                XrefErrorCategory::Source,
                XrefRecoverability::ReopenSource,
                "RPE-XREF-0005",
            ),
            XrefErrorCode::SourceFailure => (
                XrefErrorCategory::Source,
                XrefRecoverability::DoNotRetry,
                "RPE-XREF-0006",
            ),
            XrefErrorCode::UnexpectedEndOfSource => (
                XrefErrorCategory::Source,
                XrefRecoverability::ReopenSource,
                "RPE-XREF-0007",
            ),
            XrefErrorCode::StartXrefNotFound => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0008",
            ),
            XrefErrorCode::InvalidStartXref => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0009",
            ),
            XrefErrorCode::StartXrefOutOfBounds => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0010",
            ),
            XrefErrorCode::InvalidXrefKeyword => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0011",
            ),
            XrefErrorCode::InvalidSubsection => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0012",
            ),
            XrefErrorCode::InvalidEntry => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0013",
            ),
            XrefErrorCode::InvalidTrailer => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0014",
            ),
            XrefErrorCode::UnsupportedXrefStream => (
                XrefErrorCategory::Unsupported,
                XrefRecoverability::UseSupportedFeature,
                "RPE-XREF-0015",
            ),
            XrefErrorCode::UnsupportedHybridXref => (
                XrefErrorCategory::Unsupported,
                XrefRecoverability::UseSupportedFeature,
                "RPE-XREF-0016",
            ),
            XrefErrorCode::UnsupportedIncrementalRevision => (
                XrefErrorCategory::Unsupported,
                XrefRecoverability::UseSupportedFeature,
                "RPE-XREF-0017",
            ),
            XrefErrorCode::SyntaxFailure => (
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0018",
            ),
            XrefErrorCode::ResourceLimit => (
                XrefErrorCategory::Resource,
                XrefRecoverability::ReduceWorkload,
                "RPE-XREF-0019",
            ),
            XrefErrorCode::Cancelled => (
                XrefErrorCategory::Cancellation,
                XrefRecoverability::AbandonOperation,
                "RPE-XREF-0020",
            ),
            XrefErrorCode::InternalState => (
                XrefErrorCategory::Internal,
                XrefRecoverability::DoNotRetry,
                "RPE-XREF-0021",
            ),
            XrefErrorCode::JobAlreadyComplete => (
                XrefErrorCategory::Configuration,
                XrefRecoverability::CorrectConfiguration,
                "RPE-XREF-0022",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            offset,
            detail: XrefErrorDetail::None,
        }
    }

    pub(crate) const fn resource(
        kind: XrefLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: XrefErrorCode::ResourceLimit,
            category: XrefErrorCategory::Resource,
            recoverability: XrefRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-XREF-0019",
            offset,
            detail: XrefErrorDetail::Limit(XrefLimit::new(kind, limit, consumed, attempted)),
        }
    }

    pub(crate) const fn from_source(error: SourceError) -> Self {
        let category = match error.category() {
            SourceErrorCategory::Input | SourceErrorCategory::Lifecycle => {
                XrefErrorCategory::Configuration
            }
            SourceErrorCategory::Integrity | SourceErrorCategory::Availability => {
                XrefErrorCategory::Source
            }
            SourceErrorCategory::Resource => XrefErrorCategory::Resource,
            SourceErrorCategory::Internal => XrefErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SourceRecoverability::CorrectInput => XrefRecoverability::CorrectConfiguration,
            SourceRecoverability::ReopenSource => XrefRecoverability::ReopenSource,
            SourceRecoverability::ReduceWorkload => XrefRecoverability::ReduceWorkload,
            SourceRecoverability::RetrySource => XrefRecoverability::RetrySource,
            SourceRecoverability::DoNotRetry => XrefRecoverability::DoNotRetry,
        };
        Self {
            code: XrefErrorCode::SourceFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-XREF-0006",
            offset: None,
            detail: XrefErrorDetail::Source(error),
        }
    }

    pub(crate) const fn from_syntax(error: SyntaxError) -> Self {
        let (code, category, recoverability, diagnostic_id) = match error.category() {
            SyntaxErrorCategory::Configuration => (
                XrefErrorCode::SyntaxFailure,
                XrefErrorCategory::Configuration,
                XrefRecoverability::CorrectConfiguration,
                "RPE-XREF-0018",
            ),
            SyntaxErrorCategory::Syntax => (
                XrefErrorCode::SyntaxFailure,
                XrefErrorCategory::Syntax,
                XrefRecoverability::CorrectInput,
                "RPE-XREF-0018",
            ),
            SyntaxErrorCategory::Resource => (
                XrefErrorCode::SyntaxFailure,
                XrefErrorCategory::Resource,
                XrefRecoverability::ReduceWorkload,
                "RPE-XREF-0018",
            ),
            SyntaxErrorCategory::Integrity => (
                XrefErrorCode::SyntaxFailure,
                XrefErrorCategory::Source,
                XrefRecoverability::ReopenSource,
                "RPE-XREF-0018",
            ),
            SyntaxErrorCategory::Cancellation => (
                XrefErrorCode::Cancelled,
                XrefErrorCategory::Cancellation,
                XrefRecoverability::AbandonOperation,
                "RPE-XREF-0020",
            ),
            SyntaxErrorCategory::Internal => (
                XrefErrorCode::SyntaxFailure,
                XrefErrorCategory::Internal,
                XrefRecoverability::DoNotRetry,
                "RPE-XREF-0018",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            offset: error.offset(),
            detail: XrefErrorDetail::Syntax(error),
        }
    }

    /// Returns the machine-readable xref failure code.
    pub const fn code(self) -> XrefErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> XrefErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> XrefRecoverability {
        self.recoverability
    }

    /// Returns the stable project diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the absolute source offset, when known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns structured xref limit context, when applicable.
    pub const fn limit(self) -> Option<XrefLimit> {
        match self.detail {
            XrefErrorDetail::Limit(limit) => Some(limit),
            XrefErrorDetail::None | XrefErrorDetail::Source(_) | XrefErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower byte-source error, when applicable.
    pub const fn source_error(self) -> Option<SourceError> {
        match self.detail {
            XrefErrorDetail::Source(error) => Some(error),
            XrefErrorDetail::None | XrefErrorDetail::Limit(_) | XrefErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower syntax error, when applicable.
    pub const fn syntax_error(self) -> Option<SyntaxError> {
        match self.detail {
            XrefErrorDetail::Syntax(error) => Some(error),
            XrefErrorDetail::None | XrefErrorDetail::Limit(_) | XrefErrorDetail::Source(_) => None,
        }
    }
}

impl fmt::Display for XrefError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(offset) = self.offset {
            write!(formatter, " at byte {offset}")?;
        }
        if let XrefErrorDetail::Limit(limit) = self.detail {
            write!(
                formatter,
                " limit_kind={:?} limit={} consumed={} attempted={}",
                limit.kind, limit.limit, limit.consumed, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for XrefError {}
