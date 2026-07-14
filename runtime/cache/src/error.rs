use std::error::Error;
use std::fmt;

use pdf_rs_document::{DocumentError, ResolvedReference};
use pdf_rs_syntax::ObjectRef;

use crate::ReadyStoreSessionId;

/// Owner scope charged by one Ready-store budget decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyStoreScope {
    /// The charged resource belongs exclusively to one document session.
    Session(ReadyStoreSessionId),
}

/// Deterministic Ready-store budget that rejected work or allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyStoreLimitKind {
    /// One value-owned proof-bearing result.
    ValueBytes,
    /// Retained metadata backing plus value heap.
    ResidentBytes,
    /// Fallible preallocation for the fixed metadata capacity.
    Allocation,
}

/// Structured resource-limit context without document semantic values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreLimit {
    kind: ReadyStoreLimitKind,
    limit: u64,
    consumed: u64,
    attempted: u64,
    scope: ReadyStoreScope,
    reference: Option<ObjectRef>,
}

impl ReadyStoreLimit {
    pub(crate) const fn new(
        kind: ReadyStoreLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        scope: ReadyStoreScope,
        reference: Option<ObjectRef>,
    ) -> Self {
        Self {
            kind,
            limit,
            consumed,
            attempted,
            scope,
            reference,
        }
    }

    /// Returns the rejected budget dimension.
    pub const fn kind(self) -> ReadyStoreLimitKind {
        self.kind
    }

    /// Returns the configured ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the amount retained before the rejected operation.
    pub const fn consumed(self) -> u64 {
        self.consumed
    }

    /// Returns the amount the rejected operation would add or require.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }

    /// Returns the session owner charged by the rejected operation.
    pub const fn scope(self) -> ReadyStoreScope {
        self.scope
    }

    /// Returns the safe exact root associated with value admission, when any.
    pub const fn reference(self) -> Option<ObjectRef> {
        self.reference
    }
}

/// Stable machine-readable Ready-store failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyStoreErrorCode {
    /// The configured limits are zero, inconsistent, or above fixed ceilings.
    InvalidLimits,
    /// Fixed entry metadata could not be allocated within the owner budget.
    Allocation,
    /// A deterministic retained-memory budget was exhausted.
    ResourceLimit,
    /// The caller cancelled before a lookup or admission could publish.
    Cancelled,
    /// The document value could not produce checked footprint evidence.
    InvalidValueFootprint,
    /// A checked owner/accounting invariant could not be maintained.
    InternalState,
}

/// Coarse Ready-store failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyStoreErrorCategory {
    /// Invalid caller-owned configuration.
    Configuration,
    /// Deterministic retained-memory or allocation exhaustion.
    Resource,
    /// Normal runtime cancellation.
    Cancellation,
    /// Internal or lower proof-evidence invariant failure.
    Internal,
}

/// Stable recovery policy for a Ready-store failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadyStoreRecoverability {
    /// Correct the configuration before retrying.
    CorrectConfiguration,
    /// Reduce retained work or select an approved larger budget.
    ReduceWorkload,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Source-redacted Ready-store error with stable policy metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyStoreError {
    code: ReadyStoreErrorCode,
    category: ReadyStoreErrorCategory,
    recoverability: ReadyStoreRecoverability,
    diagnostic_id: &'static str,
    limit: Option<ReadyStoreLimit>,
    document_error: Option<DocumentError>,
}

impl ReadyStoreError {
    pub(crate) const fn for_code(code: ReadyStoreErrorCode) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            ReadyStoreErrorCode::InvalidLimits => (
                ReadyStoreErrorCategory::Configuration,
                ReadyStoreRecoverability::CorrectConfiguration,
                "RPE-CACHE-0001",
            ),
            ReadyStoreErrorCode::Allocation => (
                ReadyStoreErrorCategory::Resource,
                ReadyStoreRecoverability::ReduceWorkload,
                "RPE-CACHE-0002",
            ),
            ReadyStoreErrorCode::ResourceLimit => (
                ReadyStoreErrorCategory::Resource,
                ReadyStoreRecoverability::ReduceWorkload,
                "RPE-CACHE-0003",
            ),
            ReadyStoreErrorCode::Cancelled => (
                ReadyStoreErrorCategory::Cancellation,
                ReadyStoreRecoverability::AbandonOperation,
                "RPE-CACHE-0004",
            ),
            ReadyStoreErrorCode::InvalidValueFootprint => (
                ReadyStoreErrorCategory::Internal,
                ReadyStoreRecoverability::DoNotRetry,
                "RPE-CACHE-0005",
            ),
            ReadyStoreErrorCode::InternalState => (
                ReadyStoreErrorCategory::Internal,
                ReadyStoreRecoverability::DoNotRetry,
                "RPE-CACHE-0006",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            limit: None,
            document_error: None,
        }
    }

    pub(crate) const fn resource(
        kind: ReadyStoreLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
        scope: ReadyStoreScope,
        reference: Option<ObjectRef>,
    ) -> Self {
        let mut error = Self::for_code(ReadyStoreErrorCode::ResourceLimit);
        error.limit = Some(ReadyStoreLimit::new(
            kind, limit, consumed, attempted, scope, reference,
        ));
        error
    }

    pub(crate) const fn allocation(limit: u64, attempted: u64, scope: ReadyStoreScope) -> Self {
        let mut error = Self::for_code(ReadyStoreErrorCode::Allocation);
        error.limit = Some(ReadyStoreLimit::new(
            ReadyStoreLimitKind::Allocation,
            limit,
            0,
            attempted,
            scope,
            None,
        ));
        error
    }

    pub(crate) const fn from_footprint(error: DocumentError) -> Self {
        let mut result = Self::for_code(ReadyStoreErrorCode::InvalidValueFootprint);
        result.document_error = Some(error);
        result
    }

    /// Returns the stable machine-readable code.
    pub const fn code(self) -> ReadyStoreErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> ReadyStoreErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> ReadyStoreRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns structured deterministic limit context, when applicable.
    pub const fn limit(self) -> Option<ReadyStoreLimit> {
        self.limit
    }

    /// Returns the complete lower footprint error, when applicable.
    pub const fn document_error(self) -> Option<DocumentError> {
        self.document_error
    }
}

impl fmt::Display for ReadyStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)
    }
}

impl Error for ReadyStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.document_error
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

/// Admission failure that returns ownership of the successful move-only value.
pub struct ReadyStoreAdmissionError {
    error: ReadyStoreError,
    value: ResolvedReference,
}

impl ReadyStoreAdmissionError {
    pub(crate) const fn new(error: ReadyStoreError, value: ResolvedReference) -> Self {
        Self { error, value }
    }

    /// Returns the stable Ready-store failure by copy.
    pub const fn error(&self) -> ReadyStoreError {
        self.error
    }

    /// Returns the successful value to its caller without cloning or detaching proof.
    pub fn into_value(self) -> ResolvedReference {
        self.value
    }
}

impl fmt::Debug for ReadyStoreAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadyStoreAdmissionError")
            .field("error", &self.error)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ReadyStoreAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for ReadyStoreAdmissionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}
