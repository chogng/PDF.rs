use std::error::Error;
use std::fmt;

use pdf_rs_cache::{
    ReadyStoreError, ReadyStoreErrorCategory, ReadyStoreErrorCode, ReadyStoreRecoverability,
    ReadyStoreSessionId,
};
use pdf_rs_document::ResolvedReference;

/// Stable machine-readable failure at the Ready-session ownership boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadySessionErrorCode {
    /// The owner has completed close and cannot accept cache operations.
    SessionClosed,
    /// The active Ready store rejected the operation with the enclosed code.
    ReadyStore(ReadyStoreErrorCode),
}

/// Coarse Ready-session failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadySessionErrorCategory {
    /// An operation targeted an owner whose close already completed.
    Lifecycle,
    /// Invalid caller-owned cache configuration.
    Configuration,
    /// Deterministic retained-memory or allocation exhaustion.
    Resource,
    /// Normal runtime cancellation while the owner was active.
    Cancellation,
    /// An internal or lower proof-evidence invariant failed.
    Internal,
}

/// Stable recovery policy for a Ready-session failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadySessionRecoverability {
    /// Open a distinct session; the closed identity cannot become active again.
    OpenNewSession,
    /// Correct the cache configuration before constructing an owner.
    CorrectConfiguration,
    /// Reduce retained work or select an approved larger budget.
    ReduceWorkload,
    /// Treat cancellation as a completed abandoned operation.
    AbandonOperation,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Source-redacted Ready-session error with complete lower cache evidence.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ReadySessionError {
    session_id: ReadyStoreSessionId,
    code: ReadySessionErrorCode,
    category: ReadySessionErrorCategory,
    recoverability: ReadySessionRecoverability,
    diagnostic_id: &'static str,
    ready_store_error: Option<ReadyStoreError>,
}

impl ReadySessionError {
    pub(crate) const fn session_closed(session_id: ReadyStoreSessionId) -> Self {
        Self {
            session_id,
            code: ReadySessionErrorCode::SessionClosed,
            category: ReadySessionErrorCategory::Lifecycle,
            recoverability: ReadySessionRecoverability::OpenNewSession,
            diagnostic_id: "RPE-SESSION-0001",
            ready_store_error: None,
        }
    }

    pub(crate) const fn from_ready_store(
        session_id: ReadyStoreSessionId,
        error: ReadyStoreError,
    ) -> Self {
        let category = match error.category() {
            ReadyStoreErrorCategory::Configuration => ReadySessionErrorCategory::Configuration,
            ReadyStoreErrorCategory::Resource => ReadySessionErrorCategory::Resource,
            ReadyStoreErrorCategory::Cancellation => ReadySessionErrorCategory::Cancellation,
            ReadyStoreErrorCategory::Internal => ReadySessionErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            ReadyStoreRecoverability::CorrectConfiguration => {
                ReadySessionRecoverability::CorrectConfiguration
            }
            ReadyStoreRecoverability::ReduceWorkload => ReadySessionRecoverability::ReduceWorkload,
            ReadyStoreRecoverability::AbandonOperation => {
                ReadySessionRecoverability::AbandonOperation
            }
            ReadyStoreRecoverability::DoNotRetry => ReadySessionRecoverability::DoNotRetry,
        };
        Self {
            session_id,
            code: ReadySessionErrorCode::ReadyStore(error.code()),
            category,
            recoverability,
            diagnostic_id: "RPE-SESSION-0002",
            ready_store_error: Some(error),
        }
    }

    /// Returns the opaque session identity associated with the failed operation.
    pub const fn session_id(self) -> ReadyStoreSessionId {
        self.session_id
    }

    /// Returns the stable machine-readable code.
    pub const fn code(self) -> ReadySessionErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> ReadySessionErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> ReadySessionRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier for this ownership boundary.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns the complete lower cache error when an active store failed.
    pub const fn ready_store_error(self) -> Option<ReadyStoreError> {
        self.ready_store_error
    }
}

impl fmt::Debug for ReadySessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadySessionError")
            .field("session_id", &self.session_id)
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("ready_store_error", &self.ready_store_error)
            .finish()
    }
}

impl fmt::Display for ReadySessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)
    }
}

impl Error for ReadySessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.ready_store_error
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

/// Admission failure that returns the successful move-only document value.
pub struct ReadySessionAdmissionError {
    error: ReadySessionError,
    value: ResolvedReference,
}

impl ReadySessionAdmissionError {
    pub(crate) const fn new(error: ReadySessionError, value: ResolvedReference) -> Self {
        Self { error, value }
    }

    /// Returns the stable ownership-boundary failure by copy.
    pub const fn error(&self) -> ReadySessionError {
        self.error
    }

    /// Returns the successful value without cloning or detaching its proof.
    pub fn into_value(self) -> ResolvedReference {
        self.value
    }
}

impl fmt::Debug for ReadySessionAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadySessionAdmissionError")
            .field("error", &self.error)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for ReadySessionAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl Error for ReadySessionAdmissionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}
