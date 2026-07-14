use std::error::Error;
use std::fmt;

use pdf_rs_bytes::{SourceError, SourceErrorCategory, SourceRecoverability};
use pdf_rs_object::{ObjectError, ObjectErrorCategory, ObjectErrorCode, ObjectRecoverability};
use pdf_rs_syntax::{ObjectRef, SyntaxError, SyntaxErrorCategory, SyntaxRecoverability};

/// Deterministic candidate-index or revision-attestation budget that rejected work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentLimitKind {
    /// Total xref rows in one candidate revision.
    TotalEntries,
    /// In-use xref rows in one candidate revision.
    InUseEntries,
    /// Conservatively accounted allocator-reported logical and physical entry capacity.
    LogicalIndexBytes,
    /// Comparisons and swaps performed while sorting by physical offset.
    SortSteps,
    /// Fallible bounded index-capacity reservation using conservative byte accounting.
    Allocation,
    /// Immutable source bytes addressable by the revision-attestation profile.
    AttestationSourceBytes,
    /// In-use objects framed by one revision-attestation job.
    AttestationObjects,
    /// Cumulative prefix and inter-object trivia bytes.
    AttestationTriviaBytes,
    /// Bytes in one top-level PDF comment.
    AttestationCommentBytes,
    /// Cumulative exact ranges requested by child object jobs.
    AttestationObjectReadBytes,
    /// Cumulative parser-window bytes charged by child object jobs.
    AttestationObjectParseBytes,
    /// Conservatively accounted allocator capacity for retained fixed-size evidence.
    AttestationEvidenceBytes,
}

/// Structured document-composition resource-limit context without document bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentLimit {
    kind: DocumentLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
}

impl DocumentLimit {
    pub(crate) const fn new(
        kind: DocumentLimitKind,
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
    pub const fn kind(self) -> DocumentLimitKind {
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

/// Stable machine-readable candidate-index or revision-attestation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentErrorCode {
    /// Document limit configuration is zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// A deterministic index or allocation budget was exhausted.
    ResourceLimit,
    /// The owning runtime cancelled candidate-index construction.
    Cancelled,
    /// An in-use row has an offset outside the candidate revision object area.
    InvalidPhysicalOffset,
    /// Two in-use rows claim the same physical object offset.
    DuplicatePhysicalOffset,
    /// An in-use object-zero row or another impossible xref record was observed.
    InvalidXrefEntry,
    /// The trailer root is not an exact-generation in-use row in this candidate revision.
    InvalidTrailerRoot,
    /// No xref row exists for the requested object number.
    MissingObject,
    /// The requested object number is represented by a free xref row.
    FreeObject,
    /// The requested generation does not match the candidate xref row.
    GenerationMismatch,
    /// Candidate interval geometry could not form an unattested object target.
    TargetConstructionFailure,
    /// A checked candidate-index invariant could not be maintained.
    InternalState,
    /// Runtime identity or phase checkpoints for revision attestation are inconsistent.
    InvalidAttestationJobContext,
    /// The source does not begin with a supported header followed by a line ending.
    InvalidDocumentHeader,
    /// A non-trivia byte occurs between top-level object frames.
    TopLevelData,
    /// A top-level comment reaches an object or xref boundary without a line ending.
    UnterminatedTopLevelComment,
    /// One candidate object could not be strictly framed and authenticated.
    ObjectAttestationFailure,
    /// Valid object syntax requires a deliberately unsupported framing capability.
    UnsupportedObjectFraming,
    /// The byte source no longer matches the attestation job's immutable snapshot.
    SourceSnapshotMismatch,
    /// The injected byte source failed while scanning top-level trivia.
    SourceFailure,
    /// An exact request inside the known immutable source unexpectedly reached EOF.
    UnexpectedEndOfSource,
    /// A completed one-shot revision-attestation job was polled again.
    JobAlreadyComplete,
}

/// Coarse document-composition failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentErrorCategory {
    /// Invalid deterministic configuration.
    Configuration,
    /// Malformed or inconsistent xref-derived candidate metadata.
    Syntax,
    /// A requested object identity is absent, free, or has another generation.
    Lookup,
    /// Deterministic work or allocation exhaustion.
    Resource,
    /// Immutable byte-source failure or snapshot-integrity change.
    Source,
    /// Valid syntax requiring a deliberately unsupported capability.
    Unsupported,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal checked invariant failure.
    Internal,
}

/// Stable recovery policy for a document-composition failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentRecoverability {
    /// Correct the deterministic limit profile before retrying.
    CorrectConfiguration,
    /// Correct the PDF bytes or select an explicitly approved recovery policy.
    CorrectInput,
    /// Supply a reference that exists with the indexed generation and is in use.
    CorrectReference,
    /// Reduce work or select an approved larger deterministic budget.
    ReduceWorkload,
    /// Reopen against a newly bound immutable source snapshot.
    ReopenSource,
    /// Retry the host source operation while preserving snapshot identity.
    RetrySource,
    /// Select an implementation profile supporting the required feature.
    UseSupportedFeature,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DocumentErrorDetail {
    None,
    Limit(DocumentLimit),
    Object {
        error: ObjectError,
        aggregate_limit: Option<DocumentLimit>,
    },
    Source(SourceError),
    Syntax(SyntaxError),
}

/// Source-redacted document-composition error with stable policy metadata.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DocumentError {
    code: DocumentErrorCode,
    category: DocumentErrorCategory,
    recoverability: DocumentRecoverability,
    diagnostic_id: &'static str,
    reference: Option<ObjectRef>,
    offset: Option<u64>,
    detail: DocumentErrorDetail,
}

impl DocumentError {
    pub(crate) const fn for_code(
        code: DocumentErrorCode,
        reference: Option<ObjectRef>,
        offset: Option<u64>,
    ) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            DocumentErrorCode::InvalidLimits => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0001",
            ),
            DocumentErrorCode::ResourceLimit => (
                DocumentErrorCategory::Resource,
                DocumentRecoverability::ReduceWorkload,
                "RPE-DOCUMENT-0002",
            ),
            DocumentErrorCode::Cancelled => (
                DocumentErrorCategory::Cancellation,
                DocumentRecoverability::AbandonOperation,
                "RPE-DOCUMENT-0003",
            ),
            DocumentErrorCode::InvalidPhysicalOffset => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0004",
            ),
            DocumentErrorCode::DuplicatePhysicalOffset => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0005",
            ),
            DocumentErrorCode::InvalidXrefEntry => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0006",
            ),
            DocumentErrorCode::InvalidTrailerRoot => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0007",
            ),
            DocumentErrorCode::MissingObject => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0008",
            ),
            DocumentErrorCode::FreeObject => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0009",
            ),
            DocumentErrorCode::GenerationMismatch => (
                DocumentErrorCategory::Lookup,
                DocumentRecoverability::CorrectReference,
                "RPE-DOCUMENT-0010",
            ),
            DocumentErrorCode::TargetConstructionFailure => (
                DocumentErrorCategory::Internal,
                DocumentRecoverability::DoNotRetry,
                "RPE-DOCUMENT-0011",
            ),
            DocumentErrorCode::InternalState => (
                DocumentErrorCategory::Internal,
                DocumentRecoverability::DoNotRetry,
                "RPE-DOCUMENT-0012",
            ),
            DocumentErrorCode::InvalidAttestationJobContext => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0013",
            ),
            DocumentErrorCode::InvalidDocumentHeader => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0014",
            ),
            DocumentErrorCode::TopLevelData => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0015",
            ),
            DocumentErrorCode::UnterminatedTopLevelComment => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0016",
            ),
            DocumentErrorCode::ObjectAttestationFailure => (
                DocumentErrorCategory::Syntax,
                DocumentRecoverability::CorrectInput,
                "RPE-DOCUMENT-0017",
            ),
            DocumentErrorCode::UnsupportedObjectFraming => (
                DocumentErrorCategory::Unsupported,
                DocumentRecoverability::UseSupportedFeature,
                "RPE-DOCUMENT-0018",
            ),
            DocumentErrorCode::SourceSnapshotMismatch => (
                DocumentErrorCategory::Source,
                DocumentRecoverability::ReopenSource,
                "RPE-DOCUMENT-0019",
            ),
            DocumentErrorCode::SourceFailure => (
                DocumentErrorCategory::Source,
                DocumentRecoverability::RetrySource,
                "RPE-DOCUMENT-0020",
            ),
            DocumentErrorCode::UnexpectedEndOfSource => (
                DocumentErrorCategory::Source,
                DocumentRecoverability::ReopenSource,
                "RPE-DOCUMENT-0021",
            ),
            DocumentErrorCode::JobAlreadyComplete => (
                DocumentErrorCategory::Configuration,
                DocumentRecoverability::CorrectConfiguration,
                "RPE-DOCUMENT-0022",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            reference,
            offset,
            detail: DocumentErrorDetail::None,
        }
    }

    pub(crate) const fn resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        offset: Option<u64>,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: None,
            offset,
            detail: DocumentErrorDetail::Limit(DocumentLimit::new(
                kind, limit, consumed, attempted,
            )),
        }
    }

    pub(crate) const fn from_object(error: ObjectError, reference: ObjectRef, offset: u64) -> Self {
        let offset = match error.offset() {
            Some(lower_offset) => lower_offset,
            None => offset,
        };
        Self {
            code: DocumentErrorCode::TargetConstructionFailure,
            category: DocumentErrorCategory::Internal,
            recoverability: DocumentRecoverability::DoNotRetry,
            diagnostic_id: "RPE-DOCUMENT-0011",
            reference: Some(reference),
            offset: Some(offset),
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: None,
            },
        }
    }

    pub(crate) const fn from_attestation_object(
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        let code = match error.category() {
            ObjectErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
            _ => match error.code() {
                ObjectErrorCode::Cancelled => DocumentErrorCode::Cancelled,
                ObjectErrorCode::UnsupportedIndirectLength => {
                    DocumentErrorCode::UnsupportedObjectFraming
                }
                ObjectErrorCode::SnapshotMismatch => DocumentErrorCode::SourceSnapshotMismatch,
                ObjectErrorCode::SourceFailure => DocumentErrorCode::SourceFailure,
                ObjectErrorCode::UnexpectedEndOfSource => DocumentErrorCode::UnexpectedEndOfSource,
                ObjectErrorCode::InvalidTarget
                | ObjectErrorCode::InternalState
                | ObjectErrorCode::JobAlreadyComplete => DocumentErrorCode::InternalState,
                _ => DocumentErrorCode::ObjectAttestationFailure,
            },
        };
        let category = match error.category() {
            ObjectErrorCategory::Configuration => DocumentErrorCategory::Configuration,
            ObjectErrorCategory::Source => DocumentErrorCategory::Source,
            ObjectErrorCategory::Syntax => DocumentErrorCategory::Syntax,
            ObjectErrorCategory::Unsupported => DocumentErrorCategory::Unsupported,
            ObjectErrorCategory::Resource => DocumentErrorCategory::Resource,
            ObjectErrorCategory::Cancellation => DocumentErrorCategory::Cancellation,
            ObjectErrorCategory::Internal => DocumentErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            ObjectRecoverability::CorrectConfiguration => {
                DocumentRecoverability::CorrectConfiguration
            }
            ObjectRecoverability::CorrectInput => DocumentRecoverability::CorrectInput,
            ObjectRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
            ObjectRecoverability::RetrySource => DocumentRecoverability::RetrySource,
            ObjectRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            ObjectRecoverability::UseSupportedFeature => {
                DocumentRecoverability::UseSupportedFeature
            }
            ObjectRecoverability::AbandonOperation => DocumentRecoverability::AbandonOperation,
            ObjectRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
        };
        let base = Self::for_code(
            code,
            match error.reference() {
                Some(lower_reference) => Some(lower_reference),
                None => Some(reference),
            },
            match error.offset() {
                Some(lower_offset) => Some(lower_offset),
                None => Some(offset),
            },
        );
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: None,
            },
            ..base
        }
    }

    pub(crate) const fn aggregate_object_resource(
        kind: DocumentLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        error: ObjectError,
        reference: ObjectRef,
        offset: u64,
    ) -> Self {
        Self {
            code: DocumentErrorCode::ResourceLimit,
            category: DocumentErrorCategory::Resource,
            recoverability: DocumentRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-DOCUMENT-0002",
            reference: match error.reference() {
                Some(lower_reference) => Some(lower_reference),
                None => Some(reference),
            },
            offset: match error.offset() {
                Some(lower_offset) => Some(lower_offset),
                None => Some(offset),
            },
            detail: DocumentErrorDetail::Object {
                error,
                aggregate_limit: Some(DocumentLimit::new(kind, limit, consumed, attempted)),
            },
        }
    }

    pub(crate) const fn from_source(error: SourceError, offset: u64) -> Self {
        let code = match error.category() {
            SourceErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
            SourceErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
            _ => DocumentErrorCode::SourceFailure,
        };
        let category = match error.category() {
            SourceErrorCategory::Input | SourceErrorCategory::Lifecycle => {
                DocumentErrorCategory::Configuration
            }
            SourceErrorCategory::Integrity | SourceErrorCategory::Availability => {
                DocumentErrorCategory::Source
            }
            SourceErrorCategory::Resource => DocumentErrorCategory::Resource,
            SourceErrorCategory::Internal => DocumentErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SourceRecoverability::CorrectInput => DocumentRecoverability::CorrectConfiguration,
            SourceRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
            SourceRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            SourceRecoverability::RetrySource => DocumentRecoverability::RetrySource,
            SourceRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
        };
        let base = Self::for_code(code, None, Some(offset));
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Source(error),
            ..base
        }
    }

    pub(crate) const fn from_header_syntax(error: SyntaxError) -> Self {
        let code = match error.category() {
            SyntaxErrorCategory::Resource => DocumentErrorCode::ResourceLimit,
            SyntaxErrorCategory::Integrity => DocumentErrorCode::SourceSnapshotMismatch,
            SyntaxErrorCategory::Cancellation => DocumentErrorCode::Cancelled,
            SyntaxErrorCategory::Internal => DocumentErrorCode::InternalState,
            SyntaxErrorCategory::Configuration | SyntaxErrorCategory::Syntax => {
                DocumentErrorCode::InvalidDocumentHeader
            }
        };
        let category = match error.category() {
            SyntaxErrorCategory::Configuration => DocumentErrorCategory::Configuration,
            SyntaxErrorCategory::Syntax => DocumentErrorCategory::Syntax,
            SyntaxErrorCategory::Resource => DocumentErrorCategory::Resource,
            SyntaxErrorCategory::Integrity => DocumentErrorCategory::Source,
            SyntaxErrorCategory::Cancellation => DocumentErrorCategory::Cancellation,
            SyntaxErrorCategory::Internal => DocumentErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SyntaxRecoverability::CorrectConfiguration => {
                DocumentRecoverability::CorrectConfiguration
            }
            SyntaxRecoverability::CorrectInput => DocumentRecoverability::CorrectInput,
            SyntaxRecoverability::ReduceWorkload => DocumentRecoverability::ReduceWorkload,
            SyntaxRecoverability::ReopenSource => DocumentRecoverability::ReopenSource,
            SyntaxRecoverability::AbandonOperation => DocumentRecoverability::AbandonOperation,
            SyntaxRecoverability::DoNotRetry => DocumentRecoverability::DoNotRetry,
        };
        let base = Self::for_code(code, None, error.offset());
        Self {
            category,
            recoverability,
            detail: DocumentErrorDetail::Syntax(error),
            ..base
        }
    }

    /// Returns the machine-readable document-composition failure code.
    pub const fn code(self) -> DocumentErrorCode {
        self.code
    }

    /// Returns the stable coarse category.
    pub const fn category(self) -> DocumentErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> DocumentRecoverability {
        self.recoverability
    }

    /// Returns the stable project diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the involved object reference, when one exists.
    pub const fn reference(self) -> Option<ObjectRef> {
        self.reference
    }

    /// Returns the involved absolute source offset, when known.
    pub const fn offset(self) -> Option<u64> {
        self.offset
    }

    /// Returns structured deterministic limit context, when applicable.
    pub const fn limit(self) -> Option<DocumentLimit> {
        match self.detail {
            DocumentErrorDetail::Limit(limit) => Some(limit),
            DocumentErrorDetail::Object {
                aggregate_limit, ..
            } => aggregate_limit,
            DocumentErrorDetail::None
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the code of the complete retained lower object error, when applicable.
    pub const fn object_error_code(self) -> Option<ObjectErrorCode> {
        match self.detail {
            DocumentErrorDetail::Object { error, .. } => Some(error.code()),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the complete retained lower object error, when applicable.
    pub const fn object_error(self) -> Option<ObjectError> {
        match self.detail {
            DocumentErrorDetail::Object { error, .. } => Some(error),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Source(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower byte-source error, directly or through an object job.
    pub const fn source_error(self) -> Option<SourceError> {
        match self.detail {
            DocumentErrorDetail::Source(error) => Some(error),
            DocumentErrorDetail::Object { error, .. } => error.source_error(),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Syntax(_) => None,
        }
    }

    /// Returns the retained lower syntax error, directly or through an object job.
    pub const fn syntax_error(self) -> Option<SyntaxError> {
        match self.detail {
            DocumentErrorDetail::Syntax(error) => Some(error),
            DocumentErrorDetail::Object { error, .. } => error.syntax_error(),
            DocumentErrorDetail::None
            | DocumentErrorDetail::Limit(_)
            | DocumentErrorDetail::Source(_) => None,
        }
    }
}

impl fmt::Debug for DocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("reference", &self.reference)
            .field("offset", &self.offset)
            .field("detail", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for DocumentError {
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
        if let Some(limit) = self.limit() {
            write!(
                formatter,
                " limit_kind={:?} limit={} consumed={} attempted={}",
                limit.kind, limit.limit, limit.consumed, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for DocumentError {}

#[cfg(test)]
mod tests {
    use pdf_rs_bytes::{
        SourceIdentity, SourceRevision, SourceSnapshot, SourceStableId, SourceValidator,
        SourceValidatorKind,
    };
    use pdf_rs_object::IndirectObjectTarget;

    use super::*;

    #[test]
    fn lower_object_offset_survives_target_error_conversion() {
        let reference = ObjectRef::new(1, 0).unwrap();
        let snapshot = SourceSnapshot::new(
            SourceIdentity::new(SourceStableId::new([0x37; 32]), SourceRevision::new(1)),
            Some(20),
            SourceValidator::new(SourceValidatorKind::FrozenResponse, [0x91; 32]),
        );
        let lower = IndirectObjectTarget::new(snapshot, reference, 1, 12, 10).unwrap_err();
        assert_eq!(lower.offset(), Some(12));

        let error = DocumentError::from_object(lower, reference, 1);
        assert_eq!(error.code(), DocumentErrorCode::TargetConstructionFailure);
        assert_eq!(error.offset(), Some(12));
        assert_eq!(error.object_error_code(), Some(lower.code()));
        assert!(error.to_string().contains("at byte 12"));
        assert!(!format!("{error:?}").contains("FrozenResponse"));
    }
}
