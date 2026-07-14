use std::fmt;
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};

use pdf_rs_bytes::{
    ByteSource, DataTicket, JobId, RangeResponse, RangeStore, RangeStoreLimits, ResumeCheckpoint,
    SourceError, SourceErrorCategory, SourceSnapshot, SupplyOutcome, TicketStatus,
};

use crate::RangeResumeError;

static NEXT_RANGE_RESUME_ARBITER_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque process-local identity of one Range-resume arbiter.
///
/// Identities are allocated only by [`RangeResumeArbiter::new`] and bind
/// move-only permits to the arbiter that observed ticket completion.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RangeResumeArbiterId(u64);

impl RangeResumeArbiterId {
    fn allocate() -> Result<Self, RangeResumeError> {
        NEXT_RANGE_RESUME_ARBITER_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map(Self)
            .map_err(|_| RangeResumeError::arbiter_failed())
    }
}

/// Opaque generation retained with one resumable runtime job.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RangeResumeGeneration(u64);

impl RangeResumeGeneration {
    /// Creates a runtime generation identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric value for protocol adaptation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Complete scheduler identity retained while one job waits for source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeResumeTarget {
    job: JobId,
    checkpoint: ResumeCheckpoint,
    generation: RangeResumeGeneration,
}

impl RangeResumeTarget {
    /// Creates one exact job, checkpoint, and generation resume target.
    pub const fn new(
        job: JobId,
        checkpoint: ResumeCheckpoint,
        generation: RangeResumeGeneration,
    ) -> Self {
        Self {
            job,
            checkpoint,
            generation,
        }
    }

    /// Returns the waiting job identity.
    pub const fn job(self) -> JobId {
        self.job
    }

    /// Returns the retained parser checkpoint.
    pub const fn checkpoint(self) -> ResumeCheckpoint {
        self.checkpoint
    }

    /// Returns the runtime generation captured when the job suspended.
    pub const fn generation(self) -> RangeResumeGeneration {
        self.generation
    }
}

/// Public phase of a Range-resume arbiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeResumePhase {
    /// The snapshot-bound store accepts reads, registrations, and host responses.
    Active,
    /// Snapshot integrity failed and all Range resources were released.
    SourceChanged,
    /// An internal subscription invariant failed and all resources were released.
    Failed,
    /// Explicit close completed and all Range resources were released.
    Closed,
}

/// Result of registering one Pending result with runtime ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeResumeRegistrationOutcome {
    /// A new bounded registration was retained.
    Registered,
    /// The exact ticket and target were already retained.
    AlreadyRegistered,
}

/// Result of cancelling one exact job generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeResumeCancelOutcome {
    /// Pending or queued work was removed and will never be requeued.
    Cancelled {
        /// The complete removed scheduler target.
        target: RangeResumeTarget,
    },
    /// No registration matched the supplied job and generation.
    NotPending,
}

/// One one-shot scheduler disposition taken from the arbiter.
#[derive(Debug, Eq, PartialEq)]
pub enum RangeResumeDispatch {
    /// The completed ticket produced this one-shot resume permit.
    Requeue(RangeResumePermit),
    /// No completed target remains queued.
    Empty,
}

/// Move-only evidence that one exact Range ticket completed for a resume target.
///
/// Only [`RangeResumeArbiter`] can create a permit. Taking it removes the
/// underlying registration, and consuming code must still validate the issuing
/// arbiter plus the ticket, job, checkpoint, and generation before running
/// parser code.
#[derive(Debug, Eq, PartialEq)]
pub struct RangeResumePermit {
    arbiter_id: RangeResumeArbiterId,
    ticket: DataTicket,
    target: RangeResumeTarget,
}

impl RangeResumePermit {
    /// Returns the opaque arbiter identity that issued this permit.
    pub const fn arbiter_id(&self) -> RangeResumeArbiterId {
        self.arbiter_id
    }

    /// Returns the completed byte ticket carried by this one-shot permit.
    pub const fn ticket(&self) -> DataTicket {
        self.ticket
    }

    /// Returns the complete job, checkpoint, and generation resume target.
    pub const fn target(&self) -> RangeResumeTarget {
        self.target
    }
}

/// Current source and scheduler resources owned exclusively by one arbiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeResumeResources {
    registrations: usize,
    pending_tickets: usize,
    ready_requeues: usize,
    cached_bytes: u64,
    registration_metadata_bytes: u64,
    source_resident_bytes: u64,
    resident_bytes: u64,
}

impl RangeResumeResources {
    const ZERO: Self = Self {
        registrations: 0,
        pending_tickets: 0,
        ready_requeues: 0,
        cached_bytes: 0,
        registration_metadata_bytes: 0,
        source_resident_bytes: 0,
        resident_bytes: 0,
    };

    /// Returns pending plus completed-but-not-taken registrations.
    pub const fn registrations(self) -> usize {
        self.registrations
    }

    /// Returns distinct pending tickets represented by registered jobs.
    pub const fn pending_tickets(self) -> usize {
        self.pending_tickets
    }

    /// Returns completed targets not yet consumed by [`RangeResumeArbiter::take_requeue`].
    pub const fn ready_requeues(self) -> usize {
        self.ready_requeues
    }

    /// Returns unique immutable source bytes retained by the Range store.
    pub const fn cached_bytes(self) -> u64 {
        self.cached_bytes
    }

    /// Returns allocator-capacity bytes precharged for arbiter registrations.
    pub const fn registration_metadata_bytes(self) -> u64 {
        self.registration_metadata_bytes
    }

    /// Returns Range-store backing and in-flight/coalescing capacity currently charged.
    pub const fn source_resident_bytes(self) -> u64 {
        self.source_resident_bytes
    }

    /// Returns source backing plus precharged arbiter registration metadata.
    pub const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }
}

/// Stable evidence captured before a terminal transition drops the Range store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeResumeReleaseReport {
    released_registrations: usize,
    released_pending_tickets: usize,
    released_ready_requeues: usize,
    released_cached_bytes: u64,
    released_registration_metadata_bytes: u64,
    released_source_resident_bytes: u64,
    released_resident_bytes: u64,
}

impl RangeResumeReleaseReport {
    fn from_resources(resources: RangeResumeResources) -> Self {
        Self {
            released_registrations: resources.registrations,
            released_pending_tickets: resources.pending_tickets,
            released_ready_requeues: resources.ready_requeues,
            released_cached_bytes: resources.cached_bytes,
            released_registration_metadata_bytes: resources.registration_metadata_bytes,
            released_source_resident_bytes: resources.source_resident_bytes,
            released_resident_bytes: resources.resident_bytes,
        }
    }

    /// Returns pending plus queued registrations dropped by the transition.
    pub const fn released_registrations(self) -> usize {
        self.released_registrations
    }

    /// Returns distinct pending ticket identities dropped by the transition.
    pub const fn released_pending_tickets(self) -> usize {
        self.released_pending_tickets
    }

    /// Returns completed scheduler targets discarded by the transition.
    pub const fn released_ready_requeues(self) -> usize {
        self.released_ready_requeues
    }

    /// Returns unique cached source bytes dropped with the store.
    pub const fn released_cached_bytes(self) -> u64 {
        self.released_cached_bytes
    }

    /// Returns allocator-capacity bytes dropped with bounded registration metadata.
    pub const fn released_registration_metadata_bytes(self) -> u64 {
        self.released_registration_metadata_bytes
    }

    /// Returns source backing capacity detached from the arbiter with the store.
    ///
    /// An independently retained stable byte slice may keep shared backing alive
    /// after the store is dropped; this is owner accounting, not allocator telemetry.
    pub const fn released_source_resident_bytes(self) -> u64 {
        self.released_source_resident_bytes
    }

    /// Returns registration metadata plus source capacity detached by the transition.
    pub const fn released_resident_bytes(self) -> u64 {
        self.released_resident_bytes
    }
}

/// Summary of one successful host response or snapshot observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeResumeSupplyOutcome {
    cached_bytes: u64,
    ready_tickets: usize,
    queued_requeues: usize,
}

impl RangeResumeSupplyOutcome {
    /// Returns total unique bytes cached after the update.
    pub const fn cached_bytes(self) -> u64 {
        self.cached_bytes
    }

    /// Returns tickets that became ready during this update.
    pub const fn ready_tickets(self) -> usize {
        self.ready_tickets
    }

    /// Returns scheduler targets newly queued, but not resumed inline.
    pub const fn queued_requeues(self) -> usize {
        self.queued_requeues
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationPhase {
    Pending,
    Ready { sequence: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Registration {
    ticket: DataTicket,
    target: RangeResumeTarget,
    phase: RegistrationPhase,
}

struct ActiveRangeResume {
    store: RangeStore,
    registrations: Vec<Registration>,
    registration_limit: usize,
    registration_metadata_bytes: u64,
    pending_tickets: usize,
    ready_requeues: usize,
    next_ready_sequence: u64,
    cached_bytes: u64,
}

impl ActiveRangeResume {
    fn resources(&self) -> RangeResumeResources {
        let source_resident_bytes = self.store.resident_bytes();
        let resident_bytes = source_resident_bytes
            .checked_add(self.registration_metadata_bytes)
            .expect("validated Range and registration hard ceilings fit u64");
        RangeResumeResources {
            registrations: self.registrations.len(),
            pending_tickets: self.pending_tickets,
            ready_requeues: self.ready_requeues,
            cached_bytes: self.cached_bytes,
            registration_metadata_bytes: self.registration_metadata_bytes,
            source_resident_bytes,
            resident_bytes,
        }
    }

    fn process_ready_tickets(
        &mut self,
        ready_tickets: &[DataTicket],
    ) -> Result<usize, RangeResumeError> {
        let mut queued = 0_usize;
        for &ticket in ready_tickets {
            let subscriptions = self
                .store
                .take_subscriptions(ticket)
                .map_err(RangeResumeError::from_source)?;
            if subscriptions.is_empty()
                || subscriptions.iter().any(|subscription| {
                    !self.registrations.iter().any(|registration| {
                        registration.ticket == ticket
                            && registration.target.job() == subscription.job()
                            && registration.target.checkpoint() == subscription.checkpoint()
                            && registration.phase == RegistrationPhase::Pending
                    })
                })
            {
                return Err(RangeResumeError::unregistered_subscription());
            }
            let next_sequence = self
                .next_ready_sequence
                .checked_add(
                    u64::try_from(subscriptions.len())
                        .map_err(|_| RangeResumeError::arbiter_failed())?,
                )
                .ok_or_else(RangeResumeError::arbiter_failed)?;
            let next_ready_requeues = self
                .ready_requeues
                .checked_add(subscriptions.len())
                .ok_or_else(RangeResumeError::arbiter_failed)?;
            let next_queued = queued
                .checked_add(subscriptions.len())
                .ok_or_else(RangeResumeError::arbiter_failed)?;
            let mut sequence = self.next_ready_sequence;
            for subscription in subscriptions {
                let registration = self
                    .registrations
                    .iter_mut()
                    .find(|registration| {
                        registration.ticket == ticket
                            && registration.target.job() == subscription.job()
                            && registration.target.checkpoint() == subscription.checkpoint()
                            && registration.phase == RegistrationPhase::Pending
                    })
                    .expect("subscriptions were validated before mutation");
                registration.phase = RegistrationPhase::Ready { sequence };
                sequence = sequence
                    .checked_add(1)
                    .ok_or_else(RangeResumeError::arbiter_failed)?;
            }
            self.pending_tickets = self
                .pending_tickets
                .checked_sub(1)
                .ok_or_else(RangeResumeError::arbiter_failed)?;
            self.ready_requeues = next_ready_requeues;
            self.next_ready_sequence = next_sequence;
            queued = next_queued;
            self.store
                .release_ticket(ticket)
                .map_err(RangeResumeError::from_source)?;
        }
        Ok(queued)
    }

    fn rollback_subscription(
        &self,
        ticket: DataTicket,
        job: JobId,
    ) -> Result<(), RangeResumeError> {
        self.store
            .unsubscribe(ticket, job)
            .map_err(RangeResumeError::from_source)?;
        if self
            .store
            .ticket_status(ticket)
            .map_err(RangeResumeError::from_source)?
            == TicketStatus::Abandoned
        {
            let subscriptions = self
                .store
                .take_subscriptions(ticket)
                .map_err(RangeResumeError::from_source)?;
            if !subscriptions.is_empty() {
                return Err(RangeResumeError::unregistered_subscription());
            }
            self.store
                .release_ticket(ticket)
                .map_err(RangeResumeError::from_source)?;
        }
        Ok(())
    }
}

enum RangeResumeState {
    Active(ActiveRangeResume),
    SourceChanged {
        report: RangeResumeReleaseReport,
        source_error: Option<SourceError>,
    },
    Failed {
        report: RangeResumeReleaseReport,
    },
    Closed {
        report: RangeResumeReleaseReport,
    },
}

/// Actor-style owner that turns terminal Range tickets into one-shot requeues.
///
/// The arbiter owns exactly one [`RangeStore`] bound to [`Self::snapshot`]. A
/// synchronous runtime turn borrows [`Self::byte_source`], polls a resumable job,
/// drops that borrow, and immediately records every Pending result through
/// [`Self::register_pending`]. Host responses are supplied only on later turns.
/// Data arrival marks scheduler targets ready but never calls parser code inline.
///
/// This type deliberately requires `&mut self` for host updates, cancellation,
/// dispatch, and close. That models one logical session actor and prevents those
/// transitions from racing a borrowed byte source in safe Rust.
pub struct RangeResumeArbiter {
    arbiter_id: RangeResumeArbiterId,
    snapshot: SourceSnapshot,
    state: RangeResumeState,
}

impl RangeResumeArbiter {
    /// Creates a snapshot-bound store and preallocates bounded resume metadata.
    pub fn new(
        snapshot: SourceSnapshot,
        store_limits: RangeStoreLimits,
    ) -> Result<Self, RangeResumeError> {
        let arbiter_id = RangeResumeArbiterId::allocate()?;
        let registration_limit = store_limits.max_total_subscriptions();
        let mut registrations = Vec::new();
        registrations
            .try_reserve_exact(registration_limit)
            .map_err(|_| {
                RangeResumeError::registration_limit(registration_limit, registration_limit)
            })?;
        let registration_metadata_bytes = registrations
            .capacity()
            .checked_mul(mem::size_of::<Registration>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or_else(RangeResumeError::arbiter_failed)?;
        store_limits
            .max_resident_bytes()
            .checked_add(registration_metadata_bytes)
            .ok_or_else(RangeResumeError::arbiter_failed)?;
        let store =
            RangeStore::new(snapshot, store_limits).map_err(RangeResumeError::from_source)?;
        Ok(Self {
            arbiter_id,
            snapshot,
            state: RangeResumeState::Active(ActiveRangeResume {
                store,
                registrations,
                registration_limit,
                registration_metadata_bytes,
                pending_tickets: 0,
                ready_requeues: 0,
                next_ready_sequence: 1,
                cached_bytes: 0,
            }),
        })
    }

    /// Returns the opaque identity carried by every permit from this arbiter.
    pub const fn arbiter_id(&self) -> RangeResumeArbiterId {
        self.arbiter_id
    }

    /// Returns the immutable source snapshot retained across every phase.
    pub const fn snapshot(&self) -> SourceSnapshot {
        self.snapshot
    }

    /// Returns the current public phase.
    pub const fn phase(&self) -> RangeResumePhase {
        match self.state {
            RangeResumeState::Active(_) => RangeResumePhase::Active,
            RangeResumeState::SourceChanged { .. } => RangeResumePhase::SourceChanged,
            RangeResumeState::Failed { .. } => RangeResumePhase::Failed,
            RangeResumeState::Closed { .. } => RangeResumePhase::Closed,
        }
    }

    /// Borrows the active snapshot-bound byte source for one synchronous job poll.
    ///
    /// The concrete store and all host completion methods remain private. Runtime
    /// must immediately register a returned Pending ticket before another turn.
    pub fn byte_source(&self) -> Result<&dyn ByteSource, RangeResumeError> {
        match &self.state {
            RangeResumeState::Active(active) => Ok(&active.store),
            _ => Err(self.terminal_error()),
        }
    }

    /// Retains one complete Pending ticket scheduler target.
    ///
    /// Re-registering the exact same ticket and target is idempotent. One job ID
    /// cannot own a different checkpoint, generation, or ticket until its prior
    /// registration has been cancelled or dispatched.
    pub fn register_pending(
        &mut self,
        ticket: DataTicket,
        target: RangeResumeTarget,
    ) -> Result<RangeResumeRegistrationOutcome, RangeResumeError> {
        let status = match &self.state {
            RangeResumeState::Active(active) => active.store.ticket_status(ticket),
            _ => return Err(self.terminal_error()),
        };
        let status = match status {
            Ok(status) => status,
            Err(error) if error.category() == SourceErrorCategory::Integrity => {
                self.transition_source_changed(Some(error));
                return Err(RangeResumeError::source_changed(Some(error)));
            }
            Err(error) => return Err(RangeResumeError::from_source(error)),
        };
        if status == TicketStatus::SourceChanged {
            self.transition_source_changed(None);
            return Err(RangeResumeError::source_changed(None));
        }
        if !matches!(status, TicketStatus::Pending { .. }) {
            self.transition_failed();
            return Err(RangeResumeError::ticket_not_pending());
        }

        let active = match &mut self.state {
            RangeResumeState::Active(active) => active,
            _ => unreachable!("registration terminal transition returned above"),
        };
        if let Some(existing) = active
            .registrations
            .iter()
            .find(|registration| registration.target.job() == target.job())
            .copied()
        {
            if existing.ticket == ticket && existing.target == target {
                return Ok(RangeResumeRegistrationOutcome::AlreadyRegistered);
            }
            if existing.ticket != ticket
                && let Err(error) = active.rollback_subscription(ticket, target.job())
            {
                self.transition_failed();
                return Err(error);
            }
            return Err(RangeResumeError::registration_conflict());
        }
        if active.registrations.len() >= active.registration_limit {
            let attempted = active.registrations.len().saturating_add(1);
            let rollback = active.rollback_subscription(ticket, target.job());
            if let Err(error) = rollback {
                self.transition_failed();
                return Err(error);
            }
            return Err(RangeResumeError::registration_limit(
                active.registration_limit,
                attempted,
            ));
        }
        if !active.registrations.iter().any(|registration| {
            registration.ticket == ticket && registration.phase == RegistrationPhase::Pending
        }) {
            active.pending_tickets += 1;
        }
        active.registrations.push(Registration {
            ticket,
            target,
            phase: RegistrationPhase::Pending,
        });
        Ok(RangeResumeRegistrationOutcome::Registered)
    }

    /// Supplies one validated host response and queues newly ready targets.
    pub fn supply(
        &mut self,
        response: RangeResponse,
    ) -> Result<RangeResumeSupplyOutcome, RangeResumeError> {
        let result = match &self.state {
            RangeResumeState::Active(active) => active.store.supply(response),
            _ => return Err(self.terminal_error()),
        };
        self.finish_store_update(result)
    }

    /// Observes complete snapshot metadata and queues tickets resolved by EOF.
    pub fn observe_snapshot(
        &mut self,
        observed: SourceSnapshot,
    ) -> Result<RangeResumeSupplyOutcome, RangeResumeError> {
        let result = match &self.state {
            RangeResumeState::Active(active) => active.store.observe_snapshot(observed),
            _ => return Err(self.terminal_error()),
        };
        self.finish_store_update(result)
    }

    /// Atomically terminates the arbiter for an externally detected source change.
    ///
    /// Repeating the signal returns the same release report. A prior close or
    /// invariant failure remains the winning stable terminal state.
    pub fn signal_source_changed(&mut self) -> Result<RangeResumeReleaseReport, RangeResumeError> {
        match &self.state {
            RangeResumeState::SourceChanged { report, .. } => return Ok(*report),
            RangeResumeState::Closed { .. } | RangeResumeState::Failed { .. } => {
                return Err(self.terminal_error());
            }
            RangeResumeState::Active(active) => {
                if let Err(error) = active.store.signal_source_changed() {
                    let wrapped = RangeResumeError::from_source(error);
                    self.transition_failed();
                    return Err(wrapped);
                }
            }
        }
        Ok(self.transition_source_changed(None))
    }

    /// Cancels one exact job generation without disturbing shared subscribers.
    ///
    /// A completed target cancelled before dispatch is discarded. Repeating the
    /// same cancellation returns [`RangeResumeCancelOutcome::NotPending`].
    pub fn cancel(
        &mut self,
        job: JobId,
        generation: RangeResumeGeneration,
    ) -> Result<RangeResumeCancelOutcome, RangeResumeError> {
        let active = match &mut self.state {
            RangeResumeState::Active(active) => active,
            _ => return Err(self.terminal_error()),
        };
        let Some(index) = active.registrations.iter().position(|registration| {
            registration.target.job() == job && registration.target.generation() == generation
        }) else {
            return Ok(RangeResumeCancelOutcome::NotPending);
        };
        let registration = active.registrations.remove(index);
        match registration.phase {
            RegistrationPhase::Pending => {
                if !active.registrations.iter().any(|other| {
                    other.ticket == registration.ticket && other.phase == RegistrationPhase::Pending
                }) {
                    let Some(remaining) = active.pending_tickets.checked_sub(1) else {
                        self.transition_failed();
                        return Err(RangeResumeError::arbiter_failed());
                    };
                    active.pending_tickets = remaining;
                }
                if let Err(error) = active.rollback_subscription(registration.ticket, job) {
                    self.transition_failed();
                    return Err(error);
                }
            }
            RegistrationPhase::Ready { .. } => {
                let Some(remaining) = active.ready_requeues.checked_sub(1) else {
                    self.transition_failed();
                    return Err(RangeResumeError::arbiter_failed());
                };
                active.ready_requeues = remaining;
            }
        }
        Ok(RangeResumeCancelOutcome::Cancelled {
            target: registration.target,
        })
    }

    /// Takes the earliest completed target as a move-only permit exactly once.
    ///
    /// The returned permit carries the completed ticket and captured target. The
    /// execution owner must compare all identities with current job state before
    /// polling parser code; a stale permit is simply consumed and dropped.
    pub fn take_requeue(&mut self) -> Result<RangeResumeDispatch, RangeResumeError> {
        let active = match &mut self.state {
            RangeResumeState::Active(active) => active,
            _ => return Err(self.terminal_error()),
        };
        let next = active
            .registrations
            .iter()
            .enumerate()
            .filter_map(|(index, registration)| match registration.phase {
                RegistrationPhase::Pending => None,
                RegistrationPhase::Ready { sequence } => Some((index, sequence)),
            })
            .min_by_key(|(_, sequence)| *sequence);
        let Some((index, _)) = next else {
            return Ok(RangeResumeDispatch::Empty);
        };
        let registration = active.registrations.remove(index);
        let Some(remaining) = active.ready_requeues.checked_sub(1) else {
            self.transition_failed();
            return Err(RangeResumeError::arbiter_failed());
        };
        active.ready_requeues = remaining;
        Ok(RangeResumeDispatch::Requeue(RangeResumePermit {
            arbiter_id: self.arbiter_id,
            ticket: registration.ticket,
            target: registration.target,
        }))
    }

    /// Returns current owned resources, which are all zero after any terminal phase.
    pub fn resources(&self) -> RangeResumeResources {
        match &self.state {
            RangeResumeState::Active(active) => active.resources(),
            RangeResumeState::SourceChanged { .. }
            | RangeResumeState::Failed { .. }
            | RangeResumeState::Closed { .. } => RangeResumeResources::ZERO,
        }
    }

    /// Drops the complete Range store and returns an idempotent release report.
    ///
    /// If source change or an invariant failure already won, close returns that
    /// terminal transition's saved report without rewriting its phase.
    pub fn close(&mut self) -> RangeResumeReleaseReport {
        match &self.state {
            RangeResumeState::SourceChanged { report, .. }
            | RangeResumeState::Failed { report }
            | RangeResumeState::Closed { report } => *report,
            RangeResumeState::Active(active) => {
                let report = RangeResumeReleaseReport::from_resources(active.resources());
                let previous = mem::replace(&mut self.state, RangeResumeState::Closed { report });
                drop(previous);
                report
            }
        }
    }

    /// Returns the stable report after close, source change, or invariant failure.
    pub const fn release_report(&self) -> Option<RangeResumeReleaseReport> {
        match self.state {
            RangeResumeState::Active(_) => None,
            RangeResumeState::SourceChanged { report, .. }
            | RangeResumeState::Failed { report }
            | RangeResumeState::Closed { report } => Some(report),
        }
    }

    fn finish_store_update(
        &mut self,
        result: Result<SupplyOutcome, SourceError>,
    ) -> Result<RangeResumeSupplyOutcome, RangeResumeError> {
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) if error.category() == SourceErrorCategory::Integrity => {
                self.transition_source_changed(Some(error));
                return Err(RangeResumeError::source_changed(Some(error)));
            }
            Err(error) => return Err(RangeResumeError::from_source(error)),
        };
        let cached_bytes = outcome.cached_bytes();
        let ready_tickets = outcome.ready_tickets().len();
        let queued = {
            let active = match &mut self.state {
                RangeResumeState::Active(active) => active,
                _ => unreachable!("store update began only in the active phase"),
            };
            active.cached_bytes = cached_bytes;
            active.process_ready_tickets(outcome.ready_tickets())
        };
        let queued_requeues = match queued {
            Ok(queued) => queued,
            Err(error) => {
                self.transition_failed();
                return Err(error);
            }
        };
        Ok(RangeResumeSupplyOutcome {
            cached_bytes,
            ready_tickets,
            queued_requeues,
        })
    }

    fn transition_source_changed(
        &mut self,
        source_error: Option<SourceError>,
    ) -> RangeResumeReleaseReport {
        let report = match &self.state {
            RangeResumeState::Active(active) => {
                RangeResumeReleaseReport::from_resources(active.resources())
            }
            RangeResumeState::SourceChanged { report, .. }
            | RangeResumeState::Failed { report }
            | RangeResumeState::Closed { report } => return *report,
        };
        let previous = mem::replace(
            &mut self.state,
            RangeResumeState::SourceChanged {
                report,
                source_error,
            },
        );
        drop(previous);
        report
    }

    fn transition_failed(&mut self) -> RangeResumeReleaseReport {
        let report = match &self.state {
            RangeResumeState::Active(active) => {
                RangeResumeReleaseReport::from_resources(active.resources())
            }
            RangeResumeState::SourceChanged { report, .. }
            | RangeResumeState::Failed { report }
            | RangeResumeState::Closed { report } => return *report,
        };
        let previous = mem::replace(&mut self.state, RangeResumeState::Failed { report });
        drop(previous);
        report
    }

    fn terminal_error(&self) -> RangeResumeError {
        match &self.state {
            RangeResumeState::Active(_) => {
                RangeResumeError::from_source(SourceError::source_unavailable())
            }
            RangeResumeState::SourceChanged { source_error, .. } => {
                RangeResumeError::source_changed(*source_error)
            }
            RangeResumeState::Failed { .. } => RangeResumeError::arbiter_failed(),
            RangeResumeState::Closed { .. } => RangeResumeError::closed(),
        }
    }
}

impl fmt::Debug for RangeResumeArbiter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RangeResumeArbiter")
            .field("snapshot", &self.snapshot)
            .field("phase", &self.phase())
            .field("resources", &self.resources())
            .field("release_report", &self.release_report())
            .finish()
    }
}
