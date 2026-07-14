use std::error::Error;
use std::fmt;

use pdf_rs_object::{ObjectError, ObjectErrorCode};
use pdf_rs_syntax::ObjectRef;

/// Deterministic candidate-index budget that rejected work.
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
}

/// Structured candidate-index resource-limit context without document bytes.
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

/// Stable machine-readable candidate-revision index failure.
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
}

/// Coarse candidate-revision index failure category.
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
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal checked invariant failure.
    Internal,
}

/// Stable recovery policy for a candidate-revision index failure.
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
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DocumentErrorDetail {
    None,
    Limit(DocumentLimit),
    Object(ObjectErrorCode),
}

/// Source-redacted candidate-revision index error with stable policy metadata.
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
            detail: DocumentErrorDetail::Object(error.code()),
        }
    }

    /// Returns the machine-readable document-index failure code.
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
            DocumentErrorDetail::None | DocumentErrorDetail::Object(_) => None,
        }
    }

    /// Returns the retained lower object-target error code, when applicable.
    ///
    /// Only the stable code is retained so reporting malformed input never requires a secondary
    /// allocation and the document error remains cheap to return by value.
    pub const fn object_error_code(self) -> Option<ObjectErrorCode> {
        match self.detail {
            DocumentErrorDetail::Object(code) => Some(code),
            DocumentErrorDetail::None | DocumentErrorDetail::Limit(_) => None,
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
        if let DocumentErrorDetail::Limit(limit) = self.detail {
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
