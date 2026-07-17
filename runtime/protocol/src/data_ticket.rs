use std::fmt;
use std::sync::Arc;

use crate::{
    ByteRange, Correlation, DataAttachmentRole, DataTicket, FailDataCommand,
    MAX_DATA_SEGMENT_BYTES, MAX_DATA_TICKET_BYTES, NEED_DATA_EVENT_RANGES_MAX_COUNT, NeedDataEvent,
    PROVIDE_DATA_COMMAND_SEGMENTS_MAX_COUNT, ProtocolError, ProtocolErrorCode, ProvideDataCommand,
    RequestId, SessionId, SourceDescriptor, SourceFailureCode, SourceIdentity, WorkerId,
};

/// Hard ceiling for the explicitly configured outstanding data-ticket capacity.
///
/// This is a runtime allocation bound, not a wire-schema count. Each ledger chooses a non-zero
/// per-table capacity for session bindings and outstanding tickets no greater than this ceiling.
pub const MAX_OUTSTANDING_DATA_TICKETS: usize = 1_024;

#[derive(Clone, Copy, Eq, PartialEq)]
struct DataTicketKey {
    worker: WorkerId,
    session: SessionId,
    ticket: DataTicket,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct SessionSourceKey {
    worker: WorkerId,
    session: SessionId,
}

enum SessionSourceState {
    Active(SourceDescriptor),
    SourceChangeObserved(SourceDescriptor),
    SourceChanged,
}

struct SessionSourceBinding {
    key: SessionSourceKey,
    state: SessionSourceState,
}

struct OutstandingDataTicket {
    owner: DataTicketOwnerSnapshot,
    source: SourceDescriptor,
    ranges: Vec<ByteRange>,
    epoch: u64,
}

enum ValidatedDataTicketTerminal {
    Provided(ProvideDataCommand),
    Failed(FailDataCommand),
}

/// Terminal operation validated for one outstanding data ticket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataTicketTerminalKind {
    /// Exact requested bytes were supplied.
    Provided,
    /// The host supplied one validated source failure.
    Failed(SourceFailureCode),
}

/// Immutable ownership snapshot for one outstanding data ticket.
///
/// Runtime business work consumes this value instead of retaining or re-reading a mutable
/// correlation envelope.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DataTicketOwnerSnapshot {
    worker: WorkerId,
    session: SessionId,
    request: RequestId,
    ticket: DataTicket,
    checkpoint: u64,
}

impl DataTicketOwnerSnapshot {
    /// Returns the worker epoch that owns the ticket.
    pub const fn worker(self) -> WorkerId {
        self.worker
    }

    /// Returns the session that owns the ticket.
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns the request whose suspended work owns the ticket.
    pub const fn request(self) -> RequestId {
        self.request
    }

    /// Returns the exact data-ticket identity.
    pub const fn ticket(self) -> DataTicket {
        self.ticket
    }

    /// Returns the exact resume checkpoint registered by `NeedData`.
    pub const fn checkpoint(self) -> u64 {
        self.checkpoint
    }

    const fn key(self) -> DataTicketKey {
        DataTicketKey {
            worker: self.worker,
            session: self.session,
            ticket: self.ticket,
        }
    }

    const fn session_key(self) -> SessionSourceKey {
        SessionSourceKey {
            worker: self.worker,
            session: self.session,
        }
    }
}

impl fmt::Debug for DataTicketOwnerSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataTicketOwnerSnapshot")
            .field("worker", &"[REDACTED]")
            .field("session", &"[REDACTED]")
            .field("request", &"[REDACTED]")
            .field("ticket", &"[REDACTED]")
            .field("checkpoint", &"[REDACTED]")
            .finish()
    }
}

/// Typed result of atomically committing a prepared data terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataTicketCommitOutcome {
    /// One ticket reached the supplied non-SourceChanged terminal.
    TicketCompleted {
        /// Immutable owner of the completed ticket.
        owner: DataTicketOwnerSnapshot,
        /// Exact terminal that won the ticket CAS.
        terminal: DataTicketTerminalKind,
    },
    /// A SourceChanged terminal poisoned its whole session and invalidated this many tickets.
    SessionSourceChanged {
        /// Immutable owner of the ticket that reported SourceChanged.
        owner: DataTicketOwnerSnapshot,
        /// Number of outstanding tickets atomically invalidated in the poisoned session.
        invalidated_tickets: usize,
    },
}

/// Prepared ticket terminal that has not yet consumed the outstanding ledger entry.
///
/// Resource adoption and other fallible business work happens after preparation. Call
/// [`DataTicketLedger::commit`] only after that work succeeds. Dropping this value leaves the
/// ticket outstanding. SourceChanged is the security exception: its preparation has already moved
/// the session binding to a fail-closed observed state, and dropping the token does not restore
/// Active. The actor must re-prepare it if necessary and prioritize
/// [`DataTicketLedger::commit_source_changed`].
#[must_use = "a prepared data-ticket terminal must be resolved through its required commit path"]
pub struct PendingDataTicketCompletion {
    ledger_identity: Arc<()>,
    owner: DataTicketOwnerSnapshot,
    expected_epoch: u64,
    source: SourceDescriptor,
    validated: ValidatedDataTicketTerminal,
}

impl PendingDataTicketCompletion {
    /// Returns the immutable composite owner captured from the accepted `NeedData`.
    pub const fn owner(&self) -> DataTicketOwnerSnapshot {
        self.owner
    }

    /// Returns the request whose suspended work owns the ticket.
    pub const fn request(&self) -> RequestId {
        self.owner.request()
    }

    /// Returns the exact resume checkpoint registered by `NeedData`.
    pub const fn checkpoint(&self) -> u64 {
        self.owner.checkpoint()
    }

    /// Returns the exact immutable source descriptor accepted when the session opened.
    ///
    /// Resource adoption must use this retained descriptor rather than the mutable command value
    /// originally passed to `prepare_provide_data` or `prepare_fail_data`.
    pub const fn source_descriptor(&self) -> &SourceDescriptor {
        &self.source
    }

    /// Returns the validated terminal operation.
    pub const fn terminal(&self) -> DataTicketTerminalKind {
        match &self.validated {
            ValidatedDataTicketTerminal::Provided(_) => DataTicketTerminalKind::Provided,
            ValidatedDataTicketTerminal::Failed(command) => {
                DataTicketTerminalKind::Failed(command.code)
            }
        }
    }

    /// Returns the owned, validated `ProvideData` snapshot when this terminal supplies bytes.
    ///
    /// The snapshot cannot change if the caller later mutates or drops its original command.
    pub fn provided_command(&self) -> Option<&ProvideDataCommand> {
        match &self.validated {
            ValidatedDataTicketTerminal::Provided(command) => Some(command),
            ValidatedDataTicketTerminal::Failed(_) => None,
        }
    }

    /// Returns the owned, validated `FailData` snapshot when this terminal reports a failure.
    ///
    /// The snapshot cannot change if the caller later mutates or drops its original command.
    pub fn failed_command(&self) -> Option<&FailDataCommand> {
        match &self.validated {
            ValidatedDataTicketTerminal::Provided(_) => None,
            ValidatedDataTicketTerminal::Failed(command) => Some(command),
        }
    }
}

impl fmt::Debug for PendingDataTicketCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingDataTicketCompletion")
            .field("identity", &"[REDACTED]")
            .field("request", &"[REDACTED]")
            .field("checkpoint", &"[REDACTED]")
            .field("source", &"[REDACTED]")
            .field("terminal", &self.terminal())
            .finish()
    }
}

/// Bounded outstanding `NeedData` ownership and exactly-once terminal ledger.
///
/// Keys include worker and session identity, so equal numeric ticket values in different sessions
/// remain isolated. The implementation performs no I/O and uses entry-local epochs to reject a
/// stale prepared terminal even if the same numeric key is registered again later.
pub struct DataTicketLedger {
    identity: Arc<()>,
    capacity: usize,
    session_sources: Vec<SessionSourceBinding>,
    entries: Vec<OutstandingDataTicket>,
    next_epoch: u64,
}

impl DataTicketLedger {
    /// Creates a ledger with an explicit non-zero capacity.
    pub fn new(capacity: usize) -> Result<Self, ProtocolError> {
        if capacity == 0 || capacity > MAX_OUTSTANDING_DATA_TICKETS {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidLimits));
        }
        Ok(Self {
            identity: Arc::new(()),
            capacity,
            session_sources: Vec::with_capacity(capacity),
            entries: Vec::with_capacity(capacity),
            next_epoch: 1,
        })
    }

    /// Returns the configured per-table session-binding and outstanding-ticket capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of currently outstanding tickets.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether no ticket is outstanding.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of active, source-change-observed, or poisoned session bindings.
    pub fn bound_sessions(&self) -> usize {
        self.session_sources.len()
    }

    /// Binds one immutable source descriptor to an exact worker/session pair.
    ///
    /// Rebinding the exact same active descriptor is idempotent. Descriptor drift, a poisoned
    /// session, invalid identity, and exhaustion of the explicit ledger capacity are rejected.
    pub fn bind_session(
        &mut self,
        worker: WorkerId,
        session: SessionId,
        source: &SourceDescriptor,
    ) -> Result<(), ProtocolError> {
        if worker.value() == 0 || session.value() == 0 || !valid_source_descriptor(source) {
            return Err(invalid_data_ticket());
        }
        let key = SessionSourceKey { worker, session };
        if let Some(binding) = self
            .session_sources
            .iter()
            .find(|binding| binding.key == key)
        {
            return match &binding.state {
                SessionSourceState::Active(bound) if bound == source => Ok(()),
                SessionSourceState::Active(_)
                | SessionSourceState::SourceChangeObserved(_)
                | SessionSourceState::SourceChanged => Err(invalid_data_ticket()),
            };
        }
        if self.session_sources.len() == self.capacity {
            return Err(invalid_data_ticket());
        }
        self.session_sources.push(SessionSourceBinding {
            key,
            state: SessionSourceState::Active(source.clone()),
        });
        Ok(())
    }

    /// Registers one already decoded and envelope-validated `NeedData` event.
    ///
    /// The source must match a prior [`Self::bind_session`] snapshot. The exact worker, session,
    /// request, ticket, source, ranges, and checkpoint are retained until one matching terminal is
    /// committed or lifecycle invalidation removes the entry.
    pub fn register_need_data(
        &mut self,
        correlation: &Correlation,
        event: &NeedDataEvent,
    ) -> Result<(), ProtocolError> {
        let session = correlation.session.ok_or_else(invalid_data_ticket)?;
        let request = correlation.request.ok_or_else(invalid_data_ticket)?;
        if correlation.worker.value() == 0
            || session.value() == 0
            || request.value() == 0
            || correlation.generation.is_some()
            || event.ticket.value() == 0
            || event.checkpoint == 0
            || !valid_source_identity(&event.source)
        {
            return Err(invalid_data_ticket());
        }
        let session_key = SessionSourceKey {
            worker: correlation.worker,
            session,
        };
        let session_source = self.active_session_source(session_key)?.clone();
        if event.source != session_source.identity {
            return Err(invalid_data_ticket());
        }
        validate_requested_ranges(&event.ranges, session_source.length)?;

        let key = DataTicketKey {
            worker: correlation.worker,
            session,
            ticket: event.ticket,
        };
        if self.entries.len() == self.capacity
            || self.entries.iter().any(|entry| entry.owner.key() == key)
        {
            return Err(invalid_data_ticket());
        }

        let owner = DataTicketOwnerSnapshot {
            worker: correlation.worker,
            session,
            request,
            ticket: event.ticket,
            checkpoint: event.checkpoint,
        };
        let epoch = self.next_epoch;
        self.next_epoch = self
            .next_epoch
            .checked_add(1)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
        self.entries.push(OutstandingDataTicket {
            owner,
            source: session_source,
            ranges: event.ranges.clone(),
            epoch,
        });
        Ok(())
    }

    /// Prepares an exact `ProvideData` terminal without consuming its ticket.
    ///
    /// Every segment must be the corresponding requested range, in order, with the canonical
    /// logical slot, exact byte length, and immutable-range attachment role.
    pub fn prepare_provide_data(
        &self,
        correlation: &Correlation,
        command: &ProvideDataCommand,
    ) -> Result<PendingDataTicketCompletion, ProtocolError> {
        let key = response_key(correlation, command.ticket)?;
        self.active_session_source(SessionSourceKey {
            worker: key.worker,
            session: key.session,
        })?;
        let entry = self.find(key)?;
        if !valid_source_identity(&command.source)
            || command.source != entry.source.identity
            || command.segments.is_empty()
            || command.segments.len() > PROVIDE_DATA_COMMAND_SEGMENTS_MAX_COUNT
            || command.segments.len() != entry.ranges.len()
        {
            return Err(invalid_data_ticket());
        }

        for (index, (segment, expected)) in
            command.segments.iter().zip(entry.ranges.iter()).enumerate()
        {
            if segment.range != *expected
                || usize::from(segment.slot) != index
                || segment.byte_length != expected.len
                || segment.role != DataAttachmentRole::ImmutableRangeBytes
            {
                return Err(invalid_data_ticket());
            }
        }

        Ok(entry.pending(
            &self.identity,
            ValidatedDataTicketTerminal::Provided(command.clone()),
        ))
    }

    /// Prepares an exact `FailData` terminal without consuming its ticket.
    ///
    /// `SourceChanged` requires a valid observed identity different from the expected snapshot and
    /// is never retryable. Its successful preparation immediately makes the session fail-closed
    /// until [`Self::commit_source_changed`] atomically poisons it. Every other failure forbids an
    /// observed identity and leaves the active binding unchanged.
    pub fn prepare_fail_data(
        &mut self,
        correlation: &Correlation,
        command: &FailDataCommand,
    ) -> Result<PendingDataTicketCompletion, ProtocolError> {
        let key = response_key(correlation, command.ticket)?;
        let session_key = SessionSourceKey {
            worker: key.worker,
            session: key.session,
        };
        self.session_source_for_failure(
            session_key,
            command.code == SourceFailureCode::SourceChanged,
        )?;
        let entry = self.find(key)?;
        if !valid_source_identity(&command.expected) || command.expected != entry.source.identity {
            return Err(invalid_data_ticket());
        }

        let valid_failure = match command.code {
            SourceFailureCode::SourceChanged => command.observed.as_ref().is_some_and(|observed| {
                valid_source_identity(observed)
                    && observed != &command.expected
                    && !command.retryable
            }),
            _ => command.observed.is_none(),
        };
        if !valid_failure {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSourceFailure,
            ));
        }

        let pending = entry.pending(
            &self.identity,
            ValidatedDataTicketTerminal::Failed(command.clone()),
        );
        if command.code == SourceFailureCode::SourceChanged {
            self.observe_source_change(session_key)?;
        }
        Ok(pending)
    }

    /// Atomically consumes one exact non-SourceChanged terminal.
    ///
    /// A previously observed SourceChanged blocks every individual terminal in that session.
    /// SourceChanged itself must use [`Self::commit_source_changed`].
    pub fn commit(
        &mut self,
        pending: PendingDataTicketCompletion,
    ) -> Result<DataTicketCommitOutcome, ProtocolError> {
        if !Arc::ptr_eq(&self.identity, &pending.ledger_identity) {
            return Err(invalid_data_ticket());
        }
        if matches!(
            &pending.validated,
            ValidatedDataTicketTerminal::Failed(FailDataCommand {
                code: SourceFailureCode::SourceChanged,
                ..
            })
        ) {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSourceFailure,
            ));
        }
        self.active_session_source(pending.owner.session_key())?;
        let position = self
            .entries
            .iter()
            .position(|entry| {
                entry.owner.key() == pending.owner.key() && entry.epoch == pending.expected_epoch
            })
            .ok_or_else(invalid_data_ticket)?;
        self.entries.remove(position);
        Ok(DataTicketCommitOutcome::TicketCompleted {
            owner: pending.owner,
            terminal: pending.terminal(),
        })
    }

    /// Atomically commits a validated SourceChanged and poisons its whole session.
    ///
    /// The actor must call this path immediately after observing and preparing SourceChanged,
    /// before unrelated business work. Preparation already moves the binding to a fail-closed
    /// observed state, so earlier prepared ProvideData terminals cannot win while this commit is
    /// pending.
    pub fn commit_source_changed(
        &mut self,
        pending: PendingDataTicketCompletion,
    ) -> Result<DataTicketCommitOutcome, ProtocolError> {
        if !Arc::ptr_eq(&self.identity, &pending.ledger_identity)
            || !matches!(
                &pending.validated,
                ValidatedDataTicketTerminal::Failed(FailDataCommand {
                    code: SourceFailureCode::SourceChanged,
                    ..
                })
            )
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidSourceFailure,
            ));
        }
        let session_key = pending.owner.session_key();
        let binding_position = self
            .session_sources
            .iter()
            .position(|binding| binding.key == session_key)
            .ok_or_else(invalid_data_ticket)?;
        match &self.session_sources[binding_position].state {
            SessionSourceState::SourceChangeObserved(source) if source == &pending.source => {}
            SessionSourceState::Active(_)
            | SessionSourceState::SourceChangeObserved(_)
            | SessionSourceState::SourceChanged => return Err(invalid_data_ticket()),
        }
        if !self.entries.iter().any(|entry| {
            entry.owner.key() == pending.owner.key() && entry.epoch == pending.expected_epoch
        }) {
            return Err(invalid_data_ticket());
        }

        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.owner.session_key() != session_key);
        let invalidated = before - self.entries.len();
        self.session_sources[binding_position].state = SessionSourceState::SourceChanged;
        Ok(DataTicketCommitOutcome::SessionSourceChanged {
            owner: pending.owner,
            invalidated_tickets: invalidated,
        })
    }

    /// Invalidates every outstanding ticket owned by one exact worker/session pair.
    ///
    /// Its active or poisoned source binding is removed in the same operation. Returns the number
    /// of removed ticket entries; prepared terminals for removed entries become stale.
    pub fn invalidate_session(&mut self, worker: WorkerId, session: SessionId) -> usize {
        let session_key = SessionSourceKey { worker, session };
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.owner.session_key() != session_key);
        self.session_sources
            .retain(|binding| binding.key != session_key);
        before - self.entries.len()
    }

    /// Invalidates every outstanding ticket owned by one worker epoch.
    ///
    /// Every active or poisoned source binding for the worker is removed in the same operation.
    /// Returns the number of removed ticket entries.
    pub fn invalidate_worker(&mut self, worker: WorkerId) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.owner.worker() != worker);
        self.session_sources
            .retain(|binding| binding.key.worker != worker);
        before - self.entries.len()
    }

    fn find(&self, key: DataTicketKey) -> Result<&OutstandingDataTicket, ProtocolError> {
        self.entries
            .iter()
            .find(|entry| entry.owner.key() == key)
            .ok_or_else(invalid_data_ticket)
    }

    fn active_session_source(
        &self,
        key: SessionSourceKey,
    ) -> Result<&SourceDescriptor, ProtocolError> {
        let binding = self
            .session_sources
            .iter()
            .find(|binding| binding.key == key)
            .ok_or_else(invalid_data_ticket)?;
        match &binding.state {
            SessionSourceState::Active(source) => Ok(source),
            SessionSourceState::SourceChangeObserved(_) | SessionSourceState::SourceChanged => {
                Err(invalid_data_ticket())
            }
        }
    }

    fn session_source_for_failure(
        &self,
        key: SessionSourceKey,
        source_changed: bool,
    ) -> Result<&SourceDescriptor, ProtocolError> {
        let binding = self
            .session_sources
            .iter()
            .find(|binding| binding.key == key)
            .ok_or_else(invalid_data_ticket)?;
        match (&binding.state, source_changed) {
            (SessionSourceState::Active(source), _)
            | (SessionSourceState::SourceChangeObserved(source), true) => Ok(source),
            (SessionSourceState::SourceChangeObserved(_), false)
            | (SessionSourceState::SourceChanged, _) => Err(invalid_data_ticket()),
        }
    }

    fn observe_source_change(&mut self, key: SessionSourceKey) -> Result<(), ProtocolError> {
        let binding = self
            .session_sources
            .iter_mut()
            .find(|binding| binding.key == key)
            .ok_or_else(invalid_data_ticket)?;
        if let SessionSourceState::Active(source) = &binding.state {
            binding.state = SessionSourceState::SourceChangeObserved(source.clone());
        }
        match &binding.state {
            SessionSourceState::SourceChangeObserved(_) => Ok(()),
            SessionSourceState::Active(_) | SessionSourceState::SourceChanged => {
                Err(invalid_data_ticket())
            }
        }
    }
}

impl fmt::Debug for DataTicketLedger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DataTicketLedger")
            .field("capacity", &self.capacity)
            .field("bound_sessions", &self.session_sources.len())
            .field("outstanding", &self.entries.len())
            .field("session_sources", &"[REDACTED]")
            .field("entries", &"[REDACTED]")
            .finish()
    }
}

impl OutstandingDataTicket {
    fn pending(
        &self,
        ledger_identity: &Arc<()>,
        validated: ValidatedDataTicketTerminal,
    ) -> PendingDataTicketCompletion {
        PendingDataTicketCompletion {
            ledger_identity: Arc::clone(ledger_identity),
            owner: self.owner,
            expected_epoch: self.epoch,
            source: self.source.clone(),
            validated,
        }
    }
}

fn response_key(
    correlation: &Correlation,
    ticket: DataTicket,
) -> Result<DataTicketKey, ProtocolError> {
    let session = correlation.session.ok_or_else(invalid_data_ticket)?;
    if correlation.worker.value() == 0
        || session.value() == 0
        || correlation.request.is_some()
        || correlation.generation.is_some()
        || ticket.value() == 0
    {
        return Err(invalid_data_ticket());
    }
    Ok(DataTicketKey {
        worker: correlation.worker,
        session,
        ticket,
    })
}

fn validate_requested_ranges(
    ranges: &[ByteRange],
    source_length: Option<u64>,
) -> Result<(), ProtocolError> {
    if ranges.is_empty() || ranges.len() > NEED_DATA_EVENT_RANGES_MAX_COUNT {
        return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidDataRange));
    }

    let mut previous_end = None;
    let mut total = 0_u64;
    for range in ranges {
        if range.len == 0 || range.len > MAX_DATA_SEGMENT_BYTES {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidDataRange));
        }
        let end = range
            .start
            .checked_add(range.len)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
        if previous_end.is_some_and(|prior| range.start < prior)
            || source_length.is_some_and(|length| end > length)
        {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidDataRange));
        }
        total = total
            .checked_add(range.len)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
        if total > MAX_DATA_TICKET_BYTES {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidDataRange));
        }
        previous_end = Some(end);
    }
    Ok(())
}

fn valid_source_identity(source: &SourceIdentity) -> bool {
    source.revision != 0 && source.stable_id.iter().any(|byte| *byte != 0)
}

fn valid_source_descriptor(source: &SourceDescriptor) -> bool {
    valid_source_identity(&source.identity)
}

const fn invalid_data_ticket() -> ProtocolError {
    ProtocolError::for_code(ProtocolErrorCode::InvalidDataTicket)
}
