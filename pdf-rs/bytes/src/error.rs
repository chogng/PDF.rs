use std::error::Error;
use std::fmt;

/// Deterministic byte-layer budget that rejected an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceLimitKind {
    /// Immutable source total length.
    InputBytes,
    /// One exact request or supplied response range.
    ReadBytes,
    /// Unique bytes retained by the store.
    CachedBytes,
    /// Peak retained plus in-flight/coalescing bytes owned by the store operation.
    ResidentBytes,
    /// Disjoint cached backing segments.
    Segments,
    /// Pending and retained terminal data tickets.
    Tickets,
    /// Job/checkpoint subscriptions on one ticket.
    TicketSubscribers,
    /// Job/checkpoint subscriptions across the store.
    TotalSubscriptions,
    /// Disjoint missing ranges emitted for one ticket.
    MissingRanges,
    /// Fallible allocation for bounded metadata or bytes.
    Allocation,
}

/// Structured resource-limit context without source content or host paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLimit {
    kind: SourceLimitKind,
    limit: u64,
    attempted: u64,
}

impl SourceLimit {
    pub(crate) const fn new(kind: SourceLimitKind, limit: u64, attempted: u64) -> Self {
        Self {
            kind,
            limit,
            attempted,
        }
    }

    /// Returns the budget dimension that was exceeded.
    pub const fn kind(self) -> SourceLimitKind {
        self.kind
    }

    /// Returns the configured deterministic ceiling.
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// Returns the rejected count or byte total.
    pub const fn attempted(self) -> u64 {
        self.attempted
    }
}

/// Stable machine-readable failures produced by the byte layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceErrorCode {
    /// A range has zero length or its exclusive end overflows `u64`.
    InvalidRange,
    /// Configured Range-store limits are zero, inconsistent, or above hard ceilings.
    InvalidLimits,
    /// Response bytes do not exactly match the declared byte range length.
    ResponseLengthMismatch,
    /// A response extends beyond the immutable known source length.
    ResponseOutOfBounds,
    /// Response identity or validator differs from the bound snapshot.
    SourceChanged,
    /// Overlapping responses for one snapshot contain different bytes.
    ConflictingBytes,
    /// A configured deterministic byte, segment, ticket, or subscriber budget was reached.
    ResourceLimit,
    /// The requested data ticket is not retained by this store.
    UnknownTicket,
    /// A terminal ticket was asked to transition again.
    TicketAlreadyTerminal,
    /// A pending ticket was asked to be released as a retained terminal record.
    TicketNotTerminal,
    /// One job tried to attach two different checkpoints to the same pending ticket.
    CheckpointConflict,
    /// A terminal ticket still owns subscriptions that runtime has not taken.
    SubscriptionsNotTaken,
    /// The host source operation failed without changing snapshot identity.
    SourceUnavailable,
    /// Internal synchronization state could not be accessed safely.
    InternalState,
}

/// Coarse policy category for [`SourceErrorCode`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceErrorCategory {
    /// Caller-supplied range, limit, response geometry, or handle is invalid.
    Input,
    /// Immutable source identity or byte integrity no longer holds.
    Integrity,
    /// A deterministic resource budget was exhausted.
    Resource,
    /// A ticket lifecycle transition is invalid.
    Lifecycle,
    /// The bound source is temporarily unavailable without an integrity change.
    Availability,
    /// The implementation cannot safely continue from internal state.
    Internal,
}

/// Stable recovery policy for byte-layer failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRecoverability {
    /// Correct the request, response, limits, or ticket handle.
    CorrectInput,
    /// Reopen the document against a newly bound immutable source snapshot.
    ReopenSource,
    /// Reduce requested work or increase an approved deterministic budget.
    ReduceWorkload,
    /// Retry through host policy while preserving the same immutable snapshot.
    RetrySource,
    /// Repeating the same operation is not an approved recovery action.
    DoNotRetry,
}

/// Source-redacted, stable byte-layer failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceError {
    code: SourceErrorCode,
    category: SourceErrorCategory,
    recoverability: SourceRecoverability,
    diagnostic_id: &'static str,
    limit: Option<SourceLimit>,
}

impl SourceError {
    /// Constructs a source-redacted host availability failure for ticket completion.
    pub const fn source_unavailable() -> Self {
        Self::for_code(SourceErrorCode::SourceUnavailable)
    }

    pub(crate) const fn for_code(code: SourceErrorCode) -> Self {
        let (category, recoverability, diagnostic_id) = match code {
            SourceErrorCode::InvalidRange => (
                SourceErrorCategory::Input,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0001",
            ),
            SourceErrorCode::InvalidLimits => (
                SourceErrorCategory::Input,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0002",
            ),
            SourceErrorCode::ResponseLengthMismatch => (
                SourceErrorCategory::Input,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0003",
            ),
            SourceErrorCode::ResponseOutOfBounds => (
                SourceErrorCategory::Input,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0004",
            ),
            SourceErrorCode::SourceChanged => (
                SourceErrorCategory::Integrity,
                SourceRecoverability::ReopenSource,
                "RPE-BYTES-0005",
            ),
            SourceErrorCode::ConflictingBytes => (
                SourceErrorCategory::Integrity,
                SourceRecoverability::ReopenSource,
                "RPE-BYTES-0006",
            ),
            SourceErrorCode::ResourceLimit => (
                SourceErrorCategory::Resource,
                SourceRecoverability::ReduceWorkload,
                "RPE-BYTES-0007",
            ),
            SourceErrorCode::UnknownTicket => (
                SourceErrorCategory::Input,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0008",
            ),
            SourceErrorCode::TicketAlreadyTerminal => (
                SourceErrorCategory::Lifecycle,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0009",
            ),
            SourceErrorCode::TicketNotTerminal => (
                SourceErrorCategory::Lifecycle,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0010",
            ),
            SourceErrorCode::CheckpointConflict => (
                SourceErrorCategory::Lifecycle,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0011",
            ),
            SourceErrorCode::SubscriptionsNotTaken => (
                SourceErrorCategory::Lifecycle,
                SourceRecoverability::CorrectInput,
                "RPE-BYTES-0012",
            ),
            SourceErrorCode::SourceUnavailable => (
                SourceErrorCategory::Availability,
                SourceRecoverability::RetrySource,
                "RPE-BYTES-0013",
            ),
            SourceErrorCode::InternalState => (
                SourceErrorCategory::Internal,
                SourceRecoverability::DoNotRetry,
                "RPE-BYTES-0014",
            ),
        };
        Self {
            code,
            category,
            recoverability,
            diagnostic_id,
            limit: None,
        }
    }

    pub(crate) const fn resource(kind: SourceLimitKind, limit: u64, attempted: u64) -> Self {
        Self {
            code: SourceErrorCode::ResourceLimit,
            category: SourceErrorCategory::Resource,
            recoverability: SourceRecoverability::ReduceWorkload,
            diagnostic_id: "RPE-BYTES-0007",
            limit: Some(SourceLimit::new(kind, limit, attempted)),
        }
    }

    /// Returns the exact machine-readable failure code.
    pub const fn code(self) -> SourceErrorCode {
        self.code
    }

    /// Returns the stable coarse failure category.
    pub const fn category(self) -> SourceErrorCategory {
        self.category
    }

    /// Returns the approved recovery policy.
    pub const fn recoverability(self) -> SourceRecoverability {
        self.recoverability
    }

    /// Returns the stable project diagnostic identifier.
    pub const fn diagnostic_id(self) -> &'static str {
        self.diagnostic_id
    }

    /// Returns structured limit context for deterministic resource failures.
    pub const fn limit(self) -> Option<SourceLimit> {
        self.limit
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({:?})", self.diagnostic_id, self.code)?;
        if let Some(limit) = self.limit {
            write!(
                formatter,
                " limit_kind={:?} limit={} attempted={}",
                limit.kind, limit.limit, limit.attempted
            )?;
        }
        Ok(())
    }
}

impl Error for SourceError {}
