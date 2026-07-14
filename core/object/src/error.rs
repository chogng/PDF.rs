use std::error::Error;
use std::fmt;

use pdf_rs_bytes::{SourceError, SourceErrorCategory, SourceRecoverability};
use pdf_rs_syntax::{ObjectRef, SyntaxError, SyntaxErrorCategory};

/// Deterministic object-framing budget that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectLimitKind {
    /// Immutable source bytes addressable by this object profile.
    SourceBytes,
    /// Bytes in one bounded indirect-object envelope window.
    EnvelopeBytes,
    /// Bytes in one bounded stream-boundary window.
    BoundaryBytes,
    /// Declared bytes in one stream payload.
    StreamBytes,
    /// Cumulative exact byte ranges requested by one open job.
    TotalReadBytes,
    /// Cumulative complete windows parsed across retries.
    TotalParseBytes,
}

/// Structured object resource-limit context without document bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectLimit {
    kind: ObjectLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl ObjectLimit {
    pub(crate) const fn new(
        kind: ObjectLimitKind,
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
    pub const fn kind(self) -> ObjectLimitKind {
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

/// Stable machine-readable indirect-object framing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectErrorCode {
    /// Object limit configuration is zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// Job identity or phase checkpoints are invalid.
    InvalidJobContext,
    /// The object job cannot seek without a proven immutable source length.
    UnknownSourceLength,
    /// Xref-derived target, revision, or source geometry is invalid.
    InvalidTarget,
    /// A polled source no longer matches the job's immutable snapshot.
    SnapshotMismatch,
    /// The lower byte source failed.
    SourceFailure,
    /// Final input ended before the indirect-object framing was complete.
    UnexpectedEndOfObject,
    /// The xref offset does not contain the exact requested indirect-object header.
    InvalidObjectHeader,
    /// The direct value or terminating object keyword is malformed.
    InvalidObjectEnvelope,
    /// A stream dictionary does not contain `/Length`.
    MissingStreamLength,
    /// A stream dictionary contains more than one `/Length` entry.
    DuplicateStreamLength,
    /// A direct stream length is negative, ill-typed, or out of range.
    InvalidStreamLength,
    /// A valid indirect `/Length` requires a deliberately unsupported resolver phase.
    UnsupportedIndirectLength,
    /// The declared payload end is not followed by strict `endstream` and `endobj` framing.
    InvalidStreamBoundary,
    /// The lower direct-object syntax parser rejected the envelope.
    SyntaxFailure,
    /// A deterministic object budget was exhausted.
    ResourceLimit,
    /// The owning runtime cancelled this object job.
    Cancelled,
    /// An internal checked state invariant could not be maintained.
    InternalState,
    /// A completed one-shot object job was polled again.
    JobAlreadyComplete,
    /// An exact request inside the bound snapshot unexpectedly reached source EOF.
    UnexpectedEndOfSource,
}

/// Coarse indirect-object failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectErrorCategory {
    /// Invalid caller configuration or job identity.
    Configuration,
    /// Immutable byte-source or snapshot failure.
    Source,
    /// Malformed PDF indirect-object or stream framing bytes.
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

/// Stable recovery policy for an indirect-object failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectRecoverability {
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
enum ObjectErrorDetail {
    None,
    Limit(ObjectLimit),
    Source(SourceError),
    Syntax(SyntaxError),
}

/// Source-redacted indirect-object error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectError {
    code: ObjectErrorCode,
    category: ObjectErrorCategory,
    recoverability: ObjectRecoverability,
    diagnostic_id: &'static str,
    reference: Option<ObjectRef>,
    offset: Option<u64>,
    detail: ObjectErrorDetail,
}

impl ObjectError {
    pub(crate) const fn for_code(
        code: ObjectErrorCode,
        reference: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            ObjectErrorCode::InvalidLimits => (
                ObjectErrorCategory::Configuration,
                ObjectRecoverability::CorrectConfiguration,
                "RPE-OBJECT-0001",
            ),
            ObjectErrorCode::InvalidJobContext => (
                ObjectErrorCategory::Configuration,
                ObjectRecoverability::CorrectConfiguration,
                "RPE-OBJECT-0002",
            ),
            ObjectErrorCode::UnknownSourceLength => (
                ObjectErrorCategory::Configuration,
                ObjectRecoverability::CorrectConfiguration,
                "RPE-OBJECT-0003",
            ),
            ObjectErrorCode::InvalidTarget => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0004",
            ),
            ObjectErrorCode::SnapshotMismatch => (
                ObjectErrorCategory::Source,
                ObjectRecoverability::ReopenSource,
                "RPE-OBJECT-0005",
            ),
            ObjectErrorCode::SourceFailure => (
                ObjectErrorCategory::Source,
                ObjectRecoverability::DoNotRetry,
                "RPE-OBJECT-0006",
            ),
            ObjectErrorCode::UnexpectedEndOfObject => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0007",
            ),
            ObjectErrorCode::InvalidObjectHeader => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0008",
            ),
            ObjectErrorCode::InvalidObjectEnvelope => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0009",
            ),
            ObjectErrorCode::MissingStreamLength => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0010",
            ),
            ObjectErrorCode::DuplicateStreamLength => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0011",
            ),
            ObjectErrorCode::InvalidStreamLength => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0012",
            ),
            ObjectErrorCode::UnsupportedIndirectLength => (
                ObjectErrorCategory::Unsupported,
                ObjectRecoverability::UseSupportedFeature,
                "RPE-OBJECT-0013",
            ),
            ObjectErrorCode::InvalidStreamBoundary => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0014",
            ),
            ObjectErrorCode::SyntaxFailure => (
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0015",
            ),
            ObjectErrorCode::ResourceLimit => (
                ObjectErrorCategory::Resource,
                ObjectRecoverability::ReduceWorkload,
                "RPE-OBJECT-0016",
            ),
            ObjectErrorCode::Cancelled => (
                ObjectErrorCategory::Cancellation,
                ObjectRecoverability::AbandonOperation,
                "RPE-OBJECT-0017",
            ),
            ObjectErrorCode::InternalState => (
                ObjectErrorCategory::Internal,
                ObjectRecoverability::DoNotRetry,
                "RPE-OBJECT-0018",
            ),
            ObjectErrorCode::JobAlreadyComplete => (
                ObjectErrorCategory::Configuration,
                ObjectRecoverability::CorrectConfiguration,
                "RPE-OBJECT-0019",
            ),
            ObjectErrorCode::UnexpectedEndOfSource => (
                ObjectErrorCategory::Source,
                ObjectRecoverability::ReopenSource,
                "RPE-OBJECT-0020",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            reference,
            offset,
            detail: ObjectErrorDetail::None,
        }
    }

    pub(crate) const fn resource(
        kind: ObjectLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        reference: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: ObjectErrorCode::ResourceLimit,
            category: ObjectErrorCategory::Resource,
            recoverability: ObjectRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-OBJECT-0016",
            reference,
            offset,
            detail: ObjectErrorDetail::Limit(ObjectLimit::new(kind, limit, consumed, attempted)),
        }
    }

    pub(crate) const fn from_source(
        error: SourceError,
        reference: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Self {
        let category = match error.category() {
            SourceErrorCategory::Input | SourceErrorCategory::Lifecycle => {
                ObjectErrorCategory::Configuration
            }
            SourceErrorCategory::Integrity | SourceErrorCategory::Availability => {
                ObjectErrorCategory::Source
            }
            SourceErrorCategory::Resource => ObjectErrorCategory::Resource,
            SourceErrorCategory::Internal => ObjectErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SourceRecoverability::CorrectInput => ObjectRecoverability::CorrectConfiguration,
            SourceRecoverability::ReopenSource => ObjectRecoverability::ReopenSource,
            SourceRecoverability::ReduceWorkload => ObjectRecoverability::ReduceWorkload,
            SourceRecoverability::RetrySource => ObjectRecoverability::RetrySource,
            SourceRecoverability::DoNotRetry => ObjectRecoverability::DoNotRetry,
        };
        Self {
            code: ObjectErrorCode::SourceFailure,
            category,
            recoverability,
            diagnostic_id: "RPE-OBJECT-0006",
            reference,
            offset,
            detail: ObjectErrorDetail::Source(error),
        }
    }

    pub(crate) const fn from_syntax(error: SyntaxError, reference: Option<ObjectRef>) -> Self {
        let (code, category, recoverability, diagnostic_id) = match error.category() {
            SyntaxErrorCategory::Configuration => (
                ObjectErrorCode::SyntaxFailure,
                ObjectErrorCategory::Configuration,
                ObjectRecoverability::CorrectConfiguration,
                "RPE-OBJECT-0015",
            ),
            SyntaxErrorCategory::Syntax => (
                ObjectErrorCode::SyntaxFailure,
                ObjectErrorCategory::Syntax,
                ObjectRecoverability::CorrectInput,
                "RPE-OBJECT-0015",
            ),
            SyntaxErrorCategory::Resource => (
                ObjectErrorCode::SyntaxFailure,
                ObjectErrorCategory::Resource,
                ObjectRecoverability::ReduceWorkload,
                "RPE-OBJECT-0015",
            ),
            SyntaxErrorCategory::Integrity => (
                ObjectErrorCode::SyntaxFailure,
                ObjectErrorCategory::Source,
                ObjectRecoverability::ReopenSource,
                "RPE-OBJECT-0015",
            ),
            SyntaxErrorCategory::Cancellation => (
                ObjectErrorCode::Cancelled,
                ObjectErrorCategory::Cancellation,
                ObjectRecoverability::AbandonOperation,
                "RPE-OBJECT-0017",
            ),
            SyntaxErrorCategory::Internal => (
                ObjectErrorCode::SyntaxFailure,
                ObjectErrorCategory::Internal,
                ObjectRecoverability::DoNotRetry,
                "RPE-OBJECT-0015",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            reference,
            offset: error.offset(),
            detail: ObjectErrorDetail::Syntax(error),
        }
    }

    pub(crate) const fn from_syntax_for(
        semantic_code: ObjectErrorCode,
        error: SyntaxError,
        reference: Option<ObjectRef>,
    ) -> Self {
        if matches!(error.category(), SyntaxErrorCategory::Syntax) {
            let base = Self::for_code(semantic_code, reference, error.offset());
            return Self {
                detail: ObjectErrorDetail::Syntax(error),
                ..base
            };
        }
        Self::from_syntax(error, reference)
    }

    /// Returns the machine-readable indirect-object failure code.
    pub const fn code(self) -> ObjectErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> ObjectErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> ObjectRecoverability {
        self.recoverability
    }

    /// Returns the stable project diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the requested indirect-object reference, when known.
    pub const fn reference(self) -> Option<ObjectRef> {
        self.reference
    }

    /// Returns the absolute source offset, when known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns structured object limit context, when applicable.
    pub const fn limit(self) -> Option<ObjectLimit> {
        match self.detail {
            ObjectErrorDetail::Limit(limit) => Some(limit),
            ObjectErrorDetail::None
            | ObjectErrorDetail::Source(_)
            | ObjectErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower byte-source error, when applicable.
    pub const fn source_error(self) -> Option<SourceError> {
        match self.detail {
            ObjectErrorDetail::Source(error) => Some(error),
            ObjectErrorDetail::None
            | ObjectErrorDetail::Limit(_)
            | ObjectErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower syntax error, when applicable.
    pub const fn syntax_error(self) -> Option<SyntaxError> {
        match self.detail {
            ObjectErrorDetail::Syntax(error) => Some(error),
            ObjectErrorDetail::None
            | ObjectErrorDetail::Limit(_)
            | ObjectErrorDetail::Source(_) => None,
        }
    }
}

impl fmt::Display for ObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(reference) = self.reference {
            write!(
                formatter,
                " for object {} {}",
                reference.number(),
                reference.generation()
            )?;
        }
        if let Some(offset) = self.offset {
            write!(formatter, " at byte {offset}")?;
        }
        if let ObjectErrorDetail::Limit(limit) = self.detail {
            write!(
                formatter,
                " limit_kind={:?} limit={} consumed={} attempted={}",
                limit.kind, limit.limit, limit.consumed, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for ObjectError {}
