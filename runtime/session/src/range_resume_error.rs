use std::error::Error;
use std::fmt;

use pdf_rs_bytes::{SourceError, SourceErrorCategory, SourceErrorCode, SourceRecoverability};

/// Stable machine-readable failure at the Range-resume ownership boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeResumeErrorCode {
    /// The arbiter completed close and cannot accept new work.
    Closed,
    /// The immutable source snapshot changed and the arbiter released its store.
    SourceChanged,
    /// Registration targeted a ticket that was not pending.
    TicketNotPending,
    /// One job attempted to own incompatible pending or queued resume state.
    RegistrationConflict,
    /// The bounded registration and requeue metadata budget was reached.
    RegistrationLimit,
    /// A terminal store subscription had no matching runtime registration.
    UnregisteredSubscription,
    /// A prior invariant failure made the arbiter permanently unusable.
    ArbiterFailed,
    /// The byte store rejected an otherwise nonterminal operation.
    Source(SourceErrorCode),
}

/// Coarse Range-resume failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeResumeErrorCategory {
    /// The lifecycle phase rejects the operation.
    Lifecycle,
    /// The immutable source snapshot can no longer be trusted.
    Integrity,
    /// Caller-owned ticket or registration state is inconsistent.
    Input,
    /// A deterministic retained-metadata budget was reached.
    Resource,
    /// The host source operation failed without changing the snapshot.
    Availability,
    /// Runtime or lower synchronization state cannot safely continue.
    Internal,
}

/// Stable recovery policy for a Range-resume failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeResumeRecoverability {
    /// Open a distinct session because a closed owner cannot become active again.
    OpenNewSession,
    /// Reopen against a newly bound immutable source snapshot.
    ReopenSource,
    /// Correct runtime call ordering or registration identity.
    CorrectRuntimeState,
    /// Drain queued requeues or reduce concurrent pending work.
    ReduceWorkload,
    /// Retry through host policy while retaining the same immutable snapshot.
    RetrySource,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Structured bounded-registration context without source bytes or host paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeResumeLimit {
    limit: usize,
    attempted: usize,
}

impl RangeResumeLimit {
    pub(crate) const fn new(limit: usize, attempted: usize) -> Self {
        Self { limit, attempted }
    }

    /// Returns the maximum retained pending plus queued registration count.
    pub const fn limit(self) -> usize {
        self.limit
    }

    /// Returns the rejected registration count.
    pub const fn attempted(self) -> usize {
        self.attempted
    }
}

/// Source-redacted Range-resume failure with complete lower byte evidence.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RangeResumeError {
    code: RangeResumeErrorCode,
    category: RangeResumeErrorCategory,
    recoverability: RangeResumeRecoverability,
    diagnostic_id: &'static str,
    source_error: Option<SourceError>,
    limit: Option<RangeResumeLimit>,
}

impl RangeResumeError {
    pub(crate) const fn closed() -> Self {
        Self::new(
            RangeResumeErrorCode::Closed,
            RangeResumeErrorCategory::Lifecycle,
            RangeResumeRecoverability::OpenNewSession,
            "RPE-SESSION-0003",
        )
    }

    pub(crate) const fn source_changed(source_error: Option<SourceError>) -> Self {
        Self {
            code: RangeResumeErrorCode::SourceChanged,
            category: RangeResumeErrorCategory::Integrity,
            recoverability: RangeResumeRecoverability::ReopenSource,
            diagnostic_id: "RPE-SESSION-0004",
            source_error,
            limit: None,
        }
    }

    pub(crate) const fn ticket_not_pending() -> Self {
        Self::new(
            RangeResumeErrorCode::TicketNotPending,
            RangeResumeErrorCategory::Input,
            RangeResumeRecoverability::CorrectRuntimeState,
            "RPE-SESSION-0005",
        )
    }

    pub(crate) const fn registration_conflict() -> Self {
        Self::new(
            RangeResumeErrorCode::RegistrationConflict,
            RangeResumeErrorCategory::Input,
            RangeResumeRecoverability::CorrectRuntimeState,
            "RPE-SESSION-0006",
        )
    }

    pub(crate) const fn registration_limit(limit: usize, attempted: usize) -> Self {
        Self {
            code: RangeResumeErrorCode::RegistrationLimit,
            category: RangeResumeErrorCategory::Resource,
            recoverability: RangeResumeRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-SESSION-0007",
            source_error: None,
            limit: Some(RangeResumeLimit::new(limit, attempted)),
        }
    }

    pub(crate) const fn unregistered_subscription() -> Self {
        Self::new(
            RangeResumeErrorCode::UnregisteredSubscription,
            RangeResumeErrorCategory::Internal,
            RangeResumeRecoverability::DoNotRetry,
            "RPE-SESSION-0008",
        )
    }

    pub(crate) const fn arbiter_failed() -> Self {
        Self::new(
            RangeResumeErrorCode::ArbiterFailed,
            RangeResumeErrorCategory::Internal,
            RangeResumeRecoverability::DoNotRetry,
            "RPE-SESSION-0009",
        )
    }

    pub(crate) const fn from_source(error: SourceError) -> Self {
        if matches!(error.category(), SourceErrorCategory::Integrity) {
            return Self::source_changed(Some(error));
        }
        let category = match error.category() {
            SourceErrorCategory::Input | SourceErrorCategory::Lifecycle => {
                RangeResumeErrorCategory::Input
            }
            SourceErrorCategory::Integrity => RangeResumeErrorCategory::Integrity,
            SourceErrorCategory::Resource => RangeResumeErrorCategory::Resource,
            SourceErrorCategory::Availability => RangeResumeErrorCategory::Availability,
            SourceErrorCategory::Internal => RangeResumeErrorCategory::Internal,
        };
        let recoverability = match error.recoverability() {
            SourceRecoverability::CorrectInput => RangeResumeRecoverability::CorrectRuntimeState,
            SourceRecoverability::ReopenSource => RangeResumeRecoverability::ReopenSource,
            SourceRecoverability::ReduceWorkload => RangeResumeRecoverability::ReduceWorkload,
            SourceRecoverability::RetrySource => RangeResumeRecoverability::RetrySource,
            SourceRecoverability::DoNotRetry => RangeResumeRecoverability::DoNotRetry,
        };
        Self {
            code: RangeResumeErrorCode::Source(error.code()),
            category,
            recoverability,
            diagnostic_id: "RPE-SESSION-0010",
            source_error: Some(error),
            limit: None,
        }
    }

    const fn new(
        code: RangeResumeErrorCode,
        category: RangeResumeErrorCategory,
        recoverability: RangeResumeRecoverability,
        diagnostic_id: &'static str,
    ) -> Self {
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            source_error: None,
            limit: None,
        }
    }

    /// Returns the stable machine-readable code.
    pub const fn code(self) -> RangeResumeErrorCode {
        self.code
    }

    /// Returns the coarse failure category.
    pub const fn category(self) -> RangeResumeErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> RangeResumeRecoverability {
        self.recoverability
    }

    /// Returns the stable diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns complete lower byte evidence when the store rejected an operation.
    pub const fn source_error(self) -> Option<SourceError> {
        self.source_error
    }

    /// Returns structured metadata-budget context when registration was rejected.
    pub const fn limit(self) -> Option<RangeResumeLimit> {
        self.limit
    }
}

impl fmt::Debug for RangeResumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RangeResumeError")
            .field("code", &self.code)
            .field("category", &self.category)
            .field("recoverability", &self.recoverability)
            .field("diagnostic_id", &self.diagnostic_id)
            .field("source_error", &self.source_error)
            .field("limit", &self.limit)
            .finish()
    }
}

impl fmt::Display for RangeResumeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(limit) = self.limit {
            write!(
                formatter,
                " registration_limit={} attempted={}",
                limit.limit, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for RangeResumeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source_error
            .as_ref()
            .map(|error| error as &(dyn Error + 'static))
    }
}
